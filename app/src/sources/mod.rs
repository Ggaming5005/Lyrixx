//! Reading what is playing from the operating system.
//!
//! - Windows: the system media session (GSMTC), which every app in the media
//!   flyout reports to, browsers included.
//! - Linux: MPRIS over the D-Bus session bus.
//! - macOS: Spotify and Apple Music through AppleScript, or every app in the
//!   Now Playing widget when `mediaremote-adapter` is installed.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod mpris;
#[cfg(windows)]
pub mod windows;

use crate::config::SourcesConfig;
use crate::types::{PlaybackSnapshot, PlaybackStatus};
use async_trait::async_trait;

/// Something that can say what is playing right now.
#[async_trait]
pub trait NowPlayingSource: Send + Sync {
    /// Short name for logs, e.g. `windows-media`, `mpris`, `macos`.
    fn name(&self) -> &'static str;

    /// Reads the current playback. `Ok(None)` means nothing is playing or paused.
    /// Errors are for a broken source (no session bus, API failure); the engine
    /// logs them and tries again on the next poll.
    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>>;
}

/// Chooses which of several players to follow:
/// 1. drop snapshots whose `app_id` contains a `blocked` entry (case-insensitive),
/// 2. prefer `Playing` over `Paused` over `Stopped`,
/// 3. among equals, prefer the earliest match in `preferred` (substring of `app_id`,
///    case-insensitive), then players with a non-empty title,
/// 4. otherwise keep the original order.
pub fn pick_session(
    candidates: Vec<PlaybackSnapshot>,
    preferred: &[String],
    blocked: &[String],
) -> Option<PlaybackSnapshot> {
    let _ = (candidates, preferred, blocked);
    todo!()
}

/// The source for this operating system.
pub fn default_source(config: &SourcesConfig, blocked_apps: &[String]) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    let _ = (config, blocked_apps);
    todo!()
}

#[allow(dead_code)]
fn _uses(_: PlaybackStatus) {}
