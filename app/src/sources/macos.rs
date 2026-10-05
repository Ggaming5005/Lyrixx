//! macOS: the apps' own AppleScript interfaces, or `mediaremote-adapter`.
//!
//! Built in (no install needed): Spotify and Apple Music via `osascript`. A
//! script is only sent to an app that is already running (checked through
//! System Events / `application "X" is running`), so Lyrix never launches it.
//! Both apps report `name`, `artist`, `album`, `duration` (Spotify: ms; Music:
//! seconds as a real) and `player position` (seconds as a real) of the current
//! track, plus `player state` (playing/paused/stopped). Spotify's `id` is
//! `spotify:track:<id>`.
//!
//! Optional: when `macos_adapter_dir` points at an installed
//! [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter), run
//! `/usr/bin/perl <dir>/bin/mediaremote-adapter.pl <dir>/build/MediaRemoteAdapter.framework get`
//! and read its JSON (`title`, `artist`, `album`, `duration` and `elapsedTime` in
//! seconds, `timestamp` for when `elapsedTime` was true, `playing`,
//! `bundleIdentifier`, `playbackRate`). This covers every app in the Now Playing
//! widget, browsers included. If it fails, fall back to AppleScript.

use super::NowPlayingSource;
use crate::types::PlaybackSnapshot;
use async_trait::async_trait;
use std::path::PathBuf;

/// See the module docs.
pub struct MacSource {
    adapter_dir: Option<PathBuf>,
    preferred: Vec<String>,
    blocked: Vec<String>,
}

impl MacSource {
    pub fn new(adapter_dir: Option<PathBuf>, preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self { adapter_dir, preferred, blocked }
    }
}

#[async_trait]
impl NowPlayingSource for MacSource {
    fn name(&self) -> &'static str {
        "macos"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        let _ = (&self.adapter_dir, &self.preferred, &self.blocked);
        todo!()
    }
}
