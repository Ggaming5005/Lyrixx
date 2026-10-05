//! End-to-end test of the real `lyrix` binary on Linux.
//!
//! Everything the binary talks to is faked locally:
//! - a private `dbus-daemon` with a fake MPRIS player on it,
//! - a mock LRCLIB HTTP server on 127.0.0.1,
//! - a fake Discord IPC socket in `$XDG_RUNTIME_DIR/discord-ipc-0`.
//!
//! The test runs `lyrix run`, watches the lyric lines arrive at the fake
//! Discord and in the terminal output, stops it with SIGINT, and also checks
//! `lyrix now` and `lyrix lyrics`. Without `dbus-daemon` on `PATH` it prints a
//! note and passes.

#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use zbus::zvariant::{OwnedValue, Str};

const LYRIX: &str = env!("CARGO_BIN_EXE_lyrix");

const PLAYER_NAME: &str = "org.mpris.MediaPlayer2.fakeplayer";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const TITLE: &str = "Test Song";
const ARTIST: &str = "Test Artist";
const ALBUM: &str = "Test Album";
/// 3:00, in microseconds as MPRIS reports it.
const LENGTH_US: i64 = 180_000_000;
const LENGTH_MS: u64 = 180_000;
const CLIENT_ID: &str = "1234567890";
const SYNCED_LYRICS: &str =
    "[00:00.50] first line\n[00:01.50] second line\n[00:02.50] third line\n";
