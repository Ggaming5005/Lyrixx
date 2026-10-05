//! The loop that ties sources, lyrics, the clock and targets together.

use crate::clock::{ClockEvent, SyncClock};
use crate::config::{Config, Offsets};
use crate::pacing::{Pacer, RateLimitAction};
use crate::providers::{ProviderChain, Resolved};
use crate::sources::NowPlayingSource;
use crate::targets::{StatusTarget, TargetError};
use crate::types::{Lyrics, PlaybackSnapshot, PlaybackStatus, Status, Track};
use crate::view::{EngineView, LineView, LyricsView, NowView, StatusView, TargetState, TargetView};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

/// How often statuses are recomputed while something plays, at most.
pub const RENDER_TICK_MS: u64 = 100;

/// Longest wait for one `set` or `clear` while running; longer counts as a failure.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest wait for each target's final `clear` on shutdown.
const SHUTDOWN_CLEAR_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest wait for one source read; longer counts as a source error. Sources
/// have their own, shorter limits; this only guards against one that hangs.
const SOURCE_TIMEOUT: Duration = Duration::from_secs(10);
/// A target that failed (other than a rate limit) is not tried again sooner.
const FAILURE_RETRY: Duration = Duration::from_secs(1);
/// The same error message is logged as a warning at most this often.
const REPEAT_LOG_EVERY: Duration = Duration::from_secs(60);
/// Polls are never more frequent than this, whatever the settings say
/// ([`Config::validate`] reports lower values as an error).
const MIN_POLL_INTERVAL_MS: u64 = 100;
/// Longest wait for one cover art read; longer counts as a failed read.
const ARTWORK_TIMEOUT: Duration = Duration::from_secs(5);
/// After a track change the cover art is read again after every poll for
/// this long: players often hand over the new title before its cover.
const ARTWORK_REREAD_FOR: Duration = Duration::from_secs(3);
/// The view's position anchor moves when the clock is further than this from
/// where the published anchor puts the song.
const VIEW_DRIFT_MS: u64 = 250;

/// Runs Lyrix until shutdown.
///
/// Behavior:
/// - Every `general.poll_interval_ms` the source is read. Source errors are
///   logged (the same message at most once per minute) and never stop the loop.
///   `Ok(None)` resets the clock and clears every target.
/// - A track change (see [`crate::clock::ClockEvent`]) starts a lyrics lookup on
///   a background task (`ProviderChain::resolve` on the normalized track) and
///   loads that song's offset from the offsets file (missing or unreadable → 0).
///   Until the lookup finishes the status shows the song (no-lyrics template).
///   A lookup result for a song that is no longer playing is ignored.
/// - At least every [`RENDER_TICK_MS`] (sooner when a lyric line or a pacer is
///   due), the desired status is composed with
///   [`crate::status::compose_status`] using the clock position, the global
///   offset plus the song offset, and the wall clock. While the pause marker
///   exists the desired status is "cleared".
/// - Each target has its own [`crate::pacing::Pacer`] over `Option<Status>`
///   (`None` = cleared) built from the target's `min_interval`. The desired value
///   is offered to every pacer each tick; ready values are sent with `set` or `clear`.
///   On start, a clear is sent to every target so stale statuses from a crash go away.
/// - Errors: `RateLimited` → `Pacer::on_rate_limited` (switch the target off on
///   `Disable`); `Unauthorized` → switch the target off; `Unavailable` and
///   `Other` → `Pacer::on_failure` (logged, retried later). A switched-off target
///   is logged once at error level and never used again in this run.
/// - On shutdown every target that is showing something is cleared, waiting at
///   most 2 s per target.
/// - With a view ([`Engine::with_view`]), an [`EngineView`] is published right
///   after each turn of the loop updated the state and the targets, only when
///   it differs from the one published last. Every track change also reads
///   the song's cover art from the source for the view, and it is read again
///   after the polls of the next 3 s. Without a view neither happens.
///   Targets are optional: with none, the engine only follows the song for
///   the view.
///
/// Details:
/// - Targets are updated one after the other. Each `set` or `clear` may take
///   at most 5 s; a slower one counts as a failure (`Pacer::on_failure`).
/// - A failed target (other than a rate limit) is tried again after 1 s at the
///   earliest, or later when its pacer says so.
/// - The lookup is given the track as the source reports it;
///   `ProviderChain::resolve` normalizes it, so it is not normalized twice.
/// - The poll interval is at least 100 ms.
/// - The song offset is keyed by [`crate::matcher::song_key`] of the
///   normalized track. Besides every track change, the offsets file is read
///   again whenever it changes (checked on every poll), so `lyrix offset`
///   applies to the song playing now.
/// - The pause marker is checked on every poll.
/// - On every poll each target is asked whether it lost what it showed
///   ([`StatusTarget::status_lost`], e.g. Discord restarted); one that did is
///   sent the current status again, even though it did not change.
/// - Paused playback is composed with `playing = false`, so the status follows
///   `status.show_when_paused`.
/// - The source is read alongside rendering, one read at a time: a slow or
///   stuck player never holds up lyric lines (the clock keeps the song going)
///   and polls that fall due meanwhile do not stack reads. A read that takes
///   longer than 10 s counts as a source error.
/// - "Showing something" on shutdown means the last value sent was a status,
///   or a `set` was attempted after the last successful `clear`.
/// - Shutdown is noticed at any moment: a source read or a `set` / `clear`
///   still under way is abandoned (a target whose `set` was abandoned counts
///   as showing something, so it is cleared).
/// - The view's lyrics are "searching" until the lookup for this song comes
///   back, then the lines the engine uses (unsynced lyrics with their spread
///   timings), or "not found". Its status is the desired status.
/// - The view's position anchor (`position_ms` at `position_at_unix_ms`, wall
///   clock) moves only on a jump: a track change, play/pause, a rate change,
///   or the clock more than 250 ms away from where the anchor puts the song
///   now (a seek). Smooth progress publishes nothing.
/// - A target's view state follows its last send: `starting` before any
///   answer, `showing` / `cleared` after a successful `set` / `clear`,
///   `waiting` on `Unavailable`, `rateLimited` on a backoff, `retrying` on
///   another error or a timeout, `off` once switched off.
/// - The cover art is read alongside rendering, like the source, one read
///   at a time, and each read may take at most 5 s. Players often hand over
///   the new title first and the new cover a moment later (until then they
///   give the cover of the song before, or none), so the poll that sees a
///   track change starts a read, and every poll in the 3 s after it (at
///   least the next one) starts another once the read before has finished.
///   The view takes each answer, so a late or corrected cover is published.
///   A track change abandons a read for the song before; a result for a song
///   that is no longer playing is ignored. An error (logged at debug level)
///   keeps the cover read before; a URL that is not `https:`, `http:` or
///   `data:` means no cover.
pub struct Engine {
    config: Config,
    source: Box<dyn NowPlayingSource>,
    chain: Arc<ProviderChain>,
    targets: Vec<Box<dyn StatusTarget>>,
    offsets_path: Option<PathBuf>,
    pause_marker: Option<PathBuf>,
    /// Where the current [`EngineView`] is published, for a window.
    view: Option<watch::Sender<EngineView>>,
    /// Unix ms now. Tests replace it so wall time follows the paused tokio clock.
    wall_clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Engine {
    pub fn new(
        config: Config,
        source: Box<dyn NowPlayingSource>,
        chain: Arc<ProviderChain>,
        targets: Vec<Box<dyn StatusTarget>>,
    ) -> Self {
        Self {
            config,
            source,
            chain,
            targets,
            offsets_path: None,
            pause_marker: None,
            view: None,
            wall_clock: Arc::new(unix_now_ms),
        }
    }

    /// Reads per-song offsets from this file on every track change.
    pub fn with_offsets_path(mut self, path: PathBuf) -> Self {
        self.offsets_path = Some(path);
        self
    }

    /// Clears all statuses while this file exists.
    pub fn with_pause_marker(mut self, path: PathBuf) -> Self {
        self.pause_marker = Some(path);
        self
    }

    /// Publishes what the engine is doing to `view` whenever it changes (see
    /// [`crate::view`]). Smooth progress is not a change: the position anchor
    /// only moves on a jump, so a window extrapolates between updates.
    pub fn with_view(mut self, view: watch::Sender<EngineView>) -> Self {
        self.view = Some(view);
        self
    }

    /// Replaces the wall clock (Unix ms).
    #[cfg(test)]
    fn with_wall_clock(mut self, wall_clock: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.wall_clock = wall_clock;
        self
    }

    /// Runs until `shutdown` completes. See the type docs.
    pub async fn run<S>(self, shutdown: S) -> anyhow::Result<()>
    where
        S: Future<Output = ()> + Send,
    {
        let Engine {
            config,
            source,
            chain,
            targets,
            offsets_path,
            pause_marker,
            view,
            wall_clock,
        } = self;

        let mut slots: Vec<Slot> = targets.into_iter().map(Slot::new).collect();
        let (lookup_tx, mut lookup_rx) = mpsc::unbounded_channel::<(u64, Resolved)>();
        let mut state = State {
            config: &config,
            wall_clock,
            clock: SyncClock::new(),
            playing: None,
            generation: 0,
            pause_marker,
            paused_by_marker: false,
            offsets: OffsetsFile::new(offsets_path),
            source_errors: RepeatFilter::default(),
        };
        let mut view = view.map(|sender| ViewPublisher::new(sender, source.name()));

        tracing::debug!(source = source.name(), "starting");
        tokio::pin!(shutdown);
        // The targets are "starting" while the first clear is under way.
        publish(&mut view, &state, &slots);

        // Clear whatever an earlier run (or a crash) left behind.
        for slot in &mut slots {
            slot.pacer.offer(None);
        }
        let mut running = tokio::select! {
            biased;
            () = &mut shutdown => false,
            () = flush(&mut slots, &state) => true,
        };
        publish(&mut view, &state, &slots);

        let poll_every =
            Duration::from_millis(config.general.poll_interval_ms.max(MIN_POLL_INTERVAL_MS));
        let mut poll = tokio::time::interval(poll_every);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The source read under way. It runs alongside rendering, so a slow
        // or stuck player never holds up lyric lines (the clock keeps time).
        let mut reading: Option<SourceRead<'_>> = None;
        // The cover art read under way, the same way (only with a view).
        let mut artwork: Option<ArtworkRead<'_>> = None;

        while running {
            let deadline = state.next_wake(&slots, Instant::now());
            // Answers first, so a tick that is always due (after a slow
            // flush) can never keep a finished read or lookup waiting.
            let wake = tokio::select! {
                biased;
                () = &mut shutdown => break,
                result = finish_read(&mut reading), if reading.is_some() => Wake::Read(result),
                Some((generation, resolved)) = lookup_rx.recv() => {
                    Wake::Lookup(generation, resolved)
                }
                (generation, result) = finish_artwork(&mut artwork), if artwork.is_some() => {
                    Wake::Artwork(generation, result)
                }
                _ = poll.tick() => Wake::Poll,
                () = tokio::time::sleep_until(deadline) => Wake::Render,
            };

            let polled = matches!(wake, Wake::Poll);
            match wake {
                Wake::Poll => {
                    state.check_pause_marker();
                    // One read at a time: a read still under way is not
                    // stacked with another one. (A render that fell due
                    // together with this tick is flushed below.)
                    if reading.is_none() {
                        reading = Some(start_read(&*source));
                    }
                }
                Wake::Read(result) => {
                    reading = None;
                    let new_song = state.on_source(result, &chain, &lookup_tx);
                    state.refresh_offsets();
                    if view.is_some()
                        && state.artwork_read_due(new_song, artwork.is_some(), Instant::now())
                    {
                        // A new song's read replaces a read for the song before.
                        artwork = Some(start_artwork(&*source, state.generation));
                    }
                }
                Wake::Lookup(generation, resolved) => state.on_lookup(generation, resolved),
                Wake::Artwork(generation, result) => {
                    artwork = None;
                    state.on_artwork(generation, result);
                }
                Wake::Render => {}
            }

            running = tokio::select! {
                biased;
                () = &mut shutdown => false,
                () = async {
                    if polled {
                        resend_lost(&mut slots).await;
                    }
                    flush(&mut slots, &state).await;
                } => true,
            };
            publish(&mut view, &state, &slots);
        }
        // Abandon reads that are still under way.
        drop(reading);
        drop(artwork);

        tracing::debug!("shutting down");
        for slot in slots.iter_mut().filter(|s| s.enabled && s.maybe_showing()) {
            match tokio::time::timeout(SHUTDOWN_CLEAR_TIMEOUT, slot.target.clear()).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(target_name = slot.name, "could not clear the status: {e}");
                }
                Err(_) => {
                    tracing::warn!(
                        target_name = slot.name,
                        "clearing the status took too long, giving up"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Why the loop woke up.
enum Wake {
    Poll,
    Read(SourceResult),
    Lookup(u64, Resolved),
    Artwork(u64, anyhow::Result<Option<String>>),
    Render,
}

/// What one source read gives.
type SourceResult = anyhow::Result<Option<PlaybackSnapshot>>;

/// A source read under way, limited to [`SOURCE_TIMEOUT`].
type SourceRead<'a> = Pin<Box<dyn Future<Output = SourceResult> + Send + 'a>>;

/// Starts reading the source; a read slower than [`SOURCE_TIMEOUT`] is an error.
fn start_read(source: &dyn NowPlayingSource) -> SourceRead<'_> {
    let read = source.snapshot();
    Box::pin(async move {
        match tokio::time::timeout(SOURCE_TIMEOUT, read).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!(
                "the player did not answer within {} s",
                SOURCE_TIMEOUT.as_secs()
            )),
        }
    })
}

/// Waits for the read under way; never finishes when there is none.
async fn finish_read(reading: &mut Option<SourceRead<'_>>) -> SourceResult {
    match reading {
        Some(read) => read.as_mut().await,
        None => std::future::pending().await,
    }
}

/// A cover art read under way for the song of `generation`, limited to
/// [`ARTWORK_TIMEOUT`].
struct ArtworkRead<'a> {
    generation: u64,
    read: Pin<Box<dyn Future<Output = anyhow::Result<Option<String>>> + Send + 'a>>,
}

/// Starts reading the cover art of the song of `generation`.
fn start_artwork(source: &dyn NowPlayingSource, generation: u64) -> ArtworkRead<'_> {
    let read = source.artwork();
    ArtworkRead {
        generation,
        read: Box::pin(async move {
            match tokio::time::timeout(ARTWORK_TIMEOUT, read).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "the player did not hand over the cover art within {} s",
                    ARTWORK_TIMEOUT.as_secs()
                )),
            }
        }),
    }
}

