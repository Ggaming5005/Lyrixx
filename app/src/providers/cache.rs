//! On-disk cache of lookups, one small JSON file per song.

use crate::matcher::song_key;
use crate::types::{Lyrics, Track};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a "not found" result is trusted before asking the providers again,
/// so new releases get picked up once they appear.
pub const NOT_FOUND_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Two durations within this many ms are the same recording.
pub const DURATION_TOLERANCE_MS: u64 = 3_000;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Cache files larger than this are not lyrics this program wrote: a miss.
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;

/// Makes temp file names unique within this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One cached lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    /// `None` records that no provider had the song.
    pub lyrics: Option<Lyrics>,
    /// Unix seconds when the lookup happened.
    pub fetched_at: u64,
    /// Duration of the track that was looked up, if known.
    pub duration_ms: Option<u64>,
    /// The online providers that were asked (`lrclib`, `netease` …), so a
    /// lookup that may have missed better lyrics runs again once another
    /// provider is turned on. Empty in entries written before this was kept.
    #[serde(default)]
    pub providers: Vec<String>,
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
///
/// Details: the hash is written as 16 lowercase hex digits. Expiry is decided
/// from the system clock; a "not found" entry stamped more than
/// [`NOT_FOUND_TTL`] in the future (the clock was moved back) counts as
/// expired too. "Found" entries never expire. Temp files are named
/// `.<hash>.<pid>.<counter>.<nanos>.tmp` in the cache folder and are removed
/// when the write or rename fails. `get` and `remove` use `tokio::fs`; `put`
/// does its whole write (folder, temp file, rename) with `std::fs` on one
/// `spawn_blocking` task, so a `put` future dropped halfway (a newer song,
/// shutdown) still finishes or cleans up and never leaves a temp file.
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
        let path = self.entry_path(track);
        let entry = read_entry(&path).await?;
        if !durations_match(entry.duration_ms, track.duration_ms) {
            tracing::debug!(
                path = %path.display(),
                "cache entry is for a recording of a different length"
            );
            return None;
        }
        if entry.lyrics.is_none() && is_expired(entry.fetched_at, unix_now_secs()) {
            tracing::debug!(path = %path.display(), "cached \"not found\" has expired");
            return None;
        }
        Some(entry)
    }

    pub async fn put(&self, track: &Track, entry: &CacheEntry) -> anyhow::Result<()> {
        let json = serde_json::to_vec(entry).context("could not encode the cache entry")?;
        let dir = self.dir.clone();
        let path = self.entry_path(track);
        let temp = self.dir.join(temp_file_name(&file_stem(track)));
        // The whole write runs as one blocking task: once started it always
        // runs to the end, so dropping this future (a newer song, shutdown)
        // never leaves a temp file behind.
        match tokio::task::spawn_blocking(move || write_atomically(&dir, &temp, &path, &json)).await
        {
            Ok(result) => result,
            Err(err) => Err(anyhow::Error::new(err).context("the cache write did not finish")),
        }
    }

    /// Removes the entry for a track, if any.
    pub async fn remove(&self, track: &Track) -> anyhow::Result<()> {
        let path = self.entry_path(track);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => {
                Err(anyhow::Error::new(err).context(format!("could not remove {}", path.display())))
            }
        }
    }

    /// Where the entry for `track` lives.
    fn entry_path(&self, track: &Track) -> PathBuf {
        self.dir.join(format!("{}.json", file_stem(track)))
    }
}

