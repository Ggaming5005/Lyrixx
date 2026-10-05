//! What the window's buttons do to Lyrix's files, without any window: the
//! pause marker, per-song offsets, the lyrics cache and links. Each mirrors
//! the matching `lyrix` command, so the app and the command agree.

use anyhow::{bail, Context};
use lyrix::config::Offsets;
use std::path::Path;

/// The text of the pause marker file.
const PAUSE_MARKER_TEXT: &str =
    "Lyrix is paused while this file exists. Resume sharing in Lyrix, run `lyrix resume` or \
     delete this file.\n";

/// Whether sharing is paused (the pause marker exists, as a file, a folder or a link).
pub fn is_paused(marker: &Path) -> bool {
    std::fs::symlink_metadata(marker).is_ok()
}

/// Pauses sharing by creating the pause marker (and its folder), or resumes
/// it by removing the marker, like `lyrix pause` and `lyrix resume`. Asking
/// for the state it is already in changes nothing. Returns whether sharing
/// is paused now.
pub fn set_paused(marker: &Path, paused: bool) -> anyhow::Result<bool> {
    if paused {
        if is_paused(marker) {
            return Ok(true);
        }
        if let Some(parent) = marker.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("could not create the folder {}", parent.display()))?;
            }
        }
        std::fs::write(marker, PAUSE_MARKER_TEXT)
            .with_context(|| format!("could not create the pause marker {}", marker.display()))?;
    } else {
        match std::fs::remove_file(marker) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("could not remove the pause marker {}", marker.display())
                })
            }
        }
    }
    Ok(is_paused(marker))
}

/// The largest offset the window can set, either way: an hour. Larger values
/// would be no use for a song, and stay exact as JavaScript numbers.
pub const MAX_OFFSET_MS: i64 = 60 * 60 * 1000;

/// How the window changes the offset of the song playing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetChange {
    /// Add this many ms to the song's offset (positive shows lines later).
    By(i64),
    /// Remove the song's offset.
    Reset,
}

/// Changes the offset stored for `key` in the offsets file (the engine
/// re-reads the file when it changes) and returns the stored value. The
/// result is kept within ±[`MAX_OFFSET_MS`]; 0 removes the entry.
pub fn change_offset(path: &Path, key: &str, change: OffsetChange) -> anyhow::Result<i64> {
    let mut offsets = Offsets::load(path)?;
    let value = match change {
        OffsetChange::By(delta) => offsets
            .get(key)
            .saturating_add(delta)
            .clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS),
        OffsetChange::Reset => 0,
    };
    offsets.set(key, value);
    offsets.save(path)?;
    Ok(offsets.get(key))
}

/// Deletes the `*.json` files directly inside `dir` (not in subfolders, and
/// nothing else), like `lyrix cache clear`. A missing folder counts as empty.
/// Returns how many were removed.
pub fn clear_cache_dir(dir: &Path) -> anyhow::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("could not read the cache folder {}", dir.display()))
        }
    };
    let mut removed: usize = 0;
    let mut failed: Vec<String> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                failed.push(e.to_string());
                continue;
            }
        };
        let path = entry.path();
        let is_json = path.extension().is_some_and(|ext| ext == "json");
        // The entry's own type: a link is removed as a link, never followed.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(true);
        if !is_json || is_dir {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed.push(format!("{}: {e}", path.display())),
        }
    }
    if let Some(first) = failed.first() {
        bail!(
            "removed {removed} cached lyrics from {}, but {} could not be removed ({first})",
            dir.display(),
            failed.len()
        );
    }
    Ok(removed)
}

/// `url` when it is a web address the browser may open: `https://` with a
/// host and no user name or password. Anything else (`http:`, `file:`,
/// `javascript:`, …) is refused with a message for the window.
pub fn https_url(url: &str) -> Result<tauri::Url, String> {
    let refuse = || format!("Lyrix only opens https:// links, not \"{}\".", url.trim());
    let parsed = tauri::Url::parse(url.trim()).map_err(|_| refuse())?;
    let has_host = parsed.host_str().is_some_and(|host| !host.is_empty());
    let has_login = !parsed.username().is_empty() || parsed.password().is_some();
    if parsed.scheme() != "https" || !has_host || has_login {
        return Err(refuse());
    }
    Ok(parsed)
}

/// An error's message chain (`outer: inner`) without trailing spaces, line
/// breaks or periods, to go inside a sentence.
pub fn error_reason(error: &anyhow::Error) -> String {
    format!("{error:#}")
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.')
        .to_string()
}