/// Waits for the cover art read under way; never finishes when there is none.
async fn finish_artwork(
    artwork: &mut Option<ArtworkRead<'_>>,
) -> (u64, anyhow::Result<Option<String>>) {
    match artwork {
        Some(artwork) => (artwork.generation, artwork.read.as_mut().await),
        None => std::future::pending().await,
    }
}

/// The song being followed.
struct Playing {
    /// The track as the source reports it (updated on every poll).
    track: Track,
    /// The playing app as the source reports it (updated on every poll).
    app_id: String,
    /// `None` while the lookup runs and when nothing was found.
    lyrics: Option<Lyrics>,
    /// The lookup for this song has not come back yet.
    searching: bool,
    /// [`crate::matcher::song_key`] of the normalized track.
    key: String,
    song_offset_ms: i64,
    /// When the track change was seen.
    changed_at: Instant,
    /// Cover art URL, as the latest read that answered gave it (only with a
    /// view).
    artwork: Option<String>,
    /// Bumped whenever `artwork` changes, so the view sends it again.
    artwork_rev: u32,
    /// Cover art reads started for this song.
    artwork_reads: u32,
}

/// Everything the loop knows, apart from the targets.
struct State<'a> {
    config: &'a Config,
    wall_clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    clock: SyncClock,
    playing: Option<Playing>,
    /// Bumped on every track change and when playback stops, so lookup results
    /// for an earlier song are recognized and dropped.
    generation: u64,
    pause_marker: Option<PathBuf>,
    paused_by_marker: bool,
    offsets: OffsetsFile,
    source_errors: RepeatFilter,
}

impl State<'_> {
    /// What every target should show now. `None` clears.
    fn desired(&self) -> Option<Status> {
        if self.paused_by_marker {
            return None;
        }
        let playing = self.playing.as_ref()?;
        let position = self.clock.position_ms(Instant::now().into_std());
        let is_playing = self.clock.status() == Some(PlaybackStatus::Playing);
        crate::status::compose_status(
            self.config,
            &playing.track,
            playing.lyrics.as_ref(),
            position,
            is_playing,
            self.total_offset(playing),
            (self.wall_clock)(),
        )
    }

    fn total_offset(&self, playing: &Playing) -> i64 {
        self.config
            .general
            .offset_ms
            .saturating_add(playing.song_offset_ms)
    }

    /// When the loop should look again: after [`RENDER_TICK_MS`], or sooner
    /// for the next lyric line or a target that may send.
    fn next_wake(&self, slots: &[Slot], now: Instant) -> Instant {
        let mut wake = now + Duration::from_millis(RENDER_TICK_MS);
        if let Some(ms) = self.ms_until_next_line(now) {
            if ms < RENDER_TICK_MS {
                wake = now + Duration::from_millis(ms);
            }
        }
        for slot in slots.iter().filter(|s| s.enabled) {
            if let Some(ready) = slot.pacer.next_ready_at() {
                let mut ready = Instant::from_std(ready);
                if let Some(retry_at) = slot.retry_at {
                    ready = ready.max(retry_at);
                }
                wake = wake.min(ready);
            }
        }
        // Never spin: something ready "now" is handled a moment later.
        wake.max(now + Duration::from_millis(1))
    }

    /// Song time until the next lyric line starts, while playing.
    fn ms_until_next_line(&self, now: Instant) -> Option<u64> {
        if self.paused_by_marker || self.clock.status() != Some(PlaybackStatus::Playing) {
            return None;
        }
        let playing = self.playing.as_ref()?;
        let lyrics = playing.lyrics.as_ref()?;
        let position = self.clock.position_ms(now.into_std())?;
        if let Some(duration) = playing.track.duration_ms.filter(|&d| d > 0) {
            // The clock stops at the end of the song: a line after it never
            // comes, and waiting for it would only spin.
            if position >= duration {
                return None;
            }
        }
        let shifted = i128::from(position) - i128::from(self.total_offset(playing));
        let next = if shifted < 0 {
            lyrics.lines.first()?.start_ms
        } else {
            lyrics.next_change_ms(u64::try_from(shifted).unwrap_or(u64::MAX))?
        };
        u64::try_from(i128::from(next) - shifted).ok()
    }

    fn check_pause_marker(&mut self) {
        let paused = self
            .pause_marker
            .as_deref()
            .is_some_and(|path| std::fs::metadata(path).is_ok());
        if paused != self.paused_by_marker {
            if paused {
                tracing::info!("paused: statuses are cleared until `lyrix resume`");
            } else {
                tracing::info!("resumed");
            }
            self.paused_by_marker = paused;
        }
    }

    /// Takes a source reading. True when a new song started.
    fn on_source(
        &mut self,
        result: anyhow::Result<Option<PlaybackSnapshot>>,
        chain: &Arc<ProviderChain>,
        lookup_tx: &mpsc::UnboundedSender<(u64, Resolved)>,
    ) -> bool {
        match result {
            Err(e) => {
                let message = format!("{e:#}");
                if self
                    .source_errors
                    .first_in_a_while(&message, Instant::now())
                {
                    tracing::warn!("could not read what is playing: {message}");
                } else {
                    tracing::debug!("could not read what is playing: {message}");
                }
                false
            }
            Ok(None) => {
                if self.playing.is_some() {
                    tracing::info!("nothing is playing");
                }
                self.clock.reset();
                self.playing = None;
                self.generation = self.generation.wrapping_add(1);
                false
            }
            Ok(Some(snapshot)) => {
                let event = self.clock.update(&snapshot);
                match &mut self.playing {
                    Some(playing) if event != ClockEvent::TrackChanged => {
                        // Same song; take fields the identity ignores (a Spotify id).
                        playing.track = snapshot.track;
                        playing.app_id = snapshot.app_id;
                        false
                    }
                    _ => {
                        self.start_song(snapshot.track, snapshot.app_id, chain, lookup_tx);
                        true
                    }
                }
            }
        }
    }

    /// A new song: forget the old lyrics, load the offset, start the lookup.
    fn start_song(
        &mut self,
        track: Track,
        app_id: String,
        chain: &Arc<ProviderChain>,
        lookup_tx: &mpsc::UnboundedSender<(u64, Resolved)>,
    ) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        tracing::info!("now playing: {} — {}", track.title, track.artist);

        let key = crate::matcher::song_key(&crate::matcher::normalize_track(&track));
        self.offsets.reload();
        let song_offset_ms = self.offsets.get(&key);

        let chain = Arc::clone(chain);
        let lookup_tx = lookup_tx.clone();
        let lookup_track = track.clone();
        tokio::spawn(async move {
            let resolved = chain.resolve(&lookup_track).await;
            // The loop may be gone (shutdown); nobody needs the result then.
            let _ = lookup_tx.send((generation, resolved));
        });

        self.playing = Some(Playing {
            track,
            app_id,
            lyrics: None,
            searching: true,
            key,
            song_offset_ms,
            changed_at: Instant::now(),
            artwork: None,
            artwork_rev: 0,
            artwork_reads: 0,
        });
    }

    fn on_lookup(&mut self, generation: u64, resolved: Resolved) {
        if generation != self.generation {
            tracing::debug!("dropping lyrics for a song that is no longer playing");
            return;
        }
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        playing.searching = false;
        match resolved {
            Resolved::Found(lyrics) => {
                tracing::info!(
                    source = %lyrics.source,
                    synced = lyrics.synced,
                    instrumental = lyrics.instrumental,
                    "found lyrics"
                );
                playing.lyrics = Some(lyrics);
            }
            Resolved::NotFound => {
                tracing::info!("no lyrics found");
                playing.lyrics = None;
            }
        }
    }

    /// Whether to start reading the cover art of the song playing now. Asked
    /// right after each source read (only with a view): `new_song` when that
    /// read saw a track change, `in_flight` when a cover read is under way.
    /// A new song is read at once (replacing a read for the song before).
    /// After that, each poll reads it again once the read before has
    /// finished, for [`ARTWORK_REREAD_FOR`] after the track change and at
    /// least once, so a slow poll still gets a second look. Counts the read
    /// when it is due.
    fn artwork_read_due(&mut self, new_song: bool, in_flight: bool, now: Instant) -> bool {
        let Some(playing) = self.playing.as_mut() else {
            return false;
        };
        let due = new_song
            || (!in_flight
                && (playing.artwork_reads < 2 || now < playing.changed_at + ARTWORK_REREAD_FOR));
        if due {
            playing.artwork_reads = playing.artwork_reads.saturating_add(1);
        }
        due
    }

    /// The cover art read for the song of `generation` finished.
    fn on_artwork(&mut self, generation: u64, result: anyhow::Result<Option<String>>) {
        if generation != self.generation {
            tracing::debug!("dropping cover art for a song that is no longer playing");
            return;
        }
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        let artwork = match result {
            Ok(Some(url)) if is_window_url(&url) => Some(url),
            Ok(Some(url)) => {
                let start: String = url.chars().take(40).collect();
                tracing::debug!("ignoring cover art that a window cannot load: {start}");
                None
            }
            Ok(None) => None,
            Err(e) => {
                // Keep what an earlier read gave; the next one may work.
                tracing::debug!("no cover art: {e:#}");
                return;
            }
        };
        if playing.artwork != artwork {
            playing.artwork = artwork;
            playing.artwork_rev = playing.artwork_rev.wrapping_add(1);
        }
    }

    /// Picks up a changed offsets file for the song playing now.
    fn refresh_offsets(&mut self) {
        if !self.offsets.changed() {
            return;
        }
        self.offsets.reload();
        if let Some(playing) = self.playing.as_mut() {
            let offset = self.offsets.get(&playing.key);
            if offset != playing.song_offset_ms {
                tracing::info!("song offset is now {offset} ms");
                playing.song_offset_ms = offset;
            }
        }
    }
}

/// One target and its pacing.
struct Slot {
    target: Box<dyn StatusTarget>,
    name: &'static str,
    pacer: Pacer<Option<Status>>,
    /// False once the target was switched off.
    enabled: bool,
    /// No send before this moment after a failure.
    retry_at: Option<Instant>,
    /// A `set` was attempted since the last successful `clear`, so the target
    /// may show something even if the attempt looked like a failure.
    set_attempted: bool,
    /// The last send failed (for logging the recovery).
    failing: bool,
    errors: RepeatFilter,
    /// How the target is doing, for the view.
    view_state: TargetState,
    /// Why, for the states that are not fine.
    view_detail: Option<String>,
}

impl Slot {
    fn new(target: Box<dyn StatusTarget>) -> Self {
        let name = target.name();
        let pacer = Pacer::new(target.min_interval());
        Self {
            target,
            name,
            pacer,
            enabled: true,
            retry_at: None,
            set_attempted: false,
            failing: false,
            errors: RepeatFilter::default(),
            view_state: TargetState::Starting,
            view_detail: None,
        }
    }

    fn view(&self) -> TargetView {
        TargetView {
            id: self.name.to_string(),
            state: self.view_state,
            detail: self.view_detail.clone(),
        }
    }

    fn show_state(&mut self, state: TargetState, detail: Option<String>) {
        self.view_state = state;
        self.view_detail = detail;
    }

    fn maybe_showing(&self) -> bool {
        self.set_attempted || matches!(self.pacer.last_sent(), Some(Some(_)))
    }

    /// Sends `value` and reports the outcome to the pacer.
    async fn send(&mut self, value: Option<Status>) {
        let shown = if value.is_some() {
            TargetState::Showing
        } else {
            TargetState::Cleared
        };
        let attempt = match &value {
            Some(status) => {
                self.set_attempted = true;
                self.target.set(status)
            }
            None => self.target.clear(),
        };
        let result = tokio::time::timeout(SEND_TIMEOUT, attempt).await;
        let now = Instant::now();
        match result {
            Ok(Ok(())) => {
                if value.is_none() {
                    self.set_attempted = false;
                }
                self.pacer.on_success();
                self.retry_at = None;
                self.show_state(shown, None);
                if self.failing {
                    tracing::info!(target_name = self.name, "working again");
                    self.failing = false;
                }
            }
            Ok(Err(TargetError::RateLimited { retry_after })) => {
                match self
                    .pacer
                    .on_rate_limited(value, now.into_std(), retry_after)
                {
                    RateLimitAction::Backoff(wait) => {
                        self.failing = true;
                        let seconds = wait.as_millis().div_ceil(1000);
                        self.show_state(
                            TargetState::RateLimited,
                            Some(format!("rate limited, waiting {seconds} s")),
                        );
                        tracing::warn!(
                            target_name = self.name,
                            "rate limited, waiting {} ms before the next update",
                            wait.as_millis()
                        );
                    }
                    RateLimitAction::Disable => {
                        self.switch_off("rate limited too many times in a row");
                    }
                }
            }
            Ok(Err(TargetError::Unauthorized(message))) => {
                self.switch_off(&format!("not authorized: {message}"));
            }
            Ok(Err(e @ (TargetError::Unavailable(_) | TargetError::Other(_)))) => {
                self.failed(value, now, &e.to_string());
                match e {
                    TargetError::Unavailable(message) => {
                        self.show_state(TargetState::Waiting, Some(message));
                    }
                    other => self.show_state(TargetState::Retrying, Some(format!("{other:#}"))),
                }
            }
            Err(_) => {
                let message = format!(
                    "no answer within {} s, will try again",
                    SEND_TIMEOUT.as_secs()
                );
                self.failed(value, now, &message);
                self.show_state(TargetState::Retrying, Some(message));
            }
        }
    }

