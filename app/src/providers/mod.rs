//! Lyrics providers and the chain that tries them in order.

pub mod cache;
pub mod local;
pub mod lrclib;

use crate::types::{Lyrics, Track};
use async_trait::async_trait;
use std::time::Duration;

/// Consecutive errors after which a provider is skipped for [`SKIP_FOR`].
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;
/// How long a failing provider is skipped.
pub const SKIP_FOR: Duration = Duration::from_secs(10 * 60);

/// A place lyrics can come from.
#[async_trait]
pub trait LyricsProvider: Send + Sync {
    /// Short stable name, stored in [`Lyrics::source`] (e.g. `lrclib`).
    fn name(&self) -> &'static str;

    /// Looks up lyrics for an already-normalized track.
    /// `Ok(None)` means "this provider has nothing for the song";
    /// `Err` means the provider failed (network, bad response) and counts
    /// against its health.
    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>>;
}

/// The outcome of a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Found(Lyrics),
    NotFound,
}

/// Tries the cache, then each healthy provider in order.
///
/// - The track is normalized with [`crate::matcher::normalize_track`] first.
/// - A cache hit (including a fresh "not found" entry) returns immediately.
/// - Synced lyrics win: if a provider returns unsynced lyrics, they are kept as
///   a fallback and the remaining providers are still asked for synced ones.
///   Instrumental results count as final.
/// - Unsynced lyrics are returned with timings spread by
///   [`Lyrics::spread_evenly`] when the duration is known.
/// - The final outcome (found or not found) is written to the cache when one is set.
/// - [`Lyrics::source`] is set to the provider's name.
/// - Health: an `Err` increments the provider's consecutive error count;
///   `Ok` resets it; at [`MAX_CONSECUTIVE_ERRORS`] the provider is skipped until
///   [`SKIP_FOR`] has passed, then tried again. A provider error never fails
///   the whole lookup.
pub struct ProviderChain {
    _private: (),
}

impl ProviderChain {
    pub fn new(providers: Vec<Box<dyn LyricsProvider>>, cache: Option<cache::LyricsCache>) -> Self {
        let _ = (providers, cache);
        todo!()
    }

    /// See the type docs. Safe to call from several tasks at once.
    pub async fn resolve(&self, track: &Track) -> Resolved {
        let _ = track;
        todo!()
    }

    /// Names of the providers, in order, with whether each is currently skipped.
    pub fn health(&self) -> Vec<(&'static str, bool)> {
        todo!()
    }
}
