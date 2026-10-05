//! Parsing LRC lyric files and finding the line for a song position.
//!
//! Supported LRC features:
//! - `[mm:ss.xx]`, `[mm:ss.xxx]`, `[mm:ss]` and `[m:ss.x]` timestamps; minutes may exceed 59.
//! - Several timestamps on one line (`[00:10.00][01:20.00]chorus`) expand to one line per stamp.
//! - `[offset:+250]` / `[offset:-100]` (ms): positive offset makes lines appear earlier,
//!   per the LRC convention, so every stamp is shifted by `-offset` and clamped at 0.
//! - Metadata tags (`[ar:]`, `[ti:]`, `[al:]`, `[by:]`, `[length:]`, `[re:]`, `[ve:]`, `[#:]` …) are ignored.
//! - Enhanced (word-level) LRC: inline `<mm:ss.xx>` word stamps are removed from the text.
//! - Text is trimmed; CRLF line endings and a UTF-8 BOM are handled.
//! - Lines without a timestamp in a file that has timestamps are ignored.

use crate::types::{LyricLine, Lyrics};

/// Where a song position falls within the lyrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinePosition<'a> {
    /// Before the first non-empty line (song intro), or the lyrics have no lines.
    Intro,
    /// A line is being sung.
    Line { index: usize, text: &'a str },
    /// The current timed line is empty: an instrumental break between vocal sections.
    Break,
}

/// Parses LRC text. Returns `synced = true` lyrics when at least one timestamp
/// is found; otherwise falls back to [`from_plain`]. `source` is left empty for
/// the caller to fill in.
pub fn parse_lrc(input: &str) -> Lyrics {
    let _ = input;
    todo!()
}

/// Builds unsynced lyrics from plain text: one [`LyricLine`] per non-empty input
/// line, in order, each with `start_ms = 0`, and `synced = false`.
pub fn from_plain(text: &str) -> Lyrics {
    let _ = text;
    todo!()
}

/// Formats lyrics back to LRC text (`[mm:ss.xx]text` per line, newline separated).
/// Unsynced lyrics are written as plain text lines.
pub fn to_lrc(lyrics: &Lyrics) -> String {
    let _ = lyrics;
    todo!()
}

impl Lyrics {
    /// Index of the last line whose `start_ms <= position_ms`, or `None` before
    /// the first line or when there are no lines. Uses binary search.
    pub fn line_index_at(&self, position_ms: u64) -> Option<usize> {
        let _ = position_ms;
        todo!()
    }

    /// Where `position_ms` falls: [`LinePosition::Intro`] before the first
    /// non-empty line, [`LinePosition::Break`] on an empty line, otherwise the line.
    pub fn at(&self, position_ms: u64) -> LinePosition<'_> {
        let _ = position_ms;
        todo!()
    }

    /// The text of the first non-empty line after the line at `position_ms`,
    /// for a "next line" preview.
    pub fn next_text(&self, position_ms: u64) -> Option<&str> {
        let _ = position_ms;
        todo!()
    }

    /// Start time of the first line strictly after `position_ms`, used to
    /// schedule the next update.
    pub fn next_change_ms(&self, position_ms: u64) -> Option<u64> {
        let _ = position_ms;
        todo!()
    }

    /// Gives unsynced lyrics estimated timings by spreading the lines evenly
    /// over the song. The first line starts at 5% of the duration and the last
    /// starts no later than 90%; lines keep their order. Does nothing for synced
    /// lyrics, empty lyrics or a zero duration. `synced` stays false.
    pub fn spread_evenly(&mut self, duration_ms: u64) {
        let _ = duration_ms;
        todo!()
    }

    /// Returns a copy with every line shifted by `offset_ms` (positive = later),
    /// clamped at 0, still sorted.
    pub fn shifted(&self, offset_ms: i64) -> Lyrics {
        let _ = offset_ms;
        todo!()
    }

    /// True when there is at least one line with non-empty text.
    pub fn has_text(&self) -> bool {
        todo!()
    }
}

#[allow(dead_code)]
fn _uses(_: LyricLine) {}
