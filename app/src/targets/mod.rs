//! Where statuses go: Discord, the terminal, and later Slack, GitHub and others.

pub mod console;
pub mod discord_rpc;

use crate::types::Status;
use async_trait::async_trait;
use std::time::Duration;

/// Why a target could not apply a status.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// The service asked us to slow down.
    #[error("rate limited{}", .retry_after.map(|d| format!(" (retry after {} ms)", d.as_millis())).unwrap_or_default())]
    RateLimited { retry_after: Option<Duration> },
    /// The login is missing, wrong or revoked. The engine switches the target off.
    #[error("not authorized: {0}")]
    Unauthorized(String),
    /// The target cannot be reached right now (e.g. Discord is not running).
    /// The engine keeps the value and tries again later.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Anything else.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Something that can show a status.
#[async_trait]
pub trait StatusTarget: Send {
    /// Short name for logs and settings, e.g. `discord`, `console`.
    fn name(&self) -> &'static str;

    /// The fewest milliseconds that should pass between two updates.
    fn min_interval(&self) -> Duration;

    /// Shows a status, replacing the previous one.
    async fn set(&mut self, status: &Status) -> Result<(), TargetError>;

    /// Removes the status (restoring whatever the user had, where possible).
    async fn clear(&mut self) -> Result<(), TargetError>;

    /// True when what the last `set` showed is gone although nothing was
    /// sent (Discord restarted and dropped it), so the engine sends the
    /// current status again. Called on every poll; it must answer at once
    /// and not connect. The default: a status is never lost.
    async fn status_lost(&mut self) -> bool {
        false
    }
}