    fn failed(&mut self, value: Option<Status>, now: Instant, message: &str) {
        self.pacer.on_failure(value);
        self.retry_at = Some(now + FAILURE_RETRY);
        self.failing = true;
        if self.errors.first_in_a_while(message, now) {
            tracing::warn!(
                target_name = self.name,
                "could not update the status: {message}"
            );
        } else {
            tracing::debug!(
                target_name = self.name,
                "could not update the status: {message}"
            );
        }
    }

    fn switch_off(&mut self, reason: &str) {
        self.enabled = false;
        self.show_state(TargetState::Off, Some(reason.to_string()));
        tracing::error!(
            target_name = self.name,
            "{reason}; not using {} again until Lyrix restarts",
            self.name
        );
    }
}

/// Asks every target whether it lost its status
/// ([`StatusTarget::status_lost`]). One that lost a status it was showing
/// gets the current status again: its pacer forgets what it sent.
async fn resend_lost(slots: &mut [Slot]) {
    for slot in slots.iter_mut().filter(|s| s.enabled) {
        if slot.target.status_lost().await && matches!(slot.pacer.last_sent(), Some(Some(_))) {
            tracing::info!(
                target_name = slot.name,
                "the status was lost, showing it again"
            );
            slot.pacer.reset();
        }
    }
}

/// Offers the desired status to every target's pacer and sends what is ready.
async fn flush(slots: &mut [Slot], state: &State<'_>) {
    for slot in slots.iter_mut().filter(|s| s.enabled) {
        // Recomputed per target, so a slow target never makes the next one
        // send an outdated line.
        slot.pacer.offer(state.desired());
        let now = Instant::now();
        if slot.retry_at.is_some_and(|at| now < at) {
            continue;
        }
        if let Some(value) = slot.pacer.poll(now.into_std()) {
            slot.send(value).await;
        }
    }
}

/// Publishes the view, when there is one.
fn publish(view: &mut Option<ViewPublisher>, state: &State<'_>, slots: &[Slot]) {
    if let Some(view) = view {
        view.publish(state, slots);
    }
}

/// Publishes the [`EngineView`] for a window (see [`Engine::with_view`]).
struct ViewPublisher {
    sender: watch::Sender<EngineView>,
    /// [`NowPlayingSource::name`].
    source: &'static str,
    /// The position anchor published last.
    anchor: Option<Anchor>,
    /// What the published lyrics and cover art belong to.
    heavy: Option<HeavyKey>,
}

/// The lyrics and the cover art of the song in the view only change with the
/// song (its generation), when its lookup comes back and when a cover read
/// gives a different cover. While this key stays the same they are neither
/// rebuilt nor compared, so a long song with a large cover costs nothing per
/// turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeavyKey {
    generation: u64,
    searching: bool,
    /// [`Playing::artwork_rev`].
    artwork_rev: u32,
}

impl ViewPublisher {
    fn new(sender: watch::Sender<EngineView>, source: &'static str) -> Self {
        Self {
            sender,
            source,
            anchor: None,
            heavy: None,
        }
    }

    /// Sends the view when it differs from the one published last.
    fn publish(&mut self, state: &State<'_>, slots: &[Slot]) {
        let status = state.desired().map(|status| StatusView {
            text: status.text,
            kind: status.kind.into(),
            line: status.line,
            estimated: status.estimated,
        });
        let targets: Vec<TargetView> = slots.iter().map(Slot::view).collect();
        let song = state.playing.as_ref().map(|playing| {
            let anchor = Anchor::follow(self.anchor, Anchor::now(state), playing.track.duration_ms);
            (playing, anchor)
        });
        self.anchor = song.map(|(_, anchor)| anchor);
        let heavy = song.map(|(playing, _)| HeavyKey {
            generation: state.generation,
            searching: playing.searching,
            artwork_rev: playing.artwork_rev,
        });
        let same_heavy = heavy.is_some() && heavy == self.heavy;
        self.heavy = heavy;
        let source = self.source;

        self.sender.send_if_modified(|view| {
            let mut changed = false;
            if view.source != source {
                view.source = source.to_string();
                changed = true;
            }
            changed |= assign(&mut view.paused, state.paused_by_marker);
            match (song, view.now.as_mut()) {
                (Some((playing, anchor)), Some(now)) if same_heavy => {
                    // The published lyrics and cover are still right: compare
                    // everything else, without copying them.
                    let lyrics = std::mem::replace(&mut now.lyrics, LyricsView::Searching);
                    let artwork = now.artwork.take();
                    let fresh = now_view(state, playing, &anchor, LyricsView::Searching, None);
                    changed |= assign(now, fresh);
                    now.lyrics = lyrics;
                    now.artwork = artwork;
                }
                (Some((playing, anchor)), _) => {
                    let lyrics = lyrics_view(playing);
                    let fresh = now_view(state, playing, &anchor, lyrics, playing.artwork.clone());
                    changed |= assign(&mut view.now, Some(fresh));
                }
                (None, _) => changed |= assign(&mut view.now, None),
            }
            changed |= assign(&mut view.status, status);
            changed |= assign(&mut view.targets, targets);
            changed
        });
    }
}

/// The song for the view, with the given lyrics and cover art.
fn now_view(
    state: &State<'_>,
    playing: &Playing,
    anchor: &Anchor,
    lyrics: LyricsView,
    artwork: Option<String>,
) -> NowView {
    NowView {
        title: playing.track.title.clone(),
        artist: playing.track.artist.clone(),
        album: playing.track.album.clone(),
        duration_ms: playing.track.duration_ms,
        app: playing.app_id.clone(),
        playing: anchor.playing,
        position_ms: anchor.position_ms,
        position_at_unix_ms: anchor.at_unix_ms,
        rate: anchor.rate,
        artwork,
        song_key: playing.key.clone(),
        song_offset_ms: playing.song_offset_ms,
        global_offset_ms: state.config.general.offset_ms,
        lyrics,
    }
}

/// Where the song's lyrics lookup stands, with the lines the engine uses.
fn lyrics_view(playing: &Playing) -> LyricsView {
    match &playing.lyrics {
        _ if playing.searching => LyricsView::Searching,
        None => LyricsView::NotFound,
        Some(lyrics) => LyricsView::Found {
            lines: lyrics.lines.iter().map(LineView::from).collect(),
            synced: lyrics.synced,
            instrumental: lyrics.instrumental,
            source: lyrics.source.clone(),
        },
    }
}

/// Sets `slot` to `value` when they differ. True when it changed.
fn assign<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

/// URLs a window can load: `https:`, `http:` and `data:`.
fn is_window_url(url: &str) -> bool {
    ["https:", "http:", "data:"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// The song position as the view gives it: the song was at `position_ms` at
/// `at_unix_ms` (wall clock), moving at `rate` while `playing`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    /// The song it belongs to ([`State::generation`]).
    generation: u64,
    playing: bool,
    rate: f64,
    position_ms: u64,
    at_unix_ms: u64,
}

impl Anchor {
    /// Where the clock is now.
    fn now(state: &State<'_>) -> Self {
        Self {
            generation: state.generation,
            playing: state.clock.status() == Some(PlaybackStatus::Playing),
            rate: state.clock.rate().unwrap_or(1.0),
            position_ms: state
                .clock
                .position_ms(Instant::now().into_std())
                .unwrap_or(0),
            at_unix_ms: (state.wall_clock)(),
        }
    }

    /// Where this anchor puts the song at `unix_ms`. Like the clock, it stops
    /// at the end of the song (`duration_ms`, when known) and at 0.
    fn position_at(&self, unix_ms: u64, duration_ms: Option<u64>) -> u64 {
        if !self.playing {
            return self.position_ms;
        }
        // Precision loss above 2^53 ms does not matter here.
        let moved = unix_ms.saturating_sub(self.at_unix_ms) as f64 * self.rate;
        // `as` saturates: an absurd value becomes 0 or u64::MAX, never wraps.
        let position = if moved >= 0.0 {
            self.position_ms.saturating_add(moved as u64)
        } else {
            self.position_ms.saturating_sub((-moved) as u64)
        };
        match duration_ms {
            Some(duration) if duration > 0 => position.min(duration),
            _ => position,
        }
    }

    /// The anchor to publish: `published` while `current` (the clock now)
    /// only shows smooth progress from it, else `current`. A jump is another
    /// song, play/pause, another rate, or the clock more than
    /// [`VIEW_DRIFT_MS`] from where `published` puts the song now.
    fn follow(published: Option<Self>, current: Self, duration_ms: Option<u64>) -> Self {
        match published {
            Some(published)
                if published.generation == current.generation
                    && published.playing == current.playing
                    && (published.rate - current.rate).abs() <= f64::EPSILON
                    && published
                        .position_at(current.at_unix_ms, duration_ms)
                        .abs_diff(current.position_ms)
                        <= VIEW_DRIFT_MS =>
            {
                published
            }
            _ => current,
        }
    }
}

/// The offsets file, re-read when it changes.
struct OffsetsFile {
    path: Option<PathBuf>,
    /// What the file looked like when it was last read.
    stamp: Option<FileStamp>,
    offsets: Offsets,
}

/// Enough about a file to notice that it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl OffsetsFile {
    fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            stamp: None,
            offsets: Offsets::default(),
        }
    }

    fn get(&self, key: &str) -> i64 {
        self.offsets.get(key)
    }

    /// True when the file looks different from when it was last read.
    fn changed(&self) -> bool {
        match &self.path {
            Some(path) => file_stamp(path) != self.stamp,
            None => false,
        }
    }

    /// Reads the file again. Missing or unreadable → no offsets.
    fn reload(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        self.stamp = file_stamp(path);
        self.offsets = match Offsets::load(path) {
            Ok(offsets) => offsets,
            Err(e) => {
                tracing::warn!("ignoring the song offsets: {e:#}");
                Offsets::default()
            }
        };
    }
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// Lets the same message through at most once per [`REPEAT_LOG_EVERY`].
#[derive(Debug, Default)]
struct RepeatFilter {
    last_let_through: HashMap<String, Instant>,
}

impl RepeatFilter {
    /// True when `message` was not let through in the last minute (and
    /// remembers that it is now).
    fn first_in_a_while(&mut self, message: &str, now: Instant) -> bool {
        self.last_let_through
            .retain(|_, at| now.saturating_duration_since(*at) < REPEAT_LOG_EVERY);
        if self.last_let_through.contains_key(message) {
            return false;
        }
        self.last_let_through.insert(message.to_string(), now);
        true
    }
}

