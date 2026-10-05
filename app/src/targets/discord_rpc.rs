//! Discord Rich Presence over Discord's local IPC socket. No login needed: the
//! Discord desktop app must be running on the same computer.
//!
//! Protocol (as used by every Rich Presence library):
//! - Connect to the first that exists of `discord-ipc-0` … `discord-ipc-9`:
//!   - Windows: named pipes `\\?\pipe\discord-ipc-N`.
//!   - Unix: sockets in each of `$XDG_RUNTIME_DIR`, `$TMPDIR`, `$TMP`, `$TEMP`,
//!     `/tmp`, also inside their `app/com.discordapp.Discord/`,
//!     `.flatpak/com.discordapp.Discord/xdg-run/`, `snap.discord/` and
//!     `snap.discord-canary/` subfolders (Flatpak and Snap installs).
//! - Every message is a frame: opcode (u32 little-endian), payload length
//!   (u32 little-endian), then that many bytes of UTF-8 JSON. Opcodes:
//!   0 handshake, 1 frame, 2 close, 3 ping, 4 pong.
//! - Handshake: send op 0 `{"v":1,"client_id":"<id>"}`; Discord answers op 1
//!   with `"cmd":"DISPATCH","evt":"READY"`, or op 2 (close) with `code` and
//!   `message` (e.g. an invalid client id).
//! - Set activity: op 1 `{"cmd":"SET_ACTIVITY","args":{"pid":<our pid>,"activity":{…}},"nonce":"<unique>"}`;
//!   Discord answers op 1 with the same nonce, and `"evt":"ERROR"` plus
//!   `data.message` when it rejected the activity. `"activity": null` clears it.
//! - A ping (op 3) must be answered with a pong (op 4) carrying the same payload.
//!
//! The activity uses `type` 2 (Listening) and `status_display_type` 2 (Details),
//! so the member list reads "Listening to <lyric line>".

use super::{StatusTarget, TargetError};
use crate::types::Status;
use async_trait::async_trait;
use std::time::Duration;

/// Discord limits `details` and `state` to 128 characters and rejects strings
/// shorter than 2.
pub const MAX_FIELD_CHARS: usize = 128;
pub const MIN_FIELD_CHARS: usize = 2;

/// After a failed connection attempt, wait this long before trying again.
pub const RECONNECT_EVERY: Duration = Duration::from_secs(15);

/// Frame opcodes.
pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;
pub const OP_CLOSE: u32 = 2;
pub const OP_PING: u32 = 3;
pub const OP_PONG: u32 = 4;

/// Encodes one IPC frame.
pub fn encode_frame(opcode: u32, payload: &serde_json::Value) -> Vec<u8> {
    let _ = (opcode, payload);
    todo!()
}

/// Decodes one complete frame from the start of `buf`. Returns the opcode, the
/// JSON payload and how many bytes were used, or `Ok(None)` when `buf` does
/// not hold a whole frame yet. A payload over 64 KiB or invalid JSON is an error.
pub fn decode_frame(buf: &[u8]) -> anyhow::Result<Option<(u32, serde_json::Value, usize)>> {
    let _ = buf;
    todo!()
}

/// Makes `s` fit a Discord text field: truncated to [`MAX_FIELD_CHARS`] with
/// [`crate::template::truncate_chars`], and padded with U+2800 (Braille blank)
/// when shorter than [`MIN_FIELD_CHARS`] after trimming, so `♪` is accepted.
pub fn fit_field(s: &str) -> String {
    let _ = s;
    todo!()
}

/// Builds the activity JSON for a status:
/// - `type`: 2, `status_display_type`: 2
/// - `details`: `fit_field(status.text)`
/// - `state`: `fit_field("<title> · <artist>")`, or just the title when the artist
///   is empty; ` (estimated timing)` is appended when `status.estimated`
///   (before fitting)
/// - `timestamps`: when `show_progress` and `status.started_at_unix_ms` is known:
///   `start` = started_at; plus `end` = started_at + duration when the duration is known
/// - `assets.large_text`: the album, when known and at least 2 characters,
///   fitted like the other fields (only together with a `large_image`, so it is
///   left out for now)
pub fn build_activity(status: &Status, show_progress: bool) -> serde_json::Value {
    let _ = (status, show_progress);
    todo!()
}

/// Rich Presence target. Connects lazily on the first update, and reconnects
/// (at most every [`RECONNECT_EVERY`]) after Discord closes or restarts. While
/// Discord is not running, [`set`](StatusTarget::set) returns
/// [`TargetError::Unavailable`] without touching the disk more than once per
/// [`RECONNECT_EVERY`]. A rejected activity (`evt: ERROR`) is
/// [`TargetError::Other`]; a handshake close with an invalid client id is
/// [`TargetError::Unauthorized`]. Each request waits at most 5 s for the reply.
pub struct DiscordRpcTarget {
    client_id: String,
    min_interval: Duration,
    show_progress: bool,
}

impl DiscordRpcTarget {
    pub fn new(client_id: impl Into<String>, min_interval: Duration, show_progress: bool) -> Self {
        Self { client_id: client_id.into(), min_interval, show_progress }
    }

    /// For tests on Unix: connect to this socket path instead of searching.
    #[cfg(unix)]
    pub fn with_socket_path(self, path: std::path::PathBuf) -> Self {
        let _ = path;
        todo!()
    }
}

#[async_trait]
impl StatusTarget for DiscordRpcTarget {
    fn name(&self) -> &'static str {
        "discord"
    }

    fn min_interval(&self) -> Duration {
        self.min_interval
    }

    async fn set(&mut self, status: &Status) -> Result<(), TargetError> {
        let _ = (status, &self.client_id, self.show_progress);
        todo!()
    }

    async fn clear(&mut self) -> Result<(), TargetError> {
        todo!()
    }
}
