//! Prints each status change to the terminal.

use super::{StatusTarget, TargetError};
use crate::types::Status;
use async_trait::async_trait;
use std::io::Write;
use std::time::Duration;

/// Writes one line per update to the given writer (stdout by default):
/// `♫ <text>` for a status, with ` (estimated timing)` appended when
/// `status.estimated`, and `■ cleared` when cleared. A new song first prints a
/// header line `▶ <title> — <artist>` (once per track change). Write errors are
/// returned as [`TargetError::Other`].
pub struct ConsoleTarget<W: Write + Send = std::io::Stdout> {
    out: W,
}

impl ConsoleTarget<std::io::Stdout> {
    pub fn stdout() -> Self {
        Self { out: std::io::stdout() }
    }
}

impl<W: Write + Send> ConsoleTarget<W> {
    pub fn new(out: W) -> Self {
        Self { out }
    }

    /// The writer, for tests.
    pub fn into_inner(self) -> W {
        self.out
    }
}

#[async_trait]
impl<W: Write + Send> StatusTarget for ConsoleTarget<W> {
    fn name(&self) -> &'static str {
        "console"
    }

    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }

    async fn set(&mut self, status: &Status) -> Result<(), TargetError> {
        let _ = (status, &mut self.out);
        todo!()
    }

    async fn clear(&mut self) -> Result<(), TargetError> {
        todo!()
    }
}
