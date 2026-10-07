//! Lyrix turns whatever you're listening to into a live status.
//!
//! The pipeline is: a [`sources::NowPlayingSource`] reports what the operating
//! system says is playing, a [`providers::ProviderChain`] finds synced lyrics,
//! the [`clock::SyncClock`] keeps track of the song position, and the
//! [`engine::Engine`] renders a [`Status`] that each [`targets::StatusTarget`]
//! receives at its own pace (see [`pacing::Pacer`]).

pub mod clock;
pub mod config;
pub mod engine;
pub mod lrc;
pub mod matcher;
pub mod pacing;
pub mod providers;
pub mod sources;
pub mod status;
pub mod targets;
pub mod template;
pub mod types;
pub mod view;

pub use types::*;

/// User-Agent sent to every web service, as LRCLIB asks clients to identify themselves.
pub const USER_AGENT: &str = concat!(
    "Lyrix/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/Ggaming5005/Lyrixx)"
);
