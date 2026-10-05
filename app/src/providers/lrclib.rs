//! [LRCLIB](https://lrclib.net): a free, open lyrics database with timed lines.
//!
//! API used (no account or key):
//! - `GET {base}/api/get?artist_name=&track_name=&album_name=&duration=` → one
//!   record or 404. `duration` is in whole seconds; LRCLIB matches within ±2 s.
//! - `GET {base}/api/search?track_name=&artist_name=` → array of records.
//!
//! Record fields: `id`, `trackName`, `artistName`, `albumName`, `duration`
//! (seconds, may be fractional), `instrumental` (bool), `plainLyrics` (string or
//! null), `syncedLyrics` (LRC string or null).

use super::LyricsProvider;
use crate::types::{Lyrics, Track};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// Request timeout for each LRCLIB call.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum [`crate::matcher::score_candidate`] for a search result to be used.
pub const MIN_SEARCH_SCORE: f64 = 0.6;

/// One LRCLIB record.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrclibRecord {
    pub id: Option<i64>,
    pub track_name: Option<String>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub duration: Option<f64>,
    #[serde(default)]
    pub instrumental: bool,
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
}

/// See the module docs.
///
/// Lookup order:
/// 1. When album and duration are both known: `/api/get` with all four fields.
/// 2. When either is missing, or step 1 returned 404: `/api/search` with
///    `track_name` + `artist_name` (the primary artist), and if that returns
///    nothing usable, `/api/search` with `track_name` only.
/// 3. Search results are scored with [`crate::matcher::score_candidate`]; results
///    below [`MIN_SEARCH_SCORE`] are dropped; among the rest a record with synced
///    lyrics beats one without, then the higher score wins.
///
/// A record becomes [`Lyrics`] via [`record_to_lyrics`]. HTTP 404 is "not found";
/// other non-success statuses, timeouts and bad JSON are errors. Every request
/// sends [`crate::USER_AGENT`].
pub struct LrclibProvider {
    client: reqwest::Client,
    base_url: String,
}

impl LrclibProvider {
    /// `base_url` without a trailing slash, e.g. `https://lrclib.net`.
    pub fn new(base_url: impl Into<String>) -> anyhow::Result<Self> {
        let _ = base_url.into();
        todo!()
    }
}

/// Converts a record: instrumental → `Lyrics { instrumental: true, lines: [] }`;
/// synced lyrics → parsed LRC (`synced = true`); else plain lyrics →
/// [`crate::lrc::from_plain`]; nothing usable → `None`. `source` = `"lrclib"`.
pub fn record_to_lyrics(record: &LrclibRecord) -> Option<Lyrics> {
    let _ = record;
    todo!()
}

#[async_trait]
impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &'static str {
        "lrclib"
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let _ = (track, &self.client, &self.base_url);
        todo!()
    }
}
