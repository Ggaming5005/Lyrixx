//! Your own lyric files: a folder of `.lrc` and `.txt` files.

use super::LyricsProvider;
use crate::lrc::{from_plain, parse_lrc};
use crate::matcher::{clean_artist, clean_title, normalize_key, primary_artist};
use crate::types::{Lyrics, Track};
use anyhow::Context as _;
use async_trait::async_trait;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Lyric files larger than this are skipped (no real lyrics file is this big).
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Looks for a file in `dir` (not recursive) whose name matches the song.
///
/// A file matches when the [`crate::matcher::normalize_key`] of its stem equals
/// the key of `"<artist> - <title>"`, of `"<primary artist> - <title>"`, or of
/// `"<title>"` alone (in that order of preference). `.lrc` files are parsed with
/// [`crate::lrc::parse_lrc`]; `.txt` files with [`crate::lrc::from_plain`]. When
/// both exist, `.lrc` wins. A missing folder means "nothing found", not an error.
///
/// Details: the artist and title are cleaned with
/// [`crate::matcher::clean_artist`] and [`crate::matcher::clean_title`] (the
/// primary artist with [`crate::matcher::primary_artist`]) before building the
/// keys. Extensions match in any letter case. Files are read as UTF-8 (invalid
/// bytes replaced, a BOM removed; UTF-16 files with a BOM are decoded too). A
/// file that cannot be read, is larger than 16 MiB, or holds no lyric text is
/// skipped and the next best match is tried. A path that is not a folder counts
/// as missing, and so does a path below a file (`notes.txt/lyrics`); a folder
/// that exists but cannot be listed is an error.
/// [`Lyrics::source`] is `"local"`.
pub struct LocalLrcProvider {
    dir: PathBuf,
}

impl LocalLrcProvider {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

#[async_trait]
impl LyricsProvider for LocalLrcProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn is_local(&self) -> bool {
        true
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let keys = wanted_keys(track);
        if keys.is_empty() {
            return Ok(None);
        }
        let Some(mut candidates) = self.list_candidates(&keys).await? else {
            return Ok(None);
        };
        candidates.sort_by(Candidate::preference);
        for candidate in &candidates {
            if let Some(lyrics) = read_candidate(candidate).await {
                return Ok(Some(lyrics));
            }
        }
        Ok(None)
    }
}

impl LocalLrcProvider {
    /// Every lyric file in the folder whose stem matches one of `keys`.
    /// `None` when the folder does not exist.
    async fn list_candidates(&self, keys: &[String]) -> anyhow::Result<Option<Vec<Candidate>>> {
        match tokio::fs::metadata(&self.dir).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                tracing::debug!(dir = %self.dir.display(), "lyrics folder is not a folder");
                return Ok(None);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) if below_a_file(&self.dir).await => {
                tracing::debug!(dir = %self.dir.display(), "lyrics folder is inside a file");
                return Ok(None);
            }
            Err(err) => {
                return Err(anyhow::Error::new(err)
                    .context(format!("could not open {}", self.dir.display())))
            }
        }
        let mut entries = tokio::fs::read_dir(&self.dir)
            .await
            .with_context(|| format!("could not list {}", self.dir.display()))?;
        let mut candidates = Vec::new();
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!(dir = %self.dir.display(), "stopped listing lyrics folder: {err}");
                    break;
                }
            };
            let path = entry.path();
            let Some((kind, stem)) = classify(&path) else {
                continue;
            };
            let stem_key = normalize_key(&stem);
            if stem_key.is_empty() {
                continue;
            }
            if let Some(level) = keys.iter().position(|key| *key == stem_key) {
                candidates.push(Candidate { path, kind, level });
            }
        }
        Ok(Some(candidates))
    }
}

/// True when the nearest existing parent of `dir` is not a folder, so `dir`
/// cannot exist (Unix reports "not a directory" there, not "not found").
async fn below_a_file(dir: &Path) -> bool {
    for parent in dir.ancestors().skip(1) {
        if parent.as_os_str().is_empty() {
            break;
        }
        if let Ok(metadata) = tokio::fs::metadata(parent).await {
            return !metadata.is_dir();
        }
    }
    false
}

/// The kind of lyric file, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileKind {
    /// `.lrc`, preferred.
    Lrc,
    /// `.txt`.
    Txt,
}

/// A file whose name matches the song.
#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    kind: FileKind,
    /// Index of the matched key: 0 is the best match.
    level: usize,
}