/// FNV-1a 64-bit hash, used for cache file names.
pub fn fnv1a64(data: &[u8]) -> u64 {
    data.iter().fold(FNV_OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

/// The cache file name without `.json`: the song key's hash in hex.
fn file_stem(track: &Track) -> String {
    format!("{:016x}", fnv1a64(song_key(track).as_bytes()))
}

/// A temp file name no other writer (in this or another process) uses.
fn temp_file_name(stem: &str) -> String {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(".{stem}.{}.{counter}.{nanos}.tmp", std::process::id())
}

/// Creates `dir`, writes `data` to the new file `temp` and renames it to
/// `path`. The temp file is removed when the write or the rename fails.
fn write_atomically(dir: &Path, temp: &Path, path: &Path, data: &[u8]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    // `create_new`: a file that is already there belongs to someone else and
    // is left alone.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .with_context(|| format!("could not create {}", temp.display()))?;
    let written = file.write_all(data);
    // Closed before the rename, which Windows needs.
    drop(file);
    if let Err(err) = written {
        remove_quietly(temp);
        return Err(anyhow::Error::new(err).context(format!("could not write {}", temp.display())));
    }
    if let Err(err) = std::fs::rename(temp, path) {
        remove_quietly(temp);
        return Err(anyhow::Error::new(err).context(format!(
            "could not move {} to {}",
            temp.display(),
            path.display()
        )));
    }
    Ok(())
}

/// Best-effort cleanup of a temp file.
fn remove_quietly(path: &Path) {
    if let Err(err) = std::fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(path = %path.display(), "could not remove temp file: {err}");
        }
    }
}

/// Reads and decodes one entry; any problem is a miss.
async fn read_entry(path: &Path) -> Option<CacheEntry> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), "cache entry unreadable: {err}");
            }
            return None;
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_ENTRY_BYTES {
        tracing::debug!(path = %path.display(), "cache entry is not a small file");
        return None;
    }
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::debug!(path = %path.display(), "cache entry unreadable: {err}");
            return None;
        }
    };
    match serde_json::from_slice::<CacheEntry>(&bytes) {
        Ok(entry) => Some(entry),
        Err(err) => {
            tracing::debug!(path = %path.display(), "cache entry is corrupt: {err}");
            None
        }
    }
}

/// True unless both durations are known and further apart than the tolerance.
fn durations_match(cached: Option<u64>, wanted: Option<u64>) -> bool {
    match (cached, wanted) {
        (Some(a), Some(b)) => a.abs_diff(b) <= DURATION_TOLERANCE_MS,
        _ => true,
    }
}

/// Whether a "not found" entry written at `fetched_at` is too old to trust at
/// `now` (both Unix seconds). A stamp far in the future counts as expired.
fn is_expired(fetched_at: u64, now: u64) -> bool {
    fetched_at.abs_diff(now) > NOT_FOUND_TTL.as_secs()
}