const LINES: [&str; 3] = ["first line", "second line", "third line"];
/// When each line starts, in song time.
const LINE_STARTS_MS: [u64; 3] = [500, 1_500, 2_500];

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lyrix_binary_end_to_end() {
    if !on_path("dbus-daemon") {
        eprintln!("note: dbus-daemon is not on PATH, skipping the lyrix end-to-end test");
        return;
    }

    // Unix socket paths are limited to ~100 bytes, so stay in /tmp.
    let root = tempfile::Builder::new()
        .prefix("lyrix-e2e")
        .tempdir_in("/tmp")
        .expect("temp dir");
    let env = TestEnv::create(root.path());

    let bus = PrivateBus::start(&root.path().join("bus"));
    let song_start: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let _player = serve_player(&bus.address, song_start.clone()).await;
    let lrclib = MockLrclib::start().await;
    let discord = FakeDiscord::start(&env.runtime.join("discord-ipc-0"));
    let config_path = env.write_config(&lrclib.url);

    // ---- lyrix now ----------------------------------------------------------
    let now = run_once(&env, &bus, &config_path, &["now"]).await;
    assert!(now.status.success(), "lyrix now failed\n{}", now.describe());
    for expected in [
        format!("Title:    {TITLE}"),
        format!("Artist:   {ARTIST}"),
        format!("Album:    {ALBUM}"),
        "Duration: 3:00".to_string(),
        "Status:   playing".to_string(),
        format!("App:      {PLAYER_NAME}"),
    ] {
        assert!(
            now.stdout.contains(&expected),
            "lyrix now did not print {expected:?}\n{}",
            now.describe()
        );
    }

    // ---- lyrix run ----------------------------------------------------------
    // The song starts over when lyrix first reads the position.
    *song_start.lock().unwrap() = None;
    let mut run = Running::spawn(&env, &bus, &config_path, &["run"]);

    let handshake = discord
        .wait_for(Duration::from_secs(15), |events| {
            events.iter().find(|e| e.op == 0).cloned()
        })
        .await
        .unwrap_or_else(|| panic!("no Discord handshake\n{}", run.describe()));
    assert_eq!(
        handshake.payload,
        json!({"v": 1, "client_id": CLIENT_ID}),
        "handshake"
    );

    let third = discord
        .wait_for(Duration::from_secs(20), |events| {
            activities(events)
                .into_iter()
                .find(|(_, activity)| details(activity).contains(LINES[2]))
        })
        .await;
    assert!(
        third.is_some(),
        "the third line never reached Discord\nactivities: {:#?}\n{}",
        activities(&discord.events()),
        run.describe()
    );
    // Let the terminal output catch up with the last line.
    let console_has_all = wait_until(Duration::from_secs(5), || {
        run.stdout().contains(&format!("♫ 🎵 {}", LINES[2]))
    })
    .await;
    assert!(console_has_all, "terminal output\n{}", run.describe());

    let song_start = song_start
        .lock()
        .unwrap()
        .expect("lyrix read the player's position");
    let events = discord.events();
    let sent = activities(&events);
    check_activities(&sent, song_start, &run.describe());
    for event in events.iter().filter(|e| e.op == 1) {
        assert_eq!(
            event.payload["args"]["pid"],
            json!(run.pid()),
            "every command names the lyrix process: {:#}",
            event.payload
        );
    }
    check_console(&run.stdout(), &run.describe());
    check_lrclib(&lrclib.requests(), &run.describe());

    // ---- Ctrl+C -------------------------------------------------------------
    let before_stop = discord.events().len();
    let interrupted_at = Instant::now();
    let kill = Command::new("kill")
        .args(["-INT", &run.pid().to_string()])
        .status()
        .expect("run kill");
    assert!(kill.success(), "kill -INT failed");

    let status = run.wait(Duration::from_secs(5)).await;
    let exited_after = interrupted_at.elapsed();
    let status =
        status.unwrap_or_else(|| panic!("lyrix did not exit within 5 s\n{}", run.describe()));
    assert_eq!(
        status.code(),
        Some(0),
        "lyrix exit status {status}\n{}",
        run.describe()
    );

    let after_stop: Vec<Received> = discord.events().split_off(before_stop);
    let cleared = activities(&after_stop)
        .into_iter()
        .any(|(_, activity)| activity.is_null());
    assert!(
        cleared,
        "no SET_ACTIVITY with a null activity after SIGINT\nframes: {after_stop:#?}\n{}",
        run.describe()
    );
    let stdout = run.stdout();
    assert_eq!(
        stdout.lines().last(),
        Some("■ cleared"),
        "the terminal is cleared last\n{}",
        run.describe()
    );

    // ---- lyrix lyrics -------------------------------------------------------
    let lyrics = run_once(&env, &bus, &config_path, &["lyrics"]).await;
    assert!(
        lyrics.status.success(),
        "lyrix lyrics failed\n{}",
        lyrics.describe()
    );
    for (line, start) in LINES.iter().zip(LINE_STARTS_MS) {
        let expected = format!("[00:{:02}.{:02}]{line}", start / 1000, start % 1000 / 10);
        assert!(
            lyrics.stdout.lines().any(|l| l == expected),
            "lyrix lyrics did not print {expected:?}\n{}",
            lyrics.describe()
        );
    }
    assert!(
        lyrics.stdout.contains("second line"),
        "{}",
        lyrics.describe()
    );
    // Served from the cache that `lyrix run` wrote: LRCLIB was asked once in all.
    let asked = lrclib.requests();
    assert_eq!(asked.len(), 1, "{asked:#?}\n{}", lyrics.describe());

    // What the binary did, for the test log (`--nocapture`).
    eprintln!("--- lyrix run stderr ---\n{}", run.stderr());
    eprintln!("--- lyrix run stdout ---\n{stdout}");
    eprintln!("--- Discord SET_ACTIVITY requests (ms after the song started) ---");
    for (at, activity) in activities(&discord.events()) {
        let ms = at.saturating_duration_since(song_start).as_millis();
        let what = if activity.is_null() {
            "null".to_string()
        } else {
            details(&activity).to_string()
        };
        eprintln!("{ms:>6} ms  {what}");
    }
    eprintln!("exit after SIGINT took {} ms", exited_after.as_millis());
    eprintln!("--- lyrix now ---\n{}", now.stdout);
    eprintln!("--- lyrix lyrics ---\n{}{}", lyrics.stdout, lyrics.stderr);
    eprintln!("--- LRCLIB requests ---\n{:#?}", lrclib.requests());
}

