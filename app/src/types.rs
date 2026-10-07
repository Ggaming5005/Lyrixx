//! Shared data types passed between sources, providers, the engine and targets.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// A song as reported by a now-playing source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Track {
    pub title: String,
    /// All artists as one string, exactly as the source reported them
    /// (MPRIS artist lists are joined with ", ").
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    /// Spotify track id (the 22-character base62 id) when the source exposes it.
    pub spotify_id: Option<String>,
}

/// Whether the player is currently playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    Stopped,
}

/// One reading from a now-playing source.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackSnapshot {
    pub track: Track,
    pub status: PlaybackStatus,
    /// Position reported by the player, in milliseconds.
    pub position_ms: u64,
    /// The moment `position_ms` was true. Sources that report their own
    /// timestamp (Windows `LastUpdatedTime`, macOS `timestamp`) convert it to an
    /// `Instant`; sources that are read live (MPRIS) use the time of the read.
    pub position_at: Instant,
    /// Playback rate, 1.0 for normal speed.
    pub rate: f64,
    /// The playing app, e.g. `Spotify.exe`, `org.mpris.MediaPlayer2.spotify`,
    /// `com.spotify.client`. Used for privacy filters and for logs.
    pub app_id: String,
}

/// One timed line of lyrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    /// When the line starts, in milliseconds from the start of the song.
    pub start_ms: u64,
    /// The words. An empty string marks a break between vocal sections.
    pub text: String,
}

/// Lyrics for one song.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Lyrics {
    /// Lines sorted by `start_ms`. For unsynced lyrics every `start_ms` is 0
    /// until [`Lyrics::spread_evenly`](crate::lrc) assigns estimated times.
    pub lines: Vec<LyricLine>,
    /// True when the timings come from the source (LRC timestamps).
    pub synced: bool,
    /// True when the provider says the song has no vocals at all.
    pub instrumental: bool,
    /// The provider that supplied these lyrics, e.g. `lrclib`, `local`.
    pub source: String,
}

/// What kind of moment a [`Status`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatusKind {
    /// A lyric line is being sung.
    Line,
    /// Intro, instrumental break or an instrumental song.
    Instrumental,
    /// No lyrics are available (yet): the status names the song instead.
    NoLyrics,
}

/// What every target should show right now.
#[derive(Debug, Clone)]
pub struct Status {
    /// Rendered status text: template applied and profanity filtered, but not
    /// truncated (each target truncates to its own limit).
    pub text: String,
    pub kind: StatusKind,
    /// The raw lyric line, when `kind` is [`StatusKind::Line`].
    pub line: Option<String>,
    pub track: Track,
    /// Wall-clock time the song started (Unix ms), for progress bars.
    pub started_at_unix_ms: Option<u64>,
    /// Lyric timing is estimated (plain lyrics spread across the song).
    pub estimated: bool,
}

/// Two statuses are the same display when everything a person would see is the
/// same. Song start times within 2 seconds of each other count as equal, so the
/// small jitter between ticks never causes an extra update, while a seek does.
impl PartialEq for Status {
    fn eq(&self, other: &Self) -> bool {
        let started_close = match (self.started_at_unix_ms, other.started_at_unix_ms) {
            (Some(a), Some(b)) => a.abs_diff(b) <= 2_000,
            (None, None) => true,
            _ => false,
        };
        self.text == other.text
            && self.kind == other.kind
            && self.line == other.line
            && self.track == other.track
            && self.estimated == other.estimated
            && started_close
    }
}
