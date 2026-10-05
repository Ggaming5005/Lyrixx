//! Discord Rich Presence over Discord's local IPC socket. No login needed: the
//! Discord desktop app must be running on the same computer.
//!
//! Protocol (as used by every Rich Presence library):
//! - Connect to the first that exists of `discord-ipc-0` … `discord-ipc-9`:
//!   - Windows: named pipes `\\.\pipe\discord-ipc-N`.
//!   - Unix: sockets in each of `$XDG_RUNTIME_DIR`, `$TMPDIR`, `$TMP`, `$TEMP`,
//!     `/tmp`, also inside their `app/com.discordapp.Discord/`,
//!     `.flatpak/com.discordapp.Discord/xdg-run/`, `snap.discord/` and
//!     `snap.discord-canary/` subfolders (Flatpak and Snap installs).
//! - Every message is a frame: opcode (u32 little-endian), payload length
//!   (u32 little-endian), then that many bytes of UTF-8 JSON. Opcodes:
//!   0 handshake, 1 frame, 2 close, 3 ping, 4 pong.
//! - Handshake: send op 0 `{"v":1,"client_id":"<id>"}`; Discord answers op 1
//!   with `"cmd":"DISPATCH","evt":"READY"`, or op 2 (close) with `code` and
//!   `message` (e.g. close code 4000, an invalid client id). `evt: ERROR`
//!   events use a different table, the RPC error codes, where 4000 is an
//!   invalid payload and 4007 an invalid client id.
//! - Set activity: op 1 `{"cmd":"SET_ACTIVITY","args":{"pid":<our pid>,"activity":{…}},"nonce":"<unique>"}`;
//!   Discord answers op 1 with the same nonce, and `"evt":"ERROR"` plus
//!   `data.message` when it rejected the activity. `"activity": null` clears it.
//! - A ping (op 3) must be answered with a pong (op 4) carrying the same payload.
//!
//! The activity uses `type` 2 (Listening) and `status_display_type` 2 (Details),
//! so the member list reads "Listening to <lyric line>".

use super::{StatusTarget, TargetError};
use crate::template::truncate_chars;
use crate::types::Status;
use anyhow::Context as _;
use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

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

/// Opcode plus payload length.
const HEADER_BYTES: usize = 8;
/// The largest payload [`decode_frame`] accepts (64 KiB).
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// How long the handshake and each request may wait for Discord's answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// `discord-ipc-0` … `discord-ipc-9`.
const IPC_SLOTS: u32 = 10;
/// U+2800 BRAILLE PATTERN BLANK: looks empty but is not whitespace, so Discord
/// counts it towards the minimum length.
const FIELD_FILLER: char = '\u{2800}';
/// Close code (op 2 frame) Discord sends when the client id is not a valid
/// application.
const CLOSE_INVALID_CLIENT_ID: i64 = 4000;
/// RPC error code (`evt: ERROR` event) for an invalid client id. The two code
/// tables differ: in an `ERROR` event, 4000 means "invalid payload".
const ERROR_INVALID_CLIENT_ID: i64 = 4007;

/// Encodes one IPC frame.
pub fn encode_frame(opcode: u32, payload: &serde_json::Value) -> Vec<u8> {
    let body = payload.to_string();
    // Payloads are a few hundred bytes; a 4 GiB one cannot be built here.
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut frame = Vec::with_capacity(HEADER_BYTES.saturating_add(body.len()));
    frame.extend_from_slice(&opcode.to_le_bytes());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(body.as_bytes());
    frame
}

/// Reads a little-endian u32 at `at`, if `buf` is long enough.
fn read_u32_le(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at.checked_add(4)?)?;
    let bytes: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// Decodes one complete frame from the start of `buf`. Returns the opcode, the
/// JSON payload and how many bytes were used, or `Ok(None)` when `buf` does
/// not hold a whole frame yet. A payload over 64 KiB or invalid JSON is an error.
pub fn decode_frame(buf: &[u8]) -> anyhow::Result<Option<(u32, serde_json::Value, usize)>> {
    let (Some(opcode), Some(len)) = (read_u32_le(buf, 0), read_u32_le(buf, 4)) else {
        return Ok(None);
    };
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len > MAX_PAYLOAD_BYTES {
        anyhow::bail!(
            "Discord IPC frame is too large ({len} bytes, the limit is {MAX_PAYLOAD_BYTES})"
        );
    }
    // `len` is at most 64 KiB, so this cannot overflow.
    let end = HEADER_BYTES + len;
    let Some(body) = buf.get(HEADER_BYTES..end) else {
        return Ok(None);
    };
    let payload: Value =
        serde_json::from_slice(body).context("Discord IPC frame is not valid JSON")?;
    Ok(Some((opcode, payload, end)))
}

/// Makes `s` fit a Discord text field: leading and trailing whitespace is
/// removed, the rest truncated to [`MAX_FIELD_CHARS`] with
/// [`crate::template::truncate_chars`] (and shortened further when needed so it
/// is also at most [`MAX_FIELD_CHARS`] UTF-16 code units, which is what
/// Discord's validator counts, e.g. an emoji counts twice), and padded with
/// U+2800 (Braille blank) when shorter than [`MIN_FIELD_CHARS`], so `♪` is
/// accepted.
pub fn fit_field(s: &str) -> String {
    let trimmed = s.trim();
    // `truncate_chars` only looks at the first `max + 1` characters, so cutting
    // the source here keeps the loop below cheap without changing the result.
    let source: String = trimmed
        .chars()
        .take(MAX_FIELD_CHARS.saturating_add(1))
        .collect();
    let mut max = MAX_FIELD_CHARS;
    let mut out = truncate_chars(&source, max);
    while out.encode_utf16().count() > MAX_FIELD_CHARS && max > 0 {
        max -= 1;
        out = truncate_chars(&source, max);
    }
    let mut count = out.chars().count();
    while count < MIN_FIELD_CHARS {
        out.push(FIELD_FILLER);
        count += 1;
    }
    out
}

/// Builds the activity JSON for a status:
/// - `type`: 2, `status_display_type`: 2
/// - `details`: `fit_field(status.text)`
/// - `state`: `fit_field("<title> · <artist>")`, or just the title when the artist
///   is empty (just the artist when the title is empty); ` (estimated timing)`
///   is appended when `status.estimated` (before fitting)
/// - `timestamps` (Unix milliseconds): when `show_progress` and
///   `status.started_at_unix_ms` is known: `start` = started_at; plus `end` =
///   started_at + duration when the duration is known and not zero
/// - `assets.large_text`: the album, when known and at least 2 characters,
///   fitted like the other fields (only together with a `large_image`, so it is
///   left out for now)
pub fn build_activity(status: &Status, show_progress: bool) -> serde_json::Value {
    let title = status.track.title.trim();
    let artist = status.track.artist.trim();
    let mut state = match (title.is_empty(), artist.is_empty()) {
        (_, true) => title.to_string(),
        (true, false) => artist.to_string(),
        (false, false) => format!("{title} · {artist}"),
    };
    if status.estimated {
        state.push_str(" (estimated timing)");
    }

    let mut activity = Map::new();
    activity.insert("type".into(), json!(2));
    activity.insert("status_display_type".into(), json!(2));
    activity.insert("details".into(), json!(fit_field(&status.text)));
    activity.insert("state".into(), json!(fit_field(&state)));

    if show_progress {
        if let Some(start) = status.started_at_unix_ms {
            let mut timestamps = Map::new();
            timestamps.insert("start".into(), json!(start));
            if let Some(duration) = status.track.duration_ms.filter(|&d| d > 0) {
                timestamps.insert("end".into(), json!(start.saturating_add(duration)));
            }
            activity.insert("timestamps".into(), Value::Object(timestamps));
        }
    }
    Value::Object(activity)
}

/// The byte stream to Discord: a Unix socket or a Windows named pipe.
trait IpcStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> IpcStream for T {}

/// Why talking to Discord failed on an open connection.
#[derive(Debug)]
enum IpcError {
    Io(std::io::Error),
    /// Discord closed the stream.
    Eof,
    /// Discord sent something that is not a frame.
    Invalid(anyhow::Error),
    /// Discord sent a close frame (op 2). `code` is a close code.
    Closed {
        code: Option<i64>,
        message: String,
    },
    /// Discord answered the handshake with an `evt: ERROR` event. `code` is an
    /// RPC error code, which is not the same table as the close codes.
    Refused {
        code: Option<i64>,
        message: String,
    },
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcError::Io(e) => write!(f, "lost the connection to Discord: {e}"),
            IpcError::Eof => write!(f, "Discord closed the connection"),
            IpcError::Invalid(e) => write!(f, "Discord sent an invalid message: {e:#}"),
            IpcError::Closed { code, message } => {
                write!(f, "Discord closed the connection: ")?;
                write_reason(f, *code, message)
            }
            IpcError::Refused { code, message } => {
                write!(f, "Discord refused the connection: ")?;
                write_reason(f, *code, message)
            }
        }
    }
}