/// Every non-null activity is a well-formed Listening activity for the song,
/// the lines arrive in order, never before they are sung, and the first one
/// was not skipped.
fn check_activities(sent: &[(Instant, Value)], song_start: Instant, logs: &str) {
    assert!(!sent.is_empty(), "no activity was sent\n{logs}");

    let mut line_seen: Vec<(usize, Instant)> = Vec::new();
    for (at, activity) in sent {
        if activity.is_null() {
            continue;
        }
        let context = || format!("activity {activity:#}\n{logs}");
        assert_eq!(activity["type"], json!(2), "type: {}", context());
        assert_eq!(
            activity["status_display_type"],
            json!(2),
            "status_display_type: {}",
            context()
        );
        assert_eq!(
            activity["state"],
            json!(format!("{TITLE} · {ARTIST}")),
            "state: {}",
            context()
        );
        let start = activity["timestamps"]["start"]
            .as_u64()
            .unwrap_or_else(|| panic!("timestamps.start: {}", context()));
        let end = activity["timestamps"]["end"]
            .as_u64()
            .unwrap_or_else(|| panic!("timestamps.end: {}", context()));
        assert_eq!(end - start, LENGTH_MS, "timestamps: {}", context());
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(
            start <= now_unix && now_unix - start < 120_000,
            "timestamps.start is not a recent Unix time in ms: {}",
            context()
        );

        let text = details(activity);
        if let Some(index) = LINES.iter().position(|line| text.contains(line)) {
            assert_eq!(
                text,
                format!("🎵 {}", LINES[index]),
                "details: {}",
                context()
            );
            line_seen.push((index, *at));
        }
    }

    let order: Vec<usize> = line_seen.iter().map(|(i, _)| *i).collect();
    assert!(
        order.windows(2).all(|w| w[0] <= w[1]),
        "lines arrived out of order: {order:?}\n{logs}"
    );
    for (index, _) in LINES.iter().enumerate() {
        let Some((_, at)) = line_seen.iter().find(|(i, _)| *i == index) else {
            panic!(
                "line {} never reached Discord: {order:?}\n{logs}",
                index + 1
            );
        };
        // Never shown before it is sung (a little slack for the read itself).
        let after = at.saturating_duration_since(song_start);
        let due = Duration::from_millis(LINE_STARTS_MS[index]);
        assert!(
            after + Duration::from_millis(150) >= due,
            "line {} reached Discord {} ms into the song, before it starts at {} ms\n{logs}",
            index + 1,
            after.as_millis(),
            due.as_millis()
        );
    }
}

/// The terminal shows the song header once, then the three lines in order.
fn check_console(stdout: &str, logs: &str) {
    let header = format!("▶ {TITLE} — {ARTIST}");
    let lines: Vec<&str> = stdout.lines().collect();
    let headers = lines.iter().filter(|l| **l == header).count();
    assert_eq!(
        headers, 1,
        "header {header:?} printed {headers} times\n{logs}"
    );
    let header_at = lines.iter().position(|l| *l == header).unwrap_or(0);
    let mut last = header_at;
    for line in LINES {
        let expected = format!("♫ 🎵 {line}");
        let at = lines
            .iter()
            .position(|l| *l == expected)
            .unwrap_or_else(|| panic!("terminal never showed {expected:?}\n{logs}"));
        assert!(at > last, "{expected:?} is out of order\n{logs}");
        last = at;
    }
}