impl Candidate {
    /// Best first: closer name match, then `.lrc` before `.txt`, then by path
    /// so the choice does not depend on the folder listing order.
    fn preference(a: &Candidate, b: &Candidate) -> Ordering {
        a.level
            .cmp(&b.level)
            .then(a.kind.cmp(&b.kind))
            .then_with(|| a.path.cmp(&b.path))
    }
}

/// The keys a file stem may have to match the track, best first, without
/// empty or repeated keys.
fn wanted_keys(track: &Track) -> Vec<String> {
    let artist = clean_artist(&track.artist);
    let title = clean_title(&track.title);
    let primary = primary_artist(&artist);
    let mut keys: Vec<String> = Vec::with_capacity(3);
    for text in [
        format!("{artist} - {title}"),
        format!("{primary} - {title}"),
        title.clone(),
    ] {
        let key = normalize_key(&text);
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    // Without a title, an artist-only file name is no match for a song.
    if normalize_key(&title).is_empty() {
        keys.clear();
    }
    keys
}

/// The kind and stem of a lyric file name, or `None` for other files.
fn classify(path: &Path) -> Option<(FileKind, String)> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let kind = match extension.as_str() {
        "lrc" => FileKind::Lrc,
        "txt" => FileKind::Txt,
        _ => return None,
    };
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    Some((kind, stem))
}

/// Reads and parses one candidate; `None` when it cannot be used.
async fn read_candidate(candidate: &Candidate) -> Option<Lyrics> {
    let path = &candidate.path;
    // Checked first so a folder or a pipe with a lyric-like name is never read.
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_FILE_BYTES => {}
        Ok(_) => {
            tracing::debug!(path = %path.display(), "skipping: not a small regular file");
            return None;
        }
        Err(err) => {
            tracing::debug!(path = %path.display(), "skipping unreadable lyrics file: {err}");
            return None;
        }
    }
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::debug!(path = %path.display(), "skipping unreadable lyrics file: {err}");
            return None;
        }
    };
    let text = decode_text(&bytes);
    let mut lyrics = match candidate.kind {
        FileKind::Lrc => parse_lrc(&text),
        FileKind::Txt => from_plain(&text),
    };
    if !lyrics.has_text() {
        tracing::debug!(path = %path.display(), "skipping lyrics file without text");
        return None;
    }
    lyrics.source = "local".into();
    tracing::debug!(path = %path.display(), synced = lyrics.synced, "using local lyrics");
    Some(lyrics)
}

