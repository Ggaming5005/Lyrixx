//! Keeping track of the song position between source readings.
//!
//! Sources are polled every few hundred milliseconds and some (Windows) only
//! refresh their position when something happens, so the clock extrapolates:
//! `position = anchor_position + (now - anchor_time) * rate` while playing.

use crate::types::{PlaybackSnapshot, PlaybackStatus, Track};
use std::time::{Duration, Instant};

/// How far a reported position may drift from the prediction before it counts
/// as a seek, in milliseconds.
pub const SEEK_TOLERANCE_MS: u64 = 1_500;

/// Two `position_at` values this close describe the same moment: sources that
/// convert a wall-clock timestamp to an `Instant` jitter a little on every
/// read. Well below the shortest poll interval (100 ms).
const SAME_MOMENT: Duration = Duration::from_millis(50);

/// What a new snapshot changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockEvent {
    /// First snapshot, or a different track (compared on title, artist, album and duration).
    TrackChanged,
    /// Same track, but the position jumped by more than [`SEEK_TOLERANCE_MS`].
    Seeked,
    /// Paused or stopped → playing.
    Resumed,
    /// Playing → paused.
    Paused,
    /// → stopped.
    Stopped,
    /// Nothing a listener would notice.
    Steady,
}

/// Song position tracking. See the module docs.
#[derive(Debug, Default)]
pub struct SyncClock {
    /// `None` before the first snapshot and after [`SyncClock::reset`].
    state: Option<State>,
}

/// What identifies a track for [`ClockEvent::TrackChanged`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackIdentity {
    title: String,
    artist: String,
    album: Option<String>,
    duration_ms: Option<u64>,
}

impl TrackIdentity {
    fn of(track: &Track) -> Self {
        Self {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms,
        }
    }

    fn matches(&self, track: &Track) -> bool {
        self.title == track.title
            && self.artist == track.artist
            && self.album == track.album
            && self.duration_ms == track.duration_ms
    }
}

/// Everything the clock knows about the current track.
#[derive(Debug, Clone)]
struct State {
    track: TrackIdentity,
    status: PlaybackStatus,
    /// Position at `anchor_at`, as reported (not clamped).
    anchor_ms: u64,
    anchor_at: Instant,
    /// Sanitized playback rate (finite).
    rate: f64,
    /// Position from the most recent snapshot, to spot stale repeats.
    last_reported_ms: u64,
    /// `position_at` of the most recent snapshot.
    last_reported_at: Instant,
    /// The most recent snapshot echoed the one before it (same position, same
    /// moment) and agreed with the clock: the source reports on events only,
    /// so the same position with a new moment is a real jump (a restart).
    echoing: bool,
}

impl State {
    fn from_snapshot(snapshot: &PlaybackSnapshot) -> Self {
        Self {
            track: TrackIdentity::of(&snapshot.track),
            status: snapshot.status,
            anchor_ms: snapshot.position_ms,
            anchor_at: snapshot.position_at,
            rate: sanitize_rate(snapshot.rate),
            last_reported_ms: snapshot.position_ms,
            last_reported_at: snapshot.position_at,
            echoing: false,
        }
    }

    fn reanchor(&mut self, snapshot: &PlaybackSnapshot) {
        self.status = snapshot.status;
        self.anchor_ms = snapshot.position_ms;
        self.anchor_at = snapshot.position_at;
        self.rate = sanitize_rate(snapshot.rate);
    }

    fn clamp(&self, position_ms: u64) -> u64 {
        match self.track.duration_ms {
            Some(duration) if duration > 0 => position_ms.min(duration),
            _ => position_ms,
        }
    }

    /// Estimated position at `now`, clamped to `0..=duration`.
    fn position(&self, now: Instant) -> u64 {
        if self.status != PlaybackStatus::Playing {
            return self.clamp(self.anchor_ms);
        }
        let elapsed = now.saturating_duration_since(self.anchor_at);
        let delta_ms = elapsed.as_secs_f64() * 1000.0 * self.rate;
        self.clamp(shift(self.anchor_ms, delta_ms))
    }

    /// Where the clock says playback was at `at`, for comparing a reported
    /// position with. Unlike [`State::position`], a moment before the anchor
    /// is extrapolated backwards while playing, so an older but consistent
    /// reading is not mistaken for a jump.
    fn predict(&self, at: Instant) -> u64 {
        if self.status != PlaybackStatus::Playing || at >= self.anchor_at {
            return self.position(at);
        }
        let back = self.anchor_at.saturating_duration_since(at);
        let delta_ms = back.as_secs_f64() * 1000.0 * self.rate;
        self.clamp(shift(self.anchor_ms, -delta_ms))
    }

