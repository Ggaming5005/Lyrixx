//! Windows: the Global System Media Transport Controls (GSMTC) session manager,
//! the same data the volume flyout shows.
//!
//! `GlobalSystemMediaTransportControlsSessionManager::RequestAsync()` gives the
//! manager; `GetSessions()` lists every app's session. Per session:
//! - `SourceAppUserModelId()` → `app_id`
//! - `TryGetMediaPropertiesAsync()` → `Title`, `Artist`, `AlbumTitle`
//! - `GetTimelineProperties()` → `Position`, `EndTime`, `StartTime` (TimeSpan,
//!   100 ns units) and `LastUpdatedTime` (DateTime, 100 ns since 1601-01-01 UTC);
//!   the position was true at `LastUpdatedTime`, so it is converted to an
//!   `Instant` via the current system time, never later than now.
//! - `GetPlaybackInfo()` → `PlaybackStatus` (Playing/Paused/Stopped/Closed/Opened/
//!   Changing) and `PlaybackRate` (optional, default 1.0)
//!
//! Duration is `EndTime - StartTime` when positive. The WinRT calls block, so
//! they run on a blocking thread.

use super::NowPlayingSource;
use crate::types::PlaybackSnapshot;
use async_trait::async_trait;

/// See the module docs.
pub struct WindowsMediaSource {
    preferred: Vec<String>,
    blocked: Vec<String>,
}

impl WindowsMediaSource {
    pub fn new(preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self { preferred, blocked }
    }
}

#[async_trait]
impl NowPlayingSource for WindowsMediaSource {
    fn name(&self) -> &'static str {
        "windows-media"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        let _ = (&self.preferred, &self.blocked);
        todo!()
    }
}
