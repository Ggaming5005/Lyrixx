//! The loop that ties sources, lyrics, the clock and targets together.

use crate::config::Config;
use crate::providers::ProviderChain;
use crate::sources::NowPlayingSource;
use crate::targets::StatusTarget;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

/// How often statuses are recomputed while something plays, at most.
pub const RENDER_TICK_MS: u64 = 100;

/// Runs Lyrix until shutdown.
///
/// Behavior:
/// - Every `general.poll_interval_ms` the source is read. Source errors are
///   logged (the same message at most once per minute) and never stop the loop.
///   `Ok(None)` resets the clock and clears every target.
/// - A track change (see [`crate::clock::ClockEvent`]) starts a lyrics lookup on
///   a background task (`ProviderChain::resolve` on the normalized track) and
///   loads that song's offset from the offsets file (missing or unreadable → 0).
///   Until the lookup finishes the status shows the song (no-lyrics template).
///   A lookup result for a song that is no longer playing is ignored.
/// - At least every [`RENDER_TICK_MS`] (sooner when a lyric line or a pacer is
///   due), the desired status is composed with
///   [`crate::status::compose_status`] using the clock position, the global
///   offset plus the song offset, and the wall clock. While the pause marker
///   exists the desired status is "cleared".
/// - Each target has its own [`crate::pacing::Pacer`] over `Option<Status>`
///   (`None` = cleared) built from the target's `min_interval`. The desired value
///   is offered to every pacer each tick; ready values are sent with `set` or `clear`.
///   On start, a clear is sent to every target so stale statuses from a crash go away.
/// - Errors: `RateLimited` → `Pacer::on_rate_limited` (switch the target off on
///   `Disable`); `Unauthorized` → switch the target off; `Unavailable` and
///   `Other` → `Pacer::on_failure` (logged, retried later). A switched-off target
///   is logged once at error level and never used again in this run.
/// - On shutdown every target that is showing something is cleared, waiting at
///   most 2 s per target.
pub struct Engine {
    config: Config,
    source: Box<dyn NowPlayingSource>,
    chain: Arc<ProviderChain>,
    targets: Vec<Box<dyn StatusTarget>>,
    offsets_path: Option<PathBuf>,
    pause_marker: Option<PathBuf>,
}

impl Engine {
    pub fn new(
        config: Config,
        source: Box<dyn NowPlayingSource>,
        chain: Arc<ProviderChain>,
        targets: Vec<Box<dyn StatusTarget>>,
    ) -> Self {
        Self { config, source, chain, targets, offsets_path: None, pause_marker: None }
    }

    /// Reads per-song offsets from this file on every track change.
    pub fn with_offsets_path(mut self, path: PathBuf) -> Self {
        self.offsets_path = Some(path);
        self
    }

    /// Clears all statuses while this file exists.
    pub fn with_pause_marker(mut self, path: PathBuf) -> Self {
        self.pause_marker = Some(path);
        self
    }

    /// Runs until `shutdown` completes. See the type docs.
    pub async fn run<S>(self, shutdown: S) -> anyhow::Result<()>
    where
        S: Future<Output = ()> + Send,
    {
        let _ = (shutdown, &self.config, &self.source, &self.chain, &self.targets, &self.offsets_path, &self.pause_marker);
        todo!()
    }
}