    /// Moves the anchor to `at` without trusting a new position, so a rate
    /// change applies from there on without a jump.
    fn rebase(&mut self, at: Instant, rate: f64) {
        let at = if at > self.anchor_at {
            at
        } else {
            self.anchor_at
        };
        self.anchor_ms = self.position(at);
        self.anchor_at = at;
        self.rate = rate;
    }
}

/// `position + delta_ms`, saturating at 0 and `u64::MAX`.
fn shift(position: u64, delta_ms: f64) -> u64 {
    if delta_ms >= 0.0 {
        // `as` saturates: an absurd delta becomes u64::MAX, never wraps.
        position.saturating_add(delta_ms as u64)
    } else {
        position.saturating_sub((-delta_ms) as u64)
    }
}

/// `a` and `b` are at most [`SAME_MOMENT`] apart.
fn same_moment(a: Instant, b: Instant) -> bool {
    a.saturating_duration_since(b)
        .max(b.saturating_duration_since(a))
        <= SAME_MOMENT
}

/// A rate that is not a finite number counts as normal speed.
fn sanitize_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate
    } else {
        1.0
    }
}

impl SyncClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a snapshot and reports what changed.
    ///
    /// Re-anchoring rules:
    /// - Track change, status change or seek: always re-anchor to the snapshot.
    /// - Same track, still playing, position within tolerance: re-anchor only
    ///   when the snapshot is newer information, meaning `position_at` is later
    ///   than the current anchor time AND the reported position differs from
    ///   the previously reported one. A source that keeps repeating a stale
    ///   position with a fresh `position_at` must not freeze the clock.
    ///
    /// Details:
    /// - A position identical to the previous snapshot's is not new
    ///   information, so it never re-anchors within tolerance. Beyond
    ///   tolerance it counts as a seek only when the previous snapshot echoed
    ///   the one before it (same position, `position_at` within 50 ms, in
    ///   line with the clock). That is how sources that report a timestamped
    ///   position on events (Windows, macOS) poll between events, and a song
    ///   restarted from 0 that had started at 0 then reports 0 again with a
    ///   new timestamp. A source that keeps restating a stale position with a
    ///   fresh `position_at` never echoes like that, so it is never "seeked"
    ///   back (otherwise it would be pulled back every [`SEEK_TOLERANCE_MS`]).
    /// - A snapshot older than the anchor is compared with the position
    ///   extrapolated back to its `position_at` (while playing), so an older
    ///   but consistent reading is not a seek.
    /// - Seeks are detected the same way while paused or stopped (the
    ///   prediction there is the anchored position).
    /// - A rate change on the same track and status moves the anchor to the
    ///   snapshot (or, for a stale repeat, to the predicted position) so the
    ///   new rate applies from then on.
    /// - Stopped → paused reports [`ClockEvent::Paused`].
    /// - A non-finite rate counts as 1.0.
    pub fn update(&mut self, snapshot: &PlaybackSnapshot) -> ClockEvent {
        let same_track = self
            .state
            .as_ref()
            .is_some_and(|state| state.track.matches(&snapshot.track));
        if !same_track {
            self.state = Some(State::from_snapshot(snapshot));
            return ClockEvent::TrackChanged;
        }
        let Some(state) = self.state.as_mut() else {
            // Unreachable: `same_track` implies a state.
            self.state = Some(State::from_snapshot(snapshot));
            return ClockEvent::TrackChanged;
        };

        let repeated = snapshot.position_ms == state.last_reported_ms;
        let same_time = same_moment(snapshot.position_at, state.last_reported_at);
        let was_echoing = state.echoing;
        state.last_reported_ms = snapshot.position_ms;
        state.last_reported_at = snapshot.position_at;
        state.echoing = false;

        if snapshot.status != state.status {
            state.reanchor(snapshot);
            return match snapshot.status {
                PlaybackStatus::Playing => ClockEvent::Resumed,
                PlaybackStatus::Paused => ClockEvent::Paused,
                PlaybackStatus::Stopped => ClockEvent::Stopped,
            };
        }

        let rate = sanitize_rate(snapshot.rate);
        let predicted = state.predict(snapshot.position_at);
        let reported = state.clamp(snapshot.position_ms);
        let jumped = reported.abs_diff(predicted) > SEEK_TOLERANCE_MS;
        if !repeated {
            if jumped {
                state.reanchor(snapshot);
                return ClockEvent::Seeked;
            }
            if snapshot.position_at > state.anchor_at {
                state.reanchor(snapshot);
                return ClockEvent::Steady;
            }
        } else if same_time {
            state.echoing = !jumped;
        } else if was_echoing && jumped {
            state.reanchor(snapshot);
            return ClockEvent::Seeked;
        }
        if (rate - state.rate).abs() > f64::EPSILON {
            state.rebase(snapshot.position_at, rate);
        }
        ClockEvent::Steady
    }

    /// Current estimated position in ms at `now`, or `None` before any snapshot.
    /// Extrapolates while playing (respecting `rate`), stays put while paused,
    /// never goes below 0, and never exceeds the track duration when known.
    /// `now` earlier than the anchor time counts as the anchor time.
    ///
    /// A duration of 0 counts as unknown (some sources report 0 for streams).
    pub fn position_ms(&self, now: Instant) -> Option<u64> {
        self.state.as_ref().map(|state| state.position(now))
    }

    /// Status from the last snapshot, or `None` before any snapshot.
    pub fn status(&self) -> Option<PlaybackStatus> {
        self.state.as_ref().map(|state| state.status)
    }

    /// The rate the position is extrapolated with (a non-finite rate counts
    /// as 1.0), or `None` before any snapshot.
    pub fn rate(&self) -> Option<f64> {
        self.state.as_ref().map(|state| state.rate)
    }

    /// Forgets everything (used when nothing is playing any more).
    pub fn reset(&mut self) {
        self.state = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A base instant far enough from the clock's origin that tests can go
    /// back in time from it.
    fn base() -> Instant {
        Instant::now() + Duration::from_secs(1_000)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn song(title: &str, duration_ms: Option<u64>) -> Track {
        Track {
            title: title.to_string(),
            artist: "Artist".to_string(),
            album: Some("Album".to_string()),
            duration_ms,
            spotify_id: None,
        }
    }

    fn snap(
        track: &Track,
        status: PlaybackStatus,
        position_ms: u64,
        position_at: Instant,
    ) -> PlaybackSnapshot {
        PlaybackSnapshot {
            track: track.clone(),
            status,
            position_ms,
            position_at,
            rate: 1.0,
            app_id: "test".to_string(),
        }
    }

    fn playing(track: &Track, position_ms: u64, at: Instant) -> PlaybackSnapshot {
        snap(track, PlaybackStatus::Playing, position_ms, at)
    }

    fn with_rate(mut snapshot: PlaybackSnapshot, rate: f64) -> PlaybackSnapshot {
        snapshot.rate = rate;
        snapshot
    }

    #[test]
    fn empty_clock_knows_nothing() {
        let clock = SyncClock::new();
        assert_eq!(clock.position_ms(base()), None);
        assert_eq!(clock.status(), None);
        assert_eq!(clock.rate(), None);
        let default = SyncClock::default();
        assert_eq!(default.position_ms(base()), None);
        assert_eq!(default.status(), None);
    }

    #[test]
    fn first_snapshot_is_a_track_change() {
        let t0 = base();
        let mut clock = SyncClock::new();
        let track = song("A", Some(200_000));
        assert_eq!(
            clock.update(&playing(&track, 10_000, t0)),
            ClockEvent::TrackChanged
        );
        assert_eq!(clock.position_ms(t0), Some(10_000));
        assert_eq!(clock.status(), Some(PlaybackStatus::Playing));
    }

    #[test]
    fn extrapolates_at_normal_speed() {
        let t0 = base();
        let mut clock = SyncClock::new();
        let track = song("A", Some(200_000));
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(clock.position_ms(t0 + ms(1)), Some(10_001));
        assert_eq!(clock.position_ms(t0 + ms(2_500)), Some(12_500));
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(60)),
            Some(70_000)
        );
    }

    #[test]
    fn extrapolates_at_double_and_half_speed() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&track, 10_000, t0), 2.0));
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(14_000));

        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&track, 10_000, t0), 0.5));
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(11_000));

        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&track, 10_000, t0), 0.0));
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(10_000));
    }

    #[test]
    fn paused_and_stopped_stay_put() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 42_000, t0));
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(30)),
            Some(42_000)
        );
        assert_eq!(clock.status(), Some(PlaybackStatus::Paused));

        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Stopped, 0, t0));
        assert_eq!(clock.position_ms(t0 + Duration::from_secs(30)), Some(0));
        assert_eq!(clock.status(), Some(PlaybackStatus::Stopped));
    }

    #[test]
    fn position_is_clamped_to_the_duration() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 199_000, t0));
        assert_eq!(clock.position_ms(t0 + ms(500)), Some(199_500));
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(200_000));
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(3_600)),
            Some(200_000)
        );

        // A reported position past the end is clamped too, also while paused.
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 250_000, t0));
        assert_eq!(clock.position_ms(t0), Some(200_000));
    }

    #[test]
    fn unknown_or_zero_duration_does_not_clamp() {
        let t0 = base();
        for duration in [None, Some(0)] {
            let track = song("Stream", duration);
            let mut clock = SyncClock::new();
            clock.update(&playing(&track, 5_000_000, t0));
            assert_eq!(
                clock.position_ms(t0 + Duration::from_secs(10)),
                Some(5_010_000),
                "{duration:?}"
            );
        }
    }

    #[test]
    fn earlier_now_counts_as_the_anchor_time() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(clock.position_ms(t0 - Duration::from_secs(5)), Some(10_000));
    }

    #[test]
    fn detects_a_seek_forward() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        let event = clock.update(&playing(&track, 60_000, t0 + ms(1_000)));
        assert_eq!(event, ClockEvent::Seeked);
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(60_000));
        assert_eq!(clock.position_ms(t0 + ms(3_000)), Some(62_000));
    }

    #[test]
    fn detects_a_seek_back() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 100_000, t0));
        let event = clock.update(&playing(&track, 0, t0 + ms(2_000)));
        assert_eq!(event, ClockEvent::Seeked);
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(0));
        assert_eq!(clock.position_ms(t0 + ms(2_500)), Some(500));
    }

    #[test]
    fn seek_tolerance_boundary() {
        let t0 = base();
        let track = song("A", Some(300_000));

        // Prediction at t0 + 1 s is 11 000; exactly the tolerance away is not a seek.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        let at_limit = 11_000 + SEEK_TOLERANCE_MS;
        assert_eq!(
            clock.update(&playing(&track, at_limit, t0 + ms(1_000))),
            ClockEvent::Steady
        );
        // ...but it is newer information, so the clock follows it.
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(at_limit));

        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(
            clock.update(&playing(&track, at_limit + 1, t0 + ms(1_000))),
            ClockEvent::Seeked
        );

        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(
            clock.update(&playing(&track, 11_000 - SEEK_TOLERANCE_MS, t0 + ms(1_000))),
            ClockEvent::Steady
        );
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(
            clock.update(&playing(
                &track,
                11_000 - SEEK_TOLERANCE_MS - 1,
                t0 + ms(1_000)
            )),
            ClockEvent::Seeked
        );
    }

    #[test]
    fn fresh_positions_reanchor_quietly() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        // The player is 200 ms ahead of the prediction: follow it.
        assert_eq!(
            clock.update(&playing(&track, 11_200, t0 + ms(1_000))),
            ClockEvent::Steady
        );
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(11_200));
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(12_200));
    }

    #[test]
    fn stale_repeated_position_does_not_freeze_the_clock() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        for second in 1..=10u64 {
            let at = t0 + Duration::from_secs(second);
            assert_eq!(
                clock.update(&playing(&track, 10_000, at)),
                ClockEvent::Steady
            );
            assert_eq!(clock.position_ms(at), Some(10_000 + second * 1_000));
        }
        // A real new reading afterwards is followed again.
        let at = t0 + ms(10_500);
        assert_eq!(
            clock.update(&playing(&track, 20_400, at)),
            ClockEvent::Steady
        );
        assert_eq!(clock.position_ms(at), Some(20_400));
        // And a real jump is still a seek.
        let at = t0 + ms(11_000);
        assert_eq!(
            clock.update(&playing(&track, 100_000, at)),
            ClockEvent::Seeked
        );
        assert_eq!(clock.position_ms(at), Some(100_000));
    }

    #[test]
    fn restarting_on_an_event_driven_source_is_a_seek() {
        // Windows/macOS report (position, when it was true) and only refresh
        // it on events, so every poll in between repeats the same pair (with
        // a little jitter from converting the timestamp). Restarting a song
        // that started at 0 reports 0 again, with a fresh timestamp.
        let t0 = base();
        let track = song("A", Some(240_000));
        let mut clock = SyncClock::new();
        assert_eq!(
            clock.update(&playing(&track, 0, t0)),
            ClockEvent::TrackChanged
        );
        for poll in 1..=120u64 {
            let jitter = if poll % 2 == 0 { t0 + ms(1) } else { t0 };
            assert_eq!(
                clock.update(&playing(&track, 0, jitter)),
                ClockEvent::Steady,
                "poll {poll}"
            );
        }
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(60)),
            Some(60_000)
        );
        // The user restarts the song after 60 s.
        let restart = t0 + Duration::from_secs(60);
        assert_eq!(
            clock.update(&playing(&track, 0, restart)),
            ClockEvent::Seeked
        );
        assert_eq!(clock.position_ms(restart + ms(1_000)), Some(1_000));
        // Echoes of the new pair are quiet, and a second restart is caught too.
        for _ in 0..10 {
            assert_eq!(
                clock.update(&playing(&track, 0, restart)),
                ClockEvent::Steady
            );
        }
        let again = restart + Duration::from_secs(30);
        assert_eq!(clock.update(&playing(&track, 0, again)), ClockEvent::Seeked);
        assert_eq!(clock.position_ms(again + ms(500)), Some(500));
    }

    #[test]
    fn echo_window_boundary() {
        let t0 = base();
        let track = song("A", Some(240_000));
        let restart = t0 + Duration::from_secs(60);

        // 50 ms apart is the same moment: an echo, so the restart is a seek.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 0, t0));
        clock.update(&playing(&track, 0, t0 + ms(50)));
        assert_eq!(
            clock.update(&playing(&track, 0, restart)),
            ClockEvent::Seeked
        );

        // 51 ms apart is a restatement, as a stale source makes them.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 0, t0));
        clock.update(&playing(&track, 0, t0 + ms(51)));
        assert_eq!(
            clock.update(&playing(&track, 0, restart)),
            ClockEvent::Steady
        );
        assert_eq!(clock.position_ms(restart), Some(60_000));

        // The first snapshot alone does not count as an echo.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 0, t0));
        assert_eq!(
            clock.update(&playing(&track, 0, restart)),
            ClockEvent::Steady
        );
    }

    #[test]
    fn restart_after_the_clock_reached_the_end_is_a_seek() {
        // "Repeat one" on an event-driven source: the clock sits at the end of
        // the song, the player starts over at 0.
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 0, t0));
        clock.update(&playing(&track, 0, t0));
        let later = t0 + Duration::from_secs(300);
        assert_eq!(clock.position_ms(later), Some(200_000));
        assert_eq!(clock.update(&playing(&track, 0, later)), ClockEvent::Seeked);
        assert_eq!(clock.position_ms(later + ms(2_000)), Some(2_000));
    }

    #[test]
    fn echoes_while_paused_never_seek() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 30_000, t0));
        for second in 0..5u64 {
            let at = t0 + Duration::from_secs(second * 10);
            assert_eq!(
                clock.update(&snap(&track, PlaybackStatus::Paused, 30_000, at)),
                ClockEvent::Steady
            );
        }
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(99)),
            Some(30_000)
        );
    }

    #[test]
    fn restart_is_caught_after_a_late_timestamp_refresh() {
        // The source refreshes its timestamp once without moving the position
        // (within tolerance, so the clock keeps its anchor), then echoes it.
        let t0 = base();
        let track = song("A", Some(240_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 0, t0));
        clock.update(&playing(&track, 0, t0));
        assert_eq!(
            clock.update(&playing(&track, 0, t0 + ms(300))),
            ClockEvent::Steady
        );
        for _ in 0..5 {
            assert_eq!(
                clock.update(&playing(&track, 0, t0 + ms(300))),
                ClockEvent::Steady
            );
        }
        let restart = t0 + Duration::from_secs(45);
        assert_eq!(
            clock.update(&playing(&track, 0, restart)),
            ClockEvent::Seeked
        );
        assert_eq!(clock.position_ms(restart), Some(0));
    }

    #[test]
    fn stale_source_is_never_pulled_back_even_after_a_burst_of_polls() {
        // A live-read source stuck at one position: every poll restates it
        // with the time of the read. Two polls a few ms apart (a burst after
        // the loop was delayed) must not make the next one count as a seek.
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        let mut reads: Vec<u64> = (1..=10).map(|n| n * 500).collect();
        reads.extend([5_003, 5_006, 5_500, 6_000, 8_000, 12_000]);
        for read in reads {
            let at = t0 + ms(read);
            assert_eq!(
                clock.update(&playing(&track, 10_000, at)),
                ClockEvent::Steady,
                "read at {read}"
            );
            assert_eq!(clock.position_ms(at), Some(10_000 + read), "read at {read}");
        }
    }

    #[test]
    fn stale_source_polled_slowly_is_not_pulled_back() {
        // Polls further apart than the seek tolerance.
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        for second in (2..=20u64).step_by(2) {
            let at = t0 + Duration::from_secs(second);
            assert_eq!(
                clock.update(&playing(&track, 10_000, at)),
                ClockEvent::Steady,
                "{second}"
            );
            assert_eq!(clock.position_ms(at), Some(10_000 + second * 1_000));
        }
    }

    #[test]
    fn older_consistent_snapshot_is_not_a_seek() {
        // Two readings of the same playback: a fresh one, then one with an
        // older timestamp (e.g. a source that falls back between a live read
        // and a timestamped one). Extrapolated back, they agree.
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 60_000, t0));
        let older = playing(&track, 55_000, t0 - Duration::from_secs(5));
        assert_eq!(clock.update(&older), ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(61_000));
        // Alternating fresh and old readings stays quiet.
        let fresh = playing(&track, 62_000, t0 + ms(2_000));
        assert_eq!(clock.update(&fresh), ClockEvent::Steady);
        assert_eq!(clock.update(&older), ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(3_000)), Some(63_000));
        // An older reading that disagrees is still a seek.
        let jumped = playing(&track, 150_000, t0 - Duration::from_secs(1));
        assert_eq!(clock.update(&jumped), ClockEvent::Seeked);
        assert_eq!(clock.position_ms(t0), Some(151_000));
        // While paused nothing is extrapolated back.
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 60_000, t0));
        let older = snap(
            &track,
            PlaybackStatus::Paused,
            55_000,
            t0 - Duration::from_secs(5),
        );
        assert_eq!(clock.update(&older), ClockEvent::Seeked);
    }

    #[test]
    fn older_snapshots_within_tolerance_do_not_reanchor() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        // Older information (position_at before the anchor): ignored.
        let event = clock.update(&playing(&track, 9_500, t0 - ms(400)));
        assert_eq!(event, ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(11_000));
        // Same position_at as the anchor: not newer either.
        let event = clock.update(&playing(&track, 10_300, t0));
        assert_eq!(event, ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(11_000));
    }

    #[test]
    fn status_transitions() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));

        let paused = snap(&track, PlaybackStatus::Paused, 12_000, t0 + ms(2_000));
        assert_eq!(clock.update(&paused), ClockEvent::Paused);
        assert_eq!(clock.status(), Some(PlaybackStatus::Paused));
        assert_eq!(clock.position_ms(t0 + ms(9_000)), Some(12_000));

        let still_paused = snap(&track, PlaybackStatus::Paused, 12_000, t0 + ms(5_000));
        assert_eq!(clock.update(&still_paused), ClockEvent::Steady);

        let resumed = playing(&track, 12_000, t0 + ms(10_000));
        assert_eq!(clock.update(&resumed), ClockEvent::Resumed);
        assert_eq!(clock.status(), Some(PlaybackStatus::Playing));
        assert_eq!(clock.position_ms(t0 + ms(11_000)), Some(13_000));

        let stopped = snap(&track, PlaybackStatus::Stopped, 0, t0 + ms(12_000));
        assert_eq!(clock.update(&stopped), ClockEvent::Stopped);
        assert_eq!(clock.status(), Some(PlaybackStatus::Stopped));
        assert_eq!(clock.position_ms(t0 + ms(20_000)), Some(0));

        let paused_from_stop = snap(&track, PlaybackStatus::Paused, 0, t0 + ms(13_000));
        assert_eq!(clock.update(&paused_from_stop), ClockEvent::Paused);

        let stopped_from_pause = snap(&track, PlaybackStatus::Stopped, 0, t0 + ms(14_000));
        assert_eq!(clock.update(&stopped_from_pause), ClockEvent::Stopped);

        let resumed_from_stop = playing(&track, 0, t0 + ms(15_000));
        assert_eq!(clock.update(&resumed_from_stop), ClockEvent::Resumed);
        assert_eq!(clock.position_ms(t0 + ms(16_000)), Some(1_000));

        let still_playing = playing(&track, 1_000, t0 + ms(16_000));
        assert_eq!(clock.update(&still_playing), ClockEvent::Steady);
    }

    #[test]
    fn resume_anchors_at_the_resumed_position() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 50_000, t0));
        // Resumed long after the pause, at a slightly different position.
        let event = clock.update(&playing(&track, 50_100, t0 + Duration::from_secs(60)));
        assert_eq!(event, ClockEvent::Resumed);
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(61)),
            Some(51_100)
        );
    }

    #[test]
    fn seeking_while_paused_is_detected() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&snap(&track, PlaybackStatus::Paused, 10_000, t0));
        let event = clock.update(&snap(&track, PlaybackStatus::Paused, 90_000, t0 + ms(500)));
        assert_eq!(event, ClockEvent::Seeked);
        assert_eq!(clock.position_ms(t0 + ms(5_000)), Some(90_000));
        // A small nudge while paused is followed without an event.
        let event = clock.update(&snap(&track, PlaybackStatus::Paused, 90_200, t0 + ms(900)));
        assert_eq!(event, ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(5_000)), Some(90_200));
    }

    #[test]
    fn track_identity_uses_title_artist_album_and_duration() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let changes = [
            Track {
                title: "B".to_string(),
                ..track.clone()
            },
            Track {
                artist: "Other".to_string(),
                ..track.clone()
            },
            Track {
                album: None,
                ..track.clone()
            },
            Track {
                album: Some("Other".to_string()),
                ..track.clone()
            },
            Track {
                duration_ms: Some(200_001),
                ..track.clone()
            },
            Track {
                duration_ms: None,
                ..track.clone()
            },
        ];
        for changed in changes {
            let mut clock = SyncClock::new();
            clock.update(&playing(&track, 10_000, t0));
            let event = clock.update(&playing(&changed, 0, t0 + ms(100)));
            assert_eq!(event, ClockEvent::TrackChanged, "{changed:?}");
            assert_eq!(clock.position_ms(t0 + ms(100)), Some(0));
        }

        // The Spotify id is not part of the identity.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        let with_id = Track {
            spotify_id: Some("abc".to_string()),
            ..track.clone()
        };
        assert_eq!(
            clock.update(&playing(&with_id, 10_100, t0 + ms(100))),
            ClockEvent::Steady
        );
    }

    #[test]
    fn track_change_wins_over_status_change_and_seek() {
        let t0 = base();
        let mut clock = SyncClock::new();
        clock.update(&playing(&song("A", Some(200_000)), 100_000, t0));
        let next = snap(
            &song("B", Some(180_000)),
            PlaybackStatus::Paused,
            0,
            t0 + ms(100),
        );
        assert_eq!(clock.update(&next), ClockEvent::TrackChanged);
        assert_eq!(clock.status(), Some(PlaybackStatus::Paused));
        assert_eq!(clock.position_ms(t0 + ms(5_000)), Some(0));
    }

    #[test]
    fn same_song_again_after_a_different_one_is_a_change() {
        let t0 = base();
        let a = song("A", Some(200_000));
        let b = song("B", Some(200_000));
        let mut clock = SyncClock::new();
        assert_eq!(clock.update(&playing(&a, 0, t0)), ClockEvent::TrackChanged);
        assert_eq!(
            clock.update(&playing(&b, 0, t0 + ms(10))),
            ClockEvent::TrackChanged
        );
        assert_eq!(
            clock.update(&playing(&a, 0, t0 + ms(20))),
            ClockEvent::TrackChanged
        );
    }

    #[test]
    fn reset_forgets_everything() {
        let t0 = base();
        let track = song("A", Some(200_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        clock.reset();
        assert_eq!(clock.position_ms(t0), None);
        assert_eq!(clock.status(), None);
        // The same track after a reset is a new track for listeners.
        assert_eq!(
            clock.update(&playing(&track, 10_000, t0)),
            ClockEvent::TrackChanged
        );
        clock.reset();
        clock.reset();
        assert_eq!(clock.status(), None);
    }

    #[test]
    fn rate_change_applies_without_a_jump() {
        let t0 = base();
        let track = song("A", Some(300_000));
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        assert_eq!(clock.rate(), Some(1.0));
        // A fresh position with a new rate re-anchors.
        let event = clock.update(&with_rate(playing(&track, 12_000, t0 + ms(2_000)), 2.0));
        assert_eq!(event, ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(3_000)), Some(14_000));
        assert_eq!(clock.rate(), Some(2.0));

        // A stale position with a new rate rebases at the predicted position.
        let mut clock = SyncClock::new();
        clock.update(&playing(&track, 10_000, t0));
        let event = clock.update(&with_rate(playing(&track, 10_000, t0 + ms(2_000)), 2.0));
        assert_eq!(event, ClockEvent::Steady);
        assert_eq!(clock.position_ms(t0 + ms(2_000)), Some(12_000));
        assert_eq!(clock.position_ms(t0 + ms(3_000)), Some(14_000));
    }

    #[test]
    fn odd_rates_never_panic_or_go_negative() {
        let t0 = base();
        let track = song("A", Some(300_000));

        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&track, 1_000, t0), -1.0));
        assert_eq!(clock.position_ms(t0 + ms(500)), Some(500));
        assert_eq!(clock.position_ms(t0 + ms(5_000)), Some(0));

        for rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut clock = SyncClock::new();
            clock.update(&with_rate(playing(&track, 1_000, t0), rate));
            assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(2_000), "{rate}");
            assert_eq!(clock.rate(), Some(1.0), "{rate}");
        }

        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&track, 1_000, t0), 1e300));
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(300_000));

        let unknown = song("Stream", None);
        let mut clock = SyncClock::new();
        clock.update(&with_rate(playing(&unknown, 1_000, t0), 1e300));
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(u64::MAX));
        clock.update(&with_rate(playing(&unknown, 1_000, t0), -1e300));
        assert_eq!(clock.position_ms(t0 + ms(1_000)), Some(0));
    }

    #[test]
    fn huge_positions_saturate() {
        let t0 = base();
        let unknown = song("Stream", None);
        let mut clock = SyncClock::new();
        clock.update(&playing(&unknown, u64::MAX - 10, t0));
        assert_eq!(
            clock.position_ms(t0 + Duration::from_secs(3_600)),
            Some(u64::MAX)
        );
        // A second snapshot near the top of the range must not overflow either.
        let event = clock.update(&playing(&unknown, u64::MAX, t0 + ms(5)));
        assert_eq!(event, ClockEvent::Steady);
        let event = clock.update(&playing(&unknown, 0, t0 + ms(10)));
        assert_eq!(event, ClockEvent::Seeked);
        assert_eq!(clock.position_ms(t0 + ms(10)), Some(0));
    }

    #[test]
    fn realistic_polling_session() {
        // A source polled every 250 ms that refreshes its position every 1 s,
        // with a little jitter, then a seek, a pause and the next song.
        let t0 = base();
        let track = song("A", Some(240_000));
        let mut clock = SyncClock::new();
        assert_eq!(
            clock.update(&playing(&track, 0, t0)),
            ClockEvent::TrackChanged
        );
        let mut last_reported = 0;
        for tick in 1..=40u64 {
            let now = t0 + ms(tick * 250);
            let reported = if tick % 4 == 0 {
                tick * 250 + 30
            } else {
                last_reported
            };
            last_reported = reported;
            assert_eq!(
                clock.update(&playing(&track, reported, now)),
                ClockEvent::Steady
            );
            let position = clock.position_ms(now).unwrap_or_default();
            assert!(
                position.abs_diff(tick * 250) <= 30,
                "tick {tick}: {position}"
            );
        }
        let now = t0 + ms(10_250);
        assert_eq!(
            clock.update(&playing(&track, 120_000, now)),
            ClockEvent::Seeked
        );
        let now = t0 + ms(12_250);
        assert_eq!(clock.position_ms(now), Some(122_000));
        let paused = snap(&track, PlaybackStatus::Paused, 122_010, now);
        assert_eq!(clock.update(&paused), ClockEvent::Paused);
        assert_eq!(
            clock.position_ms(now + Duration::from_secs(100)),
            Some(122_010)
        );
        let next = song("B", Some(180_000));
        assert_eq!(
            clock.update(&playing(&next, 0, now + ms(500))),
            ClockEvent::TrackChanged
        );
    }
}