/// The current time in Unix seconds (0 when the clock is before 1970).
fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LyricLine;

    fn track(artist: &str, title: &str, duration_ms: Option<u64>) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: None,
            duration_ms,
            spotify_id: None,
        }
    }

    fn lyrics() -> Lyrics {
        Lyrics {
            lines: vec![
                LyricLine {
                    start_ms: 17_120,
                    text: "Never gonna give you up".into(),
                },
                LyricLine {
                    start_ms: 19_500,
                    text: "Never gonna let you down".into(),
                },
            ],
            synced: true,
            instrumental: false,
            source: "lrclib".into(),
        }
    }

    fn found(fetched_at: u64, duration_ms: Option<u64>) -> CacheEntry {
        CacheEntry {
            lyrics: Some(lyrics()),
            fetched_at,
            duration_ms,
            providers: vec!["lrclib".into()],
        }
    }

    fn not_found(fetched_at: u64, duration_ms: Option<u64>) -> CacheEntry {
        CacheEntry {
            lyrics: None,
            fetched_at,
            duration_ms,
            providers: vec!["lrclib".into(), "netease".into()],
        }
    }

    fn rick() -> Track {
        track("Rick Astley", "Never Gonna Give You Up", Some(213_000))
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    // ----- fnv1a64 ---------------------------------------------------------

    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn fnv1a64_differs_for_different_input() {
        assert_ne!(
            fnv1a64(b"rick astley - never gonna give you up"),
            fnv1a64(b"rick astley - together forever")
        );
        assert_eq!(fnv1a64(b"same"), fnv1a64(b"same"));
    }

    #[test]
    fn fnv1a64_handles_long_and_binary_input() {
        let data: Vec<u8> = (0..=255u8).cycle().take(100_000).collect();
        assert_eq!(fnv1a64(&data), fnv1a64(&data));
        assert_ne!(fnv1a64(&data), FNV_OFFSET_BASIS);
    }

    // ----- file naming -----------------------------------------------------

    #[tokio::test]
    async fn file_name_is_hex_hash_of_song_key() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let t = rick();
        cache.put(&t, &found(1, None)).await.unwrap();
        let expected = format!("{:016x}.json", fnv1a64(song_key(&t).as_bytes()));
        assert_eq!(files_in(dir.path()), vec![expected.clone()]);
        assert_eq!(expected.len(), 16 + 5);
        assert!(expected
            .trim_end_matches(".json")
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn file_stem_is_zero_padded() {
        // Whatever the hash, the stem is always 16 hex digits.
        for title in ["a", "b", "Song", "Ünïcödé 歌", ""] {
            let stem = file_stem(&track("x", title, None));
            assert_eq!(stem.len(), 16, "{stem}");
        }
    }

    #[test]
    fn dir_returns_the_folder() {
        let cache = LyricsCache::new(PathBuf::from("some/where"));
        assert_eq!(cache.dir(), &PathBuf::from("some/where"));
    }

    // ----- put / get -------------------------------------------------------

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let entry = found(unix_now_secs(), Some(213_000));
        cache.put(&rick(), &entry).await.unwrap();
        assert_eq!(cache.get(&rick()).await, Some(entry));
    }

    #[tokio::test]
    async fn get_missing_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn get_with_missing_folder_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().join("does").join("not").join("exist"));
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn put_creates_nested_folders() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("lyrics");
        let cache = LyricsCache::new(nested.clone());
        cache.put(&rick(), &found(5, None)).await.unwrap();
        assert!(nested.is_dir());
        assert_eq!(files_in(&nested).len(), 1);
        assert!(cache.get(&rick()).await.is_some());
    }

    #[tokio::test]
    async fn put_overwrites_previous_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let now = unix_now_secs();
        cache.put(&rick(), &not_found(now, None)).await.unwrap();
        let entry = found(now, None);
        cache.put(&rick(), &entry).await.unwrap();
        assert_eq!(cache.get(&rick()).await, Some(entry));
        assert_eq!(files_in(dir.path()).len(), 1);
    }

    #[tokio::test]
    async fn put_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        for i in 0..5u64 {
            let t = track("Artist", &format!("Song {i}"), None);
            cache.put(&t, &found(i, None)).await.unwrap();
        }
        let names = files_in(dir.path());
        assert_eq!(names.len(), 5);
        assert!(names
            .iter()
            .all(|n| n.ends_with(".json") && !n.starts_with('.')));
    }

    #[tokio::test]
    async fn concurrent_puts_for_the_same_song_leave_one_whole_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let mut tasks = Vec::new();
        for i in 0..16u64 {
            let cache = cache.clone();
            tasks.push(tokio::spawn(async move {
                cache.put(&rick(), &found(i, Some(213_000))).await
            }));
        }
        let mut succeeded = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(()) => succeeded += 1,
                // Windows may refuse to replace a file another rename is
                // replacing at that moment; the cache then simply keeps the
                // other write.
                Err(err) if cfg!(windows) => drop(err),
                Err(err) => panic!("concurrent put failed: {err:#}"),
            }
        }
        assert!(succeeded > 0);
        assert_eq!(files_in(dir.path()).len(), 1);
        let entry = cache.get(&rick()).await.unwrap();
        assert!(entry.fetched_at < 16);
        assert_eq!(entry.lyrics, Some(lyrics()));
    }

    /// Polls `fut` at most `polls` times, then drops it. True when it finished.
    async fn poll_then_drop<F: std::future::Future>(fut: F, polls: usize) -> bool {
        let mut fut = std::pin::pin!(fut);
        let mut count = 0;
        std::future::poll_fn(|cx| {
            if fut.as_mut().poll(cx).is_ready() {
                return std::task::Poll::Ready(true);
            }
            count += 1;
            if count >= polls {
                std::task::Poll::Ready(false)
            } else {
                std::task::Poll::Pending
            }
        })
        .await
    }

    /// Waits (in real time) until `dir` holds `entry` and no temp file;
    /// false on timeout.
    async fn settles_to(dir: &Path, entry: &str) -> bool {
        for _ in 0..500 {
            let names = files_in(dir);
            if names == [entry] {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    #[tokio::test]
    async fn cancelled_put_still_finishes_and_leaves_no_temp_file() {
        // The lookup task that writes the cache may be dropped at any await
        // point (a newer song, shutdown). The write then still completes in
        // the background: the folder ends up with the whole entry and never
        // keeps a temp file.
        for polls in 1..=12 {
            let dir = tempfile::tempdir().unwrap();
            let cache = LyricsCache::new(dir.path().join("cache"));
            let entry = found(polls as u64, Some(213_000));
            poll_then_drop(cache.put(&rick(), &entry), polls).await;
            let name = format!("{}.json", file_stem(&rick()));
            assert!(
                settles_to(cache.dir(), &name).await,
                "after dropping put at poll {polls}: {:?}",
                files_in(cache.dir())
            );
            assert_eq!(cache.get(&rick()).await, Some(entry));
        }
    }

    #[tokio::test]
    async fn put_failure_is_an_error_and_cleans_up_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        // A non-empty folder where the entry should go makes the rename fail.
        let target = cache.entry_path(&rick());
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        let err = cache.put(&rick(), &found(1, None)).await.unwrap_err();
        assert!(format!("{err:#}").contains("could not move"), "{err:#}");
        assert_eq!(
            files_in(dir.path()),
            vec![format!("{}.json", file_stem(&rick()))]
        );
    }

    #[tokio::test]
    async fn put_into_a_file_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-folder");
        std::fs::write(&file, b"x").unwrap();
        let cache = LyricsCache::new(file.clone());
        assert!(cache.put(&rick(), &found(1, None)).await.is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
    }

    #[tokio::test]
    async fn entries_are_shared_by_tracks_with_the_same_song_key() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let entry = found(1, Some(213_000));
        cache.put(&rick(), &entry).await.unwrap();
        let variant = track(
            "Rick Astley, Someone Else",
            "Never Gonna Give You Up (Remastered 2022)",
            Some(213_500),
        );
        assert_eq!(cache.get(&variant).await, Some(entry));
        let other = track("Rick Astley", "Together Forever", Some(213_000));
        assert_eq!(cache.get(&other).await, None);
    }

    #[tokio::test]
    async fn stored_json_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.put(&rick(), &found(42, Some(213_000))).await.unwrap();
        let text = std::fs::read_to_string(cache.entry_path(&rick())).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["fetched_at"], 42);
        assert_eq!(value["duration_ms"], 213_000);
        assert_eq!(value["lyrics"]["source"], "lrclib");
        assert_eq!(
            value["lyrics"]["lines"][0]["text"],
            "Never gonna give you up"
        );
        assert_eq!(value["providers"], serde_json::json!(["lrclib"]));
    }

    #[tokio::test]
    async fn entries_from_before_providers_were_kept_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let json = format!(
            r#"{{"lyrics":null,"fetched_at":{},"duration_ms":213000}}"#,
            unix_now_secs()
        );
        std::fs::write(cache.entry_path(&rick()), json).unwrap();
        let entry = cache.get(&rick()).await.unwrap();
        assert_eq!(entry.lyrics, None);
        assert!(entry.providers.is_empty());
    }

    // ----- misses ----------------------------------------------------------

    #[tokio::test]
    async fn corrupt_json_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        for junk in [
            &b""[..],
            b"{",
            b"not json at all",
            b"{\"lyrics\":null}",
            b"{\"lyrics\":null,\"fetched_at\":-5,\"duration_ms\":null}",
            b"[1,2,3]",
            b"\xff\xfe\x00garbage",
        ] {
            std::fs::write(cache.entry_path(&rick()), junk).unwrap();
            assert_eq!(cache.get(&rick()).await, None, "{junk:?}");
        }
    }

    #[tokio::test]
    async fn oversized_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.put(&rick(), &found(1, None)).await.unwrap();
        let path = cache.entry_path(&rick());
        // Valid JSON followed by padding: only the size makes it a miss.
        let mut json = std::fs::read(&path).unwrap();
        json.resize(MAX_ENTRY_BYTES as usize, b' ');
        std::fs::write(&path, &json).unwrap();
        assert!(cache.get(&rick()).await.is_some());
        json.push(b' ');
        std::fs::write(&path, &json).unwrap();
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn unreadable_entry_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        // A folder with the entry's name cannot be read as a file.
        std::fs::create_dir(cache.entry_path(&rick())).unwrap();
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn duration_mismatch_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.put(&rick(), &found(1, Some(213_000))).await.unwrap();
        let mut t = rick();
        // Exactly at the tolerance still matches, one ms more does not.
        t.duration_ms = Some(213_000 + DURATION_TOLERANCE_MS);
        assert!(cache.get(&t).await.is_some());
        t.duration_ms = Some(213_000 - DURATION_TOLERANCE_MS);
        assert!(cache.get(&t).await.is_some());
        t.duration_ms = Some(213_000 + DURATION_TOLERANCE_MS + 1);
        assert_eq!(cache.get(&t).await, None);
        t.duration_ms = Some(213_000 - DURATION_TOLERANCE_MS - 1);
        assert_eq!(cache.get(&t).await, None);
        // A live version twice as long is a different recording.
        t.duration_ms = Some(426_000);
        assert_eq!(cache.get(&t).await, None);
    }

    #[tokio::test]
    async fn unknown_duration_on_either_side_matches() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.put(&rick(), &found(1, None)).await.unwrap();
        assert!(cache.get(&rick()).await.is_some());

        cache.put(&rick(), &found(1, Some(213_000))).await.unwrap();
        let mut t = rick();
        t.duration_ms = None;
        assert!(cache.get(&t).await.is_some());
    }

    #[tokio::test]
    async fn fresh_not_found_is_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let entry = not_found(unix_now_secs().saturating_sub(60), Some(213_000));
        cache.put(&rick(), &entry).await.unwrap();
        assert_eq!(cache.get(&rick()).await, Some(entry));
    }

    #[tokio::test]
    async fn not_found_just_inside_ttl_is_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let fetched_at = unix_now_secs().saturating_sub(NOT_FOUND_TTL.as_secs() - 120);
        cache
            .put(&rick(), &not_found(fetched_at, None))
            .await
            .unwrap();
        assert!(cache.get(&rick()).await.is_some());
    }

    #[tokio::test]
    async fn expired_not_found_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let fetched_at = unix_now_secs().saturating_sub(NOT_FOUND_TTL.as_secs() + 60);
        cache
            .put(&rick(), &not_found(fetched_at, None))
            .await
            .unwrap();
        assert_eq!(cache.get(&rick()).await, None);
        // A very old one too.
        cache.put(&rick(), &not_found(0, None)).await.unwrap();
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn not_found_from_the_far_future_is_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache
            .put(&rick(), &not_found(u64::MAX, None))
            .await
            .unwrap();
        assert_eq!(cache.get(&rick()).await, None);
    }

    #[tokio::test]
    async fn found_entries_never_expire() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        for fetched_at in [0, 1, u64::MAX] {
            let entry = found(fetched_at, None);
            cache.put(&rick(), &entry).await.unwrap();
            assert_eq!(cache.get(&rick()).await, Some(entry));
        }
    }

    #[tokio::test]
    async fn instrumental_entries_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let entry = CacheEntry {
            lyrics: Some(Lyrics {
                lines: vec![],
                synced: false,
                instrumental: true,
                source: "lrclib".into(),
            }),
            fetched_at: 0,
            duration_ms: Some(300_000),
            providers: Vec::new(),
        };
        let t = track("Explosions in the Sky", "Your Hand in Mine", Some(300_000));
        cache.put(&t, &entry).await.unwrap();
        assert_eq!(cache.get(&t).await, Some(entry));
    }

    #[tokio::test]
    async fn unicode_names_get_their_own_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let a = track("宇多田ヒカル", "First Love", None);
        let b = track("Кино", "Группа крови", None);
        cache.put(&a, &found(1, None)).await.unwrap();
        assert!(cache.get(&a).await.is_some());
        assert_eq!(cache.get(&b).await, None);
        cache
            .put(&b, &not_found(unix_now_secs(), None))
            .await
            .unwrap();
        assert_eq!(cache.get(&b).await.map(|e| e.lyrics), Some(None));
        assert_eq!(files_in(dir.path()).len(), 2);
    }

    // ----- remove ----------------------------------------------------------

    #[tokio::test]
    async fn remove_deletes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.put(&rick(), &found(1, None)).await.unwrap();
        let other = track("Rick Astley", "Together Forever", None);
        cache.put(&other, &found(1, None)).await.unwrap();
        cache.remove(&rick()).await.unwrap();
        assert_eq!(cache.get(&rick()).await, None);
        assert!(cache.get(&other).await.is_some());
    }

    #[tokio::test]
    async fn remove_missing_entry_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        cache.remove(&rick()).await.unwrap();
        let gone = LyricsCache::new(dir.path().join("missing"));
        gone.remove(&rick()).await.unwrap();
    }

    #[tokio::test]
    async fn remove_failure_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LyricsCache::new(dir.path().to_path_buf());
        let target = cache.entry_path(&rick());
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"x").unwrap();
        assert!(cache.remove(&rick()).await.is_err());
    }

    // ----- helpers ---------------------------------------------------------

    #[test]
    fn expiry_boundaries() {
        let ttl = NOT_FOUND_TTL.as_secs();
        assert!(!is_expired(1_000_000, 1_000_000));
        assert!(!is_expired(1_000_000, 1_000_000 + ttl));
        assert!(is_expired(1_000_000, 1_000_000 + ttl + 1));
        assert!(!is_expired(1_000_000 + ttl, 1_000_000));
        assert!(is_expired(1_000_000 + ttl + 1, 1_000_000));
        assert!(is_expired(0, u64::MAX));
        assert!(is_expired(u64::MAX, 0));
    }

    #[test]
    fn duration_matching() {
        assert!(durations_match(None, None));
        assert!(durations_match(Some(1), None));
        assert!(durations_match(None, Some(1)));
        assert!(durations_match(Some(0), Some(DURATION_TOLERANCE_MS)));
        assert!(!durations_match(Some(0), Some(DURATION_TOLERANCE_MS + 1)));
        assert!(!durations_match(Some(u64::MAX), Some(0)));
    }

    #[test]
    fn write_atomically_never_touches_a_file_it_did_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(".someone-elses.tmp");
        let path = dir.path().join("entry.json");
        std::fs::write(&temp, b"theirs").unwrap();
        assert!(write_atomically(dir.path(), &temp, &path, b"{}").is_err());
        assert_eq!(std::fs::read(&temp).unwrap(), b"theirs");
        assert!(!path.exists());
    }

    #[test]
    fn write_atomically_replaces_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("new");
        let path = sub.join("entry.json");
        write_atomically(&sub, &sub.join("a.tmp"), &path, b"one").unwrap();
        write_atomically(&sub, &sub.join("b.tmp"), &path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(files_in(&sub), vec!["entry.json".to_string()]);
    }

    #[test]
    fn temp_names_are_unique_and_hidden() {
        let a = temp_file_name("abc");
        let b = temp_file_name("abc");
        assert_ne!(a, b);
        assert!(a.starts_with(".abc."));
        assert!(a.ends_with(".tmp"));
        assert!(a.contains(&std::process::id().to_string()));
    }
}