/// LRCLIB was asked for exactly this song (all four fields).
fn check_lrclib(requests: &[Request], logs: &str) {
    let get = requests
        .iter()
        .find(|r| r.path == "/api/get")
        .unwrap_or_else(|| panic!("no /api/get request: {requests:#?}\n{logs}"));
    let expected: HashMap<String, String> = [
        ("artist_name", ARTIST),
        ("track_name", TITLE),
        ("album_name", ALBUM),
        ("duration", "180"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(get.query, expected, "/api/get query\n{logs}");
    assert_eq!(get.user_agent, lyrix::USER_AGENT, "user agent\n{logs}");
}

// ---------------------------------------------------------------------------
// Environment and processes
// ---------------------------------------------------------------------------

/// True when `program` is an executable file in a `PATH` folder.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(program))
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// The folders lyrix sees as its home, config, data, cache and runtime dirs.
struct TestEnv {
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
}

impl TestEnv {
    fn create(root: &Path) -> Self {
        let env = Self {
            root: root.to_path_buf(),
            home: root.join("home"),
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
            runtime: root.join("run"),
        };
        for dir in [&env.home, &env.config, &env.data, &env.cache, &env.runtime] {
            std::fs::create_dir_all(dir).expect("create dir");
        }
        std::fs::set_permissions(&env.runtime, std::fs::Permissions::from_mode(0o700))
            .expect("chmod runtime dir");
        env
    }

    fn write_config(&self, lrclib_url: &str) -> PathBuf {
        let path = self.root.join("config.toml");
        let text = format!(
            "[general]\n\
             poll_interval_ms = 200\n\
             \n\
             [lyrics]\n\
             lrclib_url = \"{lrclib_url}\"\n\
             \n\
             [discord]\n\
             client_id = \"{CLIENT_ID}\"\n\
             min_interval_ms = 300\n\
             \n\
             [console]\n\
             enabled = true\n"
        );
        std::fs::write(&path, text).expect("write config");
        path
    }

    /// A `lyrix` command with this environment: private folders, the private
    /// bus, and no HTTP proxy for the mock LRCLIB.
    fn command(&self, bus: &PrivateBus, config: &Path, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(LYRIX);
        command
            .arg("--config")
            .arg(config)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("RUST_LOG", "lyrix=debug")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(key);
        }
        command
    }
}

/// A private `dbus-daemon`, stopped when dropped.
struct PrivateBus {
    child: Child,
    address: String,
}

impl PrivateBus {
    fn start(socket: &Path) -> Self {
        let mut child = Command::new("dbus-daemon")
            .arg("--session")
            .arg("--nofork")
            .arg("--print-address=1")
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start dbus-daemon");
        let stdout = child.stdout.take().expect("dbus-daemon stdout");
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line);
        let bus = Self {
            child,
            address: line.trim().to_string(),
        };
        assert!(
            read.is_ok() && bus.address.starts_with("unix:"),
            "dbus-daemon printed no address: {line:?}"
        );
        bus
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The output of a finished `lyrix` command.
struct Finished {
    args: String,
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Finished {
    fn describe(&self) -> String {
        format!(
            "lyrix {} → {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            self.args, self.status, self.stdout, self.stderr
        )
    }
}

/// Runs `lyrix <args>` to the end (at most 20 s).
async fn run_once(env: &TestEnv, bus: &PrivateBus, config: &Path, args: &[&str]) -> Finished {
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        env.command(bus, config, args).output(),
    )
    .await
    .unwrap_or_else(|_| panic!("lyrix {args:?} did not finish within 20 s"))
    .expect("run lyrix");
    Finished {
        args: args.join(" "),
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A `lyrix` process whose output is collected while it runs.
struct Running {
    child: tokio::process::Child,
    pid: u32,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    fn spawn(env: &TestEnv, bus: &PrivateBus, config: &Path, args: &[&str]) -> Self {
        let mut child = env.command(bus, config, args).spawn().expect("spawn lyrix");
        let pid = child.id().expect("lyrix pid");
        let stdout = Arc::new(Mutex::new(String::new()));
        let stderr = Arc::new(Mutex::new(String::new()));
        let readers = vec![
            collect(child.stdout.take().expect("stdout"), stdout.clone()),
            collect(child.stderr.take().expect("stderr"), stderr.clone()),
        ];
        Self {
            child,
            pid,
            stdout,
            stderr,
            readers,
        }
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn stdout(&self) -> String {
        self.stdout.lock().unwrap().clone()
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }

    fn describe(&self) -> String {
        format!(
            "--- lyrix stdout ---\n{}\n--- lyrix stderr ---\n{}",
            self.stdout(),
            self.stderr()
        )
    }

    /// The exit status, or `None` when it is still running after `limit`. The
    /// output is read to the end first.
    async fn wait(&mut self, limit: Duration) -> Option<ExitStatus> {
        let status = tokio::time::timeout(limit, self.child.wait())
            .await
            .ok()?
            .expect("wait for lyrix");
        for reader in self.readers.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
        }
        Some(status)
    }
}

/// Appends every line read from `from` to `into`.
fn collect<R>(from: R, into: Arc<Mutex<String>>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(from).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut text = into.lock().unwrap();
            text.push_str(&line);
            text.push('\n');
        }
    })
}

/// Polls `check` every 20 ms until it is true or `limit` has passed.
async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// Fake MPRIS player
// ---------------------------------------------------------------------------

/// A playing player whose position advances in real time from the moment it
/// is first read (`song_start`, reset to `None` to start the song over).
struct FakePlayer {
    song_start: Arc<Mutex<Option<Instant>>>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl FakePlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        "Playing".to_string()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut metadata = HashMap::new();
        metadata.insert(
            "mpris:trackid".to_string(),
            OwnedValue::from(
                zbus::zvariant::ObjectPath::try_from("/org/mpris/MediaPlayer2/Track/1").unwrap(),
            ),
        );
        metadata.insert(
            "xesam:title".to_string(),
            OwnedValue::from(Str::from(TITLE)),
        );
        metadata.insert(
            "xesam:artist".to_string(),
            OwnedValue::try_from(zbus::zvariant::Value::from(vec![ARTIST.to_string()])).unwrap(),
        );
        metadata.insert(
            "xesam:album".to_string(),
            OwnedValue::from(Str::from(ALBUM)),
        );
        metadata.insert("mpris:length".to_string(), OwnedValue::from(LENGTH_US));
        metadata
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        let mut start = self.song_start.lock().unwrap();
        let start = *start.get_or_insert_with(Instant::now);
        i64::try_from(start.elapsed().as_micros()).unwrap_or(i64::MAX)
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
}

/// Serves the fake player on its own connection; it goes away with the connection.
async fn serve_player(address: &str, song_start: Arc<Mutex<Option<Instant>>>) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .expect("bus address")
        .name(PLAYER_NAME)
        .expect("player name")
        .serve_at(PLAYER_PATH, FakePlayer { song_start })
        .expect("serve player")
        .build()
        .await
        .expect("connect the fake player")
}

