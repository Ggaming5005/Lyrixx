//! macOS: the apps' own AppleScript interfaces, or `mediaremote-adapter`.
//!
//! Built in (no install needed): Spotify and Apple Music via `osascript`. A
//! script is only sent to an app that is already running (checked through
//! System Events / `application "X" is running`), so Lyrix never launches it.
//! Both apps report `name`, `artist`, `album`, `duration` (Spotify: ms; Music:
//! seconds as a real) and `player position` (seconds as a real) of the current
//! track, plus `player state` (playing/paused/stopped). Spotify's `id` is
//! `spotify:track:<id>`.
//!
//! Optional: when `macos_adapter_dir` points at an installed
//! [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter), run
//! `/usr/bin/perl <dir>/bin/mediaremote-adapter.pl <dir>/build/MediaRemoteAdapter.framework get`
//! and read its JSON (`title`, `artist`, `album`, `duration` and `elapsedTime` in
//! seconds, `timestamp` for when `elapsedTime` was true, `playing`,
//! `bundleIdentifier`, `playbackRate`). This covers every app in the Now Playing
//! widget, browsers included. If it fails, fall back to AppleScript.
//!
//! Details:
//! - The running check uses `application "X" is running`, which AppleScript
//!   answers without launching the app. Every `osascript` call is limited to
//!   2 s, the adapter to 3 s; a call that takes longer is killed.
//! - Fields come back on one line separated by ASCII 31 (unit separator).
//!   Reals may use a decimal comma (`12,5`) or an exponent (`1.2E+4`).
//! - Adapter `timestamp` may be an ISO-8601 string or a number of seconds since
//!   1970 or since 2001 (also ms or µs since 1970); anything else, or a time in
//!   the future or more than a day old, counts as "now". `elapsedTimeNow`
//!   (position at the time of the call) is used when present.
//! - A Now Playing entry with neither title nor artist counts as nothing playing.
//! - Browsers play through helper processes (Safari through
//!   `com.apple.WebKit.GPU`); the adapter's `parentApplicationBundleIdentifier`
//!   names the app then. The parent, when present, is the `app_id`, and an entry
//!   is blocked when either id matches a blocked app.
//! - An app whose script fails is skipped. When nothing could be read and an
//!   app failed (for example because Lyrix was denied the Automation permission),
//!   the failure is logged as a warning at most once a minute. The snapshot is
//!   still "nothing playing", so a status from an app that has quit is cleared.
//!
//! Cover art, for the app the last snapshot picked:
//! - From mediaremote-adapter: its `artworkData` (base64) and
//!   `artworkMimeType`, handed over as a `data:` URL (at most 2 MiB).
//! - Spotify (through AppleScript, or the adapter without artwork data):
//!   `artwork url of current track`, an `https:` URL, asked only while
//!   Spotify is running.
//! - Apple Music through AppleScript: none.

use super::artwork::data_url_from_base64;
use super::{
    choose, instant_at, is_blocked, sanitize_rate, spotify_track_id,
    strip_prefix_ignore_ascii_case, system_time_from_unix_ms, NowPlayingSource,
};
use crate::types::{PlaybackSnapshot, PlaybackStatus, Track};
use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use serde_json::Value;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const OSASCRIPT: &str = "/usr/bin/osascript";
const PERL: &str = "/usr/bin/perl";
const OSASCRIPT_TIMEOUT: Duration = Duration::from_secs(2);
const ADAPTER_TIMEOUT: Duration = Duration::from_secs(3);
/// ASCII unit separator, produced in AppleScript by `character id 31`.
const FIELD_SEPARATOR: char = '\u{1f}';
const DAY_MS: f64 = 86_400_000.0;
/// Seconds from 1970-01-01 to 2001-01-01 (Apple's reference date), in ms.
const APPLE_EPOCH_OFFSET_MS: f64 = 978_307_200_000.0;
/// How often an unreadable player is logged as a warning.
const FAILURE_WARNING_INTERVAL: Duration = Duration::from_secs(60);

/// See the module docs.
pub struct MacSource {
    adapter_dir: Option<PathBuf>,
    preferred: Vec<String>,
    blocked: Vec<String>,
    /// Limits the "could not read the player" warning.
    failure_warnings: LogLimiter,
    /// Where the cover of the song the last snapshot picked comes from.
    art: Mutex<ArtSource>,
}

/// Where the cover of the song being played can be found.
#[derive(Debug, Clone, PartialEq)]
enum ArtSource {
    /// Nowhere: nothing plays, or the app has no cover to give.
    None,
    /// The AppleScript app with this name, which has `artwork url`.
    Scripted(&'static str),
    /// What mediaremote-adapter reported.
    Adapter(AdapterArtwork),
}

/// The cover art in mediaremote-adapter's output.
#[derive(Debug, Clone, PartialEq)]
struct AdapterArtwork {
    /// `artworkData`: the image, base64-encoded.
    data: String,
    /// `artworkMimeType`, e.g. `image/jpeg`.
    mime: Option<String>,
}

impl MacSource {
    pub fn new(adapter_dir: Option<PathBuf>, preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self {
            adapter_dir,
            preferred,
            blocked,
            failure_warnings: LogLimiter::new(FAILURE_WARNING_INTERVAL),
            art: Mutex::new(ArtSource::None),
        }
    }
}

#[async_trait]
impl NowPlayingSource for MacSource {
    fn name(&self) -> &'static str {
        "macos"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        self.read_with(PERL, OSASCRIPT).await
    }

    async fn artwork(&self) -> anyhow::Result<Option<String>> {
        self.artwork_with(OSASCRIPT).await
    }
}

impl MacSource {
    /// [`NowPlayingSource::snapshot`] with the programs to run (tests use fakes).
    async fn read_with(
        &self,
        perl: &str,
        osascript: &str,
    ) -> anyhow::Result<Option<PlaybackSnapshot>> {
        if let Some(dir) = &self.adapter_dir {
            match read_adapter(perl, dir, &self.blocked).await {
                Ok(found) => {
                    let (candidates, artwork) = match found {
                        Some(entry) => (vec![entry.snapshot], entry.artwork),
                        None => (Vec::new(), None),
                    };
                    let picked = choose(candidates, &self.preferred, &self.blocked);
                    self.remember_art(match (&picked, artwork) {
                        (None, _) => ArtSource::None,
                        (Some(_), Some(artwork)) => ArtSource::Adapter(artwork),
                        (Some(picked), None) => scripted_art(&picked.app_id),
                    });
                    return Ok(picked);
                }
                Err(err) => {
                    tracing::debug!("mediaremote-adapter failed, using AppleScript: {err:#}");
                }
            }
        }
        let read = read_scripted_apps(osascript, &self.blocked).await?;
        if let Some(err) = &read.unreadable {
            if self.failure_warnings.allow(Instant::now()) {
                tracing::warn!(
                    "{err:#} (if Lyrix may not control it, allow it in System Settings > \
                     Privacy & Security > Automation)"
                );
            }
        }
        let picked = choose(read.snapshots, &self.preferred, &self.blocked);
        self.remember_art(
            picked
                .as_ref()
                .map_or(ArtSource::None, |picked| scripted_art(&picked.app_id)),
        );
        Ok(picked)
    }

    fn remember_art(&self, art: ArtSource) {
        *self.art.lock().unwrap_or_else(PoisonError::into_inner) = art;
    }

    /// [`NowPlayingSource::artwork`] with the program to run (tests use a fake).
    async fn artwork_with(&self, osascript: &str) -> anyhow::Result<Option<String>> {
        let art = self
            .art
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match art {
            ArtSource::None => Ok(None),
            ArtSource::Scripted(app) => {
                let output = run_osascript(osascript, &artwork_script(app))
                    .await
                    .with_context(|| format!("could not ask {app} for the cover art"))?;
                Ok(parse_artwork_url(&output))
            }
            ArtSource::Adapter(artwork) => {
                Ok(data_url_from_base64(&artwork.data, artwork.mime.as_deref()))
            }
        }
    }
}

/// The cover of a scripted app's song: Spotify has `artwork url`, Apple
/// Music has no URL to give.
fn scripted_art(app_id: &str) -> ArtSource {
    APPS.iter()
        .find(|app| app.artwork_url && app.app_id == app_id)
        .map_or(ArtSource::None, |app| ArtSource::Scripted(app.name))
}

/// Lets a message through at most once per interval.
#[derive(Debug)]
struct LogLimiter {
    every: Duration,
    last: Mutex<Option<Instant>>,
}

impl LogLimiter {
    fn new(every: Duration) -> Self {
        Self {
            every,
            last: Mutex::new(None),
        }
    }

