//! Keeping track of the song position between source readings.
//!
//! Sources are polled every few hundred milliseconds and some (Windows) only
//! refresh their position when something happens, so the clock extrapolates:
//! `position = anchor_position + (now - anchor_time) * rate` while playing.

use crate::types::{PlaybackSnapshot, PlaybackStatus};
use std::time::Instant;

/// How far a reported position may drift from the prediction before it counts
/// as a seek, in milliseconds.
pub const SEEK_TOLERANCE_MS: u64 = 1_500;

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
    _private: (),
}

impl SyncClock {
    pub fn new() -> Self {
        todo!()
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
    pub fn update(&mut self, snapshot: &PlaybackSnapshot) -> ClockEvent {
        let _ = snapshot;
        todo!()
    }

    /// Current estimated position in ms at `now`, or `None` before any snapshot.
    /// Extrapolates while playing (respecting `rate`), stays put while paused,
    /// never goes below 0, and never exceeds the track duration when known.
    /// `now` earlier than the anchor time counts as the anchor time.
    pub fn position_ms(&self, now: Instant) -> Option<u64> {
        let _ = now;
        todo!()
    }

    /// Status from the last snapshot, or `None` before any snapshot.
    pub fn status(&self) -> Option<PlaybackStatus> {
        todo!()
    }

    /// Forgets everything (used when nothing is playing any more).
    pub fn reset(&mut self) {
        todo!()
    }
}