/// Unix ms now, 0 when the system clock is before 1970.
fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::LyricsProvider;
    use crate::types::{LyricLine, StatusKind};
    use crate::view::StatusKindView;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    /// Wall-clock time at the start of every test, in Unix ms.
    const WALL_BASE: u64 = 1_700_000_000_000;

    // ---------------------------------------------------------------------
    // Fake source
    // ---------------------------------------------------------------------

    struct Playback {
        track: Track,
        status: PlaybackStatus,
        anchor_ms: u64,
        anchor_at: Instant,
    }

    impl Playback {
        fn position(&self, now: Instant) -> u64 {
            match self.status {
                PlaybackStatus::Playing => {
                    let elapsed = now.saturating_duration_since(self.anchor_at).as_millis();
                    self.anchor_ms + elapsed as u64
                }
                _ => self.anchor_ms,
            }
        }
    }

    #[derive(Default)]
    struct SourceState {
        playback: Option<Playback>,
        error: Option<String>,
        reads: usize,
        /// How long each read takes; `None` = reads never finish.
        latency: Option<Duration>,
        /// Reads that have started but not finished.
        in_flight: usize,
        /// The most reads in flight at the same time.
        max_in_flight: usize,
        /// What cover art reads give.
        covers: Covers,
        /// Cover art reads fail with this message.
        cover_error: Option<String>,
        /// How long each cover art read takes; `None` = never finishes.
        cover_latency: Option<Duration>,
        cover_reads: usize,
        /// Cover art reads that have started but not finished.
        covers_in_flight: usize,
    }

    /// The cover URL the fake player gives for `title`.
    fn cover_of(title: &str) -> String {
        format!("https://covers.example/{}.jpg", title.replace(' ', "-"))
    }

    /// What the fake player gives as cover art.
    #[derive(Default)]
    enum Covers {
        /// No cover.
        #[default]
        Missing,
        /// [`cover_of`] the song playing when asked.
        OfTheSong,
        /// This URL, whatever plays: a player that did not catch up with the
        /// song yet.
        Stuck(String),
    }

    /// Controls a [`FakeSource`] from the test.
    #[derive(Clone)]
    struct SourceHandle(Arc<Mutex<SourceState>>);

    impl Default for SourceHandle {
        fn default() -> Self {
            Self(Arc::new(Mutex::new(SourceState {
                latency: Some(Duration::ZERO),
                cover_latency: Some(Duration::ZERO),
                ..SourceState::default()
            })))
        }
    }

    /// Counts a read as finished when dropped, also when it is cancelled.
    struct InFlight(SourceHandle);

    impl Drop for InFlight {
        fn drop(&mut self) {
            let mut state = self.0 .0.lock().unwrap();
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }

    /// The same for a cover art read.
    struct CoverInFlight(SourceHandle);

    impl Drop for CoverInFlight {
        fn drop(&mut self) {
            let mut state = self.0 .0.lock().unwrap();
            state.covers_in_flight = state.covers_in_flight.saturating_sub(1);
        }
    }

    impl SourceHandle {
        /// Plays `track` from `from_ms` on, starting now.
        fn play(&self, track: Track, from_ms: u64) {
            let mut state = self.0.lock().unwrap();
            state.error = None;
            state.playback = Some(Playback {
                track,
                status: PlaybackStatus::Playing,
                anchor_ms: from_ms,
                anchor_at: Instant::now(),
            });
        }

        fn pause(&self) {
            let mut state = self.0.lock().unwrap();
            let now = Instant::now();
            if let Some(p) = state.playback.as_mut() {
                p.anchor_ms = p.position(now);
                p.anchor_at = now;
                p.status = PlaybackStatus::Paused;
            }
        }

        fn resume(&self) {
            let mut state = self.0.lock().unwrap();
            if let Some(p) = state.playback.as_mut() {
                p.anchor_at = Instant::now();
                p.status = PlaybackStatus::Playing;
            }
        }

        fn stop(&self) {
            self.0.lock().unwrap().playback = None;
        }

        fn fail(&self, message: &str) {
            self.0.lock().unwrap().error = Some(message.into());
        }

        fn reads(&self) -> usize {
            self.0.lock().unwrap().reads
        }

        /// Every read from now on takes `ms` (the position is measured when
        /// the read starts, like a timestamped source).
        fn slow(&self, ms: u64) {
            self.0.lock().unwrap().latency = Some(Duration::from_millis(ms));
        }

        /// Reads from now on never finish.
        fn hang(&self) {
            self.0.lock().unwrap().latency = None;
        }

        fn max_in_flight(&self) -> usize {
            self.0.lock().unwrap().max_in_flight
        }

        /// The player has cover art from now on (see [`cover_of`]).
        fn with_covers(&self) {
            self.0.lock().unwrap().covers = Covers::OfTheSong;
        }

        /// The player has no cover art from now on.
        fn without_covers(&self) {
            self.0.lock().unwrap().covers = Covers::Missing;
        }

        /// The player gives `url` as the cover from now on, whatever plays.
        fn stuck_cover(&self, url: String) {
            self.0.lock().unwrap().covers = Covers::Stuck(url);
        }

        /// Cover art reads from now on take `ms`.
        fn slow_covers(&self, ms: u64) {
            self.0.lock().unwrap().cover_latency = Some(Duration::from_millis(ms));
        }

        /// Cover art reads from now on never finish.
        fn hang_covers(&self) {
            self.0.lock().unwrap().cover_latency = None;
        }

        fn fail_covers(&self, message: &str) {
            self.0.lock().unwrap().cover_error = Some(message.into());
        }

        /// Cover art reads work again from now on.
        fn fix_covers(&self) {
            self.0.lock().unwrap().cover_error = None;
        }

        fn cover_reads(&self) -> usize {
            self.0.lock().unwrap().cover_reads
        }

        fn covers_in_flight(&self) -> usize {
            self.0.lock().unwrap().covers_in_flight
        }
    }

    /// A player that is read live, like MPRIS.
    struct FakeSource(SourceHandle);

    #[async_trait]
    impl NowPlayingSource for FakeSource {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
            let (result, latency) = {
                let mut state = self.0 .0.lock().unwrap();
                state.reads += 1;
                state.in_flight += 1;
                state.max_in_flight = state.max_in_flight.max(state.in_flight);
                let now = Instant::now();
                let result = match &state.error {
                    Some(message) => Err(anyhow::anyhow!(message.clone())),
                    None => Ok(state.playback.as_ref().map(|p| PlaybackSnapshot {
                        track: p.track.clone(),
                        status: p.status,
                        position_ms: p.position(now),
                        position_at: now.into_std(),
                        rate: 1.0,
                        app_id: "fake.player".into(),
                    })),
                };
                (result, state.latency)
            };
            let _in_flight = InFlight(self.0.clone());
            match latency {
                Some(latency) if latency.is_zero() => {}
                Some(latency) => tokio::time::sleep(latency).await,
                None => std::future::pending::<()>().await,
            }
            result
        }

        /// The cover of the song playing when asked (not when answering).
        async fn artwork(&self) -> anyhow::Result<Option<String>> {
            let (result, latency) = {
                let mut state = self.0 .0.lock().unwrap();
                state.cover_reads += 1;
                state.covers_in_flight += 1;
                let result = match (&state.cover_error, &state.covers) {
                    (Some(message), _) => Err(anyhow::anyhow!(message.clone())),
                    (None, Covers::Missing) => Ok(None),
                    (None, Covers::OfTheSong) => {
                        Ok(state.playback.as_ref().map(|p| cover_of(&p.track.title)))
                    }
                    (None, Covers::Stuck(url)) => Ok(state.playback.as_ref().map(|_| url.clone())),
                };
                (result, state.cover_latency)
            };
            let _in_flight = CoverInFlight(self.0.clone());
            match latency {
                Some(latency) if latency.is_zero() => {}
                Some(latency) => tokio::time::sleep(latency).await,
                None => std::future::pending::<()>().await,
            }
            result
        }
    }

    // ---------------------------------------------------------------------
    // Fake lyrics provider
    // ---------------------------------------------------------------------

    /// Answers by title after a delay; unknown titles have no lyrics.
    #[derive(Default)]
    struct FakeLyrics {
        songs: HashMap<String, (u64, Lyrics)>,
        seen: Arc<Mutex<Vec<Track>>>,
    }

    impl FakeLyrics {
        fn with(mut self, title: &str, delay_ms: u64, lyrics: Lyrics) -> Self {
            self.songs.insert(title.into(), (delay_ms, lyrics));
            self
        }
    }

    #[async_trait]
    impl LyricsProvider for FakeLyrics {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
            self.seen.lock().unwrap().push(track.clone());
            let Some((delay_ms, lyrics)) = self.songs.get(&track.title) else {
                return Ok(None);
            };
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            Ok(Some(lyrics.clone()))
        }
    }

    // ---------------------------------------------------------------------
    // Fake target
    // ---------------------------------------------------------------------

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Set { text: String, kind: StatusKind },
        Clear,
    }

    #[derive(Debug, Clone, Copy)]
    enum Reply {
        Ok,
        RateLimited,
        Unauthorized,
        Unavailable,
        Other,
        Hang,
    }

    #[derive(Default)]
    struct TargetLog {
        calls: Vec<(u64, Call)>,
        /// `started_at_unix_ms` of every `set`, with its time.
        starts: Vec<(u64, Option<u64>)>,
        replies: VecDeque<Reply>,
        hang_from_now_on: bool,
        /// The next `status_lost` says the status was lost.
        lost: bool,
        /// How many times `status_lost` was asked.
        lost_checks: usize,
    }

    /// Reads what a [`FakeTarget`] received, with times in ms since `t0`.
    #[derive(Clone)]
    struct TargetHandle {
        log: Arc<Mutex<TargetLog>>,
        t0: Instant,
    }

    impl TargetHandle {
        fn calls(&self) -> Vec<(u64, Call)> {
            self.log.lock().unwrap().calls.clone()
        }

        fn times(&self) -> Vec<u64> {
            self.calls().into_iter().map(|(at, _)| at).collect()
        }

        fn starts(&self) -> Vec<(u64, Option<u64>)> {
            self.log.lock().unwrap().starts.clone()
        }

        /// Texts of the `set` calls, with their times.
        fn sets(&self) -> Vec<(u64, String)> {
            self.calls()
                .into_iter()
                .filter_map(|(at, call)| match call {
                    Call::Set { text, .. } => Some((at, text)),
                    Call::Clear => None,
                })
                .collect()
        }

        /// The last call made at or before `ms`.
        fn last_call_until(&self, ms: u64) -> Option<Call> {
            self.calls()
                .into_iter()
                .rev()
                .find(|(at, _)| *at <= ms)
                .map(|(_, call)| call)
        }

        fn hang_from_now_on(&self) {
            self.log.lock().unwrap().hang_from_now_on = true;
        }

        /// The status is gone, like Discord after a restart.
        fn lose_status(&self) {
            self.log.lock().unwrap().lost = true;
        }

        fn lost_checks(&self) -> usize {
            self.log.lock().unwrap().lost_checks
        }
    }

    struct FakeTarget {
        name: &'static str,
        interval: Duration,
        handle: TargetHandle,
    }

    impl FakeTarget {
        fn new(name: &'static str, interval_ms: u64, t0: Instant) -> (Self, TargetHandle) {
            Self::scripted(name, interval_ms, t0, &[])
        }

        /// Answers with `replies` first, then `Ok`.
        fn scripted(
            name: &'static str,
            interval_ms: u64,
            t0: Instant,
            replies: &[Reply],
        ) -> (Self, TargetHandle) {
            let handle = TargetHandle {
                log: Arc::new(Mutex::new(TargetLog {
                    replies: replies.iter().copied().collect(),
                    ..TargetLog::default()
                })),
                t0,
            };
            let target = Self {
                name,
                interval: Duration::from_millis(interval_ms),
                handle: handle.clone(),
            };
            (target, handle)
        }

        async fn record(&mut self, call: Call) -> Result<(), TargetError> {
            let reply = {
                let mut log = self.handle.log.lock().unwrap();
                let at = Instant::now().duration_since(self.handle.t0).as_millis() as u64;
                log.calls.push((at, call));
                if log.hang_from_now_on {
                    Reply::Hang
                } else {
                    log.replies.pop_front().unwrap_or(Reply::Ok)
                }
            };
            match reply {
                Reply::Ok => Ok(()),
                Reply::RateLimited => Err(TargetError::RateLimited { retry_after: None }),
                Reply::Unauthorized => Err(TargetError::Unauthorized("token revoked".into())),
                Reply::Unavailable => Err(TargetError::Unavailable("not running".into())),
                Reply::Other => Err(TargetError::Other(anyhow::anyhow!("broken pipe"))),
                Reply::Hang => std::future::pending().await,
            }
        }
    }

    #[async_trait]
    impl StatusTarget for FakeTarget {
        fn name(&self) -> &'static str {
            self.name
        }

        fn min_interval(&self) -> Duration {
            self.interval
        }

        async fn set(&mut self, status: &Status) -> Result<(), TargetError> {
            {
                let mut log = self.handle.log.lock().unwrap();
                let at = Instant::now().duration_since(self.handle.t0).as_millis() as u64;
                log.starts.push((at, status.started_at_unix_ms));
            }
            self.record(Call::Set {
                text: status.text.clone(),
                kind: status.kind,
            })
            .await
        }

        async fn clear(&mut self) -> Result<(), TargetError> {
            self.record(Call::Clear).await
        }

        async fn status_lost(&mut self) -> bool {
            let mut log = self.handle.log.lock().unwrap();
            log.lost_checks += 1;
            std::mem::take(&mut log.lost)
        }
    }

    // ---------------------------------------------------------------------
    // Harness
    // ---------------------------------------------------------------------

    struct Running {
        t0: Instant,
        stop: oneshot::Sender<()>,
        task: JoinHandle<anyhow::Result<()>>,
    }

    impl Running {
        /// Sleeps until `ms` after the start.
        async fn until(&self, ms: u64) {
            tokio::time::sleep_until(self.t0 + Duration::from_millis(ms)).await;
        }

        /// Signals shutdown and waits for `run` to return. Returns when it
        /// returned, in ms since the start.
        async fn stop(self) -> u64 {
            self.stop.send(()).unwrap();
            self.task.await.unwrap().unwrap();
            Instant::now().duration_since(self.t0).as_millis() as u64
        }
    }

    fn start(engine: Engine, t0: Instant) -> Running {
        let wall: Arc<dyn Fn() -> u64 + Send + Sync> =
            Arc::new(move || WALL_BASE + Instant::now().duration_since(t0).as_millis() as u64);
        let engine = engine.with_wall_clock(wall);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(engine.run(async move {
            let _ = stopped.await;
        }));
        Running { t0, stop, task }
    }

    /// What a window saw of the engine's view.
    struct ViewLog {
        receiver: watch::Receiver<EngineView>,
        /// Every view seen, with its time in ms since the start.
        seen: Arc<Mutex<Vec<(u64, EngineView)>>>,
    }

    impl ViewLog {
        /// The view published last.
        fn current(&self) -> EngineView {
            self.receiver.borrow().clone()
        }

        /// The song in the view published last.
        #[track_caller]
        fn now(&self) -> NowView {
            self.current().now.expect("a song in the view")
        }

        fn seen(&self) -> Vec<(u64, EngineView)> {
            self.seen.lock().unwrap().clone()
        }

        /// When views were seen within `from..=to` ms.
        fn times_between(&self, from: u64, to: u64) -> Vec<u64> {
            self.seen()
                .into_iter()
                .map(|(at, _)| at)
                .filter(|at| (from..=to).contains(at))
                .collect()
        }
    }

    /// Makes `engine` publish its view, watched like a window does.
    fn watched(engine: Engine, t0: Instant) -> (Engine, ViewLog) {
        let (sender, receiver) = watch::channel(EngineView::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut window = receiver.clone();
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while window.changed().await.is_ok() {
                let view = window.borrow_and_update().clone();
                let at = Instant::now().duration_since(t0).as_millis() as u64;
                log.lock().unwrap().push((at, view));
            }
        });
        (engine.with_view(sender), ViewLog { receiver, seen })
    }

    fn engine(
        config: Config,
        source: &SourceHandle,
        lyrics: FakeLyrics,
        targets: Vec<FakeTarget>,
    ) -> Engine {
        let chain = Arc::new(ProviderChain::new(vec![Box::new(lyrics)], None));
        let targets = targets
            .into_iter()
            .map(|t| Box::new(t) as Box<dyn StatusTarget>)
            .collect();
        Engine::new(config, Box::new(FakeSource(source.clone())), chain, targets)
    }

    fn song(title: &str) -> Track {
        Track {
            title: title.into(),
            artist: "Artist".into(),
            album: None,
            duration_ms: Some(300_000),
            spotify_id: None,
        }
    }

    fn synced(lines: &[(u64, &str)]) -> Lyrics {
        Lyrics {
            lines: lines
                .iter()
                .map(|&(start_ms, text)| LyricLine {
                    start_ms,
                    text: text.into(),
                })
                .collect(),
            synced: true,
            instrumental: false,
            source: String::new(),
        }
    }

    fn line(text: &str) -> Call {
        Call::Set {
            text: format!("🎵 {text}"),
            kind: StatusKind::Line,
        }
    }

    fn no_lyrics(title: &str) -> Call {
        Call::Set {
            text: format!("{title} · Artist"),
            kind: StatusKind::NoLyrics,
        }
    }

    fn intro() -> Call {
        Call::Set {
            text: "♪".into(),
            kind: StatusKind::Instrumental,
        }
    }

    /// Checks the calls and that each came at its time or at most
    /// `tolerance` ms later.
    #[track_caller]
    fn assert_calls(actual: &[(u64, Call)], expected: &[(u64, Call)], tolerance: u64) {
        let actual_calls: Vec<&Call> = actual.iter().map(|(_, c)| c).collect();
        let expected_calls: Vec<&Call> = expected.iter().map(|(_, c)| c).collect();
        assert_eq!(actual_calls, expected_calls, "calls: {actual:?}");
        for ((at, call), (want, _)) in actual.iter().zip(expected) {
            assert!(
                *at >= *want && *at <= want + tolerance,
                "{call:?} at {at} ms, expected at {want} ms (all calls: {actual:?})"
            );
        }
    }

    /// The smallest gap between consecutive calls.
    fn min_gap(times: &[u64]) -> Option<u64> {
        times.windows(2).map(|w| w[1] - w[0]).min()
    }

    fn config() -> Config {
        Config::default()
    }

    // ---------------------------------------------------------------------
    // Startup, lookup and lines
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn startup_clears_every_target() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        let (fast, fast_log) = FakeTarget::new("fast", 0, t0);
        let (slow, slow_log) = FakeTarget::new("slow", 2_000, t0);
        let run = start(
            engine(config(), &source, FakeLyrics::default(), vec![fast, slow]),
            t0,
        );

        run.until(5_000).await;
        run.stop().await;

        // Nothing played, so nothing more was sent, not even on shutdown.
        assert_eq!(fast_log.calls(), [(0, Call::Clear)]);
        assert_eq!(slow_log.calls(), [(0, Call::Clear)]);
        assert!(source.reads() >= 10, "polled every 500 ms");
    }

    #[tokio::test(start_paused = true)]
    async fn shows_the_song_until_lyrics_arrive_then_lines_at_their_song_times() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with(
            "Song",
            2_000,
            synced(&[
                (1_000, "One"),
                (3_000, "Two"),
                (5_000, ""),
                (6_250, "Three"),
                (8_030, "Four"),
            ]),
        );
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(9_000).await;
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                // The lookup took 2 s; "One" started at 1 s already.
                (2_000, line("One")),
                (3_000, line("Two")),
                (5_000, intro()),
                (6_250, line("Three")),
                (8_030, line("Four")),
                (9_000, Call::Clear),
            ],
            2,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn intro_shows_the_instrumental_text_until_the_first_line() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(1_500, "First")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(2_000).await;
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, intro()),
                (1_500, line("First")),
                (2_000, Call::Clear),
            ],
            2,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_status_is_shown_again() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(1_000, "Only")]));
        let (discord, log) = FakeTarget::new("discord", 2_000, t0);
        let run = start(engine(config(), &source, lyrics, vec![discord]), t0);

        // Shown at 2 s (the startup clear used the 2 s budget), then unchanged.
        run.until(5_000).await;
        // Discord restarts: the line is gone although nothing changed.
        log.lose_status();
        run.until(8_000).await;
        // Asked on every poll; a status that is still there is not sent again.
        assert!(log.lost_checks() >= 14, "{}", log.lost_checks());
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (2_000, line("Only")),
                // Noticed on the next poll (every 500 ms) and sent right away.
                (5_000, line("Only")),
                (8_000, Call::Clear),
            ],
            500,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lost_clear_is_not_resent() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        let (discord, log) = FakeTarget::new("discord", 0, t0);
        let run = start(
            engine(config(), &source, FakeLyrics::default(), vec![discord]),
            t0,
        );

        run.until(1_000).await;
        // Nothing was showing, so there is nothing to show again.
        log.lose_status();
        run.until(3_000).await;
        run.stop().await;

        assert_eq!(log.calls(), [(0, Call::Clear)]);
    }

    #[tokio::test(start_paused = true)]
    async fn songs_without_lyrics_show_the_song() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Unknown"), 0);
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(
            engine(config(), &source, FakeLyrics::default(), vec![fast]),
            t0,
        );

        run.until(3_000).await;
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Unknown")),
                (3_000, Call::Clear),
            ],
            0,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_lookup_gets_the_normalized_track() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(
            Track {
                title: "Song (Remastered 2011)".into(),
                artist: "ArtistVEVO".into(),
                ..song("x")
            },
            0,
        );
        let lyrics = FakeLyrics::default();
        let seen = Arc::clone(&lyrics.seen);
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(1_000).await;
        run.stop().await;

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].title, "Song");
        assert_eq!(seen[0].artist, "Artist");
        // The status shows the song as the player reports it.
        assert_eq!(
            log.sets(),
            [(0, "Song (Remastered 2011) · ArtistVEVO".to_string())]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_seek_moves_to_the_right_line_without_a_new_lookup() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with(
            "Song",
            0,
            synced(&[(0, "Start"), (60_000, "Minute"), (61_000, "After")]),
        );
        let seen = Arc::clone(&lyrics.seen);
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(2_050).await;
        source.play(song("Song"), 60_000);
        run.until(4_000).await;
        run.stop().await;

        assert_eq!(seen.lock().unwrap().len(), 1, "a seek is not a new song");
        let sets = log.sets();
        let texts: Vec<&str> = sets.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            ["Song · Artist", "🎵 Start", "🎵 Minute", "🎵 After"]
        );
        // The jump to 60 s at 2.05 s is seen at the next poll (2.5 s);
        // "After" (61 s) comes when the song gets there, at 3.05 s.
        assert_eq!(sets[2].0, 2_500);
        assert!((3_050..=3_052).contains(&sets[3].0), "{sets:?}");
    }

    // ---------------------------------------------------------------------
    // Pacing
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn each_target_gets_updates_at_its_own_pace() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Fast"), 0);
        let words: Vec<String> = (0..60).map(|i| format!("L{i}")).collect();
        let lines: Vec<(u64, &str)> = words
            .iter()
            .enumerate()
            .map(|(i, w)| (i as u64 * 250, w.as_str()))
            .collect();
        let lyrics = FakeLyrics::default().with("Fast", 0, synced(&lines));
        let (fast, fast_log) = FakeTarget::new("fast", 0, t0);
        let (slow, slow_log) = FakeTarget::new("slow", 2_000, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast, slow]), t0);

        run.until(10_100).await;
        let fast_sets = fast_log.sets();
        let slow_calls = slow_log.calls();
        run.stop().await;

        // The fast target saw every line, each right when it started.
        for i in 1..=40u64 {
            let text = format!("🎵 L{i}");
            let shown: Vec<u64> = fast_sets
                .iter()
                .filter(|(_, t)| *t == text)
                .map(|(at, _)| *at)
                .collect();
            assert_eq!(shown.len(), 1, "{text}: {fast_sets:?}");
            assert!(
                (i * 250..=i * 250 + 2).contains(&shown[0]),
                "{text} at {}",
                shown[0]
            );
        }

        // The slow one: a clear at 0, then every 2 s the line playing then.
        let times: Vec<u64> = slow_calls.iter().map(|(at, _)| *at).collect();
        assert_eq!(times, [0, 2_000, 4_000, 6_000, 8_000, 10_000]);
        assert!(min_gap(&times).unwrap() >= 2_000);
        for (at, call) in slow_calls.iter().skip(1) {
            assert_eq!(*call, line(&format!("L{}", at / 250)), "at {at}");
        }
        assert!(slow_calls.len() * 5 < fast_sets.len());
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_target_never_gets_updates_closer_than_its_interval() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        // Lines at irregular times, some close together.
        let lyrics = FakeLyrics::default().with(
            "Song",
            700,
            synced(&[
                (100, "a"),
                (900, "b"),
                (950, "c"),
                (2_500, "d"),
                (2_600, ""),
                (4_100, "e"),
                (7_777, "f"),
            ]),
        );
        let (slow, log) = FakeTarget::new("slow", 1_500, t0);
        let run = start(engine(config(), &source, lyrics, vec![slow]), t0);

        run.until(3_000).await;
        source.pause();
        run.until(4_000).await;
        source.resume();
        run.until(9_000).await;
        run.stop().await;

        let times = log.times();
        assert!(times.len() >= 5, "{:?}", log.calls());
        // The final clear on shutdown is not paced.
        let paced = &times[..times.len() - 1];
        assert!(min_gap(paced).unwrap() >= 1_500, "calls: {:?}", log.calls());
    }

    // ---------------------------------------------------------------------
    // Track changes
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn a_lookup_result_for_an_earlier_song_is_ignored() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song A"), 0);
        let lyrics = FakeLyrics::default()
            .with("Song A", 3_000, synced(&[(0, "A words")]))
            .with("Song B", 5_000, synced(&[(0, "B words")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(1_750).await;
        source.play(song("Song B"), 0);
        run.until(8_000).await;
        run.stop().await;

        // A's lyrics arrived at 3 s, while B was playing: never shown.
        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song A")),
                (2_000, no_lyrics("Song B")),
                (7_000, line("B words")),
                (8_000, Call::Clear),
            ],
            0,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn going_back_to_a_song_looks_it_up_again() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song A"), 0);
        let lyrics = FakeLyrics::default()
            .with("Song A", 1_000, synced(&[(0, "A words")]))
            .with("Song B", 0, synced(&[(0, "B words")]));
        let seen = Arc::clone(&lyrics.seen);
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(250).await;
        source.play(song("Song B"), 0);
        run.until(750).await;
        source.play(song("Song A"), 0);
        run.until(3_000).await;
        run.stop().await;

        // The first lookup of A (started at 0 s, done at 1 s) belongs to an
        // older track change than the second one (started at 1 s).
        let titles: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.title.clone())
            .collect();
        assert_eq!(titles, ["Song A", "Song B", "Song A"]);
        assert_eq!(
            log.sets(),
            [
                (0, "Song A · Artist".to_string()),
                (500, "Song B · Artist".to_string()),
                (500, "🎵 B words".to_string()),
                (1_000, "Song A · Artist".to_string()),
                (2_000, "🎵 A words".to_string()),
            ]
        );
    }

    // ---------------------------------------------------------------------
    // Clearing
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn nothing_playing_clears_and_a_new_song_shows_again() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "Hello")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(2_050).await;
        source.stop();
        run.until(4_050).await;
        source.play(song("Song"), 0);
        run.until(5_000).await;
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, line("Hello")),
                (2_500, Call::Clear),
                (4_500, no_lyrics("Song")),
                (4_500, line("Hello")),
                (5_000, Call::Clear),
            ],
            0,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pausing_the_player_clears_unless_show_when_paused() {
        for show_when_paused in [false, true] {
            let t0 = Instant::now();
            let source = SourceHandle::default();
            source.play(song("Song"), 0);
            let lyrics =
                FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (3_000, "Two")]));
            let (fast, log) = FakeTarget::new("fast", 0, t0);
            let mut config = config();
            config.status.show_when_paused = show_when_paused;
            let run = start(engine(config, &source, lyrics, vec![fast]), t0);

            run.until(1_050).await;
            source.pause();
            run.until(5_050).await;
            source.resume();
            run.until(8_000).await;
            run.stop().await;

            let calls: Vec<Call> = log.calls().into_iter().map(|(_, c)| c).collect();
            if show_when_paused {
                // Paused at 1.05 s (seen at the 1.5 s poll): "One" stays and
                // is sent once more, without a song start time (no progress
                // bar while paused). Nothing while paused: a paused status
                // does not change. Resumed at 5.05 s (seen at 5.5 s, the song
                // at 1.5 s): sent with its start time again. "Two" (3 s)
                // comes 1.95 s after resuming.
                let sets = log.sets();
                let texts: Vec<(u64, &str)> =
                    sets.iter().map(|(at, t)| (*at, t.as_str())).collect();
                assert_eq!(
                    &texts[..4],
                    [
                        (0, "Song · Artist"),
                        (0, "🎵 One"),
                        (1_500, "🎵 One"),
                        (5_500, "🎵 One")
                    ],
                    "{sets:?}"
                );
                assert_eq!(texts.len(), 5, "{sets:?}");
                assert_eq!(texts[4].1, "🎵 Two");
                assert!((7_000..=7_002).contains(&texts[4].0), "{sets:?}");
                let starts = log.starts();
                assert_eq!(starts[1], (0, Some(WALL_BASE)));
                assert_eq!(starts[2], (1_500, None));
                assert_eq!(starts[3], (5_500, Some(WALL_BASE + 4_000)));
                assert_eq!(calls.last(), Some(&Call::Clear));
            } else {
                assert_eq!(
                    calls,
                    [
                        Call::Clear,
                        no_lyrics("Song"),
                        line("One"),
                        Call::Clear,
                        line("One"),
                        line("Two"),
                        Call::Clear
                    ]
                );
                assert_eq!(log.times()[3], 1_500);
                assert_eq!(log.times()[4], 5_500);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_pause_marker_clears_and_removing_it_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("paused");
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with(
            "Song",
            0,
            synced(&[(0, "One"), (3_000, "Two"), (6_000, "Three")]),
        );
        let (fast, fast_log) = FakeTarget::new("fast", 0, t0);
        let (slow, slow_log) = FakeTarget::new("slow", 2_000, t0);
        let run = start(
            engine(config(), &source, lyrics, vec![fast, slow]).with_pause_marker(marker.clone()),
            t0,
        );

        run.until(2_050).await;
        std::fs::write(&marker, b"").unwrap();
        run.until(5_050).await;
        std::fs::remove_file(&marker).unwrap();
        run.until(7_000).await;
        run.stop().await;

        assert_calls(
            &fast_log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, line("One")),
                (2_500, Call::Clear),
                // Nothing while paused, though "Two" started at 3 s.
                (5_500, line("Two")),
                (6_000, line("Three")),
                (7_000, Call::Clear),
            ],
            2,
        );
        assert_calls(
            &slow_log.calls(),
            &[
                (0, Call::Clear),
                (2_000, line("One")),
                (4_000, Call::Clear),
                (6_000, line("Three")),
                (7_000, Call::Clear),
            ],
            2,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_clears_only_targets_that_show_something() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "Hello")]));
        let (a, a_log) = FakeTarget::new("a", 0, t0);
        let (b, b_log) = FakeTarget::new("b", 1_000, t0);
        // Never gets past its startup clear within the test.
        let (c, c_log) = FakeTarget::new("c", 60_000, t0);
        let run = start(engine(config(), &source, lyrics, vec![a, b, c]), t0);

        run.until(2_050).await;
        run.stop().await;

        assert_eq!(a_log.calls().last(), Some(&(2_050, Call::Clear)));
        assert_calls(
            &b_log.calls(),
            &[
                (0, Call::Clear),
                (1_000, line("Hello")),
                (2_050, Call::Clear),
            ],
            0,
        );
        assert_eq!(c_log.calls(), [(0, Call::Clear)]);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_waits_at_most_two_seconds_per_target() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "Hello")]));
        let (a, a_log) = FakeTarget::new("a", 0, t0);
        let (b, b_log) = FakeTarget::new("b", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![a, b]), t0);

        run.until(1_050).await;
        a_log.hang_from_now_on();
        b_log.hang_from_now_on();
        let returned_at = run.stop().await;

        assert_eq!(returned_at, 1_050 + 2 * 2_000);
        assert_eq!(a_log.calls().last(), Some(&(1_050, Call::Clear)));
        assert_eq!(b_log.calls().last(), Some(&(3_050, Call::Clear)));
    }

    // ---------------------------------------------------------------------
    // Offsets
    // ---------------------------------------------------------------------

    fn save_offset(path: &Path, track: &Track, offset_ms: i64) {
        let mut offsets = Offsets::default();
        let key = crate::matcher::song_key(&crate::matcher::normalize_track(track));
        offsets.set(&key, offset_ms);
        offsets.save(path).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_song_offset_and_the_global_offset_shift_lines() {
        let dir = tempfile::tempdir().unwrap();
        let offsets = dir.path().join("offsets.toml");
        // Saved under the normalized song, so the player's noisy title matches.
        save_offset(&offsets, &song("Song"), 1_000);
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(
            Track {
                title: "Song (Official Video)".into(),
                ..song("x")
            },
            0,
        );
        let lyrics =
            FakeLyrics::default().with("Song", 0, synced(&[(2_000, "One"), (4_000, "Two")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let mut config = config();
        config.general.offset_ms = 500;
        let run = start(
            engine(config, &source, lyrics, vec![fast]).with_offsets_path(offsets),
            t0,
        );

        run.until(6_000).await;
        run.stop().await;

        let sets = log.sets();
        let texts: Vec<&str> = sets.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            ["Song (Official Video) · Artist", "♪", "🎵 One", "🎵 Two"]
        );
        assert!((3_500..=3_502).contains(&sets[2].0), "{sets:?}");
        assert!((5_500..=5_502).contains(&sets[3].0), "{sets:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_negative_offset_shows_lines_earlier() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics =
            FakeLyrics::default().with("Song", 0, synced(&[(2_000, "One"), (4_000, "Two")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let mut config = config();
        config.general.offset_ms = -1_500;
        let run = start(engine(config, &source, lyrics, vec![fast]), t0);

        run.until(3_000).await;
        run.stop().await;

        let sets = log.sets();
        assert_eq!(sets[2].1, "🎵 One");
        assert!((500..=502).contains(&sets[2].0), "{sets:?}");
        assert_eq!(sets[3].1, "🎵 Two");
        assert!((2_500..=2_502).contains(&sets[3].0), "{sets:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_changed_offsets_file_applies_to_the_song_playing_now() {
        let dir = tempfile::tempdir().unwrap();
        let offsets = dir.path().join("offsets.toml");
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with(
            "Song",
            0,
            synced(&[(2_000, "One"), (4_000, "Two"), (6_000, "Three")]),
        );
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(
            engine(config(), &source, lyrics, vec![fast]).with_offsets_path(offsets.clone()),
            t0,
        );

        run.until(1_050).await;
        save_offset(&offsets, &song("Song"), 2_000);
        run.until(4_550).await;
        save_offset(&offsets, &song("Song"), 0);
        run.until(7_000).await;
        run.stop().await;

        let sets = log.sets();
        let texts: Vec<&str> = sets.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            texts,
            ["Song · Artist", "♪", "🎵 One", "🎵 Two", "🎵 Three"]
        );
        // +2 s from 1.5 s on: "One" at 4 s. Back to 0 at 5 s: "Two" right
        // away (the song is at 5 s), "Three" at 6 s.
        assert!((4_000..=4_002).contains(&sets[2].0), "{sets:?}");
        assert_eq!(sets[3].0, 5_000);
        assert!((6_000..=6_002).contains(&sets[4].0), "{sets:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unreadable_offsets_file_means_no_offset() {
        let dir = tempfile::tempdir().unwrap();
        let offsets = dir.path().join("offsets.toml");
        std::fs::write(&offsets, "this is [[ not toml").unwrap();
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(1_000, "One")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(
            engine(config(), &source, lyrics, vec![fast]).with_offsets_path(offsets),
            t0,
        );

        run.until(2_000).await;
        run.stop().await;

        let sets = log.sets();
        let last = sets.last().unwrap();
        assert_eq!(last.1, "🎵 One");
        assert!((1_000..=1_002).contains(&last.0), "{sets:?}");
    }

    // ---------------------------------------------------------------------
    // Target errors
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn rate_limits_back_off_and_the_third_in_a_row_switches_the_target_off() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let words: Vec<String> = (0..20).map(|i| format!("L{i}")).collect();
        let lines: Vec<(u64, &str)> = words
            .iter()
            .enumerate()
            .map(|(i, w)| (i as u64 * 500, w.as_str()))
            .collect();
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&lines));
        let (limited, limited_log) = FakeTarget::scripted(
            "limited",
            0,
            t0,
            &[Reply::RateLimited, Reply::RateLimited, Reply::RateLimited],
        );
        let (fine, fine_log) = FakeTarget::new("fine", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![limited, fine]), t0);

        run.until(8_000).await;
        run.stop().await;

        // 0 s: limited (interval → 1 s); 1 s: limited (→ 2 s); 3 s: limited
        // a third time → switched off, not even cleared on shutdown.
        assert_eq!(limited_log.times(), [0, 1_000, 3_000]);
        assert_eq!(limited_log.calls()[0].1, Call::Clear);

        // The other target never noticed.
        let fine_sets = fine_log.sets();
        for i in 1..16u64 {
            let text = format!("🎵 L{i}");
            assert!(
                fine_sets
                    .iter()
                    .any(|(at, t)| *t == text && (i * 500..=i * 500 + 2).contains(at)),
                "{text}: {fine_sets:?}"
            );
        }
        assert_eq!(fine_log.calls().last(), Some(&(8_000, Call::Clear)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limit_followed_by_success_keeps_the_target() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (5_000, "Two")]));
        let (target, log) = FakeTarget::scripted(
            "t",
            0,
            t0,
            &[
                Reply::RateLimited,
                Reply::RateLimited,
                Reply::Ok,
                Reply::RateLimited,
            ],
        );
        let run = start(engine(config(), &source, lyrics, vec![target]), t0);

        run.until(12_000).await;
        run.stop().await;

        // Clear at 0 (limited, interval 1 s), "One" at 1 s (limited, 2 s),
        // "One" at 3 s (ok), "Two" at 5 s (limited again, but the count was
        // reset: interval 4 s), "Two" at 9 s (ok).
        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (1_000, line("One")),
                (3_000, line("One")),
                (5_000, line("Two")),
                (9_000, line("Two")),
                (12_000, Call::Clear),
            ],
            2,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unauthorized_switches_the_target_off() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (1_000, "Two")]));
        let (denied, denied_log) = FakeTarget::scripted("denied", 0, t0, &[Reply::Unauthorized]);
        let (fine, fine_log) = FakeTarget::new("fine", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![denied, fine]), t0);

        run.until(3_000).await;
        run.stop().await;

        assert_eq!(denied_log.calls(), [(0, Call::Clear)]);
        assert_calls(
            &fine_log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, line("One")),
                (1_000, line("Two")),
                (3_000, Call::Clear),
            ],
            2,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unauthorized_set_switches_the_target_off_too() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (1_000, "Two")]));
        let (denied, denied_log) =
            FakeTarget::scripted("denied", 0, t0, &[Reply::Ok, Reply::Unauthorized]);
        let run = start(engine(config(), &source, lyrics, vec![denied]), t0);

        run.until(3_000).await;
        run.stop().await;

        // Switched off after its first set, and so not cleared on shutdown.
        assert_eq!(
            denied_log.calls(),
            [(0, Call::Clear), (0, no_lyrics("Song"))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unavailable_and_other_errors_are_retried() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (2_200, "Two")]));
        let (flaky, log) = FakeTarget::scripted(
            "flaky",
            0,
            t0,
            &[Reply::Unavailable, Reply::Other, Reply::Unavailable],
        );
        let run = start(engine(config(), &source, lyrics, vec![flaky]), t0);

        run.until(5_000).await;
        run.stop().await;

        // Failed at 0, 1 and 2 s (retries wait 1 s); then the newest value.
        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (1_000, line("One")),
                (2_000, line("One")),
                (3_000, line("Two")),
                (5_000, Call::Clear),
            ],
            0,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_target_that_does_not_answer_times_out_and_is_retried() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (8_000, "Two")]));
        let (stuck, stuck_log) = FakeTarget::scripted("stuck", 0, t0, &[Reply::Hang]);
        let (fine, fine_log) = FakeTarget::new("fine", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![stuck, fine]), t0);

        run.until(10_000).await;
        run.stop().await;

        // The startup clear hung for 5 s, holding up the other target once.
        assert_eq!(fine_log.calls()[0], (5_000, Call::Clear));
        assert_eq!(fine_log.last_call_until(7_999), Some(line("One")));
        assert_eq!(fine_log.last_call_until(9_999), Some(line("Two")));
        // Retried a second after the timeout, then fine.
        let stuck_calls = stuck_log.calls();
        assert_eq!(stuck_calls[0], (0, Call::Clear));
        assert_eq!(stuck_calls[1].0, 6_000);
        assert_eq!(stuck_log.last_call_until(9_999), Some(line("Two")));
    }

    // ---------------------------------------------------------------------
    // Source errors and polling
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn source_errors_do_not_stop_the_loop() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.fail("no session bus");
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(2_050).await;
        let failed_reads = source.reads();
        source.play(song("Song"), 0);
        run.until(3_050).await;
        source.fail("player went away");
        run.until(4_000).await;
        run.stop().await;

        assert_eq!(failed_reads, 5, "read at 0, 0.5, 1, 1.5 and 2 s");
        // An error keeps the last status: the song may still be playing.
        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (2_500, no_lyrics("Song")),
                (2_500, line("One")),
                (4_000, Call::Clear),
            ],
            0,
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_poll_interval_comes_from_the_settings_with_a_floor() {
        for (setting, expected_reads) in [(250u64, 9usize), (0, 21), (1_000, 3)] {
            let t0 = Instant::now();
            let source = SourceHandle::default();
            let mut config = config();
            config.general.poll_interval_ms = setting;
            let run = start(
                engine(config, &source, FakeLyrics::default(), Vec::new()),
                t0,
            );
            run.until(2_050).await;
            run.stop().await;
            assert_eq!(
                source.reads(),
                expected_reads,
                "poll_interval_ms = {setting}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_source_does_not_hold_up_lines_or_targets() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        // Every read takes 400 ms, most of the 500 ms poll interval.
        source.slow(400);
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with(
            "Song",
            0,
            synced(&[
                (1_000, "One"),
                (1_200, "Two"),
                (2_100, "Three"),
                (3_050, "Four"),
            ]),
        );
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(3_500).await;
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                // The first reading arrives at 400 ms.
                (400, no_lyrics("Song")),
                (400, intro()),
                (1_000, line("One")),
                (1_200, line("Two")),
                (2_100, line("Three")),
                (3_050, line("Four")),
                (3_500, Call::Clear),
            ],
            2,
        );
        assert_eq!(source.max_in_flight(), 1, "one read at a time");
    }

    #[tokio::test(start_paused = true)]
    async fn lines_keep_coming_while_the_source_does_not_answer() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics =
            FakeLyrics::default().with("Song", 0, synced(&[(2_000, "One"), (4_000, "Two")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![fast]), t0);

        run.until(1_050).await;
        source.hang();
        run.until(4_500).await;
        let reads = source.reads();
        let returned_at = run.stop().await;

        // The clock keeps the song going while the stuck read waits.
        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, intro()),
                (2_000, line("One")),
                (4_000, line("Two")),
                (4_500, Call::Clear),
            ],
            2,
        );
        // The stuck read is not stacked with new ones; shutdown does not wait for it.
        assert_eq!(reads, 4, "read at 0, 0.5, 1 and 1.5 s, then stuck");
        assert_eq!(source.max_in_flight(), 1);
        assert_eq!(returned_at, 4_500);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_that_never_finishes_times_out_and_polling_goes_on() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.hang();
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        // Polls at 0, 0.3, 0.6 … s: the time limit (10 s) falls between two.
        let mut config = config();
        config.general.poll_interval_ms = 300;
        let run = start(engine(config, &source, lyrics, vec![fast]), t0);

        run.until(1_000).await;
        source.slow(0);
        source.play(song("Song"), 0);
        let timeout = SOURCE_TIMEOUT.as_millis() as u64;
        let next_poll = timeout.div_ceil(300) * 300;
        assert!(next_poll > timeout);
        run.until(next_poll + 150).await;
        let reads = source.reads();
        run.stop().await;

        // The first read hung until the time limit (the polls meanwhile did
        // not stack reads on it); the next poll worked.
        assert_eq!(reads, 2, "at 0 s, then at the first poll after the limit");
        assert_eq!(source.max_in_flight(), 1);
        assert_eq!(
            log.calls(),
            [
                (0, Call::Clear),
                (next_poll, no_lyrics("Song")),
                (next_poll, line("One")),
                (next_poll + 150, Call::Clear),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_does_not_wait_for_a_send_that_hangs() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "Hello")]));
        // The startup clear works; the first set never answers.
        let (stuck, stuck_log) = FakeTarget::scripted("stuck", 0, t0, &[Reply::Ok, Reply::Hang]);
        let (fine, fine_log) = FakeTarget::new("fine", 0, t0);
        let run = start(engine(config(), &source, lyrics, vec![stuck, fine]), t0);

        run.until(1_000).await;
        let returned_at = run.stop().await;

        // The hanging set (until 5 s) is abandoned; the target may show it,
        // so it is cleared. The other target never got its turn.
        assert_eq!(returned_at, 1_000);
        assert_eq!(
            stuck_log.calls(),
            [
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (1_000, Call::Clear)
            ]
        );
        assert_eq!(fine_log.calls(), [(0, Call::Clear)]);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_the_startup_clear_returns_at_once() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        let (stuck, stuck_log) = FakeTarget::scripted("stuck", 0, t0, &[Reply::Hang]);
        let run = start(
            engine(config(), &source, FakeLyrics::default(), vec![stuck]),
            t0,
        );

        run.until(500).await;
        let returned_at = run.stop().await;

        assert_eq!(returned_at, 500);
        // Nothing was ever set, so there is nothing to clear on the way out.
        assert_eq!(stuck_log.calls(), [(0, Call::Clear)]);
        assert_eq!(source.reads(), 0);
    }

    // ---------------------------------------------------------------------
    // The view
    // ---------------------------------------------------------------------

    fn target_view(id: &str, state: TargetState, detail: Option<&str>) -> TargetView {
        TargetView {
            id: id.into(),
            state,
            detail: detail.map(str::to_string),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_follows_the_song_from_searching_to_found() {
        let dir = tempfile::tempdir().unwrap();
        let offsets = dir.path().join("offsets.toml");
        save_offset(&offsets, &song("Song"), 1_000);
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics =
            FakeLyrics::default().with("Song", 2_000, synced(&[(1_000, "One"), (3_000, "Two")]));
        let mut config = config();
        config.general.offset_ms = 500;
        // No targets: a window can run the engine only to show lyrics.
        let (engine, view) = watched(
            engine(config, &source, lyrics, Vec::new()).with_offsets_path(offsets),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_000).await;
        let searching = view.current();
        run.until(2_600).await;
        let found = view.current();
        run.stop().await;

        assert_eq!(searching.source, "fake");
        assert!(!searching.paused);
        assert!(searching.targets.is_empty());
        assert_eq!(
            searching.now,
            Some(NowView {
                title: "Song".into(),
                artist: "Artist".into(),
                album: None,
                duration_ms: Some(300_000),
                app: "fake.player".into(),
                playing: true,
                position_ms: 0,
                position_at_unix_ms: WALL_BASE,
                rate: 1.0,
                artwork: None,
                song_key: crate::matcher::song_key(&crate::matcher::normalize_track(&song("Song"))),
                song_offset_ms: 1_000,
                global_offset_ms: 500,
                lyrics: LyricsView::Searching,
            })
        );
        assert_eq!(
            searching.status,
            Some(StatusView {
                text: "Song · Artist".into(),
                kind: StatusKindView::NoLyrics,
                line: None,
                estimated: false,
            })
        );

        let now = found.now.unwrap();
        assert_eq!(
            now.lyrics,
            LyricsView::Found {
                lines: vec![
                    LineView {
                        start_ms: 1_000,
                        text: "One".into()
                    },
                    LineView {
                        start_ms: 3_000,
                        text: "Two".into()
                    },
                ],
                synced: true,
                instrumental: false,
                source: "fake".into(),
            }
        );
        // The anchor did not move: the song only went on.
        assert_eq!((now.position_ms, now.position_at_unix_ms), (0, WALL_BASE));
        // 1.5 s of offsets: "One" (1 s) from 2.5 s on.
        assert_eq!(
            found.status,
            Some(StatusView {
                text: "🎵 One".into(),
                kind: StatusKindView::Line,
                line: Some("One".into()),
                estimated: false,
            })
        );

        let mut states: Vec<LyricsView> = view
            .seen()
            .into_iter()
            .filter_map(|(_, v)| v.now.map(|now| now.lyrics))
            .collect();
        states.dedup();
        assert_eq!(states.len(), 2, "{states:?}");
        assert_eq!(states[0], LyricsView::Searching);
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_has_the_lines_the_engine_uses_or_not_found() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Plain"), 0);
        let plain = Lyrics {
            lines: ["a", "b", "c"]
                .iter()
                .map(|text| LyricLine {
                    start_ms: 0,
                    text: text.to_string(),
                })
                .collect(),
            synced: false,
            instrumental: false,
            source: String::new(),
        };
        let lyrics = FakeLyrics::default().with("Plain", 0, plain.clone());
        let (engine, view) = watched(engine(config(), &source, lyrics, Vec::new()), t0);
        let run = start(engine, t0);

        run.until(100_000).await;
        let plain_view = view.current();
        source.play(song("Unknown"), 0);
        run.until(101_000).await;
        let unknown_view = view.current();
        run.stop().await;

        // Spread over the song, exactly as the status uses them.
        let mut spread = plain;
        spread.spread_evenly(300_000);
        assert!(spread.lines[1].start_ms > 0);
        assert_eq!(
            plain_view.now.unwrap().lyrics,
            LyricsView::Found {
                lines: spread.lines.iter().map(LineView::from).collect(),
                synced: false,
                instrumental: false,
                source: "fake".into(),
            }
        );
        let status = plain_view.status.unwrap();
        assert!(status.estimated);
        assert_eq!(status.kind, StatusKindView::Line);

        let now = unknown_view.now.unwrap();
        assert_eq!(now.title, "Unknown");
        assert_eq!(now.lyrics, LyricsView::NotFound);
        assert_eq!(unknown_view.status.unwrap().kind, StatusKindView::NoLyrics);
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_anchor_moves_on_a_seek_and_play_pause_only() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "Hello")]));
        let (fast, _log) = FakeTarget::new("fast", 0, t0);
        let (engine, view) = watched(engine(config(), &source, lyrics, vec![fast]), t0);
        let run = start(engine, t0);

        run.until(2_050).await;
        let start = view.now();
        source.play(song("Song"), 60_000);
        run.until(4_050).await;
        let seeked = view.now();
        source.pause();
        run.until(6_050).await;
        let paused = view.now();
        source.resume();
        run.until(8_000).await;
        let resumed = view.now();
        run.stop().await;

        let anchor = |now: &NowView| (now.playing, now.position_ms, now.position_at_unix_ms);
        assert_eq!(anchor(&start), (true, 0, WALL_BASE));
        // Seen at the 2.5 s poll, 450 ms after the seek.
        assert_eq!(anchor(&seeked), (true, 60_450, WALL_BASE + 2_500));
        // Paused at 4.05 s (at 62 s in the song), seen at 4.5 s.
        assert_eq!(anchor(&paused), (false, 62_000, WALL_BASE + 4_500));
        // Resumed at 6.05 s, seen at 6.5 s.
        assert_eq!(anchor(&resumed), (true, 62_450, WALL_BASE + 6_500));

        // Nothing else was published: smooth progress is not a change.
        assert_eq!(view.times_between(1, 8_000), [2_500, 4_500, 6_500]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_anchor_moves_when_the_clock_drifts_more_than_250_ms() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        // The player is 200 ms ahead at 1.05 s, seen at 1.5 s: the clock
        // follows (well below a seek), the view does not need to.
        run.until(1_050).await;
        source.play(song("Song"), 1_250);
        run.until(2_050).await;
        let small = view.now();
        // 200 ms more: 400 ms from the published anchor.
        source.play(song("Song"), 2_450);
        run.until(3_000).await;
        let drifted = view.now();
        run.stop().await;

        assert_eq!(
            (small.position_ms, small.position_at_unix_ms),
            (0, WALL_BASE)
        );
        assert_eq!(
            (drifted.position_ms, drifted.position_at_unix_ms),
            (2_900, WALL_BASE + 2_500)
        );
        assert_eq!(view.times_between(1, 3_000), [2_500]);
    }

    #[test]
    fn anchor_jumps() {
        let anchor = Anchor {
            generation: 1,
            playing: true,
            rate: 1.0,
            position_ms: 10_000,
            at_unix_ms: WALL_BASE,
        };
        let later = |ms: u64, position_ms: u64| Anchor {
            position_ms,
            at_unix_ms: WALL_BASE + ms,
            ..anchor
        };
        let follow = |current: Anchor| Anchor::follow(Some(anchor), current, Some(300_000));

        assert_eq!(Anchor::follow(None, anchor, None), anchor);
        // Smooth progress and small differences keep the anchor.
        assert_eq!(follow(later(5_000, 15_000)), anchor);
        assert_eq!(follow(later(5_000, 15_250)), anchor);
        assert_eq!(follow(later(5_000, 14_750)), anchor);
        // More than 250 ms either way, another song, play/pause or another
        // rate is a jump.
        for current in [
            later(5_000, 15_251),
            later(5_000, 14_749),
            later(5_000, 60_000),
            Anchor {
                generation: 2,
                ..later(5_000, 15_000)
            },
            Anchor {
                playing: false,
                ..later(5_000, 15_000)
            },
            Anchor {
                rate: 1.5,
                ..later(5_000, 15_000)
            },
        ] {
            assert_eq!(follow(current), current, "{current:?}");
        }
    }

    #[test]
    fn anchor_positions() {
        let anchor = Anchor {
            generation: 1,
            playing: true,
            rate: 2.0,
            position_ms: 10_000,
            at_unix_ms: WALL_BASE,
        };
        assert_eq!(anchor.position_at(WALL_BASE + 1_000, None), 12_000);
        // Stops at the end of the song, like the clock.
        assert_eq!(
            anchor.position_at(WALL_BASE + 100_000, Some(30_000)),
            30_000
        );
        assert_eq!(anchor.position_at(WALL_BASE + 100_000, Some(0)), 210_000);
        // A wall clock that went back does not move the song back.
        assert_eq!(anchor.position_at(WALL_BASE - 5_000, None), 10_000);
        let paused = Anchor {
            playing: false,
            ..anchor
        };
        assert_eq!(paused.position_at(WALL_BASE + 5_000, None), 10_000);
        let backwards = Anchor {
            rate: -1.0,
            ..anchor
        };
        assert_eq!(backwards.position_at(WALL_BASE + 4_000, None), 6_000);
        assert_eq!(backwards.position_at(WALL_BASE + 40_000, None), 0);
        let absurd = Anchor {
            rate: 1e300,
            ..anchor
        };
        assert_eq!(absurd.position_at(WALL_BASE + 1, None), u64::MAX);

        // At the end of a song both the clock and the anchor stand still,
        // so a player that goes on past the end publishes nothing.
        let at_end = Anchor {
            rate: 1.0,
            position_ms: 30_000,
            ..anchor
        };
        let current = Anchor {
            position_ms: 30_000,
            at_unix_ms: WALL_BASE + 60_000,
            ..at_end
        };
        assert_eq!(Anchor::follow(Some(at_end), current, Some(30_000)), at_end);
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_shows_how_each_target_is_doing() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One"), (2_200, "Two")]));
        let (flaky, _) = FakeTarget::scripted(
            "flaky",
            0,
            t0,
            &[Reply::Unavailable, Reply::Other, Reply::Unavailable],
        );
        let (denied, _) = FakeTarget::scripted("denied", 0, t0, &[Reply::Unauthorized]);
        let (limited, _) = FakeTarget::scripted("limited", 0, t0, &[Reply::RateLimited]);
        let (fine, _) = FakeTarget::new("fine", 0, t0);
        let (engine, view) = watched(
            engine(
                config(),
                &source,
                lyrics,
                vec![flaky, denied, limited, fine],
            ),
            t0,
        );
        let run = start(engine, t0);

        let mut targets = Vec::new();
        for ms in [500, 1_500, 2_500, 3_500] {
            run.until(ms).await;
            targets.push(view.current().targets);
        }
        source.stop();
        run.until(4_500).await;
        targets.push(view.current().targets);
        run.stop().await;

        let off = target_view(
            "denied",
            TargetState::Off,
            Some("not authorized: token revoked"),
        );
        let showing = |id: &str| target_view(id, TargetState::Showing, None);
        // Flaky: unavailable at 0 s (the startup clear), another error at
        // 1 s, unavailable again at 2 s, fine at 3 s. Limited: rate limited
        // at 0 s, fine at 1 s.
        assert_eq!(
            targets[0],
            [
                target_view("flaky", TargetState::Waiting, Some("not running")),
                off.clone(),
                target_view(
                    "limited",
                    TargetState::RateLimited,
                    Some("rate limited, waiting 1 s")
                ),
                showing("fine"),
            ]
        );
        assert_eq!(
            targets[1],
            [
                target_view("flaky", TargetState::Retrying, Some("broken pipe")),
                off.clone(),
                showing("limited"),
                showing("fine"),
            ]
        );
        assert_eq!(
            targets[2][0],
            target_view("flaky", TargetState::Waiting, Some("not running"))
        );
        assert_eq!(
            targets[3],
            [
                showing("flaky"),
                off.clone(),
                showing("limited"),
                showing("fine")
            ]
        );
        // Nothing plays any more: cleared, except the one switched off.
        let cleared = |id: &str| target_view(id, TargetState::Cleared, None);
        assert_eq!(
            targets[4],
            [cleared("flaky"), off, cleared("limited"), cleared("fine")]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_target_is_starting_until_it_answers() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        let (stuck, _) = FakeTarget::scripted("stuck", 0, t0, &[Reply::Hang]);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), vec![stuck]),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_000).await;
        let starting = view.current();
        run.until(5_500).await;
        let timed_out = view.current();
        run.until(6_500).await;
        let retried = view.current();
        run.stop().await;

        assert_eq!(starting.source, "fake");
        assert_eq!(
            starting.targets,
            [target_view("stuck", TargetState::Starting, None)]
        );
        assert_eq!(
            timed_out.targets,
            [target_view(
                "stuck",
                TargetState::Retrying,
                Some("no answer within 5 s, will try again")
            )]
        );
        // Tried again a second later, and cleared.
        assert_eq!(
            retried.targets,
            [target_view("stuck", TargetState::Cleared, None)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_view_shows_the_pause_marker() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("paused");
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let lyrics = FakeLyrics::default().with("Song", 0, synced(&[(0, "One")]));
        let (fast, _) = FakeTarget::new("fast", 0, t0);
        let (engine, view) = watched(
            engine(config(), &source, lyrics, vec![fast]).with_pause_marker(marker.clone()),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_000).await;
        let sharing = view.current();
        std::fs::write(&marker, b"").unwrap();
        run.until(2_000).await;
        let paused = view.current();
        std::fs::remove_file(&marker).unwrap();
        run.until(3_000).await;
        let resumed = view.current();
        run.stop().await;

        assert!(!sharing.paused);
        assert_eq!(sharing.status.unwrap().text, "🎵 One");
        // Paused: the song is still followed, but nothing is shown.
        assert!(paused.paused);
        assert_eq!(paused.status, None);
        assert_eq!(paused.now.unwrap().title, "Song");
        assert_eq!(
            paused.targets,
            [target_view("fast", TargetState::Cleared, None)]
        );
        assert!(!resumed.paused);
        assert_eq!(resumed.status.unwrap().text, "🎵 One");
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_playing_means_no_song_in_the_view() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_050).await;
        let nothing = view.current();
        source.play(song("Song"), 0);
        run.until(2_050).await;
        let playing = view.current();
        source.stop();
        run.until(3_050).await;
        let stopped = view.current();
        run.stop().await;

        assert_eq!(nothing.source, "fake");
        assert_eq!(nothing.now, None);
        assert_eq!(nothing.status, None);
        assert_eq!(playing.now.unwrap().title, "Song");
        assert!(playing.status.is_some());
        assert_eq!(stopped.now, None);
        assert_eq!(stopped.status, None);
    }

    // ---------------------------------------------------------------------
    // Cover art
    // ---------------------------------------------------------------------

    /// The covers the view showed for the song `title`, in order, without
    /// repeats, from the first one on (before its first read answers, a
    /// song has none in the view).
    fn covers_shown(view: &ViewLog, title: &str) -> Vec<Option<String>> {
        let mut covers: Vec<Option<String>> = view
            .seen()
            .into_iter()
            .filter_map(|(_, v)| v.now.filter(|now| now.title == title))
            .map(|now| now.artwork)
            .skip_while(Option::is_none)
            .collect();
        covers.dedup();
        covers
    }

    #[tokio::test(start_paused = true)]
    async fn the_cover_is_read_again_for_3_s_after_each_track_change() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.play(song("Song A"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_050).await;
        let first = view.now();
        // A seek, a pause and a resume are the same song.
        source.play(song("Song A"), 60_000);
        run.until(2_050).await;
        source.pause();
        run.until(3_050).await;
        source.resume();
        run.until(4_050).await;
        let reads_for_a = source.cover_reads();
        source.play(song("Song B"), 0);
        run.until(9_050).await;
        let second = view.now();
        run.stop().await;

        assert_eq!(first.artwork, Some(cover_of("Song A")));
        // On the track change at 0 s, then after the polls at 0.5 s to 2.5 s.
        assert_eq!(reads_for_a, 6);
        assert_eq!(second.title, "Song B");
        assert_eq!(second.artwork, Some(cover_of("Song B")));
        // B was seen at 4.5 s: read then and after the polls up to 7 s.
        assert_eq!(source.cover_reads(), 12);
        // Reading the same cover again never takes it away.
        for title in ["Song A", "Song B"] {
            assert_eq!(covers_shown(&view, title), [Some(cover_of(title))]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_cover_that_comes_after_the_title_is_picked_up() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.play(song("Song A"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        // Like Chromium: B's title comes with A's cover still in place (seen
        // at 4.5 s), and B's cover a little later.
        run.until(4_050).await;
        source.stuck_cover(cover_of("Song A"));
        source.play(song("Song B"), 0);
        run.until(4_600).await;
        let stale = view.now();
        run.until(5_250).await;
        source.with_covers();
        run.until(5_600).await;
        let corrected = view.now();
        // Like Firefox: C's title comes with no cover (seen at 8.5 s), and
        // C's cover 2.25 s later.
        run.until(8_050).await;
        source.without_covers();
        source.play(song("Song C"), 0);
        run.until(8_600).await;
        let missing = view.now();
        run.until(10_750).await;
        source.with_covers();
        run.until(11_100).await;
        let arrived = view.now();
        run.stop().await;

        assert_eq!(stale.title, "Song B");
        assert_eq!(stale.artwork, Some(cover_of("Song A")));
        assert_eq!(corrected.artwork, Some(cover_of("Song B")));
        assert_eq!(missing.title, "Song C");
        assert_eq!(missing.artwork, None);
        assert_eq!(arrived.artwork, Some(cover_of("Song C")));
        // Published as soon as a read gave them.
        let published_at = |title: &str, cover: &str| {
            view.seen()
                .into_iter()
                .find(|(_, v)| {
                    v.now.as_ref().is_some_and(|now| {
                        now.title == title && now.artwork.as_deref() == Some(cover)
                    })
                })
                .map(|(at, _)| at)
        };
        assert_eq!(published_at("Song B", &cover_of("Song B")), Some(5_500));
        assert_eq!(published_at("Song C", &cover_of("Song C")), Some(11_000));
        assert_eq!(
            covers_shown(&view, "Song B"),
            [Some(cover_of("Song A")), Some(cover_of("Song B"))]
        );
        assert_eq!(covers_shown(&view, "Song C"), [Some(cover_of("Song C"))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_cover_that_comes_3_s_after_the_title_is_not_read_anymore() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        run.until(3_250).await;
        source.with_covers();
        run.until(10_000).await;
        run.stop().await;

        assert_eq!(source.cover_reads(), 6);
        assert_eq!(view.now().artwork, None);
    }

    #[tokio::test(start_paused = true)]
    async fn with_a_slow_poll_the_cover_is_still_read_again_on_the_next_one() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.play(song("Song"), 0);
        let mut config = config();
        config.general.poll_interval_ms = 5_000;
        let (engine, view) = watched(
            engine(config, &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        run.until(1_000).await;
        let missing = view.now();
        source.with_covers();
        run.until(5_100).await;
        let arrived = view.now();
        run.until(12_000).await;
        run.stop().await;

        assert_eq!(missing.artwork, None);
        assert_eq!(arrived.artwork, Some(cover_of("Song")));
        // At 0 s and 5 s; not at 10 s.
        assert_eq!(source.cover_reads(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_cover_read_keeps_the_cover_and_the_next_poll_tries_again() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.fail_covers("the previous cover art read is still running");
        source.play(song("Song"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        run.until(750).await;
        let failed = view.now();
        source.fix_covers();
        run.until(1_100).await;
        let read = view.now();
        // The reads at 1.5 s and 2 s fail: the cover stays.
        source.fail_covers("the cover art did not arrive within 5 s");
        run.until(2_250).await;
        let kept = view.now();
        // The player says it has no cover anymore (read at 2.5 s).
        source.fix_covers();
        source.without_covers();
        run.until(2_600).await;
        let gone = view.now();
        run.stop().await;

        assert_eq!(failed.artwork, None);
        assert_eq!(read.artwork, Some(cover_of("Song")));
        assert_eq!(kept.artwork, Some(cover_of("Song")));
        assert_eq!(gone.artwork, None);
        assert_eq!(source.cover_reads(), 6);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_cover_never_holds_up_lines() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.slow_covers(3_000);
        source.play(song("Song"), 0);
        let lyrics =
            FakeLyrics::default().with("Song", 0, synced(&[(1_000, "One"), (2_000, "Two")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let (engine, view) = watched(engine(config(), &source, lyrics, vec![fast]), t0);
        let run = start(engine, t0);

        run.until(2_500).await;
        let waiting = view.now();
        run.until(3_500).await;
        let arrived = view.now();
        run.stop().await;

        assert_calls(
            &log.calls(),
            &[
                (0, Call::Clear),
                (0, no_lyrics("Song")),
                (0, intro()),
                (1_000, line("One")),
                (2_000, line("Two")),
                (3_500, Call::Clear),
            ],
            2,
        );
        assert_eq!(waiting.artwork, None);
        assert_eq!(arrived.artwork, Some(cover_of("Song")));
        // Published as soon as it came.
        assert_eq!(view.times_between(2_001, 3_400), [3_000]);
        // The song went on while the cover was read: the anchor never moved.
        assert_eq!(
            (arrived.position_ms, arrived.position_at_unix_ms),
            (0, WALL_BASE)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cover_for_an_earlier_song_is_never_shown() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.slow_covers(2_000);
        source.play(song("Song A"), 0);
        let (engine, view) = watched(
            engine(config(), &source, FakeLyrics::default(), Vec::new()),
            t0,
        );
        let run = start(engine, t0);

        // A's cover would come at 2 s; B starts at 1.5 s.
        run.until(1_050).await;
        source.play(song("Song B"), 0);
        run.until(3_000).await;
        let waiting = view.now();
        run.until(4_200).await;
        let arrived = view.now();
        run.stop().await;

        assert_eq!(waiting.title, "Song B");
        assert_eq!(waiting.artwork, None);
        assert_eq!(arrived.artwork, Some(cover_of("Song B")));
        // A's, B's (asked at 1.5 s, here at 3.5 s), and B's again (asked at
        // 3.5 s, still under way).
        assert_eq!(source.cover_reads(), 3);
        let a_cover = Some(cover_of("Song A"));
        assert!(
            view.seen()
                .iter()
                .all(|(_, v)| v.now.as_ref().map(|now| &now.artwork) != Some(&a_cover)),
            "{:?}",
            view.seen()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cover_read_gives_up_after_5_s_and_errors_mean_no_cover() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.hang_covers();
        source.play(song("Song A"), 0);
        let lyrics = FakeLyrics::default().with("Song A", 0, synced(&[(4_000, "Late")]));
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let (engine, view) = watched(engine(config(), &source, lyrics, vec![fast]), t0);
        let run = start(engine, t0);

        run.until(4_900).await;
        let before = (source.cover_reads(), source.covers_in_flight());
        run.until(5_100).await;
        let after = (source.cover_reads(), source.covers_in_flight());
        // The next song's cover works again; the one after fails.
        source.slow_covers(0);
        source.play(song("Song B"), 0);
        run.until(5_600).await;
        let b = view.now();
        source.fail_covers("no cover for this one");
        source.play(song("Song C"), 0);
        run.until(6_600).await;
        let c = view.now();
        run.stop().await;

        // One read, under way: the polls meanwhile did not stack more.
        assert_eq!(before, (1, 1));
        // Abandoned at 5 s; the poll then asked again (that read hangs too).
        assert_eq!(after, (2, 1));
        assert_eq!(log.last_call_until(4_500), Some(line("Late")));
        assert_eq!(b.artwork, Some(cover_of("Song B")));
        assert_eq!(c.title, "Song C");
        assert_eq!(c.artwork, None);
        // C was asked at 6 s and 6.5 s.
        assert_eq!(source.cover_reads(), 5);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_view_the_cover_is_never_read() {
        let t0 = Instant::now();
        let source = SourceHandle::default();
        source.with_covers();
        source.play(song("Song"), 0);
        let (fast, log) = FakeTarget::new("fast", 0, t0);
        let run = start(
            engine(config(), &source, FakeLyrics::default(), vec![fast]),
            t0,
        );

        run.until(4_000).await;
        run.stop().await;

        assert_eq!(source.cover_reads(), 0);
        assert_eq!(log.sets(), [(0, "Song · Artist".to_string())]);
    }

    #[test]
    fn window_urls() {
        for url in [
            "https://i.scdn.co/image/abc",
            "HTTP://example.com/a.png",
            "data:image/png;base64,iVBORw0KGgo=",
        ] {
            assert!(is_window_url(url), "{url}");
        }
        for url in [
            "",
            "file:///tmp/a.png",
            "javascript:alert(1)",
            "/tmp/a.png",
            "http",
            "ftp://example.com/a.png",
            "♫♫♫♫♫",
        ] {
            assert!(!is_window_url(url), "{url}");
        }
    }

    // ---------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn repeat_filter_lets_a_message_through_once_a_minute() {
        let mut filter = RepeatFilter::default();
        let t = Instant::now();
        assert!(filter.first_in_a_while("bus down", t));
        assert!(!filter.first_in_a_while("bus down", t));
        assert!(filter.first_in_a_while("other", t + Duration::from_secs(1)));
        assert!(!filter.first_in_a_while("bus down", t + Duration::from_secs(30)));
        assert!(!filter.first_in_a_while("other", t + Duration::from_secs(59)));
        assert!(!filter.first_in_a_while("bus down", t + Duration::from_millis(59_999)));
        assert!(filter.first_in_a_while("bus down", t + Duration::from_secs(60)));
        assert!(!filter.first_in_a_while("bus down", t + Duration::from_secs(61)));
        assert!(filter.first_in_a_while("other", t + Duration::from_secs(61)));
        assert_eq!(filter.last_let_through.len(), 2);
        // Old entries are forgotten.
        assert!(filter.first_in_a_while("third", t + Duration::from_secs(300)));
        assert_eq!(filter.last_let_through.len(), 1);
    }

    #[test]
    fn offsets_file_notices_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        let mut file = OffsetsFile::new(Some(path.clone()));
        assert!(!file.changed(), "missing and never read");
        file.reload();
        assert_eq!(file.get("a - b"), 0);

        let mut offsets = Offsets::default();
        offsets.set("a - b", 250);
        offsets.save(&path).unwrap();
        assert!(file.changed());
        file.reload();
        assert!(!file.changed());
        assert_eq!(file.get("a - b"), 250);

        std::fs::remove_file(&path).unwrap();
        assert!(file.changed());
        file.reload();
        assert_eq!(file.get("a - b"), 0);

        let mut none = OffsetsFile::new(None);
        assert!(!none.changed());
        none.reload();
        assert_eq!(none.get("a - b"), 0);
    }

    #[test]
    fn unix_now_is_after_2020() {
        assert!(unix_now_ms() > 1_577_836_800_000);
    }
}