/// File bytes as text: UTF-16 when the file starts with a UTF-16 BOM,
/// otherwise UTF-8 with invalid bytes replaced. Any BOM is removed.
fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(rest, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// Decodes UTF-16 code units, replacing invalid ones (and an odd last byte).
fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks(2).map(|pair| match pair {
        [a, b] => unit([*a, *b]),
        _ => 0xfffd,
    });
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LyricLine;

    fn track(artist: &str, title: &str) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: None,
            duration_ms: Some(200_000),
            spotify_id: None,
        }
    }

    const LRC: &str = "[ar:Rick Astley]\n[ti:Never Gonna Give You Up]\n[00:18.50]We're no strangers to love\n[00:22.80]You know the rules and so do I\n";
    const TXT: &str = "We're no strangers to love\nYou know the rules and so do I\n";

    fn write(dir: &Path, name: &str, content: impl AsRef<[u8]>) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    async fn fetch(dir: &Path, track: &Track) -> Option<Lyrics> {
        LocalLrcProvider::new(dir.to_path_buf())
            .fetch(track)
            .await
            .unwrap()
    }

    fn rick() -> Track {
        track("Rick Astley", "Never Gonna Give You Up")
    }

    #[test]
    fn name_is_local() {
        assert_eq!(LocalLrcProvider::new(PathBuf::new()).name(), "local");
    }

    #[tokio::test]
    async fn finds_artist_title_lrc() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.lrc", LRC);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(lyrics.synced);
        assert!(!lyrics.instrumental);
        assert_eq!(lyrics.source, "local");
        assert_eq!(
            lyrics.lines,
            vec![
                LyricLine {
                    start_ms: 18_500,
                    text: "We're no strangers to love".into()
                },
                LyricLine {
                    start_ms: 22_800,
                    text: "You know the rules and so do I".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn finds_plain_txt() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.txt", TXT);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.source, "local");
        assert_eq!(lyrics.lines.len(), 2);
        assert!(lyrics.lines.iter().all(|l| l.start_ms == 0));
        assert_eq!(lyrics.lines[0].text, "We're no strangers to love");
    }

    #[tokio::test]
    async fn lrc_wins_over_txt() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.txt", TXT);
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.lrc", LRC);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(lyrics.synced);
    }

    #[tokio::test]
    async fn txt_is_parsed_as_plain_even_with_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.txt",
            "[00:01.00]line one\n",
        );
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines[0].text, "[00:01.00]line one");
    }

    #[tokio::test]
    async fn lrc_without_timestamps_is_plain() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.lrc",
            "[ar:Rick]\nline one\nline two\n",
        );
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.source, "local");
    }

    #[tokio::test]
    async fn extension_matches_in_any_case() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.LRC", LRC);
        assert!(fetch(dir.path(), &rick()).await.unwrap().synced);

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.Txt", TXT);
        assert!(!fetch(dir.path(), &rick()).await.unwrap().synced);
    }

    #[tokio::test]
    async fn names_compare_by_key() {
        let dir = tempfile::tempdir().unwrap();
        // Case, punctuation and repeated spaces do not matter.
        write(
            dir.path(),
            "RICK  ASTLEY -- never gonna give you up!.lrc",
            LRC,
        );
        assert!(fetch(dir.path(), &rick()).await.is_some());
        // Punctuation is dropped, not read as a space.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "rick_astley-never.gonna.give.you.up.lrc", LRC);
        assert_eq!(fetch(dir.path(), &rick()).await, None);
    }

    #[tokio::test]
    async fn accents_and_ampersands_compare_by_key() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Beyonce and Jay Z - Deja Vu.lrc", LRC);
        let t = track("Beyoncé & Jay Z", "Déjà Vu");
        assert!(fetch(dir.path(), &t).await.is_some());
    }

    #[tokio::test]
    async fn primary_artist_name_matches() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Daft Punk - Get Lucky.lrc", LRC);
        let t = track("Daft Punk, Pharrell Williams, Nile Rodgers", "Get Lucky");
        assert!(fetch(dir.path(), &t).await.is_some());
    }

    #[tokio::test]
    async fn title_only_name_matches() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Never Gonna Give You Up.lrc", LRC);
        assert!(fetch(dir.path(), &rick()).await.is_some());
    }

    #[tokio::test]
    async fn full_artist_beats_primary_beats_title_only() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Get Lucky.lrc", "[00:01.00]title only\n");
        write(
            dir.path(),
            "Daft Punk - Get Lucky.lrc",
            "[00:01.00]primary\n",
        );
        write(
            dir.path(),
            "Daft Punk, Pharrell Williams - Get Lucky.lrc",
            "[00:01.00]full\n",
        );
        let t = track("Daft Punk, Pharrell Williams", "Get Lucky");
        assert_eq!(fetch(dir.path(), &t).await.unwrap().lines[0].text, "full");

        std::fs::remove_file(
            dir.path()
                .join("Daft Punk, Pharrell Williams - Get Lucky.lrc"),
        )
        .unwrap();
        assert_eq!(
            fetch(dir.path(), &t).await.unwrap().lines[0].text,
            "primary"
        );

        std::fs::remove_file(dir.path().join("Daft Punk - Get Lucky.lrc")).unwrap();
        assert_eq!(
            fetch(dir.path(), &t).await.unwrap().lines[0].text,
            "title only"
        );
    }

    #[tokio::test]
    async fn better_name_match_beats_lrc_extension() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.lrc",
            "[00:01.00]title only\n",
        );
        write(
            dir.path(),
            "Rick Astley - Never Gonna Give You Up.txt",
            "artist and title\n",
        );
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(lyrics.lines[0].text, "artist and title");
        assert!(!lyrics.synced);
    }

    #[tokio::test]
    async fn noisy_track_titles_are_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Queen - Bohemian Rhapsody.lrc", LRC);
        let t = track("Queen", "Bohemian Rhapsody (Remastered 2011)");
        assert!(fetch(dir.path(), &t).await.is_some());
        let t = track("QueenVEVO", "Bohemian Rhapsody");
        assert!(fetch(dir.path(), &t).await.is_some());
    }

    #[tokio::test]
    async fn kept_version_markers_must_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Artist - Song (Live).lrc", "[00:01.00]live\n");
        write(dir.path(), "Artist - Song.lrc", "[00:01.00]studio\n");
        let live = track("Artist", "Song (Live)");
        assert_eq!(
            fetch(dir.path(), &live).await.unwrap().lines[0].text,
            "live"
        );
        let studio = track("Artist", "Song");
        assert_eq!(
            fetch(dir.path(), &studio).await.unwrap().lines[0].text,
            "studio"
        );
    }

    #[tokio::test]
    async fn other_songs_and_files_do_not_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Together Forever.lrc", LRC);
        write(
            dir.path(),
            "Rick Astley - Never Gonna Give You Up.mp3",
            b"ID3",
        );
        write(dir.path(), "Rick Astley - Never Gonna Give You Up", LRC);
        write(
            dir.path(),
            "Rick Astley - Never Gonna Give You Up.lrc.bak",
            LRC,
        );
        write(dir.path(), "Rick Astley.lrc", LRC);
        write(dir.path(), "Never Gonna Give You Up Again.lrc", LRC);
        assert_eq!(fetch(dir.path(), &rick()).await, None);
    }

    #[tokio::test]
    async fn not_recursive() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Rick Astley");
        std::fs::create_dir(&sub).unwrap();
        write(&sub, "Never Gonna Give You Up.lrc", LRC);
        assert_eq!(fetch(dir.path(), &rick()).await, None);
    }

    #[tokio::test]
    async fn folder_with_lyric_name_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Rick Astley - Never Gonna Give You Up.lrc")).unwrap();
        assert_eq!(fetch(dir.path(), &rick()).await, None);
        write(dir.path(), "Never Gonna Give You Up.txt", TXT);
        assert!(fetch(dir.path(), &rick()).await.is_some());
    }

    #[tokio::test]
    async fn missing_folder_is_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no").join("such").join("folder");
        let provider = LocalLrcProvider::new(missing);
        assert_eq!(provider.fetch(&rick()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn file_instead_of_folder_is_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lyrics");
        std::fs::write(&file, LRC).unwrap();
        let provider = LocalLrcProvider::new(file);
        assert_eq!(provider.fetch(&rick()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn folder_below_a_file_is_nothing_found() {
        // `<file>/lyrics` cannot exist; Linux reports "not a directory" rather
        // than "not found", Windows "path not found": both mean missing.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lyrics.txt");
        std::fs::write(&file, LRC).unwrap();
        for missing in [file.join("lyrics"), file.join("a").join("b")] {
            let provider = LocalLrcProvider::new(missing.clone());
            assert_eq!(provider.fetch(&rick()).await.unwrap(), None, "{missing:?}");
        }
    }

    #[tokio::test]
    async fn empty_folder_is_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(fetch(dir.path(), &rick()).await, None);
    }

    #[tokio::test]
    async fn bom_and_crlf_are_handled() {
        let dir = tempfile::tempdir().unwrap();
        let mut content = vec![0xef, 0xbb, 0xbf];
        content.extend_from_slice(b"[00:01.00]First\r\n[00:02.00]Second\r\n");
        write(dir.path(), "Never Gonna Give You Up.lrc", &content);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines[0].text, "First");
        assert_eq!(lyrics.lines[1].text, "Second");

        let dir = tempfile::tempdir().unwrap();
        let mut content = vec![0xef, 0xbb, 0xbf];
        content.extend_from_slice(b"First\r\nSecond\r\n");
        write(dir.path(), "Never Gonna Give You Up.txt", &content);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(lyrics.lines[0].text, "First");
        assert_eq!(lyrics.lines.len(), 2);
    }

    #[tokio::test]
    async fn invalid_utf8_is_read_lossily() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.lrc",
            b"[00:01.00]caf\xe9 au lait\n[00:02.00]fine\n",
        );
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(lyrics.lines[0].text, "caf\u{fffd} au lait");
        assert_eq!(lyrics.lines[1].text, "fine");
    }

    #[tokio::test]
    async fn utf16_files_with_bom_are_decoded() {
        let text = "[00:01.00]Ünïcödé line\n[00:02.00]二行目\n";
        let dir = tempfile::tempdir().unwrap();
        let mut le = vec![0xff, 0xfe];
        le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        write(dir.path(), "Never Gonna Give You Up.lrc", &le);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(lyrics.lines[0].text, "Ünïcödé line");
        assert_eq!(lyrics.lines[1].text, "二行目");

        let dir = tempfile::tempdir().unwrap();
        let mut be = vec![0xfe, 0xff];
        be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
        write(dir.path(), "Never Gonna Give You Up.lrc", &be);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(lyrics.lines[1].text, "二行目");
    }

    #[tokio::test]
    async fn empty_file_is_skipped_for_the_next_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.lrc", "");
        assert_eq!(fetch(dir.path(), &rick()).await, None);
        write(dir.path(), "Rick Astley - Never Gonna Give You Up.txt", TXT);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines.len(), 2);
    }

    #[tokio::test]
    async fn oversized_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("Rick Astley - Never Gonna Give You Up.lrc");
        std::fs::write(&big, LRC).unwrap();
        // Up to the limit the file is used ...
        std::fs::OpenOptions::new()
            .write(true)
            .open(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES)
            .unwrap();
        assert!(fetch(dir.path(), &rick()).await.unwrap().synced);
        // ... one byte more and it is skipped for the next match.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&big)
            .unwrap()
            .set_len(MAX_FILE_BYTES + 1)
            .unwrap();
        assert_eq!(fetch(dir.path(), &rick()).await, None);
        write(dir.path(), "Never Gonna Give You Up.txt", TXT);
        assert!(!fetch(dir.path(), &rick()).await.unwrap().synced);
    }

    #[tokio::test]
    async fn metadata_only_lrc_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.lrc",
            "[ar:Rick Astley]\n[ti:Never]\n",
        );
        assert_eq!(fetch(dir.path(), &rick()).await, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unreadable_file_is_skipped_not_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("Rick Astley - Never Gonna Give You Up.lrc");
        std::fs::write(&locked, LRC).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        write(dir.path(), "Never Gonna Give You Up.txt", TXT);
        let result = LocalLrcProvider::new(dir.path().to_path_buf())
            .fetch(&rick())
            .await
            .unwrap();
        // Running as root can still read the file; either way there is no error.
        let lyrics = result.unwrap();
        if std::fs::read(&locked).is_err() {
            assert!(!lyrics.synced);
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn folder_that_cannot_be_listed_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let lyrics = dir.path().join("lyrics");
        std::fs::create_dir(&lyrics).unwrap();
        write(&lyrics, "Never Gonna Give You Up.lrc", LRC);
        std::fs::set_permissions(&lyrics, std::fs::Permissions::from_mode(0o000)).unwrap();
        let listable = std::fs::read_dir(&lyrics).is_ok();
        let result = LocalLrcProvider::new(lyrics.clone()).fetch(&rick()).await;
        std::fs::set_permissions(&lyrics, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root can list anything; then the file is simply found.
        if listable {
            assert!(result.unwrap().is_some());
        } else {
            let err = result.unwrap_err();
            assert!(format!("{err:#}").contains("could not list"), "{err:#}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn broken_symlink_is_skipped_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("gone.lrc"),
            dir.path().join("Rick Astley - Never Gonna Give You Up.lrc"),
        )
        .unwrap();
        assert_eq!(fetch(dir.path(), &rick()).await, None);
        write(dir.path(), "Never Gonna Give You Up.txt", TXT);
        let lyrics = fetch(dir.path(), &rick()).await.unwrap();
        assert!(!lyrics.synced);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_files_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let real = elsewhere.path().join("real.lrc");
        std::fs::write(&real, LRC).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("Never Gonna Give You Up.lrc")).unwrap();
        assert!(fetch(dir.path(), &rick()).await.unwrap().synced);
    }

    #[tokio::test]
    async fn same_level_ties_are_broken_by_name() {
        // Both names have the key "never gonna give you up" (punctuation is
        // dropped), so they tie on level and kind; the path decides, not the
        // order the folder happens to list them in.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "Never Gonna Give You Up.lrc",
            "[00:01.00]plain name\n",
        );
        write(
            dir.path(),
            "Never Gonna Give You Up!.lrc",
            "[00:01.00]exclaimed name\n",
        );
        let keys = wanted_keys(&rick());
        let provider = LocalLrcProvider::new(dir.path().to_path_buf());
        let candidates = provider.list_candidates(&keys).await.unwrap().unwrap();
        assert_eq!(candidates.len(), 2, "{candidates:?}");
        assert!(candidates.iter().all(|c| c.level == candidates[0].level));
        // "!" sorts before ".", so the exclaimed name is first.
        let first = fetch(dir.path(), &rick()).await.unwrap();
        assert_eq!(first.lines[0].text, "exclaimed name");
        for _ in 0..3 {
            assert_eq!(fetch(dir.path(), &rick()).await.unwrap(), first);
        }
    }

    #[test]
    fn preference_orders_level_then_kind_then_path() {
        let c = |path: &str, kind, level| Candidate {
            path: PathBuf::from(path),
            kind,
            level,
        };
        let mut list = [
            c("b.lrc", FileKind::Lrc, 2),
            c("z.txt", FileKind::Txt, 0),
            c("y.lrc", FileKind::Lrc, 1),
            c("b.lrc", FileKind::Lrc, 0),
            c("a.lrc", FileKind::Lrc, 0),
            c("a.txt", FileKind::Txt, 1),
        ];
        list.sort_by(Candidate::preference);
        let order: Vec<(&str, usize)> = list
            .iter()
            .map(|c| (c.path.to_str().unwrap(), c.level))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a.lrc", 0),
                ("b.lrc", 0),
                ("z.txt", 0),
                ("y.lrc", 1),
                ("a.txt", 1),
                ("b.lrc", 2),
            ]
        );
    }

    #[tokio::test]
    async fn unicode_names_match() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Кино - Группа крови.lrc", LRC);
        write(dir.path(), "宇多田ヒカル - First Love.txt", TXT);
        assert!(
            fetch(dir.path(), &track("Кино", "Группа крови"))
                .await
                .unwrap()
                .synced
        );
        assert!(
            !fetch(dir.path(), &track("宇多田ヒカル", "First Love"))
                .await
                .unwrap()
                .synced
        );
    }

    #[tokio::test]
    async fn empty_title_never_matches() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Rick Astley.lrc", LRC);
        write(dir.path(), "Rick Astley - .lrc", LRC);
        write(dir.path(), "---.lrc", LRC);
        write(dir.path(), ".lrc", LRC);
        assert_eq!(fetch(dir.path(), &track("Rick Astley", "")).await, None);
        assert_eq!(fetch(dir.path(), &track("Rick Astley", "!!!")).await, None);
    }

    #[tokio::test]
    async fn empty_artist_matches_by_title() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Never Gonna Give You Up.lrc", LRC);
        assert!(fetch(dir.path(), &track("", "Never Gonna Give You Up"))
            .await
            .is_some());
    }

    #[test]
    fn wanted_keys_order_and_dedup() {
        assert_eq!(
            wanted_keys(&track("Daft Punk, Pharrell Williams", "Get Lucky")),
            vec![
                "daft punk pharrell williams get lucky".to_string(),
                "daft punk get lucky".to_string(),
                "get lucky".to_string(),
            ]
        );
        assert_eq!(
            wanted_keys(&track("Queen", "Bohemian Rhapsody")),
            vec![
                "queen bohemian rhapsody".to_string(),
                "bohemian rhapsody".to_string()
            ]
        );
        assert_eq!(wanted_keys(&track("", "Song")), vec!["song".to_string()]);
        assert!(wanted_keys(&track("Artist", "")).is_empty());
    }

    #[test]
    fn classify_extensions() {
        assert_eq!(
            classify(Path::new("/x/A - B.lrc")),
            Some((FileKind::Lrc, "A - B".to_string()))
        );
        assert_eq!(
            classify(Path::new("A.b.TXT")),
            Some((FileKind::Txt, "A.b".to_string()))
        );
        assert_eq!(classify(Path::new("A.mp3")), None);
        assert_eq!(classify(Path::new("lrc")), None);
        assert_eq!(classify(Path::new("A.lrc.bak")), None);
    }

    #[test]
    fn decode_text_variants() {
        assert_eq!(decode_text(b"plain"), "plain");
        assert_eq!(decode_text(b"\xef\xbb\xbfbom"), "bom");
        assert_eq!(decode_text(b"\xff\xfeh\x00i\x00"), "hi");
        assert_eq!(decode_text(b"\xfe\xff\x00h\x00i"), "hi");
        // Odd trailing byte and lone surrogate are replaced, not a panic.
        assert_eq!(decode_text(b"\xff\xfeh\x00i"), "h\u{fffd}");
        assert_eq!(decode_text(b"\xff\xfe\x00\xd8"), "\u{fffd}");
        assert_eq!(decode_text(b""), "");
        assert_eq!(decode_text(b"\xc3"), "\u{fffd}");
    }
}