// ---------------------------------------------------------------------------
// Mock LRCLIB
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Request {
    path: String,
    query: HashMap<String, String>,
    user_agent: String,
}

struct MockLrclib {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl MockLrclib {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind LRCLIB");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(answer_http(stream, log.clone()));
            }
        });
        Self { url, requests }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

fn lrclib_record() -> Value {
    json!({
        "id": 4242,
        "trackName": TITLE,
        "artistName": ARTIST,
        "albumName": ALBUM,
        "duration": 180.0,
        "instrumental": false,
        "plainLyrics": LINES.join("\n"),
        "syncedLyrics": SYNCED_LYRICS,
    })
}

/// Answers HTTP requests on one connection (keep-alive), one at a time.
async fn answer_http(mut stream: TcpStream, log: Arc<Mutex<Vec<Request>>>) {
    let mut buf = Vec::new();
    loop {
        let head_end = loop {
            if let Some(at) = find(&buf, b"\r\n\r\n") {
                break at;
            }
            if buf.len() > 64 * 1024 {
                return;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        buf.drain(..head_end + 4);

        let mut lines = head.split("\r\n");
        let target = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("/")
            .to_string();
        let user_agent = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.trim().to_string())
            .unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let query: HashMap<String, String> = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (url_decode(k), url_decode(v))
            })
            .collect();
        log.lock().unwrap().push(Request {
            path: path.to_string(),
            query,
            user_agent,
        });

        let (status, body) = match path {
            "/api/get" => ("200 OK", lrclib_record().to_string()),
            "/api/search" => ("200 OK", json!([lrclib_record()]).to_string()),
            _ => ("404 Not Found", json!({"message": "not found"}).to_string()),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        if stream.write_all(response.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Decodes `application/x-www-form-urlencoded` text (`+` and `%XX`).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                Some(byte) => {
                    out.push(byte);
                    i += 2;
                }
                None => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Fake Discord IPC
// ---------------------------------------------------------------------------

/// One frame the fake Discord received.
#[derive(Debug, Clone)]
struct Received {
    at: Instant,
    op: u32,
    payload: Value,
}

struct FakeDiscord {
    events: Arc<Mutex<Vec<Received>>>,
}

impl FakeDiscord {
    /// Listens at `path`: answers the handshake with READY and every command
    /// with a reply carrying the same nonce, and records every frame.
    fn start(path: &Path) -> Self {
        let listener = UnixListener::bind(path).expect("bind the fake Discord socket");
        let events = Arc::new(Mutex::new(Vec::new()));
        let log = events.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve_discord(stream, log.clone()));
            }
        });
        Self { events }
    }

    fn events(&self) -> Vec<Received> {
        self.events.lock().unwrap().clone()
    }

    /// Polls `find` over the frames so far until it finds something.
    async fn wait_for<T>(
        &self,
        limit: Duration,
        mut find: impl FnMut(&[Received]) -> Option<T>,
    ) -> Option<T> {
        let mut found = None;
        wait_until(limit, || {
            found = find(&self.events());
            found.is_some()
        })
        .await;
        found
    }
}