/// `<message> (code <code>)`, with a placeholder for a missing message.
fn write_reason(
    f: &mut std::fmt::Formatter<'_>,
    code: Option<i64>,
    message: &str,
) -> std::fmt::Result {
    let message = if message.is_empty() {
        "no reason given"
    } else {
        message
    };
    match code {
        Some(code) => write!(f, "{message} (code {code})"),
        None => write!(f, "{message}"),
    }
}

/// Reads `code` (a number or a numeric string) and `message` from a close
/// frame or an error event's `data`.
fn code_and_message(value: Option<&Value>) -> (Option<i64>, String) {
    let code = value.and_then(|v| v.get("code")).and_then(|c| {
        c.as_i64()
            .or_else(|| c.as_str().and_then(|s| s.trim().parse().ok()))
    });
    let message = value
        .and_then(|v| v.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (code, message)
}

/// The error for a close frame's payload.
fn close_details(value: Option<&Value>) -> IpcError {
    let (code, message) = code_and_message(value);
    IpcError::Closed { code, message }
}

/// The error for an `ERROR` event's `data` during the handshake.
fn refusal_details(value: Option<&Value>) -> IpcError {
    let (code, message) = code_and_message(value);
    IpcError::Refused { code, message }
}

/// An open, handshaken connection to Discord.
struct Connection {
    stream: Box<dyn IpcStream>,
    /// Bytes read but not yet decoded (frames can arrive split across reads).
    buf: Vec<u8>,
}

impl Connection {
    fn new(stream: Box<dyn IpcStream>) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    async fn send(&mut self, opcode: u32, payload: &Value) -> Result<(), IpcError> {
        let frame = encode_frame(opcode, payload);
        self.stream.write_all(&frame).await.map_err(IpcError::Io)?;
        self.stream.flush().await.map_err(IpcError::Io)
    }

    /// The next frame, reading more bytes as needed.
    async fn recv(&mut self) -> Result<(u32, Value), IpcError> {
        loop {
            if let Some((opcode, payload, used)) =
                decode_frame(&self.buf).map_err(IpcError::Invalid)?
            {
                let used = used.min(self.buf.len());
                self.buf.drain(..used);
                return Ok((opcode, payload));
            }
            self.buf.reserve(4096);
            let read = self
                .stream
                .read_buf(&mut self.buf)
                .await
                .map_err(IpcError::Io)?;
            if read == 0 {
                return Err(IpcError::Eof);
            }
        }
    }

    /// The next frame that is not a ping or a close: pings are answered with a
    /// pong, a close frame is an error.
    async fn recv_message(&mut self) -> Result<(u32, Value), IpcError> {
        loop {
            let (opcode, payload) = self.recv().await?;
            match opcode {
                OP_PING => self.send(OP_PONG, &payload).await?,
                OP_CLOSE => return Err(close_details(Some(&payload))),
                _ => return Ok((opcode, payload)),
            }
        }
    }

    /// Sends the handshake and waits for `READY`.
    async fn handshake(&mut self, client_id: &str) -> Result<(), IpcError> {
        self.send(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id }))
            .await?;
        loop {
            let (opcode, payload) = self.recv_message().await?;
            if opcode != OP_FRAME {
                continue;
            }
            match payload.get("evt").and_then(Value::as_str) {
                Some("READY") => return Ok(()),
                Some("ERROR") => return Err(refusal_details(payload.get("data"))),
                _ => continue,
            }
        }
    }

    /// Sends a command and returns Discord's reply with the same nonce,
    /// skipping unrelated frames.
    async fn request(&mut self, payload: &Value, nonce: &str) -> Result<Value, IpcError> {
        self.send(OP_FRAME, payload).await?;
        loop {
            let (opcode, reply) = self.recv_message().await?;
            if opcode == OP_FRAME && reply.get("nonce").and_then(Value::as_str) == Some(nonce) {
                return Ok(reply);
            }
        }
    }
}

/// How the last connection attempt failed, repeated until the next attempt.
#[derive(Debug, Clone)]
enum Failure {
    Unauthorized(String),
    Unavailable(String),
}

impl Failure {
    fn to_error(&self) -> TargetError {
        match self {
            Failure::Unauthorized(m) => TargetError::Unauthorized(m.clone()),
            Failure::Unavailable(m) => TargetError::Unavailable(m.clone()),
        }
    }
}

/// The socket paths to try on Unix, in order: every `discord-ipc-N` slot (lowest
/// first) in every candidate folder. `env` reads an environment variable.
#[cfg(unix)]
fn unix_socket_candidates(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    const SUBFOLDERS: [&str; 4] = [
        "app/com.discordapp.Discord",
        ".flatpak/com.discordapp.Discord/xdg-run",
        "snap.discord",
        "snap.discord-canary",
    ];

    let mut bases: Vec<PathBuf> = Vec::new();
    let from_env = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .filter_map(&env)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    for base in from_env.chain(std::iter::once(PathBuf::from("/tmp"))) {
        if !bases.contains(&base) {
            bases.push(base);
        }
    }

    let mut folders = Vec::new();
    for base in &bases {
        folders.push(base.clone());
        folders.extend(SUBFOLDERS.iter().map(|sub| base.join(sub)));
    }

    let mut paths = Vec::with_capacity(folders.len().saturating_mul(IPC_SLOTS as usize));
    for slot in 0..IPC_SLOTS {
        let name = format!("discord-ipc-{slot}");
        paths.extend(folders.iter().map(|folder| folder.join(&name)));
    }
    paths
}

