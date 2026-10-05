//! User settings, stored as TOML in the platform config directory.
//!
//! Every field has a default, so a missing file or a file that only sets a few
//! values both work. Unknown keys are ignored so older builds can read newer files.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Shown whenever an Advanced mode option is turned on or used.
pub const BAN_WARNING: &str = "USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.";

/// The Discord application Lyrix uses for Rich Presence until the project's own
/// application id is filled in. Empty means "not configured".
pub const DEFAULT_DISCORD_CLIENT_ID: &str = "";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub status: StatusConfig,
    pub privacy: PrivacyConfig,
    pub lyrics: LyricsConfig,
    pub sources: SourcesConfig,
    pub discord: DiscordConfig,
    pub console: ConsoleConfig,
    pub advanced: AdvancedConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// How often the now-playing source is read, in ms.
    pub poll_interval_ms: u64,
    /// Shifts every song's lyrics, in ms. Positive shows lines later.
    pub offset_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusConfig {
    /// Used while a lyric line is sung. Placeholders: {line} {next} {title} {artist} {album}.
    pub line_template: String,
    /// Used when no lyrics are found, while they load, and in title-only mode.
    pub no_lyrics_template: String,
    /// Shown during the intro and instrumental breaks.
    pub instrumental_text: String,
    /// Keep the status while the music is paused (otherwise it is cleared).
    pub show_when_paused: bool,
    /// Mask swear words before they reach any target.
    pub profanity_filter: bool,
    /// Words to mask. Empty uses the built-in list.
    pub profanity_words: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    /// Players to ignore, matched case-insensitively as a substring of the app id
    /// (e.g. `chrome`, `firefox`, `vlc`).
    pub blocked_apps: Vec<String>,
    /// Artists to never show, matched case-insensitively on the whole cleaned name.
    pub blocked_artists: Vec<String>,
    /// Show only the song, never lyric lines.
    pub title_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricsConfig {
    /// Folder of your own `.lrc` (and `.txt`) files, searched before any website.
    /// Defaults to `<data dir>/lyrics`.
    pub lyrics_dir: Option<PathBuf>,
    /// Keep every lyric found on disk so songs load instantly next time.
    pub cache: bool,
    /// Use LRCLIB (free, open, no account).
    pub lrclib: bool,
    /// LRCLIB server, in case you run your own copy.
    pub lrclib_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    /// Players to prefer when several are playing, matched like `blocked_apps`.
    pub preferred_apps: Vec<String>,
    /// macOS only: folder of an installed `mediaremote-adapter` (containing
    /// `bin/mediaremote-adapter.pl` and `build/MediaRemoteAdapter.framework`),
    /// which lets Lyrix read any app in the Now Playing widget. Without it
    /// Lyrix reads Spotify and Apple Music directly.
    pub macos_adapter_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordConfig {
    /// Show lyrics through Discord Rich Presence (needs the Discord desktop app).
    pub enabled: bool,
    /// The Discord application id whose name appears as "Listening to <name>".
    pub client_id: String,
    /// Minimum time between updates, in ms.
    pub min_interval_ms: u64,
    /// Show a progress bar for the song.
    pub show_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsoleConfig {
    /// Print each status change in the terminal.
    pub enabled: bool,
}

/// Options that use your own account in ways the services do not allow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdvancedConfig {
    /// You have read [`BAN_WARNING`] and accept the risk. Nothing in this
    /// section runs while this is false.
    pub accept_ban_risk: bool,
    /// Put the lyric line in your Discord custom status (uses your account token).
    pub discord_custom_status: bool,
    /// Get lyrics from Spotify with your own browser login.
    pub spotify_cookie_lyrics: bool,
}

/// A problem or notice found by [`Config::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            general: GeneralConfig::default(),
            status: StatusConfig::default(),
            privacy: PrivacyConfig::default(),
            lyrics: LyricsConfig::default(),
            sources: SourcesConfig::default(),
            discord: DiscordConfig::default(),
            console: ConsoleConfig::default(),
            advanced: AdvancedConfig::default(),
        }
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 500,
            offset_ms: 0,
        }
    }
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            line_template: "🎵 {line}".into(),
            no_lyrics_template: "{title} · {artist}".into(),
            instrumental_text: "♪".into(),
            show_when_paused: false,
            profanity_filter: false,
            profanity_words: Vec::new(),
        }
    }
}

impl Default for LyricsConfig {
    fn default() -> Self {
        Self {
            lyrics_dir: None,
            cache: true,
            lrclib: true,
            lrclib_url: "https://lrclib.net".into(),
        }
    }
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            client_id: DEFAULT_DISCORD_CLIENT_ID.into(),
            min_interval_ms: 2_000,
            show_progress: true,
        }
    }
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    /// Reads the config file. A missing file gives the defaults; a file that
    /// cannot be parsed is an error that names the file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let _ = path;
        todo!()
    }

    /// Writes the config as TOML, creating parent folders. Writes to a temporary
    /// file in the same folder and renames it over the target, so a crash never
    /// leaves a half-written file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let _ = path;
        todo!()
    }

    /// `<config dir>/config.toml`, e.g. `%APPDATA%\Lyrix\config.toml`,
    /// `~/Library/Application Support/Lyrix/config.toml`, `~/.config/lyrix/config.toml`.
    pub fn default_path() -> PathBuf {
        todo!()
    }

    /// Where the lyrics cache lives (platform cache dir + `lyrics`).
    pub fn cache_dir() -> PathBuf {
        todo!()
    }

    /// Where per-song timing offsets are stored (`offsets.toml` next to the config).
    pub fn offsets_path() -> PathBuf {
        todo!()
    }

    /// The pause marker file: while it exists a running Lyrix clears every
    /// status and stops updating (`lyrix pause` / `lyrix resume`).
    pub fn pause_marker_path() -> PathBuf {
        todo!()
    }

    /// The configured lyrics folder, or `<data dir>/lyrics`.
    pub fn lyrics_dir(&self) -> PathBuf {
        todo!()
    }

    /// The Advanced options that are switched on, by config key name, whether
    /// or not the risk was accepted.
    pub fn advanced_requested(&self) -> Vec<&'static str> {
        todo!()
    }

    /// Checks the settings and reports problems:
    /// - Error: `poll_interval_ms` below 100.
    /// - Warning: Discord enabled with an empty `client_id`.
    /// - Warning: an Advanced option is on but `accept_ban_risk` is false
    ///   (the message includes [`BAN_WARNING`] and says the option stays off).
    /// - Warning: an Advanced option is on and accepted (message includes [`BAN_WARNING`]).
    /// - Warning: no target enabled at all.
    /// - Warning: an empty `line_template` or `no_lyrics_template`.
    pub fn validate(&self) -> Vec<ConfigIssue> {
        todo!()
    }
}

/// Per-song timing nudges, keyed by [`crate::matcher::song_key`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Offsets {
    pub songs: BTreeMap<String, i64>,
}

impl Offsets {
    /// Missing file → empty; unreadable file → error naming the file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let _ = path;
        todo!()
    }

    /// Atomic write, like [`Config::save`].
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let _ = path;
        todo!()
    }

    /// The nudge for a song key, 0 when none.
    pub fn get(&self, key: &str) -> i64 {
        let _ = key;
        todo!()
    }

    /// Sets a nudge; 0 removes the entry.
    pub fn set(&mut self, key: &str, offset_ms: i64) {
        let _ = (key, offset_ms);
        todo!()
    }
}
