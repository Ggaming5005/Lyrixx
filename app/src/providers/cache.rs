//! On-disk cache of lookups, one small JSON file per song.

use crate::types::{Lyrics, Track};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// How long a "not found" result is trusted before asking the providers again,
/// so new releases get picked up once they appear.
pub const NOT_FOUND_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Two durations within this many ms are the same recording.
pub const DURATION_TOLERANCE_MS: u64 = 3_000;

/// One cached lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// `None` records that no provider had the song.
    pub lyrics: Option<Lyrics>,
    /// Unix seconds when the lookup happened.
    pub fetched_at: u64,
    /// Duration of the track that was looked up, if known.
    pub duration_ms: Option<u64>,
}

/// See the module docs.
///
/// - The file name is a hex FNV-1a 64-bit hash of [`crate::matcher::song_key`]
///   plus `.json` (no extra hashing dependency).
/// - [`get`](Self::get) ignores entries whose recorded duration differs from the
///   track's by more than [`DURATION_TOLERANCE_MS`] (when both are known),
///   "not found" entries older than [`NOT_FOUND_TTL`], and unreadable or corrupt
///   files (treated as a miss, never an error).
/// - [`put`](Self::put) creates the folder and writes atomically (temp file + rename).
#[derive(Debug, Clone)]
pub struct LyricsCache {
    dir: PathBuf,
}

impl LyricsCache {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    pub async fn get(&self, track: &Track) -> Option<CacheEntry> {
        let _ = track;
        todo!()
    }

    pub async fn put(&self, track: &Track, entry: &CacheEntry) -> anyhow::Result<()> {
        let _ = (track, entry);
        todo!()
    }

    /// Removes the entry for a track, if any.
    pub async fn remove(&self, track: &Track) -> anyhow::Result<()> {
        let _ = track;
        todo!()
    }
}

/// FNV-1a 64-bit hash, used for cache file names.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let _ = data;
    todo!()
}

#[allow(dead_code)]
fn _uses(_: Duration) {}