/// Connects to the first candidate socket that accepts, skipping missing files
/// and stale or unusable sockets.
#[cfg(unix)]
async fn connect_first_unix(
    candidates: &[std::path::PathBuf],
) -> Result<Box<dyn IpcStream>, String> {
    let mut last_error = None;
    for path in candidates {
        match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => {
                tracing::debug!(path = %path.display(), "connected to Discord IPC");
                return Ok(Box::new(stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => last_error = Some(format!("{}: {e}", path.display())),
        }
    }
    Err(match last_error {
        Some(e) => format!("Discord is not running (could not connect to {e})"),
        None => "Discord is not running (no Discord IPC socket found)".to_string(),
    })
}

/// Where the Discord app listens.
#[derive(Debug, Clone, Default)]
struct Endpoint {
    /// Test override: connect to this socket instead of searching.
    #[cfg(unix)]
    socket_path: Option<std::path::PathBuf>,
}

impl Endpoint {
    /// Opens the byte stream to the Discord app.
    #[cfg(unix)]
    async fn open_stream(&self) -> Result<Box<dyn IpcStream>, String> {
        let candidates = match &self.socket_path {
            Some(path) => vec![path.clone()],
            None => unix_socket_candidates(|key| std::env::var_os(key)),
        };
        connect_first_unix(&candidates).await
    }

    /// Opens the byte stream to the Discord app.
    #[cfg(windows)]
    async fn open_stream(&self) -> Result<Box<dyn IpcStream>, String> {
        use tokio::net::windows::named_pipe::ClientOptions;

        let mut last_error = None;
        for slot in 0..IPC_SLOTS {
            let name = format!(r"\\.\pipe\discord-ipc-{slot}");
            match ClientOptions::new().open(&name) {
                Ok(pipe) => {
                    tracing::debug!(pipe = %name, "connected to Discord IPC");
                    return Ok(Box::new(pipe));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => last_error = Some(format!("{name}: {e}")),
            }
        }
        Err(match last_error {
            Some(e) => format!("Discord is not running (could not connect to {e})"),
            None => "Discord is not running (no Discord IPC pipe found)".to_string(),
        })
    }

    /// Opens the byte stream to the Discord app.
    #[cfg(not(any(unix, windows)))]
    async fn open_stream(&self) -> Result<Box<dyn IpcStream>, String> {
        Err("Discord Rich Presence is not supported on this platform".to_string())
    }

    /// Connects and completes the handshake.
    async fn connect(&self, client_id: &str) -> Result<Connection, Failure> {
        let stream = self.open_stream().await.map_err(Failure::Unavailable)?;
        let mut conn = Connection::new(stream);
        match conn.handshake(client_id).await {
            Ok(()) => Ok(conn),
            Err(IpcError::Closed {
                code: Some(CLOSE_INVALID_CLIENT_ID),
                message,
            })
            | Err(IpcError::Refused {
                code: Some(ERROR_INVALID_CLIENT_ID),
                message,
            }) => Err(Failure::Unauthorized(format!(
                "Discord rejected the client id {client_id:?}: {}",
                if message.is_empty() {
                    "invalid client id"
                } else {
                    message.as_str()
                }
            ))),
            Err(e) => Err(Failure::Unavailable(e.to_string())),
        }
    }
}

/// Rich Presence target. Connects lazily on the first update, and reconnects
/// (at most every [`RECONNECT_EVERY`]) after Discord closes or restarts. While
/// Discord is not running, [`set`](StatusTarget::set) returns
/// [`TargetError::Unavailable`] without touching the disk more than once per
/// [`RECONNECT_EVERY`]. A rejected activity (`evt: ERROR`) is
/// [`TargetError::Other`]; a handshake close with an invalid client id (close
/// code 4000, or an `evt: ERROR` answer to the handshake with RPC error code
/// 4007) is [`TargetError::Unauthorized`], and so is an empty client id (without
/// connecting); any other handshake close or error is
/// [`TargetError::Unavailable`]. Each request waits at most 5 s for the reply; no reply, a read
/// or write error, or Discord closing the connection drops the connection and
/// is [`TargetError::Unavailable`]. [`clear`](StatusTarget::clear) without an
/// open connection is `Ok(())` and does not connect: Discord removes the
/// activity by itself when the connection closes.
pub struct DiscordRpcTarget {
    client_id: String,
    min_interval: Duration,
    show_progress: bool,
    /// The open connection, if any.
    conn: Option<Connection>,
    /// Where to connect.
    endpoint: Endpoint,
    /// When the last connection attempt started.
    last_attempt: Option<Instant>,
    /// Why the connection was lost or could not be made, until the next attempt.
    last_failure: Option<Failure>,
    reconnect_every: Duration,
    request_timeout: Duration,
    /// Makes nonces unique within this process.
    nonce_counter: u64,
}

impl DiscordRpcTarget {
    pub fn new(client_id: impl Into<String>, min_interval: Duration, show_progress: bool) -> Self {
        Self {
            client_id: client_id.into(),
            min_interval,
            show_progress,
            conn: None,
            endpoint: Endpoint::default(),
            last_attempt: None,
            last_failure: None,
            reconnect_every: RECONNECT_EVERY,
            request_timeout: REQUEST_TIMEOUT,
            nonce_counter: 0,
        }
    }

    /// For tests on Unix: connect to this socket path instead of searching.
    #[cfg(unix)]
    pub fn with_socket_path(mut self, path: std::path::PathBuf) -> Self {
        self.endpoint.socket_path = Some(path);
        self
    }

    /// For tests: wait this long between connection attempts instead of
    /// [`RECONNECT_EVERY`].
    #[cfg(test)]
    pub(crate) fn with_reconnect_every(mut self, every: Duration) -> Self {
        self.reconnect_every = every;
        self
    }

    /// For tests: wait this long for each reply instead of 5 s.
    #[cfg(test)]
    pub(crate) fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// For tests: whether a connection is open.
    #[cfg(test)]
    pub(crate) fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    /// `<process id>-<counter>`.
    fn next_nonce(&mut self) -> String {
        let n = self.nonce_counter;
        self.nonce_counter = n.wrapping_add(1);
        format!("{}-{}", std::process::id(), n)
    }

    /// Makes sure a connection is open, connecting when allowed.
    async fn ensure_connected(&mut self) -> Result<(), TargetError> {
        if self.conn.is_some() {
            return Ok(());
        }
        let now = Instant::now();
        if let Some(last) = self.last_attempt {
            let waited = now.saturating_duration_since(last);
            if waited < self.reconnect_every {
                return Err(self.waiting_error(self.reconnect_every.saturating_sub(waited)));
            }
        }
        self.last_attempt = Some(now);

        let client_id = self.client_id.trim().to_string();
        let timeout = self.request_timeout;
        let outcome = match tokio::time::timeout(timeout, self.endpoint.connect(&client_id)).await {
            Ok(outcome) => outcome,
            Err(_) => Err(Failure::Unavailable(format!(
                "Discord did not answer the handshake within {timeout:?}"
            ))),
        };
        match outcome {
            Ok(conn) => {
                self.conn = Some(conn);
                self.last_failure = None;
                Ok(())
            }
            Err(failure) => {
                tracing::debug!(?failure, "could not connect to Discord");
                let error = failure.to_error();
                self.last_failure = Some(failure);
                Err(error)
            }
        }
    }

    /// The error while waiting `wait` before the next connection attempt.
    fn waiting_error(&self, wait: Duration) -> TargetError {
        let seconds = wait.as_millis().div_ceil(1000);
        match &self.last_failure {
            Some(Failure::Unauthorized(m)) => TargetError::Unauthorized(m.clone()),
            Some(Failure::Unavailable(m)) => {
                TargetError::Unavailable(format!("{m}; trying again in {seconds} s"))
            }
            None => TargetError::Unavailable(format!(
                "not connected to Discord; trying again in {seconds} s"
            )),
        }
    }

    /// Drops the connection after an error and returns the error to report.
    fn disconnect(&mut self, message: String) -> TargetError {
        tracing::debug!(%message, "dropping the Discord connection");
        self.conn = None;
        self.last_failure = Some(Failure::Unavailable(message.clone()));
        TargetError::Unavailable(message)
    }

    /// Sends `SET_ACTIVITY` with `activity` (null clears) on the open connection.
    async fn send_activity(&mut self, activity: Value) -> Result<(), TargetError> {
        let nonce = self.next_nonce();
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": std::process::id(), "activity": activity },
            "nonce": nonce,
        });
        let timeout = self.request_timeout;
        let Some(conn) = self.conn.as_mut() else {
            return Err(TargetError::Unavailable(
                "not connected to Discord".to_string(),
            ));
        };
        let reply = match tokio::time::timeout(timeout, conn.request(&payload, &nonce)).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => return Err(self.disconnect(e.to_string())),
            Err(_) => {
                return Err(self.disconnect(format!("Discord did not answer within {timeout:?}")))
            }
        };
        if reply.get("evt").and_then(Value::as_str) == Some("ERROR") {
            let message = reply
                .pointer("/data/message")
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            return Err(TargetError::Other(anyhow::anyhow!(
                "Discord rejected the activity: {message}"
            )));
        }
        Ok(())
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
        if self.client_id.trim().is_empty() {
            return Err(TargetError::Unauthorized(
                "Discord client id is not configured".to_string(),
            ));
        }
        self.ensure_connected().await?;
        let activity = build_activity(status, self.show_progress);
        self.send_activity(activity).await
    }

    async fn clear(&mut self) -> Result<(), TargetError> {
        if self.conn.is_none() {
            return Ok(());
        }
        self.send_activity(Value::Null).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StatusKind, Track};

    fn track(title: &str, artist: &str, duration_ms: Option<u64>) -> Track {
        Track {
            title: title.into(),
            artist: artist.into(),
            album: Some("Whenever You Need Somebody".into()),
            duration_ms,
            spotify_id: None,
        }
    }

    fn status(text: &str) -> Status {
        Status {
            text: text.into(),
            kind: StatusKind::Line,
            line: Some(text.into()),
            track: track("Never Gonna Give You Up", "Rick Astley", Some(213_000)),
            started_at_unix_ms: Some(1_700_000_000_000),
            estimated: false,
        }
    }

    fn frame(opcode: u32, len: u32, body: &[u8]) -> Vec<u8> {
        let mut out = opcode.to_le_bytes().to_vec();
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    // ---- encode_frame / decode_frame ----

    #[test]
    fn encode_frame_layout() {
        let payload = json!({"v": 1, "client_id": "123"});
        let bytes = encode_frame(OP_HANDSHAKE, &payload);
        let body = payload.to_string();
        assert_eq!(&bytes[0..4], &0u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &(body.len() as u32).to_le_bytes());
        assert_eq!(&bytes[8..], body.as_bytes());
        assert_eq!(bytes.len(), 8 + body.len());

        let pong = encode_frame(OP_PONG, &json!(null));
        assert_eq!(pong, frame(4, 4, b"null"));
    }

    #[test]
    fn encode_frame_keeps_unicode_as_utf8() {
        let payload = json!({"details": "♪ გამარჯობა 🎵"});
        let bytes = encode_frame(OP_FRAME, &payload);
        let len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(len, bytes.len() - 8);
        let text = std::str::from_utf8(&bytes[8..]).unwrap();
        assert!(text.contains("♪ გამარჯობა 🎵"));
    }

    #[test]
    fn decode_round_trip() {
        for op in [OP_HANDSHAKE, OP_FRAME, OP_CLOSE, OP_PING, OP_PONG, 99] {
            let payload = json!({"cmd": "SET_ACTIVITY", "nonce": "1-2", "n": op});
            let bytes = encode_frame(op, &payload);
            let (got_op, got, used) = decode_frame(&bytes).unwrap().unwrap();
            assert_eq!(got_op, op);
            assert_eq!(got, payload);
            assert_eq!(used, bytes.len());
        }
    }

    #[test]
    fn decode_uses_only_the_first_frame() {
        let mut bytes = encode_frame(OP_PING, &json!({"a": 1}));
        let first_len = bytes.len();
        bytes.extend(encode_frame(OP_FRAME, &json!({"b": 2})));
        let (op, payload, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!((op, used), (OP_PING, first_len));
        assert_eq!(payload, json!({"a": 1}));
        let (op, payload, used) = decode_frame(&bytes[used..]).unwrap().unwrap();
        assert_eq!(op, OP_FRAME);
        assert_eq!(payload, json!({"b": 2}));
        assert_eq!(used, bytes.len() - first_len);
    }

    #[test]
    fn decode_partial_buffers_are_none() {
        let bytes = encode_frame(OP_FRAME, &json!({"cmd": "DISPATCH", "evt": "READY"}));
        for cut in 0..bytes.len() {
            assert!(
                decode_frame(&bytes[..cut]).unwrap().is_none(),
                "cut at {cut} should be incomplete"
            );
        }
        assert!(decode_frame(&bytes).unwrap().is_some());
    }

    #[test]
    fn decode_rejects_oversize_payload_before_it_arrives() {
        let header = frame(OP_FRAME, (MAX_PAYLOAD_BYTES + 1) as u32, b"");
        let err = decode_frame(&header).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err}");
        assert!(decode_frame(&frame(OP_FRAME, u32::MAX, b"{}")).is_err());
    }

    #[test]
    fn decode_accepts_exactly_64_kib() {
        let body = format!("\"{}\"", "a".repeat(MAX_PAYLOAD_BYTES - 2));
        assert_eq!(body.len(), MAX_PAYLOAD_BYTES);
        let bytes = frame(OP_FRAME, body.len() as u32, body.as_bytes());
        let (_, payload, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(payload.as_str().map(str::len), Some(MAX_PAYLOAD_BYTES - 2));
    }

    #[test]
    fn decode_rejects_invalid_json_and_utf8() {
        assert!(decode_frame(&frame(OP_FRAME, 5, b"{nope")).is_err());
        assert!(decode_frame(&frame(OP_FRAME, 0, b"")).is_err());
        assert!(decode_frame(&frame(OP_FRAME, 3, &[b'"', 0xff, b'"'])).is_err());
        let err = decode_frame(&frame(OP_FRAME, 2, b"{]")).unwrap_err();
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[test]
    fn decode_any_json_value() {
        let (op, payload, used) = decode_frame(&frame(7, 4, b"null")).unwrap().unwrap();
        assert_eq!((op, payload, used), (7, Value::Null, 12));
    }

    // ---- fit_field ----

    #[test]
    fn fit_field_pads_short_strings() {
        assert_eq!(fit_field("♪"), "♪\u{2800}");
        assert_eq!(fit_field(""), "\u{2800}\u{2800}");
        assert_eq!(fit_field("   "), "\u{2800}\u{2800}");
        assert_eq!(fit_field(" a "), "a\u{2800}");
        assert_eq!(fit_field("\t\n"), "\u{2800}\u{2800}");
        for s in ["♪", "", " ", "x", "🎵"] {
            assert!(fit_field(s).chars().count() >= MIN_FIELD_CHARS, "{s:?}");
        }
    }

    #[test]
    fn fit_field_keeps_fitting_strings() {
        assert_eq!(fit_field("ab"), "ab");
        assert_eq!(fit_field("  hello world  "), "hello world");
        assert_eq!(fit_field("გამარჯობა"), "გამარჯობა");
        let exact = "x".repeat(MAX_FIELD_CHARS);
        assert_eq!(fit_field(&exact), exact);
    }

    #[test]
    fn fit_field_truncates_long_strings() {
        let long = "a".repeat(200);
        let fitted = fit_field(&long);
        assert_eq!(fitted.chars().count(), MAX_FIELD_CHARS);
        assert!(fitted.ends_with('…'));
        assert_eq!(fitted, format!("{}…", "a".repeat(127)));

        let one_over = "b".repeat(MAX_FIELD_CHARS + 1);
        assert_eq!(fit_field(&one_over).chars().count(), MAX_FIELD_CHARS);

        let words = "never gonna give you up ".repeat(20);
        let fitted = fit_field(&words);
        assert!(fitted.chars().count() <= MAX_FIELD_CHARS);
        assert!(fitted.ends_with('…'));
        assert!(fitted.starts_with("never gonna give you up"));
    }

    #[test]
    fn fit_field_respects_utf16_length() {
        let emojis = "🎵".repeat(200);
        let fitted = fit_field(&emojis);
        assert!(fitted.encode_utf16().count() <= MAX_FIELD_CHARS, "{fitted}");
        assert!(fitted.ends_with('…'));
        assert_eq!(fitted, format!("{}…", "🎵".repeat(63)));

        // 128 characters but 129 UTF-16 units.
        let mixed = format!("🎵{}", "a".repeat(127));
        assert_eq!(mixed.chars().count(), 128);
        let fitted = fit_field(&mixed);
        assert!(fitted.encode_utf16().count() <= MAX_FIELD_CHARS);
        assert!(fitted.ends_with('…'));

        // Exactly 128 UTF-16 units (127 characters) is kept unchanged.
        let exact_units = format!("{}🎵", "a".repeat(126));
        assert_eq!(exact_units.encode_utf16().count(), MAX_FIELD_CHARS);
        assert_eq!(fit_field(&exact_units), exact_units);
        // One emoji alone is 2 UTF-16 units but 1 character, so it is padded.
        assert_eq!(fit_field("🎵"), "🎵\u{2800}");

        let georgian = "ქ".repeat(300);
        let fitted = fit_field(&georgian);
        assert_eq!(fitted.chars().count(), MAX_FIELD_CHARS);
    }

    #[test]
    fn fit_field_huge_input() {
        let huge = "word ".repeat(100_000);
        let fitted = fit_field(&huge);
        assert!(fitted.chars().count() <= MAX_FIELD_CHARS);
        assert!(fitted.chars().count() >= MIN_FIELD_CHARS);
    }

    // ---- build_activity ----

    #[test]
    fn activity_for_a_lyric_line() {
        let activity = build_activity(&status("Never gonna give you up"), true);
        assert_eq!(
            activity,
            json!({
                "type": 2,
                "status_display_type": 2,
                "details": "Never gonna give you up",
                "state": "Never Gonna Give You Up · Rick Astley",
                "timestamps": { "start": 1_700_000_000_000u64, "end": 1_700_000_213_000u64 },
            })
        );
        assert!(activity.get("assets").is_none());
    }

    #[test]
    fn activity_marks_estimated_timing() {
        let mut s = status("a line");
        s.estimated = true;
        let activity = build_activity(&s, true);
        assert_eq!(
            activity["state"],
            "Never Gonna Give You Up · Rick Astley (estimated timing)"
        );
        assert_eq!(activity["details"], "a line");
    }

    #[test]
    fn activity_without_artist() {
        let mut s = status("x line");
        s.track.artist = String::new();
        assert_eq!(build_activity(&s, true)["state"], "Never Gonna Give You Up");
        s.track.artist = "   ".into();
        assert_eq!(build_activity(&s, true)["state"], "Never Gonna Give You Up");
        s.estimated = true;
        assert_eq!(
            build_activity(&s, true)["state"],
            "Never Gonna Give You Up (estimated timing)"
        );
    }

    #[test]
    fn activity_without_title_or_artist() {
        let mut s = status("x line");
        s.track.title = String::new();
        assert_eq!(build_activity(&s, true)["state"], "Rick Astley");
        s.track.artist = String::new();
        assert_eq!(build_activity(&s, true)["state"], "\u{2800}\u{2800}");
    }

    #[test]
    fn activity_with_unknown_duration_has_only_start() {
        let mut s = status("x line");
        s.track.duration_ms = None;
        let activity = build_activity(&s, true);
        assert_eq!(
            activity["timestamps"],
            json!({"start": 1_700_000_000_000u64})
        );
        s.track.duration_ms = Some(0);
        let activity = build_activity(&s, true);
        assert_eq!(
            activity["timestamps"],
            json!({"start": 1_700_000_000_000u64})
        );
    }

    #[test]
    fn activity_without_progress() {
        let activity = build_activity(&status("x line"), false);
        assert!(activity.get("timestamps").is_none());
        let mut s = status("x line");
        s.started_at_unix_ms = None;
        assert!(build_activity(&s, true).get("timestamps").is_none());
    }

    #[test]
    fn activity_timestamps_never_overflow() {
        let mut s = status("x line");
        s.started_at_unix_ms = Some(u64::MAX - 10);
        s.track.duration_ms = Some(u64::MAX);
        let activity = build_activity(&s, true);
        assert_eq!(activity["timestamps"]["end"], json!(u64::MAX));
        assert_eq!(activity["timestamps"]["start"], json!(u64::MAX - 10));
    }

    #[test]
    fn activity_fields_are_fitted() {
        let mut s = status(&"lyric ".repeat(100));
        s.track.title = "T".repeat(300);
        let activity = build_activity(&s, true);
        let details = activity["details"].as_str().unwrap();
        let state = activity["state"].as_str().unwrap();
        assert!(details.chars().count() <= MAX_FIELD_CHARS);
        assert!(state.chars().count() <= MAX_FIELD_CHARS);
        assert!(details.ends_with('…') && state.ends_with('…'));

        let mut s = status("♪");
        s.kind = StatusKind::Instrumental;
        assert_eq!(build_activity(&s, true)["details"], "♪\u{2800}");
    }

    // ---- target basics ----

    #[test]
    fn defaults_and_test_overrides() {
        let target = DiscordRpcTarget::new("1", Duration::ZERO, true);
        assert_eq!(target.reconnect_every, RECONNECT_EVERY);
        assert_eq!(target.request_timeout, Duration::from_secs(5));
        assert!(!target.is_connected());
        let target = target
            .with_reconnect_every(Duration::from_millis(7))
            .with_request_timeout(Duration::from_millis(9));
        assert_eq!(target.reconnect_every, Duration::from_millis(7));
        assert_eq!(target.request_timeout, Duration::from_millis(9));
    }

    #[test]
    fn waiting_error_repeats_the_last_failure() {
        let mut target = DiscordRpcTarget::new("1", Duration::ZERO, true);
        match target.waiting_error(Duration::from_millis(1_001)) {
            TargetError::Unavailable(m) => assert!(m.ends_with("trying again in 2 s"), "{m}"),
            other => panic!("{other:?}"),
        }
        target.last_failure = Some(Failure::Unavailable("Discord is not running".into()));
        match target.waiting_error(Duration::ZERO) {
            TargetError::Unavailable(m) => {
                assert_eq!(m, "Discord is not running; trying again in 0 s")
            }
            other => panic!("{other:?}"),
        }
        target.last_failure = Some(Failure::Unauthorized("bad id".into()));
        match target.waiting_error(Duration::from_secs(3)) {
            TargetError::Unauthorized(m) => assert_eq!(m, "bad id"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn name_interval_and_nonces() {
        let mut target = DiscordRpcTarget::new("123", Duration::from_millis(2_000), true);
        assert_eq!(target.name(), "discord");
        assert_eq!(target.min_interval(), Duration::from_secs(2));
        let pid = std::process::id();
        assert_eq!(target.next_nonce(), format!("{pid}-0"));
        assert_eq!(target.next_nonce(), format!("{pid}-1"));
        target.nonce_counter = u64::MAX;
        assert_eq!(target.next_nonce(), format!("{pid}-{}", u64::MAX));
        assert_eq!(target.next_nonce(), format!("{pid}-0"));
    }

    #[tokio::test]
    async fn empty_client_id_is_unauthorized() {
        for id in ["", "   "] {
            let mut target = DiscordRpcTarget::new(id, Duration::ZERO, true);
            let err = target.set(&status("x line")).await.unwrap_err();
            match err {
                TargetError::Unauthorized(m) => {
                    assert_eq!(m, "Discord client id is not configured")
                }
                other => panic!("expected Unauthorized, got {other:?}"),
            }
            assert!(!target.is_connected());
            assert!(target.last_attempt.is_none());
            target.clear().await.unwrap();
        }
    }

    #[tokio::test]
    async fn clear_while_never_connected_is_ok() {
        let mut target = DiscordRpcTarget::new("123", Duration::ZERO, true);
        target.clear().await.unwrap();
        assert!(target.last_attempt.is_none());
        assert!(!target.is_connected());
    }

    #[test]
    fn close_details_reads_codes() {
        match close_details(Some(&json!({"code": 4000, "message": "Invalid Client ID"}))) {
            IpcError::Closed { code, message } => {
                assert_eq!(code, Some(4000));
                assert_eq!(message, "Invalid Client ID");
            }
            other => panic!("{other:?}"),
        }
        match close_details(Some(&json!({"code": "4004"}))) {
            IpcError::Closed { code, message } => {
                assert_eq!(code, Some(4004));
                assert_eq!(message, "");
            }
            other => panic!("{other:?}"),
        }
        match close_details(None) {
            IpcError::Closed { code, .. } => assert_eq!(code, None),
            other => panic!("{other:?}"),
        }
        let shown = close_details(Some(&json!({"code": 4004}))).to_string();
        assert!(
            shown.contains("4004") && shown.contains("no reason given"),
            "{shown}"
        );
        // Codes that are neither numbers nor numeric strings are ignored.
        match close_details(Some(&json!({"code": [1], "message": 5}))) {
            IpcError::Closed { code, message } => {
                assert_eq!(code, None);
                assert_eq!(message, "");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            close_details(Some(&json!({"message": "bye"}))).to_string(),
            "Discord closed the connection: bye"
        );
    }

    #[test]
    fn refusal_details_keep_the_rpc_error_code() {
        let error = refusal_details(Some(&json!({"code": 4000, "message": "Invalid Payload"})));
        match &error {
            IpcError::Refused { code, message } => {
                assert_eq!(*code, Some(4000));
                assert_eq!(message, "Invalid Payload");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            error.to_string(),
            "Discord refused the connection: Invalid Payload (code 4000)"
        );
        assert_eq!(
            refusal_details(None).to_string(),
            "Discord refused the connection: no reason given"
        );
    }

    // ---- Unix socket discovery ----

    #[cfg(unix)]
    #[test]
    fn unix_candidates_cover_every_folder_and_slot() {
        use std::ffi::OsString;
        use std::path::PathBuf;

        let env = |key: &str| -> Option<OsString> {
            match key {
                "XDG_RUNTIME_DIR" => Some("/run/user/1000".into()),
                "TMPDIR" => Some("/tmp/".into()),
                "TMP" => Some("".into()),
                _ => None,
            }
        };
        let paths = unix_socket_candidates(env);
        // Two distinct bases (/run/user/1000 and /tmp), five folders each, ten slots.
        assert_eq!(paths.len(), 2 * 5 * 10);
        assert_eq!(paths[0], PathBuf::from("/run/user/1000/discord-ipc-0"));
        assert_eq!(
            paths[1],
            PathBuf::from("/run/user/1000/app/com.discordapp.Discord/discord-ipc-0")
        );
        assert!(paths.contains(&PathBuf::from(
            "/run/user/1000/.flatpak/com.discordapp.Discord/xdg-run/discord-ipc-3"
        )));
        assert!(paths.contains(&PathBuf::from("/tmp/snap.discord/discord-ipc-9")));
        assert!(paths.contains(&PathBuf::from("/tmp/snap.discord-canary/discord-ipc-0")));
        assert_eq!(paths[5], PathBuf::from("/tmp/discord-ipc-0"));
        assert_eq!(paths[10], PathBuf::from("/run/user/1000/discord-ipc-1"));
        assert_eq!(
            paths.last().unwrap(),
            &PathBuf::from("/tmp/snap.discord-canary/discord-ipc-9")
        );

        let only_tmp = unix_socket_candidates(|_| None);
        assert_eq!(only_tmp.len(), 5 * 10);
        assert_eq!(only_tmp[0], PathBuf::from("/tmp/discord-ipc-0"));
    }

    // ---- fake Discord IPC server (Unix) ----

    #[cfg(unix)]
    mod fake {
        use super::super::*;
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        use tokio::net::{UnixListener, UnixStream};
        use tokio::task::JoinHandle;

        /// How the fake Discord behaves.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Script {
            /// READY, then answers every request.
            Normal,
            /// Pings during the handshake and expects a pong before READY.
            PingInHandshake,
            /// Pings before each reply and expects a pong first.
            PingFirst,
            /// Answers requests with `evt: ERROR`.
            ErrorReply,
            /// Closes the handshake with this code.
            CloseHandshake(i64),
            /// Answers the handshake with an `ERROR` event carrying this code.
            ErrorInHandshake(i64),
            /// READY, then answers each request with a close frame with this code.
            CloseOnRequest(i64),
            /// READY, then answers each request with half a frame and hangs up.
            PartialReplyThenEof,
            /// Answers requests with `evt: ERROR` and no `data.message`.
            ErrorReplyWithoutMessage,
            /// Answers the handshake with an oversize frame header.
            GarbageHandshake,
            /// Drops the connection when a request arrives.
            DropOnRequest,
            /// Sends unrelated frames, then the reply one byte at a time.
            SplitFrames,
            /// Never answers requests.
            NeverReply,
            /// Never answers the handshake.
            SilentHandshake,
            /// Answers requests with an oversize frame header.
            GarbageReply,
        }

        pub struct FakeDiscord {
            _dir: Option<tempfile::TempDir>,
            pub path: PathBuf,
            frames: Arc<Mutex<Vec<(u32, Value)>>>,
            accepted: Arc<AtomicUsize>,
            tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
        }

        pub fn socket_dir() -> tempfile::TempDir {
            // Unix socket paths are limited to ~100 bytes, so stay in /tmp.
            tempfile::Builder::new()
                .prefix("lyrix-ipc")
                .tempdir_in("/tmp")
                .expect("tempdir")
        }

        impl FakeDiscord {
            pub fn start(script: Script) -> Self {
                let dir = socket_dir();
                let path = dir.path().join("discord-ipc-0");
                let mut fake = Self::start_at(&path, script);
                fake._dir = Some(dir);
                fake
            }

            pub fn start_at(path: &Path, script: Script) -> Self {
                let _ = std::fs::remove_file(path);
                let listener = UnixListener::bind(path).expect("bind fake Discord socket");
                let frames = Arc::new(Mutex::new(Vec::new()));
                let accepted = Arc::new(AtomicUsize::new(0));
                let tasks: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
                let accept_task = {
                    let frames = frames.clone();
                    let accepted = accepted.clone();
                    let tasks = tasks.clone();
                    tokio::spawn(async move {
                        while let Ok((stream, _)) = listener.accept().await {
                            accepted.fetch_add(1, Ordering::SeqCst);
                            let handle = tokio::spawn(serve(stream, script, frames.clone()));
                            tasks.lock().unwrap().push(handle);
                        }
                    })
                };
                tasks.lock().unwrap().push(accept_task);
                Self {
                    _dir: None,
                    path: path.to_path_buf(),
                    frames,
                    accepted,
                    tasks,
                }
            }

            /// Stops listening and closes every connection.
            pub async fn stop(&self) {
                let handles: Vec<_> = self.tasks.lock().unwrap().drain(..).collect();
                for handle in handles {
                    handle.abort();
                    let _ = handle.await;
                }
                let _ = std::fs::remove_file(&self.path);
            }

            pub fn accepted(&self) -> usize {
                self.accepted.load(Ordering::SeqCst)
            }

            pub fn frames(&self) -> Vec<(u32, Value)> {
                self.frames.lock().unwrap().clone()
            }

            /// The `SET_ACTIVITY` requests received.
            pub fn requests(&self) -> Vec<Value> {
                self.frames()
                    .into_iter()
                    .filter(|(op, v)| *op == OP_FRAME && v["cmd"] == "SET_ACTIVITY")
                    .map(|(_, v)| v)
                    .collect()
            }
        }

        async fn next_frame(stream: &mut UnixStream, buf: &mut Vec<u8>) -> Option<(u32, Value)> {
            loop {
                if let Some((op, value, used)) = decode_frame(buf).ok()? {
                    buf.drain(..used);
                    return Some((op, value));
                }
                if stream.read_buf(buf).await.ok()? == 0 {
                    return None;
                }
            }
        }

        async fn write(stream: &mut UnixStream, op: u32, payload: Value) -> Option<()> {
            stream.write_all(&encode_frame(op, &payload)).await.ok()?;
            stream.flush().await.ok()
        }

        /// Reads the next frame, records it, and checks it is a pong for `ping`.
        async fn expect_pong(
            stream: &mut UnixStream,
            buf: &mut Vec<u8>,
            frames: &Mutex<Vec<(u32, Value)>>,
            ping: &Value,
        ) -> Option<()> {
            let (op, value) = next_frame(stream, buf).await?;
            frames.lock().unwrap().push((op, value.clone()));
            (op == OP_PONG && &value == ping).then_some(())
        }

        fn ready() -> Value {
            json!({
                "cmd": "DISPATCH",
                "evt": "READY",
                "data": { "v": 1, "user": { "id": "1", "username": "tester" } },
                "nonce": null,
            })
        }

        async fn serve(
            mut stream: UnixStream,
            script: Script,
            frames: Arc<Mutex<Vec<(u32, Value)>>>,
        ) {
            let mut buf = Vec::new();
            let _ = serve_inner(&mut stream, &mut buf, script, &frames).await;
        }

        async fn serve_inner(
            stream: &mut UnixStream,
            buf: &mut Vec<u8>,
            script: Script,
            frames: &Mutex<Vec<(u32, Value)>>,
        ) -> Option<()> {
            loop {
                let (op, value) = next_frame(stream, buf).await?;
                frames.lock().unwrap().push((op, value.clone()));
                match op {
                    OP_HANDSHAKE => match script {
                        Script::CloseHandshake(code) => {
                            let message = if code == 4000 {
                                "Invalid Client ID"
                            } else {
                                "Invalid version"
                            };
                            write(stream, OP_CLOSE, json!({"code": code, "message": message}))
                                .await?;
                            return Some(());
                        }
                        Script::ErrorInHandshake(code) => {
                            let message = if code == 4007 {
                                "Invalid Client ID"
                            } else {
                                "Invalid Payload"
                            };
                            let error = json!({
                                "cmd": "DISPATCH",
                                "evt": "ERROR",
                                "data": {"code": code, "message": message},
                            });
                            write(stream, OP_FRAME, error).await?;
                        }
                        Script::GarbageHandshake => {
                            let header = [1u8, 0, 0, 0, 0, 0, 0x10, 0];
                            stream.write_all(&header).await.ok()?;
                        }
                        Script::SilentHandshake => {}
                        Script::PingInHandshake => {
                            let ping = json!({"hs": "ping"});
                            write(stream, OP_PING, ping.clone()).await?;
                            expect_pong(stream, buf, frames, &ping).await?;
                            write(stream, OP_FRAME, ready()).await?;
                        }
                        _ => write(stream, OP_FRAME, ready()).await?,
                    },
                    OP_FRAME => {
                        let nonce = value["nonce"].clone();
                        let reply = json!({
                            "cmd": "SET_ACTIVITY",
                            "data": value["args"]["activity"].clone(),
                            "evt": null,
                            "nonce": nonce,
                        });
                        match script {
                            Script::PingFirst => {
                                let ping = json!({"ping": 42});
                                write(stream, OP_PING, ping.clone()).await?;
                                expect_pong(stream, buf, frames, &ping).await?;
                                write(stream, OP_FRAME, reply).await?;
                            }
                            Script::ErrorReply => {
                                let error = json!({
                                    "cmd": "SET_ACTIVITY",
                                    "evt": "ERROR",
                                    "nonce": nonce,
                                    "data": {
                                        "code": 4000,
                                        "message": "child \"activity\" fails because [child \"details\" fails]",
                                    },
                                });
                                write(stream, OP_FRAME, error).await?;
                            }
                            Script::DropOnRequest => return Some(()),
                            Script::CloseOnRequest(code) => {
                                let close = json!({"code": code, "message": "Closing"});
                                write(stream, OP_CLOSE, close).await?;
                                return Some(());
                            }
                            Script::PartialReplyThenEof => {
                                let bytes = encode_frame(OP_FRAME, &reply);
                                let half = bytes.get(..bytes.len() / 2).unwrap_or_default();
                                stream.write_all(half).await.ok()?;
                                stream.flush().await.ok()?;
                                return Some(());
                            }
                            Script::ErrorReplyWithoutMessage => {
                                let error = json!({
                                    "cmd": "SET_ACTIVITY",
                                    "evt": "ERROR",
                                    "nonce": nonce,
                                    "data": {"code": 4000},
                                });
                                write(stream, OP_FRAME, error).await?;
                            }
                            Script::NeverReply => {}
                            Script::GarbageReply => {
                                let header = [1u8, 0, 0, 0, 0, 0, 0x10, 0];
                                stream.write_all(&header).await.ok()?;
                            }
                            Script::SplitFrames => {
                                let mut bytes = encode_frame(
                                    OP_FRAME,
                                    &json!({"cmd": "DISPATCH", "evt": "ACTIVITY_JOIN", "nonce": null}),
                                );
                                bytes.extend(encode_frame(
                                    OP_FRAME,
                                    &json!({"cmd": "SET_ACTIVITY", "evt": null, "nonce": "someone-else"}),
                                ));
                                bytes.extend(encode_frame(OP_PONG, &json!({"stray": true})));
                                bytes.extend(encode_frame(OP_FRAME, &reply));
                                for byte in bytes {
                                    stream.write_all(&[byte]).await.ok()?;
                                    stream.flush().await.ok()?;
                                    tokio::task::yield_now().await;
                                }
                            }
                            _ => write(stream, OP_FRAME, reply).await?,
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[cfg(unix)]
    mod ipc {
        use super::fake::{socket_dir, FakeDiscord, Script};
        use super::*;

        fn target_for(fake: &FakeDiscord) -> DiscordRpcTarget {
            DiscordRpcTarget::new("1234567890", Duration::from_millis(2_000), true)
                .with_socket_path(fake.path.clone())
        }

        #[tokio::test]
        async fn handshake_then_set_activity() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&fake);
            target
                .set(&status("Never gonna give you up"))
                .await
                .unwrap();
            assert!(target.is_connected());

            let frames = fake.frames();
            assert_eq!(frames.len(), 2, "{frames:?}");
            assert_eq!(frames[0].0, OP_HANDSHAKE);
            assert_eq!(frames[0].1, json!({"v": 1, "client_id": "1234567890"}));
            assert_eq!(frames[1].0, OP_FRAME);
            let request = &frames[1].1;
            assert_eq!(request["cmd"], "SET_ACTIVITY");
            assert_eq!(request["args"]["pid"], json!(std::process::id()));
            let nonce = request["nonce"].as_str().unwrap();
            assert!(
                nonce.starts_with(&format!("{}-", std::process::id())),
                "{nonce}"
            );

            let activity = &request["args"]["activity"];
            assert_eq!(activity["type"], 2);
            assert_eq!(activity["status_display_type"], 2);
            assert_eq!(activity["details"], "Never gonna give you up");
            assert_eq!(activity["state"], "Never Gonna Give You Up · Rick Astley");
            assert_eq!(activity["timestamps"]["start"], json!(1_700_000_000_000u64));
            assert_eq!(activity["timestamps"]["end"], json!(1_700_000_213_000u64));
        }

        #[tokio::test]
        async fn client_id_is_trimmed() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = DiscordRpcTarget::new("  42  ", Duration::ZERO, true)
                .with_socket_path(fake.path.clone());
            target.set(&status("x line")).await.unwrap();
            assert_eq!(fake.frames()[0].1["client_id"], "42");
        }

        #[tokio::test]
        async fn show_progress_false_sends_no_timestamps() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = DiscordRpcTarget::new("1", Duration::ZERO, false)
                .with_socket_path(fake.path.clone());
            target.set(&status("x line")).await.unwrap();
            let requests = fake.requests();
            assert!(requests[0]["args"]["activity"].get("timestamps").is_none());
        }

        #[tokio::test]
        async fn updates_reuse_the_connection_with_new_nonces() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&fake);
            target.set(&status("line one")).await.unwrap();
            target.set(&status("line two")).await.unwrap();
            target.set(&status("line three")).await.unwrap();
            assert_eq!(fake.accepted(), 1);
            let requests = fake.requests();
            assert_eq!(requests.len(), 3);
            let nonces: Vec<_> = requests.iter().map(|r| r["nonce"].clone()).collect();
            assert_ne!(nonces[0], nonces[1]);
            assert_ne!(nonces[1], nonces[2]);
            assert_eq!(requests[2]["args"]["activity"]["details"], "line three");
        }

        #[tokio::test]
        async fn clear_sends_null_activity() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&fake);
            target.set(&status("x line")).await.unwrap();
            target.clear().await.unwrap();
            let requests = fake.requests();
            assert_eq!(requests.len(), 2);
            assert!(requests[1]["args"].get("activity").is_some());
            assert_eq!(requests[1]["args"]["activity"], Value::Null);
            assert_eq!(requests[1]["args"]["pid"], json!(std::process::id()));
            assert!(target.is_connected());
        }

        #[tokio::test]
        async fn clear_does_not_connect() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&fake);
            target.clear().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(fake.accepted(), 0);
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn ping_is_answered_with_pong() {
            let fake = FakeDiscord::start(Script::PingFirst);
            let mut target = target_for(&fake);
            target.set(&status("x line")).await.unwrap();
            let frames = fake.frames();
            assert!(
                frames.contains(&(OP_PONG, json!({"ping": 42}))),
                "{frames:?}"
            );
        }

        #[tokio::test]
        async fn ping_during_handshake_is_answered() {
            let fake = FakeDiscord::start(Script::PingInHandshake);
            let mut target = target_for(&fake);
            target.set(&status("x line")).await.unwrap();
            assert!(fake.frames().contains(&(OP_PONG, json!({"hs": "ping"}))));
        }

        #[tokio::test]
        async fn error_reply_is_other_and_keeps_the_connection() {
            let fake = FakeDiscord::start(Script::ErrorReply);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Other(e) => {
                    let text = e.to_string();
                    assert!(text.contains("child \"activity\" fails"), "{text}");
                }
                other => panic!("expected Other, got {other:?}"),
            }
            assert!(target.is_connected());
            assert!(target.set(&status("x line")).await.is_err());
            assert_eq!(fake.accepted(), 1);
        }

        #[tokio::test]
        async fn close_4000_is_unauthorized() {
            let fake = FakeDiscord::start(Script::CloseHandshake(4000));
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unauthorized(m) => assert!(m.contains("Invalid Client ID"), "{m}"),
                other => panic!("expected Unauthorized, got {other:?}"),
            }
            assert!(!target.is_connected());
            // The next call does not reconnect but reports the same problem.
            let err = target.set(&status("x line")).await.unwrap_err();
            assert!(matches!(err, TargetError::Unauthorized(_)), "{err:?}");
            assert_eq!(fake.accepted(), 1);
        }

        /// `ERROR` events use the RPC error codes, where 4007 is Invalid Client ID.
        #[tokio::test]
        async fn error_event_with_invalid_client_id_in_handshake_is_unauthorized() {
            let fake = FakeDiscord::start(Script::ErrorInHandshake(4007));
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unauthorized(m) => assert!(m.contains("Invalid Client ID"), "{m}"),
                other => panic!("expected Unauthorized, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        /// In an `ERROR` event 4000 is Invalid Payload (only a close frame's 4000
        /// means an invalid client id), so it must not switch the target off.
        #[tokio::test]
        async fn error_event_with_invalid_payload_in_handshake_is_unavailable() {
            let fake = FakeDiscord::start(Script::ErrorInHandshake(4000));
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => {
                    assert!(m.contains("Invalid Payload") && m.contains("4000"), "{m}")
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn close_frame_during_a_request_drops_the_connection() {
            // 4000 after the handshake is not an invalid client id: just a lost connection.
            for code in [4000, 1000] {
                let fake = FakeDiscord::start(Script::CloseOnRequest(code));
                let mut target = target_for(&fake);
                let err = target.set(&status("x line")).await.unwrap_err();
                match &err {
                    TargetError::Unavailable(m) => {
                        assert!(
                            m.contains("Closing") && m.contains(&code.to_string()),
                            "{m}"
                        )
                    }
                    other => panic!("expected Unavailable, got {other:?}"),
                }
                assert!(!target.is_connected());
                assert_eq!(fake.accepted(), 1);
            }
        }

        #[tokio::test]
        async fn half_a_reply_then_eof_is_unavailable() {
            let fake = FakeDiscord::start(Script::PartialReplyThenEof);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("closed"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn error_reply_without_message_is_other() {
            let fake = FakeDiscord::start(Script::ErrorReplyWithoutMessage);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Other(e) => {
                    let text = e.to_string();
                    assert!(
                        text.contains("rejected") && text.contains("no reason given"),
                        "{text}"
                    );
                }
                other => panic!("expected Other, got {other:?}"),
            }
            assert!(target.is_connected());
        }

        #[tokio::test]
        async fn garbage_during_handshake_is_unavailable() {
            let fake = FakeDiscord::start(Script::GarbageHandshake);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("invalid"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn clear_on_a_lost_connection_is_unavailable_then_ok() {
            let first = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&first);
            target.set(&status("x line")).await.unwrap();
            first.stop().await;
            let second = FakeDiscord::start_at(&first.path, Script::Normal);
            // The old connection is gone: clearing on it fails and drops it.
            let err = target.clear().await.unwrap_err();
            assert!(matches!(err, TargetError::Unavailable(_)), "{err:?}");
            assert!(!target.is_connected());
            // With no connection, clearing is a no-op and does not reconnect.
            target.clear().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(second.accepted(), 0);
        }

        #[tokio::test]
        async fn clear_error_reply_is_other() {
            let fake = FakeDiscord::start(Script::ErrorReply);
            let mut target = target_for(&fake);
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Other(_))
            ));
            let err = target.clear().await.unwrap_err();
            assert!(matches!(err, TargetError::Other(_)), "{err:?}");
            assert!(target.is_connected());
            let requests = fake.requests();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1]["args"]["activity"], Value::Null);
        }

        #[tokio::test]
        async fn unauthorized_is_retried_after_the_interval() {
            let fake = FakeDiscord::start(Script::CloseHandshake(4000));
            let mut target = target_for(&fake);
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Unauthorized(_))
            ));
            // Pretend the reconnect interval has passed.
            target.last_attempt = target
                .last_attempt
                .and_then(|t| t.checked_sub(RECONNECT_EVERY));
            assert!(target.last_attempt.is_some());
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Unauthorized(_))
            ));
            assert_eq!(fake.accepted(), 2);
        }

        #[tokio::test]
        async fn other_close_codes_are_unavailable() {
            let fake = FakeDiscord::start(Script::CloseHandshake(4004));
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => {
                    assert!(m.contains("Invalid version") && m.contains("4004"), "{m}")
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn missing_server_is_unavailable_and_not_retried_immediately() {
            let dir = socket_dir();
            let path = dir.path().join("discord-ipc-0");
            let mut target =
                DiscordRpcTarget::new("1", Duration::ZERO, true).with_socket_path(path.clone());
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("not running"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }

            // Discord starts, but the target waits RECONNECT_EVERY before looking again.
            let fake = FakeDiscord::start_at(&path, Script::Normal);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("trying again in"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert_eq!(fake.accepted(), 0);
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn reconnects_after_the_interval() {
            let dir = socket_dir();
            let path = dir.path().join("discord-ipc-0");
            let mut target = DiscordRpcTarget::new("1", Duration::ZERO, true)
                .with_socket_path(path.clone())
                .with_reconnect_every(Duration::from_millis(300));
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Unavailable(_))
            ));
            let fake = FakeDiscord::start_at(&path, Script::Normal);
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Unavailable(_))
            ));
            assert_eq!(fake.accepted(), 0);

            tokio::time::sleep(Duration::from_millis(350)).await;
            target.set(&status("x line")).await.unwrap();
            assert_eq!(fake.accepted(), 1);
            assert_eq!(fake.requests().len(), 1);
        }

        #[tokio::test]
        async fn discovery_skips_missing_and_stale_sockets() {
            let dir = socket_dir();
            let missing = dir.path().join("discord-ipc-0");
            let stale = dir.path().join("discord-ipc-1");
            drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
            let plain_file = dir.path().join("discord-ipc-2");
            std::fs::write(&plain_file, b"not a socket").unwrap();
            let live = FakeDiscord::start_at(&dir.path().join("discord-ipc-3"), Script::Normal);
            let unused = FakeDiscord::start_at(&dir.path().join("discord-ipc-4"), Script::Normal);

            let candidates = vec![
                missing.clone(),
                stale.clone(),
                plain_file.clone(),
                live.path.clone(),
                unused.path.clone(),
            ];
            let stream = connect_first_unix(&candidates).await.unwrap();
            let mut conn = Connection::new(stream);
            conn.handshake("1").await.unwrap();
            assert_eq!(live.accepted(), 1);
            assert_eq!(unused.accepted(), 0);

            // Only missing paths: Discord is not running.
            let err = connect_first_unix(&[missing.clone(), dir.path().join("nope/discord-ipc-0")])
                .await
                .err()
                .unwrap();
            assert!(err.contains("no Discord IPC socket found"), "{err}");

            // A stale socket is named in the error.
            let err = connect_first_unix(&[missing, stale.clone()])
                .await
                .err()
                .unwrap();
            assert!(
                err.contains("not running") && err.contains(&stale.display().to_string()),
                "{err}"
            );

            assert!(connect_first_unix(&[]).await.is_err());
        }

        #[tokio::test]
        async fn stale_socket_file_is_unavailable() {
            let fake = FakeDiscord::start(Script::Normal);
            let path = fake.path.clone();
            fake.stop().await;
            // Leave a socket file behind with nobody listening.
            let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
            drop(listener);
            let mut target =
                DiscordRpcTarget::new("1", Duration::ZERO, true).with_socket_path(path);
            let err = target.set(&status("x line")).await.unwrap_err();
            assert!(matches!(err, TargetError::Unavailable(_)), "{err:?}");
        }

        #[tokio::test]
        async fn server_gone_is_unavailable_then_reconnects() {
            let fake = FakeDiscord::start(Script::Normal);
            let path = fake.path.clone();
            let mut target = target_for(&fake).with_reconnect_every(Duration::ZERO);
            target.set(&status("x line")).await.unwrap();

            fake.stop().await;
            let err = target.set(&status("x line")).await.unwrap_err();
            assert!(matches!(err, TargetError::Unavailable(_)), "{err:?}");
            assert!(!target.is_connected());
            // Clearing with no connection is fine: Discord already dropped the activity.
            target.clear().await.unwrap();

            let restarted = FakeDiscord::start_at(&path, Script::Normal);
            target.set(&status("back again")).await.unwrap();
            assert_eq!(restarted.accepted(), 1);
            assert_eq!(
                restarted.requests()[0]["args"]["activity"]["details"],
                "back again"
            );
            // Keep the temp dir alive until the end.
            drop(fake);
        }

        #[tokio::test]
        async fn dropped_connection_is_unavailable() {
            let fake = FakeDiscord::start(Script::DropOnRequest);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => {
                    assert!(m.contains("closed") || m.contains("lost"), "{m}")
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
            // Within the reconnect interval, no new connection is made.
            assert!(matches!(
                target.set(&status("x line")).await,
                Err(TargetError::Unavailable(_))
            ));
            assert_eq!(fake.accepted(), 1);
        }

        #[tokio::test]
        async fn frames_split_across_reads() {
            let fake = FakeDiscord::start(Script::SplitFrames);
            let mut target = target_for(&fake);
            target.set(&status("x line")).await.unwrap();
            target.set(&status("y line")).await.unwrap();
            assert!(target.is_connected());
            assert_eq!(fake.requests().len(), 2);
        }

        #[tokio::test]
        async fn no_reply_times_out() {
            let fake = FakeDiscord::start(Script::NeverReply);
            let mut target = target_for(&fake).with_request_timeout(Duration::from_millis(200));
            let started = Instant::now();
            let err = target.set(&status("x line")).await.unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(3));
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("did not answer"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn silent_handshake_times_out() {
            let fake = FakeDiscord::start(Script::SilentHandshake);
            let mut target = target_for(&fake).with_request_timeout(Duration::from_millis(200));
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("handshake"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn garbage_reply_drops_the_connection() {
            let fake = FakeDiscord::start(Script::GarbageReply);
            let mut target = target_for(&fake);
            let err = target.set(&status("x line")).await.unwrap_err();
            match &err {
                TargetError::Unavailable(m) => assert!(m.contains("invalid"), "{m}"),
                other => panic!("expected Unavailable, got {other:?}"),
            }
            assert!(!target.is_connected());
        }

        #[tokio::test]
        async fn unicode_lines_reach_discord_intact() {
            let fake = FakeDiscord::start(Script::Normal);
            let mut target = target_for(&fake);
            let mut s = status("გაზაფხული მოვიდა 🎵 春が来た");
            s.track.artist = "ნინო".into();
            target.set(&s).await.unwrap();
            let activity = fake.requests()[0]["args"]["activity"].clone();
            assert_eq!(activity["details"], "გაზაფხული მოვიდა 🎵 春が来た");
            assert_eq!(activity["state"], "Never Gonna Give You Up · ნინო");
        }
    }
}