    /// True when the message may be logged at `now` (and remembers it).
    fn allow(&self, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        match *last {
            Some(at) if now.saturating_duration_since(at) < self.every => false,
            _ => {
                *last = Some(now);
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AppleScript
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationUnit {
    Millis,
    Seconds,
}

/// An app Lyrix can ask directly.
#[derive(Debug)]
struct ScriptedApp {
    /// The AppleScript application name.
    name: &'static str,
    /// Reported as `app_id`.
    app_id: &'static str,
    duration_unit: DurationUnit,
    /// The track `id` is a Spotify URI.
    spotify_ids: bool,
    /// The track has an `artwork url` (its cover as a web URL).
    artwork_url: bool,
}

const APPS: [ScriptedApp; 2] = [
    ScriptedApp {
        name: "Spotify",
        app_id: "com.spotify.client",
        duration_unit: DurationUnit::Millis,
        spotify_ids: true,
        artwork_url: true,
    },
    ScriptedApp {
        name: "Music",
        app_id: "com.apple.Music",
        duration_unit: DurationUnit::Seconds,
        spotify_ids: false,
        artwork_url: false,
    },
];

/// What [`read_scripted_apps`] found.
#[derive(Debug, Default)]
struct ScriptedRead {
    snapshots: Vec<PlaybackSnapshot>,
    /// Set when nothing could be read and a running app failed: its error.
    unreadable: Option<anyhow::Error>,
}

/// Asks which of `apps` are running (never launching any), then reads each
/// running one. An app whose script fails is skipped (and reported in
/// `unreadable` when nothing could be read). Failing to run `osascript` at
/// all is an error.
async fn read_scripted_apps(osascript: &str, blocked: &[String]) -> anyhow::Result<ScriptedRead> {
    let apps: Vec<&ScriptedApp> = APPS
        .iter()
        .filter(|app| !is_blocked(app.app_id, blocked))
        .collect();
    if apps.is_empty() {
        return Ok(ScriptedRead::default());
    }

    let output = run_osascript(osascript, &running_check_script(&apps))
        .await
        .context("could not ask macOS which music apps are running")?;
    let running = parse_running(&output, apps.len());

    let mut snapshots = Vec::new();
    let mut first_failure: Option<anyhow::Error> = None;
    for (app, running) in apps.into_iter().zip(running) {
        if !running {
            continue;
        }
        let before = Instant::now();
        let output = match run_osascript(osascript, &query_script(app)).await {
            Ok(output) => output,
            Err(err) => {
                tracing::debug!(app = app.name, "could not read the player: {err:#}");
                if first_failure.is_none() {
                    first_failure = Some(err.context(format!("could not read {}", app.name)));
                }
                continue;
            }
        };
        let after = Instant::now();
        if let Some(reading) = parse_player_output(&output, app.duration_unit, app.spotify_ids) {
            // The middle of the call is the best guess for when the position was read.
            let read_at = before + after.saturating_duration_since(before) / 2;
            snapshots.push(reading.into_snapshot(app.app_id, read_at));
        }
    }
    let unreadable = first_failure.filter(|_| snapshots.is_empty());
    Ok(ScriptedRead {
        snapshots,
        unreadable,
    })
}

async fn run_osascript(osascript: &str, script: &[String]) -> anyhow::Result<String> {
    let mut args: Vec<&OsStr> = Vec::with_capacity(script.len() * 2);
    for line in script {
        args.push(OsStr::new("-e"));
        args.push(OsStr::new(line.as_str()));
    }
    run(osascript, &args, OSASCRIPT_TIMEOUT).await
}

/// One line per `-e`: prints `true`/`false` for each app, separated by ASCII 31.
/// `application "X" is running` does not launch the app, and a missing app is
/// simply not running.
fn running_check_script(apps: &[&ScriptedApp]) -> Vec<String> {
    let checks: Vec<String> = apps
        .iter()
        .map(|app| format!("((my isRunning(\"{}\")) as text)", app.name))
        .collect();
    vec![
        "on isRunning(appName)".to_string(),
        "try".to_string(),
        "return (application appName is running)".to_string(),
        "on error".to_string(),
        "return false".to_string(),
        "end try".to_string(),
        "end isRunning".to_string(),
        format!("return {}", checks.join(" & (character id 31) & ")),
    ]
}

/// One line per `-e`: prints `state␟name␟artist␟album␟duration␟position␟id`, or
/// just `stopped`, or nothing when the app quit in the meantime.
fn query_script(app: &ScriptedApp) -> Vec<String> {
    let mut lines = vec![
        format!("if application \"{}\" is running then", app.name),
        format!("tell application \"{}\"", app.name),
        "set d to (character id 31)".to_string(),
        "set st to ((player state) as text)".to_string(),
        "if st is \"stopped\" then return st".to_string(),
        "set t to current track".to_string(),
        "set trackId to \"\"".to_string(),
    ];
    if app.spotify_ids {
        lines.extend([
            "try".to_string(),
            "set trackId to ((id of t) as text)".to_string(),
            "end try".to_string(),
        ]);
    }
    lines.extend([
        "return st & d & ((name of t) as text) & d & ((artist of t) as text) & d & \
         ((album of t) as text) & d & ((duration of t) as text) & d & \
         ((player position) as text) & d & trackId"
            .to_string(),
        "end tell".to_string(),
        "end if".to_string(),
        "return \"\"".to_string(),
    ]);
    lines
}

/// One line per `-e`: prints the `artwork url` of the app's current track, or
/// nothing when the app is not running (it is never launched).
fn artwork_script(app: &str) -> Vec<String> {
    vec![
        format!("if application \"{app}\" is running then"),
        format!("tell application \"{app}\""),
        "return ((artwork url of current track) as text)".to_string(),
        "end tell".to_string(),
        "end if".to_string(),
        "return \"\"".to_string(),
    ]
}

/// The URL printed by [`artwork_script`]: an `https:` or `http:` URL, else
/// `None` (`missing value`, an empty line, anything else).
fn parse_artwork_url(output: &str) -> Option<String> {
    let url = output.trim();
    let web = ["https://", "http://"].into_iter().any(|scheme| {
        strip_prefix_ignore_ascii_case(url, scheme).is_some_and(|rest| !rest.is_empty())
    });
    (web && !url.contains(char::is_whitespace)).then(|| url.to_string())
}

/// `true␟false` → `[true, false]`, padded with `false` to `count` entries.
fn parse_running(output: &str, count: usize) -> Vec<bool> {
    let mut running: Vec<bool> = output
        .trim()
        .split(FIELD_SEPARATOR)
        .map(|field| field.trim().eq_ignore_ascii_case("true"))
        .take(count)
        .collect();
    running.resize(count, false);
    running
}

/// What one app's script reported.
#[derive(Debug, Clone, PartialEq)]
struct Reading {
    status: PlaybackStatus,
    track: Track,
    position_ms: u64,
}

impl Reading {
    fn into_snapshot(self, app_id: &str, read_at: Instant) -> PlaybackSnapshot {
        PlaybackSnapshot {
            track: self.track,
            status: self.status,
            position_ms: self.position_ms,
            position_at: read_at,
            rate: 1.0,
            app_id: app_id.to_string(),
        }
    }
}

/// Parses the line printed by [`query_script`]. `None` for a stopped player,
/// an unknown state or a line with too few fields.
fn parse_player_output(output: &str, unit: DurationUnit, spotify_ids: bool) -> Option<Reading> {
    let line = output.trim_end_matches(['\r', '\n']);
    let fields: Vec<&str> = line.split(FIELD_SEPARATOR).collect();
    let status = parse_state(fields.first()?)?;
    if status == PlaybackStatus::Stopped || fields.len() < 6 {
        return None;
    }
    let field = |index: usize| fields.get(index).map(|f| clean_field(f)).unwrap_or("");

    let duration_ms = parse_number(field(4))
        .and_then(|value| match unit {
            DurationUnit::Millis => ms_from(value),
            DurationUnit::Seconds => ms_from(value * 1000.0),
        })
        .filter(|ms| *ms > 0);
    let mut position_ms = parse_number(field(5))
        .and_then(|secs| ms_from(secs * 1000.0))
        .unwrap_or(0);
    // Some player versions report the position in ms instead of seconds; a
    // "position" far past the end that fits the song as ms is taken as ms.
    if let Some(duration) = duration_ms {
        let slack = duration.saturating_add(5_000);
        if position_ms > slack && position_ms / 1000 <= slack {
            position_ms /= 1000;
        }
    }
    let spotify_id = if spotify_ids {
        spotify_track_id(field(6))
    } else {
        None
    };

    Some(Reading {
        status,
        track: Track {
            title: field(1).to_string(),
            artist: field(2).to_string(),
            album: Some(field(3))
                .filter(|album| !album.trim().is_empty())
                .map(str::to_string),
            duration_ms,
            spotify_id,
        },
        position_ms,
    })
}

/// AppleScript prints an absent value as `missing value`.
fn clean_field(field: &str) -> &str {
    if field.trim() == "missing value" {
        ""
    } else {
        field
    }
}

/// `player state` as text. Also understands the raw four-letter codes printed
/// when the app's terminology is unavailable (`kPSP`, `kPSp`, `kPSS`, ...).
/// Fast forwarding and rewinding count as playing.
fn parse_state(state: &str) -> Option<PlaybackStatus> {
    let trimmed = state.trim();
    let lower = trimmed.to_ascii_lowercase();
    // Codes first: they differ only in case.
    if trimmed.contains("kPSP") || trimmed.contains("kPSF") || trimmed.contains("kPSR") {
        Some(PlaybackStatus::Playing)
    } else if trimmed.contains("kPSp") {
        Some(PlaybackStatus::Paused)
    } else if trimmed.contains("kPSS") {
        Some(PlaybackStatus::Stopped)
    } else {
        match lower.as_str() {
            "playing" | "fast forwarding" | "rewinding" => Some(PlaybackStatus::Playing),
            "paused" => Some(PlaybackStatus::Paused),
            "stopped" => Some(PlaybackStatus::Stopped),
            _ => None,
        }
    }
}

/// A number as AppleScript prints it: `215000`, `12.5`, `12,5` (decimal comma),
/// `1.2345E+4`. Spaces (including no-break spaces) are ignored. Only finite
/// values are returned.
fn parse_number(text: &str) -> Option<f64> {
    let normalized: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| match c {
            ',' | '\u{066b}' => '.',
            other => other,
        })
        .collect();
    if normalized.is_empty() {
        return None;
    }
    let value: f64 = normalized.parse().ok()?;
    value.is_finite().then_some(value)
}

/// A non-negative finite number of milliseconds, rounded and saturated to `u64`.
fn ms_from(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 {
        // `as` saturates for floats.
        Some(value.round() as u64)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// mediaremote-adapter
// ---------------------------------------------------------------------------

/// What mediaremote-adapter reported.
#[derive(Debug, Clone, PartialEq)]
struct AdapterEntry {
    snapshot: PlaybackSnapshot,
    artwork: Option<AdapterArtwork>,
}

async fn read_adapter(
    perl: &str,
    dir: &Path,
    blocked: &[String],
) -> anyhow::Result<Option<AdapterEntry>> {
    let script = dir.join("bin").join("mediaremote-adapter.pl");
    let framework = dir.join("build").join("MediaRemoteAdapter.framework");
    let args = [script.as_os_str(), framework.as_os_str(), OsStr::new("get")];
    let output = run(perl, &args, ADAPTER_TIMEOUT)
        .await
        .context("mediaremote-adapter failed")?;
    parse_adapter_output(&output, blocked, SystemTime::now(), Instant::now())
}

/// Parses `mediaremote-adapter … get`. `null` or empty output means nothing is
/// playing; output that is not a JSON object is an error (so the caller falls
/// back to AppleScript). `now_sys` and `now` are the time of the call.
///
/// Browsers play through helper processes: Safari's media is reported by
/// `com.apple.WebKit.GPU` with `parentApplicationBundleIdentifier`
/// `com.apple.Safari`. The parent (when present) is the `app_id`, and an entry
/// whose bundle id or parent id is `blocked` gives `None`.
///
/// `artworkData` (base64) and `artworkMimeType` are kept as they are for
/// [`NowPlayingSource::artwork`]; blank data counts as no artwork.
fn parse_adapter_output(
    output: &str,
    blocked: &[String],
    now_sys: SystemTime,
    now: Instant,
) -> anyhow::Result<Option<AdapterEntry>> {
    let output = output.trim();
    if output.is_empty() || output == "null" {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(output)
        .context("mediaremote-adapter printed something that is not JSON")?;
    // The streaming format wraps the data in {"type": …, "payload": {…}}.
    let info = match value.get("payload") {
        Some(payload) if value.get("title").is_none() => payload,
        _ => &value,
    };
    let info = match info {
        Value::Null => return Ok(None),
        Value::Object(_) => info,
        _ => bail!("mediaremote-adapter printed unexpected JSON"),
    };

    let bundle_id = json_text(info, "bundleIdentifier");
    let parent_id = json_text(info, "parentApplicationBundleIdentifier");
    if is_blocked(&bundle_id, blocked) || is_blocked(&parent_id, blocked) {
        return Ok(None);
    }
    let app_id = if parent_id.trim().is_empty() {
        bundle_id
    } else {
        parent_id
    };

    let title = json_text(info, "title");
    let artist = json_text(info, "artist");
    if title.trim().is_empty() && artist.trim().is_empty() {
        return Ok(None);
    }
    let album = Some(json_text(info, "album")).filter(|album| !album.trim().is_empty());
    let duration_ms = json_number(info, "duration")
        .and_then(|secs| ms_from(secs * 1000.0))
        .filter(|ms| *ms > 0);

    let rate_reported = json_number(info, "playbackRate");
    let playing = json_bool(info, "playing").unwrap_or(rate_reported.is_some_and(|r| r > 0.0));
    let rate = rate_reported
        .filter(|rate| playing && *rate > 0.0)
        .map(sanitize_rate)
        .unwrap_or(1.0);

    let (position_ms, position_at) = if let Some(secs) = json_number(info, "elapsedTimeNow") {
        (ms_from(secs * 1000.0).unwrap_or(0), now)
    } else if let Some(secs) = json_number(info, "elapsedTime") {
        (
            ms_from(secs * 1000.0).unwrap_or(0),
            adapter_timestamp(info, now_sys, now),
        )
    } else {
        (0, now)
    };

    let snapshot = PlaybackSnapshot {
        track: Track {
            title,
            artist,
            album,
            duration_ms,
            spotify_id: None,
        },
        status: if playing {
            PlaybackStatus::Playing
        } else {
            PlaybackStatus::Paused
        },
        position_ms,
        position_at,
        rate,
        app_id,
    };
    let data = json_text(info, "artworkData");
    let artwork = (!data.trim().is_empty()).then(|| AdapterArtwork {
        data,
        mime: Some(json_text(info, "artworkMimeType")).filter(|mime| !mime.trim().is_empty()),
    });
    Ok(Some(AdapterEntry { snapshot, artwork }))
}

/// A string field; anything else is empty.
fn json_text(info: &Value, key: &str) -> String {
    match info.get(key) {
        Some(Value::String(text)) => text.clone(),
        _ => String::new(),
    }
}

/// A number field, also accepted as a numeric string.
fn json_number(info: &Value, key: &str) -> Option<f64> {
    match info.get(key)? {
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::String(text) => parse_number(text),
        _ => None,
    }
}

/// A boolean field, also accepted as a number (non-zero) or a string.
fn json_bool(info: &Value, key: &str) -> Option<bool> {
    match info.get(key)? {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => number.as_f64().map(|n| n != 0.0),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// When `elapsedTime` was true, as an `Instant` (see the module docs).
fn adapter_timestamp(info: &Value, now_sys: SystemTime, now: Instant) -> Instant {
    let now_unix_ms = match now_sys.duration_since(UNIX_EPOCH) {
        Ok(since) => since.as_secs_f64() * 1000.0,
        Err(_) => return now,
    };
    let unix_ms = json_number(info, "timestampEpochMicros")
        .map(|micros| micros / 1000.0)
        .filter(|ms| plausible(*ms, now_unix_ms))
        .or_else(|| {
            info.get("timestamp")
                .and_then(|t| timestamp_unix_ms(t, now_unix_ms))
        });
    unix_ms
        // `as` saturates; the value is finite and plausible here.
        .and_then(|ms| system_time_from_unix_ms(ms.round() as i64))
        .map(|at| instant_at(at, now_sys, now))
        .unwrap_or(now)
}

/// Within a day before now and a minute after (clock skew).
fn plausible(unix_ms: f64, now_unix_ms: f64) -> bool {
    unix_ms.is_finite() && unix_ms >= now_unix_ms - DAY_MS && unix_ms <= now_unix_ms + 60_000.0
}

/// A `timestamp` value as Unix ms: an ISO-8601 string, or a number (possibly
/// as a string) of seconds since 1970, seconds since 2001, ms or µs since
/// 1970 — whichever is plausible. `None` when none is.
fn timestamp_unix_ms(value: &Value, now_unix_ms: f64) -> Option<f64> {
    let number = match value {
        Value::String(text) => {
            if let Some(ms) = parse_iso8601_ms(text) {
                // An exact date is trusted as is; `instant_at` checks its age.
                return Some(ms as f64);
            }
            parse_number(text)?
        }
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    [
        number * 1000.0,
        number * 1000.0 + APPLE_EPOCH_OFFSET_MS,
        number,
        number / 1000.0,
    ]
    .into_iter()
    .find(|ms| plausible(*ms, now_unix_ms))
}

/// Parses `YYYY-MM-DD[T ]HH:MM:SS[.fff][Z|±HH[:MM]|±HHMM]` (also the
/// `2025-05-26 13:34:21 +0000` form) into Unix milliseconds. No zone means UTC.
fn parse_iso8601_ms(text: &str) -> Option<i64> {
    let bytes = text.trim().as_bytes();
    let number = |from: usize, len: usize| -> Option<i64> {
        let digits = bytes.get(from..from.checked_add(len)?)?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        digits.iter().try_fold(0_i64, |acc, d| {
            acc.checked_mul(10)?.checked_add(i64::from(d - b'0'))
        })
    };
    let expect = |at: usize, allowed: &[u8]| bytes.get(at).is_some_and(|b| allowed.contains(b));

    let year = number(0, 4)?;
    if !expect(4, b"-") {
        return None;
    }
    let month = number(5, 2)?;
    if !expect(7, b"-") {
        return None;
    }
    let day = number(8, 2)?;
    if !expect(10, b"Tt ") {
        return None;
    }
    let hour = number(11, 2)?;
    if !expect(13, b":") {
        return None;
    }
    let minute = number(14, 2)?;
    if !expect(16, b":") {
        return None;
    }
    let second = number(17, 2)?;
    let mut at = 19;

    let mut millis = 0_i64;
    if expect(at, b".,") {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
        // The first three digits are milliseconds; pad shorter fractions.
        let digits = bytes.get(start..at)?;
        for index in 0..3 {
            let digit = digits.get(index).map_or(0, |d| i64::from(d - b'0'));
            millis = millis * 10 + digit;
        }
    }

    while bytes.get(at) == Some(&b' ') {
        at += 1;
    }
    let offset_secs = match bytes.get(at) {
        None => 0,
        Some(b'Z' | b'z') => {
            at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            let hours = number(at + 1, 2)?;
            at += 3;
            if bytes.get(at) == Some(&b':') {
                at += 1;
            }
            let minutes = if bytes.get(at).is_some_and(u8::is_ascii_digit) {
                let minutes = number(at, 2)?;
                at += 2;
                minutes
            } else {
                0
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            sign * (hours * 3600 + minutes * 60)
        }
        Some(_) => return None,
    };
    if !bytes.get(at..)?.iter().all(u8::is_ascii_whitespace) {
        return None;
    }

    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs;
    Some(secs * 1000 + millis)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
/// Inputs are already range-checked (4-digit year).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

/// Runs `program` and returns its standard output. The process is killed when
/// it takes longer than `timeout`; a non-zero exit is an error that quotes
/// the start of its standard error.
async fn run(program: &str, args: &[&OsStr], timeout: Duration) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| anyhow!("{program} did not finish within {} ms", timeout.as_millis()))?
        .with_context(|| format!("could not run {program}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr: String = stderr.trim().chars().take(300).collect();
        bail!("{program} failed ({}): {stderr}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";
    const SEP: &str = "\u{1f}";

    fn line(fields: &[&str]) -> String {
        fields.join(SEP) + "\n"
    }

    // ---- numbers & states ------------------------------------------------------

    #[test]
    fn numbers_in_every_locale_format() {
        assert_eq!(parse_number("215000"), Some(215_000.0));
        assert_eq!(parse_number("12.5"), Some(12.5));
        assert_eq!(parse_number("12,5"), Some(12.5));
        assert_eq!(parse_number(" 12,5 \n"), Some(12.5));
        assert_eq!(parse_number("-3,25"), Some(-3.25));
        assert_eq!(parse_number("1.23456E+5"), Some(123_456.0));
        assert_eq!(parse_number("1,5E+3"), Some(1_500.0));
        assert_eq!(parse_number("2.5e-1"), Some(0.25));
        assert_eq!(parse_number("12\u{066b}5"), Some(12.5));
        assert_eq!(parse_number("1\u{a0}234"), Some(1_234.0));
        assert_eq!(parse_number("0"), Some(0.0));
    }

    #[test]
    fn numbers_that_are_not_numbers() {
        for text in [
            "",
            "   ",
            "missing value",
            "abc",
            "NaN",
            "inf",
            "-inf",
            "1.2.3",
            "1,234.5",
            "1e999",
            "١٢",
        ] {
            assert_eq!(parse_number(text), None, "{text:?}");
        }
    }

    #[test]
    fn ms_from_bounds() {
        assert_eq!(ms_from(0.0), Some(0));
        assert_eq!(ms_from(1.4), Some(1));
        assert_eq!(ms_from(1.5), Some(2));
        assert_eq!(ms_from(-0.1), None);
        assert_eq!(ms_from(f64::NAN), None);
        assert_eq!(ms_from(f64::INFINITY), None);
        assert_eq!(ms_from(1e300), Some(u64::MAX));
    }

    #[test]
    fn player_states() {
        assert_eq!(parse_state("playing"), Some(PlaybackStatus::Playing));
        assert_eq!(parse_state("Playing\n"), Some(PlaybackStatus::Playing));
        assert_eq!(parse_state("paused"), Some(PlaybackStatus::Paused));
        assert_eq!(parse_state("stopped"), Some(PlaybackStatus::Stopped));
        assert_eq!(
            parse_state("fast forwarding"),
            Some(PlaybackStatus::Playing)
        );
        assert_eq!(parse_state("rewinding"), Some(PlaybackStatus::Playing));
        assert_eq!(
            parse_state("«constant ****kPSP»"),
            Some(PlaybackStatus::Playing)
        );
        assert_eq!(
            parse_state("«constant ****kPSp»"),
            Some(PlaybackStatus::Paused)
        );
        assert_eq!(
            parse_state("«constant ****kPSS»"),
            Some(PlaybackStatus::Stopped)
        );
        assert_eq!(
            parse_state("«constant ****kPSF»"),
            Some(PlaybackStatus::Playing)
        );
        assert_eq!(parse_state(""), None);
        assert_eq!(parse_state("buffering"), None);
        assert_eq!(parse_state("lecture"), None);
    }

    // ---- AppleScript output ------------------------------------------------------

    #[test]
    fn spotify_line() {
        let output = line(&[
            "playing",
            "Song Title",
            "Artist One, Artist Two",
            "Album",
            "215672",
            "61.5",
            &format!("spotify:track:{ID}"),
        ]);
        assert_eq!(
            parse_player_output(&output, DurationUnit::Millis, true),
            Some(Reading {
                status: PlaybackStatus::Playing,
                track: Track {
                    title: "Song Title".into(),
                    artist: "Artist One, Artist Two".into(),
                    album: Some("Album".into()),
                    duration_ms: Some(215_672),
                    spotify_id: Some(ID.into()),
                },
                position_ms: 61_500,
            })
        );
    }

    #[test]
    fn music_line_with_decimal_commas() {
        let output = line(&[
            "paused", "Chanson", "Artiste", "Album", "245,333", "12,5", "",
        ]);
        assert_eq!(
            parse_player_output(&output, DurationUnit::Seconds, false),
            Some(Reading {
                status: PlaybackStatus::Paused,
                track: Track {
                    title: "Chanson".into(),
                    artist: "Artiste".into(),
                    album: Some("Album".into()),
                    duration_ms: Some(245_333),
                    spotify_id: None,
                },
                position_ms: 12_500,
            })
        );
    }

    #[test]
    fn music_ignores_ids_even_if_they_look_like_spotify() {
        let output = line(&[
            "playing",
            "T",
            "A",
            "",
            "200",
            "1",
            &format!("spotify:track:{ID}"),
        ]);
        let reading = parse_player_output(&output, DurationUnit::Seconds, false).unwrap();
        assert_eq!(reading.track.spotify_id, None);
        assert_eq!(reading.track.album, None);
        assert_eq!(reading.track.duration_ms, Some(200_000));
    }

    #[test]
    fn stopped_empty_and_short_output() {
        assert_eq!(
            parse_player_output("stopped\n", DurationUnit::Millis, true),
            None
        );
        assert_eq!(parse_player_output("", DurationUnit::Millis, true), None);
        assert_eq!(parse_player_output("\n", DurationUnit::Millis, true), None);
        assert_eq!(parse_player_output(SEP, DurationUnit::Millis, true), None);
        let short = line(&["playing", "Title", "Artist"]);
        assert_eq!(
            parse_player_output(&short, DurationUnit::Millis, true),
            None
        );
        let unknown = line(&["buffering", "T", "A", "B", "1", "1", ""]);
        assert_eq!(
            parse_player_output(&unknown, DurationUnit::Millis, true),
            None
        );
    }

    #[test]
    fn missing_values_and_bad_numbers() {
        let output = line(&[
            "playing",
            "Radio Stream",
            "missing value",
            "missing value",
            "missing value",
            "oops",
            "missing value",
        ]);
        let reading = parse_player_output(&output, DurationUnit::Seconds, true).unwrap();
        assert_eq!(reading.track.title, "Radio Stream");
        assert_eq!(reading.track.artist, "");
        assert_eq!(reading.track.album, None);
        assert_eq!(reading.track.duration_ms, None);
        assert_eq!(reading.track.spotify_id, None);
        assert_eq!(reading.position_ms, 0);
    }

    #[test]
    fn six_fields_without_id() {
        let output = ["playing", "T", "A", "B", "1000", "0,25"].join(SEP);
        let reading = parse_player_output(&output, DurationUnit::Millis, true).unwrap();
        assert_eq!(reading.track.spotify_id, None);
        assert_eq!(reading.position_ms, 250);
        assert_eq!(reading.track.duration_ms, Some(1_000));
    }

    #[test]
    fn negative_zero_and_huge_numbers() {
        let output = line(&["playing", "T", "A", "B", "-5", "-3", ""]);
        let reading = parse_player_output(&output, DurationUnit::Millis, false).unwrap();
        assert_eq!(reading.track.duration_ms, None);
        assert_eq!(reading.position_ms, 0);

        let output = line(&["playing", "T", "A", "B", "0", "0", ""]);
        let reading = parse_player_output(&output, DurationUnit::Seconds, false).unwrap();
        assert_eq!(reading.track.duration_ms, None);

        let output = line(&["playing", "T", "A", "B", "1E+300", "1E+300", ""]);
        let reading = parse_player_output(&output, DurationUnit::Seconds, false).unwrap();
        assert_eq!(reading.track.duration_ms, Some(u64::MAX));
        assert_eq!(reading.position_ms, u64::MAX);
    }

    #[test]
    fn position_reported_in_ms_is_detected() {
        // 61500 "seconds" in a 215 s song is really 61.5 s.
        let output = line(&["playing", "T", "A", "B", "215000", "61500", ""]);
        let reading = parse_player_output(&output, DurationUnit::Millis, true).unwrap();
        assert_eq!(reading.position_ms, 61_500);
        // A position slightly past the end stays as it is.
        let output = line(&["playing", "T", "A", "B", "215000", "216", ""]);
        let reading = parse_player_output(&output, DurationUnit::Millis, true).unwrap();
        assert_eq!(reading.position_ms, 216_000);
    }

    #[test]
    fn unicode_titles_and_crlf() {
        let output = [
            "playing",
            "夜に駆ける",
            "YOASOBI",
            "THE BOOK",
            "261000",
            "3,5",
            "",
        ]
        .join(SEP)
            + "\r\n";
        let reading = parse_player_output(&output, DurationUnit::Millis, true).unwrap();
        assert_eq!(reading.track.title, "夜に駆ける");
        assert_eq!(reading.track.album.as_deref(), Some("THE BOOK"));
        assert_eq!(reading.position_ms, 3_500);
        // Spaces inside titles are kept.
        let output = line(&["paused", "  Intro  ", "A", "B", "1000", "0", ""]);
        let reading = parse_player_output(&output, DurationUnit::Millis, true).unwrap();
        assert_eq!(reading.track.title, "  Intro  ");
    }

    #[test]
    fn reading_into_snapshot() {
        let read_at = Instant::now();
        let reading = Reading {
            status: PlaybackStatus::Paused,
            track: Track {
                title: "T".into(),
                ..Track::default()
            },
            position_ms: 42,
        };
        let snapshot = reading.into_snapshot("com.apple.Music", read_at);
        assert_eq!(snapshot.app_id, "com.apple.Music");
        assert_eq!(snapshot.position_at, read_at);
        assert_eq!(snapshot.position_ms, 42);
        assert_eq!(snapshot.rate, 1.0);
        assert_eq!(snapshot.status, PlaybackStatus::Paused);
    }

    #[test]
    fn log_limiter_once_per_interval() {
        let limiter = LogLimiter::new(Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow(start));
        assert!(!limiter.allow(start));
        assert!(!limiter.allow(start + Duration::from_secs(59)));
        assert!(limiter.allow(start + Duration::from_secs(60)));
        assert!(!limiter.allow(start + Duration::from_secs(61)));
        // A clock reading earlier than the last message never lets one through.
        assert!(!limiter.allow(start));
        assert!(limiter.allow(start + Duration::from_secs(121)));

        let never_twice = LogLimiter::new(Duration::MAX);
        assert!(never_twice.allow(start));
        assert!(!never_twice.allow(start + Duration::from_secs(1_000_000)));
        let always = LogLimiter::new(Duration::ZERO);
        assert!(always.allow(start));
        assert!(always.allow(start));
    }

    #[test]
    fn running_output() {
        assert_eq!(parse_running("true\u{1f}false\n", 2), vec![true, false]);
        assert_eq!(parse_running("false\u{1f}TRUE", 2), vec![false, true]);
        assert_eq!(parse_running("true", 2), vec![true, false]);
        assert_eq!(parse_running("", 2), vec![false, false]);
        assert_eq!(
            parse_running("true\u{1f}true\u{1f}true", 2),
            vec![true, true]
        );
        assert_eq!(parse_running("yes\u{1f}1", 2), vec![false, false]);
        assert_eq!(parse_running("true", 0), Vec::<bool>::new());
    }

    #[test]
    fn scripts_never_tell_an_app_that_is_not_running() {
        let apps: Vec<&ScriptedApp> = APPS.iter().collect();
        let check = running_check_script(&apps);
        assert!(check.iter().all(|line| !line.contains("tell application")));
        assert!(check.iter().any(|line| line.contains("is running")));
        assert!(check.last().unwrap().contains("isRunning(\"Spotify\")"));
        assert!(check.last().unwrap().contains("isRunning(\"Music\")"));
        assert!(check.last().unwrap().contains("character id 31"));

        for app in &APPS {
            let query = query_script(app);
            assert_eq!(
                query[0],
                format!("if application \"{}\" is running then", app.name)
            );
            assert_eq!(query[1], format!("tell application \"{}\"", app.name));
            assert!(query
                .iter()
                .all(|line| !line.is_empty() && !line.contains('\n')));
            assert_eq!(
                query.iter().any(|line| line.contains("id of t")),
                app.spotify_ids
            );

            let artwork = artwork_script(app.name);
            assert_eq!(
                artwork[0],
                format!("if application \"{}\" is running then", app.name)
            );
            assert_eq!(artwork[1], format!("tell application \"{}\"", app.name));
            assert!(artwork
                .iter()
                .all(|line| !line.is_empty() && !line.contains('\n')));
        }
    }

    // ---- cover art -------------------------------------------------------------------

    #[test]
    fn artwork_urls_from_applescript() {
        let url = "https://i.scdn.co/image/ab67616d0000b273d9194aa18fa4c9362b47464f";
        assert_eq!(parse_artwork_url(&format!("{url}\n")).as_deref(), Some(url));
        assert_eq!(
            parse_artwork_url(" http://i.scdn.co/image/abc\r\n").as_deref(),
            Some("http://i.scdn.co/image/abc")
        );
        for output in [
            "",
            "\n",
            "missing value",
            "https://",
            "https://a b",
            "file:///tmp/cover.jpg",
            "spotify:image:abc",
            "javascript:alert(1)",
        ] {
            assert_eq!(parse_artwork_url(output), None, "{output:?}");
        }
    }

    #[test]
    fn only_spotify_has_a_scripted_cover() {
        assert_eq!(
            scripted_art("com.spotify.client"),
            ArtSource::Scripted("Spotify")
        );
        assert_eq!(scripted_art("com.apple.Music"), ArtSource::None);
        assert_eq!(scripted_art("com.google.Chrome"), ArtSource::None);
        assert_eq!(scripted_art(""), ArtSource::None);
    }

    // ---- mediaremote-adapter -------------------------------------------------------

    /// 2027-01-15T08:00:00Z
    const NOW_UNIX_SECS: u64 = 1_800_000_000;

    fn fixed_now() -> (SystemTime, Instant) {
        (
            UNIX_EPOCH + Duration::from_secs(NOW_UNIX_SECS),
            Instant::now(),
        )
    }

    fn adapter(json: &str) -> Option<PlaybackSnapshot> {
        let (now_sys, now) = fixed_now();
        parse_adapter_output(json, &[], now_sys, now)
            .unwrap()
            .map(|entry| entry.snapshot)
    }

    fn age_of(json: &str) -> Duration {
        let (now_sys, now) = fixed_now();
        let snapshot = parse_adapter_output(json, &[], now_sys, now)
            .unwrap()
            .unwrap()
            .snapshot;
        now.duration_since(snapshot.position_at)
    }

    #[test]
    fn adapter_full_record_with_iso_timestamp() {
        let (now_sys, now) = fixed_now();
        let json = r#"{
            "bundleIdentifier": "com.spotify.client",
            "playing": true,
            "title": "Song Title",
            "artist": "Artist",
            "album": "Album",
            "duration": 215.672,
            "elapsedTime": 61.5,
            "timestamp": "2027-01-15T07:59:58Z",
            "playbackRate": 1,
            "artworkMimeType": "image/jpeg",
            "artworkData": "/9j/4AAQ"
        }"#;
        let entry = parse_adapter_output(json, &[], now_sys, now)
            .unwrap()
            .unwrap();
        assert_eq!(
            entry.artwork,
            Some(AdapterArtwork {
                data: "/9j/4AAQ".into(),
                mime: Some("image/jpeg".into()),
            })
        );
        let snapshot = entry.snapshot;
        assert_eq!(
            snapshot.track,
            Track {
                title: "Song Title".into(),
                artist: "Artist".into(),
                album: Some("Album".into()),
                duration_ms: Some(215_672),
                spotify_id: None,
            }
        );
        assert_eq!(snapshot.status, PlaybackStatus::Playing);
        assert_eq!(snapshot.position_ms, 61_500);
        assert_eq!(
            now.duration_since(snapshot.position_at),
            Duration::from_secs(2)
        );
        assert_eq!(snapshot.rate, 1.0);
        assert_eq!(snapshot.app_id, "com.spotify.client");
    }

    #[test]
    fn adapter_nothing_playing() {
        assert_eq!(adapter(""), None);
        assert_eq!(adapter("  \n"), None);
        assert_eq!(adapter("null\n"), None);
        assert_eq!(adapter("{}"), None);
        assert_eq!(adapter(r#"{"type":"data","payload":null}"#), None);
        assert_eq!(adapter(r#"{"type":"data","payload":{}}"#), None);
        assert_eq!(
            adapter(r#"{"playing":true,"title":"","artist":"  "}"#),
            None
        );
    }

    #[test]
    fn adapter_bad_output_is_an_error() {
        let (now_sys, now) = fixed_now();
        for output in [
            "not json",
            "{\"title\":",
            "[1,2,3]",
            "\"text\"",
            "42",
            "{\"payload\": 5}",
        ] {
            assert!(
                parse_adapter_output(output, &[], now_sys, now).is_err(),
                "{output}"
            );
        }
    }

    #[test]
    fn adapter_stream_payload_wrapper() {
        let snapshot = adapter(
            r#"{"type":"data","diff":false,"payload":{"title":"T","artist":"A","playing":false,"bundleIdentifier":"com.apple.Music"}}"#,
        )
        .unwrap();
        assert_eq!(snapshot.track.title, "T");
        assert_eq!(snapshot.status, PlaybackStatus::Paused);
        assert_eq!(snapshot.app_id, "com.apple.Music");
    }

    #[test]
    fn adapter_missing_fields_use_defaults() {
        let (_, now) = fixed_now();
        let snapshot = adapter(r#"{"title":"Only a title"}"#).unwrap();
        assert_eq!(snapshot.track.artist, "");
        assert_eq!(snapshot.track.album, None);
        assert_eq!(snapshot.track.duration_ms, None);
        assert_eq!(snapshot.status, PlaybackStatus::Paused);
        assert_eq!(snapshot.position_ms, 0);
        assert!(snapshot.position_at >= now);
        assert_eq!(snapshot.rate, 1.0);
        assert_eq!(snapshot.app_id, "");
    }

    #[test]
    fn adapter_wrong_types_are_tolerated() {
        let snapshot = adapter(
            r#"{"title":"T","artist":7,"album":null,"duration":"215,5","elapsedTime":"10,25","playing":"true","playbackRate":"1.0","bundleIdentifier":["x"]}"#,
        )
        .unwrap();
        assert_eq!(snapshot.track.artist, "");
        assert_eq!(snapshot.track.album, None);
        assert_eq!(snapshot.track.duration_ms, Some(215_500));
        assert_eq!(snapshot.position_ms, 10_250);
        assert_eq!(snapshot.status, PlaybackStatus::Playing);
        assert_eq!(snapshot.app_id, "");
    }

    #[test]
    fn adapter_playing_from_rate_when_missing() {
        let snapshot = adapter(r#"{"title":"T","playbackRate":1.5}"#).unwrap();
        assert_eq!(snapshot.status, PlaybackStatus::Playing);
        assert_eq!(snapshot.rate, 1.5);
        let snapshot = adapter(r#"{"title":"T","playbackRate":0}"#).unwrap();
        assert_eq!(snapshot.status, PlaybackStatus::Paused);
        assert_eq!(snapshot.rate, 1.0);
        // Paused players often report rate 0: still 1.0 for when they resume.
        let snapshot = adapter(r#"{"title":"T","playing":false,"playbackRate":0}"#).unwrap();
        assert_eq!(snapshot.rate, 1.0);
        let snapshot = adapter(r#"{"title":"T","playing":1,"playbackRate":-1}"#).unwrap();
        assert_eq!(snapshot.status, PlaybackStatus::Playing);
        assert_eq!(snapshot.rate, 1.0);
    }

    #[test]
    fn adapter_parent_bundle_identifier_names_the_app() {
        let snapshot =
            adapter(r#"{"title":"Video","parentApplicationBundleIdentifier":"com.google.Chrome"}"#)
                .unwrap();
        assert_eq!(snapshot.app_id, "com.google.Chrome");
        // A helper process is reported as the app that owns it.
        let snapshot = adapter(
            r#"{"title":"Video","bundleIdentifier":"com.apple.WebKit.GPU","parentApplicationBundleIdentifier":"com.apple.Safari"}"#,
        )
        .unwrap();
        assert_eq!(snapshot.app_id, "com.apple.Safari");
        // A blank parent does not hide the bundle id.
        let snapshot = adapter(
            r#"{"title":"Song","bundleIdentifier":"com.spotify.client","parentApplicationBundleIdentifier":"  "}"#,
        )
        .unwrap();
        assert_eq!(snapshot.app_id, "com.spotify.client");
    }

    #[test]
    fn adapter_blocked_bundle_or_parent_is_nothing() {
        let (now_sys, now) = fixed_now();
        let blocked =
            |patterns: &[&str]| -> Vec<String> { patterns.iter().map(|p| p.to_string()).collect() };
        let helper = r#"{"title":"Video","playing":true,"bundleIdentifier":"com.apple.WebKit.GPU","parentApplicationBundleIdentifier":"com.apple.Safari"}"#;
        for patterns in [&["SAFARI"][..], &["webkit"], &["x", " apple "]] {
            assert_eq!(
                parse_adapter_output(helper, &blocked(patterns), now_sys, now).unwrap(),
                None,
                "{patterns:?}"
            );
        }
        let kept = parse_adapter_output(helper, &blocked(&["chrome", "", "  "]), now_sys, now)
            .unwrap()
            .unwrap();
        assert_eq!(kept.snapshot.app_id, "com.apple.Safari");

        let plain = r#"{"title":"Song","bundleIdentifier":"com.spotify.client"}"#;
        assert_eq!(
            parse_adapter_output(plain, &blocked(&["Spotify"]), now_sys, now).unwrap(),
            None
        );
        // Blocking never turns bad output into "nothing": it is still an error.
        assert!(parse_adapter_output("not json", &blocked(&["spotify"]), now_sys, now).is_err());
    }

    #[test]
    fn adapter_artwork_fields() {
        let (now_sys, now) = fixed_now();
        let artwork = |json: &str| {
            parse_adapter_output(json, &[], now_sys, now)
                .unwrap()
                .unwrap()
                .artwork
        };
        assert_eq!(
            artwork(r#"{"title":"T","artworkData":"R0lGODlh"}"#),
            Some(AdapterArtwork {
                data: "R0lGODlh".into(),
                mime: None,
            })
        );
        assert_eq!(
            artwork(r#"{"title":"T","artworkData":"R0lGODlh","artworkMimeType":" "}"#)
                .unwrap()
                .mime,
            None
        );
        for json in [
            r#"{"title":"T"}"#,
            r#"{"title":"T","artworkMimeType":"image/png"}"#,
            r#"{"title":"T","artworkData":"","artworkMimeType":"image/png"}"#,
            r#"{"title":"T","artworkData":"  ","artworkMimeType":"image/png"}"#,
            r#"{"title":"T","artworkData":null}"#,
            r#"{"title":"T","artworkData":[1,2]}"#,
        ] {
            assert_eq!(artwork(json), None, "{json}");
        }
    }

    #[test]
    fn adapter_numeric_timestamps() {
        let unix = NOW_UNIX_SECS as f64 - 3.0;
        // Seconds since 1970.
        let json = format!(r#"{{"title":"T","elapsedTime":1,"timestamp":{unix}}}"#);
        assert_eq!(age_of(&json), Duration::from_secs(3));
        // Seconds since 2001 (Apple's reference date).
        let apple = unix - 978_307_200.0;
        let json = format!(r#"{{"title":"T","elapsedTime":1,"timestamp":{apple}}}"#);
        assert_eq!(age_of(&json), Duration::from_secs(3));
        // Milliseconds and microseconds since 1970.
        let json = format!(
            r#"{{"title":"T","elapsedTime":1,"timestamp":{}}}"#,
            unix * 1000.0
        );
        assert_eq!(age_of(&json), Duration::from_secs(3));
        let json = format!(
            r#"{{"title":"T","elapsedTime":1,"timestamp":{}}}"#,
            unix * 1_000_000.0
        );
        assert_eq!(age_of(&json), Duration::from_secs(3));
        // As a numeric string, with a decimal comma.
        let json = format!(
            r#"{{"title":"T","elapsedTime":1,"timestamp":"{}"}}"#,
            (unix + 0.5).to_string().replace('.', ",")
        );
        assert_eq!(age_of(&json), Duration::from_millis(2_500));
        // timestampEpochMicros wins over timestamp.
        let json = format!(
            r#"{{"title":"T","elapsedTime":1,"timestamp":{unix},"timestampEpochMicros":{}}}"#,
            (unix - 1.0) * 1_000_000.0
        );
        assert_eq!(age_of(&json), Duration::from_secs(4));
    }

    #[test]
    fn adapter_untrusted_timestamps_mean_now() {
        for timestamp in [
            "0",
            "-5",
            "1e300",
            "\"yesterday\"",
            "\"\"",
            "null",
            "true",
            "{}",
            // Two days old.
            "1799827200",
            // In the future.
            "\"2027-01-15T09:00:00Z\"",
            "\"2020-01-01T00:00:00Z\"",
        ] {
            let json = format!(r#"{{"title":"T","elapsedTime":5,"timestamp":{timestamp}}}"#);
            assert_eq!(age_of(&json), Duration::ZERO, "{timestamp}");
        }
        // No timestamp at all.
        assert_eq!(age_of(r#"{"title":"T","elapsedTime":5}"#), Duration::ZERO);
    }

    #[test]
    fn adapter_elapsed_time_now_wins() {
        let json = r#"{"title":"T","elapsedTime":5,"elapsedTimeNow":7.25,"timestamp":"2027-01-15T07:59:00Z"}"#;
        let snapshot = adapter(json).unwrap();
        assert_eq!(snapshot.position_ms, 7_250);
        assert_eq!(age_of(json), Duration::ZERO);
    }

    #[test]
    fn adapter_negative_and_huge_values() {
        let snapshot = adapter(r#"{"title":"T","duration":-1,"elapsedTime":-3}"#).unwrap();
        assert_eq!(snapshot.track.duration_ms, None);
        assert_eq!(snapshot.position_ms, 0);
        let snapshot = adapter(r#"{"title":"T","duration":1e300,"elapsedTime":1e300}"#).unwrap();
        assert_eq!(snapshot.track.duration_ms, Some(u64::MAX));
        assert_eq!(snapshot.position_ms, u64::MAX);
    }

    #[test]
    fn adapter_unicode() {
        let snapshot =
            adapter(r#"{"title":"Ëñçødîñg 🎵","artist":"Ŝíñgér","playing":true}"#).unwrap();
        assert_eq!(snapshot.track.title, "Ëñçødîñg 🎵");
        assert_eq!(snapshot.track.artist, "Ŝíñgér");
    }

    // ---- ISO-8601 -------------------------------------------------------------------

    #[test]
    fn iso8601_forms() {
        let base = 1_748_266_461_000_i64; // 2025-05-26T13:34:21Z
        assert_eq!(parse_iso8601_ms("2025-05-26T13:34:21Z"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26T13:34:21z"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26T13:34:21"), Some(base));
        assert_eq!(parse_iso8601_ms(" 2025-05-26t13:34:21Z\n"), Some(base));
        assert_eq!(
            parse_iso8601_ms("2025-05-26T13:34:21.123Z"),
            Some(base + 123)
        );
        assert_eq!(parse_iso8601_ms("2025-05-26T13:34:21.1Z"), Some(base + 100));
        assert_eq!(
            parse_iso8601_ms("2025-05-26T13:34:21.123456789Z"),
            Some(base + 123)
        );
        assert_eq!(parse_iso8601_ms("2025-05-26T13:34:21,5Z"), Some(base + 500));
        assert_eq!(parse_iso8601_ms("2025-05-26T15:34:21+02:00"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26T15:34:21+0200"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26T15:34:21+02"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26T08:04:21-05:30"), Some(base));
        assert_eq!(parse_iso8601_ms("2025-05-26 13:34:21 +0000"), Some(base));
        assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_ms("1969-12-31T23:59:59Z"), Some(-1_000));
        assert_eq!(
            parse_iso8601_ms("2001-01-01T00:00:00Z"),
            Some(978_307_200_000)
        );
        assert_eq!(
            parse_iso8601_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(
            parse_iso8601_ms("2000-02-29T00:00:00Z"),
            Some(951_782_400_000)
        );
        assert_eq!(
            parse_iso8601_ms("2016-12-31T23:59:60Z"),
            Some(1_483_228_800_000)
        );
        assert_eq!(
            parse_iso8601_ms("0000-03-01T00:00:00Z"),
            Some(-62_162_035_200_000)
        );
        assert_eq!(
            parse_iso8601_ms("9999-12-31T23:59:59Z"),
            Some(253_402_300_799_000)
        );
    }

    #[test]
    fn iso8601_rejects_invalid() {
        for text in [
            "",
            "2025",
            "2025-05-26",
            "2025-05-26T13:34",
            "2025-13-01T00:00:00Z",
            "2025-00-01T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2025-04-31T00:00:00Z",
            "2025-05-00T00:00:00Z",
            "2025-05-26T24:00:00Z",
            "2025-05-26T13:60:00Z",
            "2025-05-26T13:34:61Z",
            "2025-05-26T13:34:21.Z",
            "2025-05-26T13:34:21+25:00",
            "2025-05-26T13:34:21+02:61",
            "2025-05-26T13:34:21+2",
            "2025-05-26T13:34:21Q",
            "2025-05-26T13:34:21Z garbage",
            "2025/05/26T13:34:21Z",
            "２０２５-05-26T13:34:21Z",
            "2025-05-26T13:34:2１Z",
            "2025-05-26T13:34:21+",
            "+2025-05-26T13:34:21Z",
            "yesterday",
            "🎵🎵🎵🎵-05-26T13:34:21Z",
        ] {
            assert_eq!(parse_iso8601_ms(text), None, "{text}");
        }
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2025, 13), 0);
    }

    #[test]
    fn timestamp_heuristics() {
        let now_ms = 1_800_000_000_000.0;
        assert_eq!(
            timestamp_unix_ms(&Value::from(1_800_000_000.0), now_ms),
            Some(now_ms)
        );
        assert_eq!(
            timestamp_unix_ms(&Value::from(1_800_000_000.0 - 978_307_200.0), now_ms),
            Some(now_ms)
        );
        assert_eq!(
            timestamp_unix_ms(&Value::from(1_800_000_000_000_u64), now_ms),
            Some(now_ms)
        );
        assert_eq!(timestamp_unix_ms(&Value::from(12), now_ms), None);
        assert_eq!(timestamp_unix_ms(&Value::Null, now_ms), None);
        assert_eq!(timestamp_unix_ms(&Value::from("soon"), now_ms), None);
        // An ISO date is returned as is, however old: the age check happens later.
        assert_eq!(
            timestamp_unix_ms(&Value::from("1970-01-01T00:00:00Z"), now_ms),
            Some(0.0)
        );
    }

    // ---- processes (local, hermetic) -----------------------------------------------

    /// Writes an executable `#!/bin/sh` script running `body` to `dir/name`
    /// and returns its path.
    ///
    /// A child shell writes the file, never this process. Tests run on many
    /// threads, and while this process holds a file open for writing, every
    /// process another thread starts inherits that descriptor until its own
    /// exec. Running the script while such a copy is open fails with ETXTBSY
    /// ("Text file busy"). The child's descriptor is closed once it exits,
    /// before this returns.
    #[cfg(unix)]
    fn fake_program(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        let written = std::process::Command::new("/bin/sh")
            .args(["-c", r#"printf '%s\n' "$1" > "$2" && chmod 755 "$2""#, "sh"])
            .arg(format!("#!/bin/sh\n{body}"))
            .arg(&path)
            .status()
            .unwrap();
        assert!(written.success(), "could not write {}", path.display());
        path.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn fake_programs_run_their_body() {
        let dir = tempfile::tempdir().unwrap();
        let body = r#"printf '%s|%s\n' "$1" '100% \n "quoted"'"#;
        let program = fake_program(dir.path(), "fake", body);
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            format!("#!/bin/sh\n{body}\n")
        );
        let output = std::process::Command::new(&program)
            .arg("arg")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "arg|100% \\n \"quoted\"\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_returns_stdout() {
        let out = run(
            "/bin/sh",
            &[OsStr::new("-c"), OsStr::new("printf 'héllo'")],
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(out, "héllo");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_failure_quotes_stderr() {
        let err = run(
            "/bin/sh",
            &[
                OsStr::new("-c"),
                OsStr::new("echo 'execution error: nope' >&2; exit 3"),
            ],
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("execution error: nope"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_kills_slow_programs() {
        let started = Instant::now();
        let err = run(
            "/bin/sh",
            &[OsStr::new("-c"), OsStr::new("sleep 10")],
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("did not finish"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn run_missing_program_is_an_error() {
        let err = run(
            "/definitely/not/here/osascript",
            &[],
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("could not run"), "{err}");
    }

    /// A stand-in for osascript: answers the running check, Spotify's cover
    /// art, then each app's query.
    #[cfg(unix)]
    const FAKE_OSASCRIPT: &str = r#"case "$*" in
  *"on isRunning"*) printf 'true\037true\n' ;;
  *"artwork url"*) printf 'https://i.scdn.co/image/ab67616d0000b273\n' ;;
  *'tell application "Spotify"'*) printf 'playing\037Song\037Artist\037Album\037215000\03712,5\037spotify:track:4uLU6hMCjMI75M1A2tKUQC\n' ;;
  *'tell application "Music"'*) printf 'paused\037Other Song\037Someone\037\037200,5\0373\037\n' ;;
  *) exit 1 ;;
esac"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_apps_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(dir.path(), "osascript", FAKE_OSASCRIPT);

        let read = read_scripted_apps(&osascript, &[]).await.unwrap();
        assert!(read.unreadable.is_none());
        let snapshots = read.snapshots;
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].app_id, "com.spotify.client");
        assert_eq!(snapshots[0].status, PlaybackStatus::Playing);
        assert_eq!(snapshots[0].position_ms, 12_500);
        assert_eq!(snapshots[0].track.duration_ms, Some(215_000));
        assert_eq!(snapshots[0].track.spotify_id.as_deref(), Some(ID));
        assert_eq!(snapshots[1].app_id, "com.apple.Music");
        assert_eq!(snapshots[1].status, PlaybackStatus::Paused);
        assert_eq!(snapshots[1].track.duration_ms, Some(200_500));
        assert_eq!(snapshots[1].track.album, None);
        assert_eq!(snapshots[1].position_ms, 3_000);

        let picked = choose(snapshots, &[], &[]).unwrap();
        assert_eq!(picked.app_id, "com.spotify.client");

        // A blocked app is never asked anything.
        let snapshots = read_scripted_apps(&osascript, &["spotify".to_string()])
            .await
            .unwrap()
            .snapshots;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].app_id, "com.apple.Music");

        // Everything blocked: osascript is not even started.
        let read = read_scripted_apps(
            "/definitely/not/here",
            &["spotify".to_string(), "music".to_string()],
        )
        .await
        .unwrap();
        assert!(read.snapshots.is_empty());
        assert!(read.unreadable.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_apps_not_running_are_not_queried() {
        let dir = tempfile::tempdir().unwrap();
        // Any query (anything but the running check) fails loudly.
        let osascript = fake_program(
            dir.path(),
            "osascript",
            r#"case "$*" in *"on isRunning"*) printf 'false\037false\n' ;; *) exit 7 ;; esac"#,
        );
        let read = read_scripted_apps(&osascript, &[]).await.unwrap();
        assert!(read.snapshots.is_empty());
        assert!(read.unreadable.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_app_failure_skips_only_that_app() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(
            dir.path(),
            "osascript",
            r#"case "$*" in
  *"on isRunning"*) printf 'true\037true\n' ;;
  *'tell application "Spotify"'*) echo "Spotify got an error" >&2; exit 1 ;;
  *) printf 'playing\037T\037A\037B\037100\0371\037\n' ;;
esac"#,
        );
        let read = read_scripted_apps(&osascript, &[]).await.unwrap();
        assert_eq!(read.snapshots.len(), 1);
        assert_eq!(read.snapshots[0].app_id, "com.apple.Music");
        // Something could be read: Spotify's failure is not reported.
        assert!(read.unreadable.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_apps_running_check_failure_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(dir.path(), "osascript", "exit 1");
        assert!(read_scripted_apps(&osascript, &[]).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn adapter_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let perl = fake_program(
            dir.path(),
            "perl",
            r#"case "$1|$2|$3" in
  */bin/mediaremote-adapter.pl\|*/build/MediaRemoteAdapter.framework\|get)
    printf '{"bundleIdentifier":"com.google.Chrome","playing":true,"title":"Video","artist":"Channel","duration":100,"elapsedTime":4}\n' ;;
  *) exit 2 ;;
esac"#,
        );
        let snapshot = read_adapter(&perl, Path::new("/opt/mediaremote-adapter"), &[])
            .await
            .unwrap()
            .unwrap()
            .snapshot;
        assert_eq!(snapshot.app_id, "com.google.Chrome");
        assert_eq!(snapshot.position_ms, 4_000);
        assert_eq!(snapshot.track.duration_ms, Some(100_000));

        let failing = fake_program(
            dir.path(),
            "perl-broken",
            "echo 'Can not locate' >&2; exit 2",
        );
        assert!(read_adapter(&failing, dir.path(), &[]).await.is_err());
    }

    /// Safari's media is reported by its WebKit helper process, with Safari as
    /// the parent app. Blocking "safari" must hide it, and the app is Safari.
    #[cfg(unix)]
    #[tokio::test]
    async fn adapter_helper_process_is_blocked_through_its_parent_app() {
        let dir = tempfile::tempdir().unwrap();
        let perl = fake_program(
            dir.path(),
            "perl",
            r#"printf '{"bundleIdentifier":"com.apple.WebKit.GPU","parentApplicationBundleIdentifier":"com.apple.Safari","playing":true,"title":"Private video","artist":"Channel"}\n'"#,
        );
        let osascript = fake_program(dir.path(), "osascript", "exit 9");
        let adapter_dir = Some(dir.path().to_path_buf());

        let source = MacSource::new(adapter_dir.clone(), vec![], vec!["safari".into()]);
        assert_eq!(source.read_with(&perl, &osascript).await.unwrap(), None);

        // Blocking the helper itself works too.
        let source = MacSource::new(adapter_dir.clone(), vec![], vec!["WebKit".into()]);
        assert_eq!(source.read_with(&perl, &osascript).await.unwrap(), None);

        let source = MacSource::new(adapter_dir.clone(), vec![], vec!["chrome".into()]);
        let snapshot = source.read_with(&perl, &osascript).await.unwrap().unwrap();
        assert_eq!(snapshot.app_id, "com.apple.Safari");
        assert_eq!(snapshot.track.title, "Private video");
    }

    /// When every running app refuses to answer (for example because Lyrix
    /// was denied the Automation permission), the failure is reported so it
    /// can be logged, but the snapshot is still "nothing playing": an error
    /// would keep a stale status on screen.
    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_apps_all_failing_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(
            dir.path(),
            "osascript",
            r#"case "$*" in
  *"on isRunning"*) printf 'true\037true\n' ;;
  *) echo "execution error: Not authorized to send Apple events to Spotify. (-1743)" >&2; exit 1 ;;
esac"#,
        );
        let read = read_scripted_apps(&osascript, &[]).await.unwrap();
        assert!(read.snapshots.is_empty());
        let err = read.unreadable.expect("the failure is reported");
        let message = format!("{err:#}");
        assert!(message.contains("-1743"), "{message}");
        assert!(message.contains("Spotify"), "{message}");

        let source = MacSource::new(None, vec![], vec![]);
        assert_eq!(
            source
                .read_with("/definitely/not/perl", &osascript)
                .await
                .unwrap(),
            None
        );
        // The warning was logged once and is now held back for a minute.
        assert!(!source.failure_warnings.allow(Instant::now()));
    }

    /// One app failing while another answers (even with "stopped") is fine as
    /// long as something could be read.
    #[cfg(unix)]
    #[tokio::test]
    async fn scripted_apps_one_failing_one_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(
            dir.path(),
            "osascript",
            r#"case "$*" in
  *"on isRunning"*) printf 'true\037true\n' ;;
  *'tell application "Spotify"'*) echo "Spotify got an error" >&2; exit 1 ;;
  *) printf 'stopped\n' ;;
esac"#,
        );
        // Music answered "stopped": nothing is playing there, and Spotify could not
        // be read, so the failure is reported (it may be what is playing).
        let read = read_scripted_apps(&osascript, &[]).await.unwrap();
        assert!(read.snapshots.is_empty());
        assert!(read.unreadable.is_some());
        // With Spotify blocked, Music's "stopped" is a valid answer: nothing plays.
        let read = read_scripted_apps(&osascript, &["spotify".to_string()])
            .await
            .unwrap();
        assert!(read.snapshots.is_empty());
        assert!(read.unreadable.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mac_source_falls_back_to_applescript_when_the_adapter_fails() {
        let dir = tempfile::tempdir().unwrap();
        let perl = fake_program(dir.path(), "perl", "echo 'Can not locate' >&2; exit 2");
        let osascript = fake_program(dir.path(), "osascript", FAKE_OSASCRIPT);
        let source = MacSource::new(Some(dir.path().to_path_buf()), vec![], vec![]);
        let snapshot = source.read_with(&perl, &osascript).await.unwrap().unwrap();
        assert_eq!(snapshot.app_id, "com.spotify.client");

        // Preferences apply among the scripted apps too (both are not equal here:
        // Spotify plays, Music is paused, so status still wins).
        let source = MacSource::new(None, vec!["music".into()], vec![]);
        let snapshot = source.read_with(&perl, &osascript).await.unwrap().unwrap();
        assert_eq!(snapshot.app_id, "com.spotify.client");

        // An adapter that reports nothing means nothing plays: no AppleScript.
        let perl_null = fake_program(dir.path(), "perl-null", "printf 'null\\n'");
        let source = MacSource::new(Some(dir.path().to_path_buf()), vec![], vec![]);
        assert_eq!(
            source.read_with(&perl_null, &osascript).await.unwrap(),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artwork_of_the_picked_app() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(dir.path(), "osascript", FAKE_OSASCRIPT);
        let no_osascript = "/definitely/not/osascript";

        let source = MacSource::new(None, vec![], vec![]);
        assert_eq!(source.artwork_with(no_osascript).await.unwrap(), None);

        // Spotify plays: its cover URL is asked from Spotify.
        let picked = source.read_with(no_osascript, &osascript).await.unwrap();
        assert_eq!(picked.unwrap().app_id, "com.spotify.client");
        assert_eq!(
            source.artwork_with(&osascript).await.unwrap().as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273")
        );
        assert!(source.artwork_with(no_osascript).await.is_err());

        // Apple Music: no cover, and osascript is not even started.
        let source = MacSource::new(None, vec![], vec!["spotify".into()]);
        let picked = source.read_with(no_osascript, &osascript).await.unwrap();
        assert_eq!(picked.unwrap().app_id, "com.apple.Music");
        assert_eq!(source.artwork_with(no_osascript).await.unwrap(), None);

        // Nothing playing (everything blocked): no cover either.
        let source = MacSource::new(None, vec![], vec!["spotify".into(), "music".into()]);
        assert_eq!(
            source.read_with(no_osascript, &osascript).await.unwrap(),
            None
        );
        assert_eq!(source.artwork_with(no_osascript).await.unwrap(), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artwork_from_the_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let osascript = fake_program(dir.path(), "osascript", FAKE_OSASCRIPT);
        let no_osascript = "/definitely/not/osascript";
        let adapter_dir = Some(dir.path().to_path_buf());

        // A PNG, with the type the adapter reports.
        let perl = fake_program(
            dir.path(),
            "perl",
            r#"printf '{"bundleIdentifier":"com.google.Chrome","playing":true,"title":"Video","artist":"Channel","artworkMimeType":"image/png","artworkData":"iVBORw0KGgoAAAANSUhEUg=="}\n'"#,
        );
        let source = MacSource::new(adapter_dir.clone(), vec![], vec![]);
        source
            .read_with(&perl, no_osascript)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            source.artwork_with(no_osascript).await.unwrap().as_deref(),
            Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==")
        );

        // Not an image: nothing.
        let perl = fake_program(
            dir.path(),
            "perl-text",
            r#"printf '{"title":"Video","playing":true,"artworkMimeType":"text/html","artworkData":"PGh0bWw+"}\n'"#,
        );
        source
            .read_with(&perl, no_osascript)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(source.artwork_with(no_osascript).await.unwrap(), None);

        // Spotify through the adapter without artwork data: asked from Spotify.
        let perl = fake_program(
            dir.path(),
            "perl-spotify",
            r#"printf '{"bundleIdentifier":"com.spotify.client","playing":true,"title":"Song","artist":"Artist"}\n'"#,
        );
        source
            .read_with(&perl, no_osascript)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            source.artwork_with(&osascript).await.unwrap().as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273")
        );

        // Nothing playing: no cover.
        let perl = fake_program(dir.path(), "perl-null", "printf 'null\\n'");
        assert_eq!(source.read_with(&perl, no_osascript).await.unwrap(), None);
        assert_eq!(source.artwork_with(no_osascript).await.unwrap(), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn mac_source_without_osascript_is_an_error() {
        // Neither the adapter (nothing installed there) nor osascript (not macOS) works.
        let dir = tempfile::tempdir().unwrap();
        let source = MacSource::new(Some(dir.path().to_path_buf()), vec![], vec![]);
        assert_eq!(source.name(), "macos");
        assert!(source.snapshot().await.is_err());
        let source = MacSource::new(None, vec![], vec![]);
        assert!(source.snapshot().await.is_err());
        // Everything blocked: nothing to ask, nothing playing.
        let source = MacSource::new(None, vec![], vec!["spotify".into(), "music".into()]);
        assert_eq!(source.snapshot().await.unwrap(), None);
    }
}