/// An error as a sentence for the window: capitalized, ending with a period.
pub fn error_sentence(error: &anyhow::Error) -> String {
    let reason = error_reason(error);
    let mut chars = reason.chars();
    match chars.next() {
        Some(first) => format!("{}{}.", first.to_uppercase(), chars.as_str()),
        None => "Something went wrong.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_sentences() {
        let error = anyhow::anyhow!("permission denied\n").context("could not save the file");
        assert_eq!(
            error_reason(&error),
            "could not save the file: permission denied"
        );
        assert_eq!(
            error_sentence(&error),
            "Could not save the file: permission denied."
        );
        assert_eq!(error_sentence(&anyhow::anyhow!("étrange...")), "Étrange.");
        assert_eq!(
            error_sentence(&anyhow::anyhow!(" ")),
            "Something went wrong."
        );
    }

    #[test]
    fn pausing_creates_the_marker_and_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("config").join("paused");

        assert!(!is_paused(&marker));
        assert!(set_paused(&marker, true).unwrap());
        assert!(marker.is_file());
        assert!(std::fs::read_to_string(&marker)
            .unwrap()
            .contains("lyrix resume"));

        // Pausing again keeps the marker as it is.
        std::fs::write(&marker, "mine").unwrap();
        assert!(set_paused(&marker, true).unwrap());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "mine");
    }

    #[test]
    fn resuming_removes_the_marker_and_is_fine_when_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("paused");
        std::fs::write(&marker, "").unwrap();

        assert!(!set_paused(&marker, false).unwrap());
        assert!(!marker.exists());
        assert!(!set_paused(&marker, false).unwrap());
    }

    #[test]
    fn a_marker_that_cannot_be_removed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("paused");
        // A folder is a marker too (it pauses), but remove_file cannot remove it.
        std::fs::create_dir(&marker).unwrap();
        assert!(is_paused(&marker));
        let error = set_paused(&marker, false).unwrap_err();
        assert!(format!("{error:#}").contains("pause marker"));
    }

    #[test]
    fn offsets_add_up_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("offsets.toml");

        assert_eq!(
            change_offset(&path, "artist - song", OffsetChange::By(250)).unwrap(),
            250
        );
        assert_eq!(
            change_offset(&path, "artist - song", OffsetChange::By(-100)).unwrap(),
            150
        );
        assert_eq!(Offsets::load(&path).unwrap().get("artist - song"), 150);

        // Other songs keep theirs.
        change_offset(&path, "other - song", OffsetChange::By(-500)).unwrap();
        assert_eq!(
            change_offset(&path, "artist - song", OffsetChange::Reset).unwrap(),
            0
        );
        let offsets = Offsets::load(&path).unwrap();
        assert!(!offsets.songs.contains_key("artist - song"));
        assert_eq!(offsets.get("other - song"), -500);
    }

    #[test]
    fn an_offset_back_at_zero_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        change_offset(&path, "a - b", OffsetChange::By(50)).unwrap();
        assert_eq!(
            change_offset(&path, "a - b", OffsetChange::By(-50)).unwrap(),
            0
        );
        assert!(Offsets::load(&path).unwrap().songs.is_empty());
    }

    #[test]
    fn offsets_stay_within_an_hour() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        assert_eq!(
            change_offset(&path, "a - b", OffsetChange::By(i64::MAX)).unwrap(),
            MAX_OFFSET_MS
        );
        assert_eq!(
            change_offset(&path, "a - b", OffsetChange::By(i64::MIN)).unwrap(),
            -MAX_OFFSET_MS
        );
    }

    #[test]
    fn a_broken_offsets_file_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        std::fs::write(&path, "this is [[ not toml").unwrap();
        let error = change_offset(&path, "a - b", OffsetChange::By(50)).unwrap_err();
        assert!(format!("{error:#}").contains("offsets"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "this is [[ not toml"
        );
    }

    #[test]
    fn clearing_the_cache_removes_only_json_files_at_the_top() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("lyrics");
        std::fs::create_dir_all(cache.join("sub")).unwrap();
        std::fs::write(cache.join("one.json"), "{}").unwrap();
        std::fs::write(cache.join("two.json"), "{}").unwrap();
        std::fs::write(cache.join("notes.txt"), "keep").unwrap();
        std::fs::write(cache.join("sub").join("three.json"), "{}").unwrap();
        std::fs::create_dir(cache.join("folder.json")).unwrap();

        assert_eq!(clear_cache_dir(&cache).unwrap(), 2);
        assert!(!cache.join("one.json").exists());
        assert!(!cache.join("two.json").exists());
        assert!(cache.join("notes.txt").exists());
        assert!(cache.join("sub").join("three.json").exists());
        assert!(cache.join("folder.json").is_dir());
        assert_eq!(clear_cache_dir(&cache).unwrap(), 0);
    }

    #[test]
    fn a_missing_cache_folder_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(clear_cache_dir(&dir.path().join("nope")).unwrap(), 0);
    }

    #[test]
    fn only_https_links_open() {
        assert_eq!(
            https_url("https://lrclib.net/").unwrap().as_str(),
            "https://lrclib.net/"
        );
        assert_eq!(
            https_url("  https://discord.com/developers/applications ")
                .unwrap()
                .as_str(),
            "https://discord.com/developers/applications"
        );
        for refused in [
            "http://lrclib.net",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,hi",
            "https://user:secret@example.com",
            "https://user@example.com",
            "lrclib.net",
            "",
            "https://",
        ] {
            let message = https_url(refused).unwrap_err();
            assert!(message.contains("https://"), "{refused}: {message}");
        }
    }
}