/// The `SET_ACTIVITY` requests: when each arrived and its activity (null clears).
fn activities(events: &[Received]) -> Vec<(Instant, Value)> {
    events
        .iter()
        .filter(|e| e.op == 1 && e.payload["cmd"] == "SET_ACTIVITY")
        .map(|e| (e.at, e.payload["args"]["activity"].clone()))
        .collect()
}

fn details(activity: &Value) -> &str {
    activity["details"].as_str().unwrap_or("")
}

async fn serve_discord(mut stream: UnixStream, log: Arc<Mutex<Vec<Received>>>) {
    let mut buf = Vec::new();
    while let Some((op, payload)) = read_frame(&mut stream, &mut buf).await {
        log.lock().unwrap().push(Received {
            at: Instant::now(),
            op,
            payload: payload.clone(),
        });
        let reply = match op {
            0 => Some((
                1,
                json!({
                    "cmd": "DISPATCH",
                    "evt": "READY",
                    "data": {"v": 1, "user": {"id": "1", "username": "tester"}},
                    "nonce": null,
                }),
            )),
            1 => Some((
                1,
                json!({
                    "cmd": payload["cmd"].clone(),
                    "data": payload["args"]["activity"].clone(),
                    "evt": null,
                    "nonce": payload["nonce"].clone(),
                }),
            )),
            2 => return,
            3 => Some((4, payload)),
            _ => None,
        };
        if let Some((op, reply)) = reply {
            if stream.write_all(&frame(op, &reply)).await.is_err() {
                return;
            }
        }
    }
}

/// opcode (u32 LE), length (u32 LE), JSON.
fn frame(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

async fn read_frame(stream: &mut UnixStream, buf: &mut Vec<u8>) -> Option<(u32, Value)> {
    loop {
        if buf.len() >= 8 {
            let op = u32::from_le_bytes(buf[0..4].try_into().ok()?);
            let len = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
            if buf.len() >= 8 + len {
                let payload = serde_json::from_slice(&buf[8..8 + len]).ok()?;
                buf.drain(..8 + len);
                return Some((op, payload));
            }
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}
