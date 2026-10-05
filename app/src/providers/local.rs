//! Your own lyric files: a folder of `.lrc` and `.txt` files.

use super::LyricsProvider;
use crate::types::{Lyrics, Track};
use async_trait::async_trait;
use std::path::PathBuf;

/// Looks for a file in `dir` (not recursive) whose name matches the song.
///
/// A file matches when the [`crate::matcher::normalize_key`] of its stem equals
/// the key of `"<artist> - <title>"`, of `"<primary artist> - <title>"`, or of
/// `"<title>"` alone (in that order of preference). `.lrc` files are parsed with
/// [`crate::lrc::parse_lrc`]; `.txt` files with [`crate::lrc::from_plain`]. When
/// both exist, `.lrc` wins. A missing folder means "nothing found", not an error.
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

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let _ = (track, &self.dir);
        todo!()
    }
}
