//! What the engine is doing, in a shape a user interface can show.
//!
//! The engine publishes an [`EngineView`] through a `tokio::sync::watch`
//! channel (see `Engine::with_view`). Every type here serializes to the JSON
//! described in `desktop/CONTRACT.md` (camelCase keys, tagged enums), which is
//! what the desktop window receives.

use crate::types::{LyricLine, StatusKind};
use serde::Serialize;

/// Everything a window needs to draw the current moment.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineView {
    /// Name of the now-playing source, e.g. `windows-media`, `mpris`, `macos`.
    pub source: String,
    /// True while statuses are paused by the user (the pause marker exists).
    pub paused: bool,
    /// The song being followed, `None` when nothing is playing or paused.
    pub now: Option<NowView>,
    /// What the targets are asked to show right now, `None` when cleared.
    pub status: Option<StatusView>,
    /// One entry per target, in the order they were given to the engine.
    pub targets: Vec<TargetView>,
}

/// The song being followed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NowView {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    /// The playing app as the source reports it, e.g. `Spotify.exe`.
    pub app: String,
    pub playing: bool,
    /// Playback position at `position_at_unix_ms`. While `playing`, the
    /// position at any later moment is
    /// `position_ms + (now_unix_ms - position_at_unix_ms) * rate`, stopping
    /// at `duration_ms` when it is known (as the engine's clock does).
    /// The engine only moves this anchor on a jump (track change, seek,
    /// play/pause, rate change, or drift above 250 ms), so it does not change
    /// on every poll.
    pub position_ms: u64,
    pub position_at_unix_ms: u64,
    pub rate: f64,
    /// Cover art as a URL the window can load: `https:`, `http:` or `data:`.
    /// `None` when the player has none (or it is still being read).
    pub artwork: Option<String>,
    /// [`crate::matcher::song_key`] of the normalized track; per-song offsets
    /// are stored under this key.
    pub song_key: String,
    /// This song's offset (positive = lyrics later).
    pub song_offset_ms: i64,
    /// `general.offset_ms`, applied to every song on top of the song offset.
    pub global_offset_ms: i64,
    pub lyrics: LyricsView,
}

/// Where the lyrics lookup for the current song stands.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LyricsView {
    /// The lookup is under way.
    Searching,
    /// No provider had lyrics (or the lookup failed).
    NotFound,
    /// Lyrics as the engine uses them: unsynced lyrics already carry the
    /// estimated timings the engine spread over the song.
    #[serde(rename_all = "camelCase")]
    Found {
        lines: Vec<LineView>,
        /// Timings come from the source (LRC timestamps).
        synced: bool,
        /// The provider says the song has no vocals.
        instrumental: bool,
        /// Provider name, e.g. `lrclib`, `local`, `cache`.
        source: String,
    },
}

/// One lyric line. The line at a position `p` is the last one whose
/// `start_ms <= p - (song_offset_ms + global_offset_ms)`; empty text is a
/// break between vocal sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineView {
    pub start_ms: u64,
    pub text: String,
}

impl From<&LyricLine> for LineView {
    fn from(line: &LyricLine) -> Self {
        Self {
            start_ms: line.start_ms,
            text: line.text.clone(),
        }
    }
}

/// The status every target is asked to show.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    /// Rendered text (template applied, profanity filtered, not truncated).
    pub text: String,
    pub kind: StatusKindView,
    /// The raw lyric line when `kind` is `line`.
    pub line: Option<String>,
    /// Lyric timing is estimated (plain lyrics spread over the song).
    pub estimated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum StatusKindView {
    Line,
    Instrumental,
    NoLyrics,
}

impl From<StatusKind> for StatusKindView {
    fn from(kind: StatusKind) -> Self {
        match kind {
            StatusKind::Line => Self::Line,
            StatusKind::Instrumental => Self::Instrumental,
            StatusKind::NoLyrics => Self::NoLyrics,
        }
    }
}

/// How one target is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetView {
    /// The target's `name()`, e.g. `discord`.
    pub id: String,
    pub state: TargetState,
    /// A short, human explanation for `waiting`, `retrying`, `rateLimited`
    /// and `off` (e.g. the error message), otherwise `None`.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetState {
    /// Nothing was sent yet.
    Starting,
    /// The last `set` worked: the status is showing.
    Showing,
    /// The last `clear` worked: nothing is showing.
    Cleared,
    /// The target cannot be reached right now (`Unavailable`), e.g. Discord
    /// is not running. Retried automatically.
    Waiting,
    /// The service asked to slow down; retried after a backoff.
    RateLimited,
    /// Another error; retried automatically.
    Retrying,
    /// Switched off for this run (`Unauthorized`, or rate limited too often).
    Off,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_contract_shape() {
        let view = EngineView {
            source: "mpris".into(),
            paused: false,
            now: Some(NowView {
                title: "Song".into(),
                artist: "Artist".into(),
                album: None,
                duration_ms: Some(200_000),
                app: "org.mpris.MediaPlayer2.spotify".into(),
                playing: true,
                position_ms: 1_000,
                position_at_unix_ms: 1_700_000_000_000,
                rate: 1.0,
                artwork: None,
                song_key: "artist - song".into(),
                song_offset_ms: -250,
                global_offset_ms: 0,
                lyrics: LyricsView::Found {
                    lines: vec![LineView {
                        start_ms: 0,
                        text: "la".into(),
                    }],
                    synced: true,
                    instrumental: false,
                    source: "lrclib".into(),
                },
            }),
            status: Some(StatusView {
                text: "🎵 la".into(),
                kind: StatusKindView::NoLyrics,
                line: None,
                estimated: false,
            }),
            targets: vec![TargetView {
                id: "discord".into(),
                state: TargetState::RateLimited,
                detail: Some("slow down".into()),
            }],
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json["now"]["positionAtUnixMs"], 1_700_000_000_000u64);
        assert_eq!(json["now"]["songOffsetMs"], -250);
        assert_eq!(json["now"]["lyrics"]["state"], "found");
        assert_eq!(json["now"]["lyrics"]["lines"][0]["startMs"], 0);
        assert_eq!(json["status"]["kind"], "noLyrics");
        assert_eq!(json["targets"][0]["state"], "rateLimited");
        assert_eq!(
            serde_json::to_value(LyricsView::Searching).unwrap(),
            serde_json::json!({"state": "searching"})
        );
        assert_eq!(
            serde_json::to_value(LyricsView::NotFound).unwrap(),
            serde_json::json!({"state": "notFound"})
        );
    }
}
