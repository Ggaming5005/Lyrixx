//! User settings, stored as TOML in the platform config directory.
//!
//! Every field has a default, so a missing file or a file that only sets a few
//! values both work. Unknown keys are ignored so older builds can read newer files.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

/// Shown whenever an Advanced mode option is turned on or used.
pub const BAN_WARNING: &str = "USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.";

/// Lyrix's own Discord application, so Rich Presence works with no setup and
/// profiles show "Listening to Lyrix". `discord.client_id` can name another
/// application to show a different name. Application ids are public.
pub const DEFAULT_DISCORD_CLIENT_ID: &str = "1556752305653809272";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
        load_toml(path, "config")
    }

    /// Writes the config as TOML, creating parent folders. Writes to a temporary
    /// file in the same folder and renames it over the target, so a crash never
    /// leaves a half-written file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        save_toml(self, path, "config")
    }

    /// `<config dir>/config.toml`, e.g. `%APPDATA%\Lyrix\config\config.toml`,
    /// `~/Library/Application Support/Lyrix/config.toml`, `~/.config/lyrix/config.toml`.
    /// When the platform folders cannot be found (no home folder), the current
    /// folder is used instead (and `<current folder>/cache` for the cache).
    pub fn default_path() -> PathBuf {
        app_dirs().config.join("config.toml")
    }

    /// Where the lyrics cache lives (platform cache dir + `lyrics`).
    pub fn cache_dir() -> PathBuf {
        app_dirs().cache.join("lyrics")
    }

    /// Where per-song timing offsets are stored (`offsets.toml` next to the config).
    pub fn offsets_path() -> PathBuf {
        app_dirs().config.join("offsets.toml")
    }

    /// The pause marker file: while it exists a running Lyrix clears every
    /// status and stops updating (`lyrix pause` / `lyrix resume`).
    pub fn pause_marker_path() -> PathBuf {
        app_dirs().config.join("paused")
    }

    /// The configured lyrics folder, or `<data dir>/lyrics`.
    ///
    /// An empty `lyrics_dir` (`lyrics_dir = ""`) counts as not configured.
    pub fn lyrics_dir(&self) -> PathBuf {
        match &self.lyrics.lyrics_dir {
            Some(dir) if !dir.as_os_str().is_empty() => dir.clone(),
            _ => app_dirs().data.join("lyrics"),
        }
    }

    /// The Advanced options that are switched on, by config key name, whether
    /// or not the risk was accepted.
    pub fn advanced_requested(&self) -> Vec<&'static str> {
        let mut on = Vec::new();
        if self.advanced.discord_custom_status {
            on.push("discord_custom_status");
        }
        if self.advanced.spotify_cookie_lyrics {
            on.push("spotify_cookie_lyrics");
        }
        on
    }

    /// Checks the settings and reports problems:
    /// - Error: `poll_interval_ms` below 100.
    /// - Warning: Discord enabled with an empty `client_id`.
    /// - Warning: an Advanced option is on but `accept_ban_risk` is false
    ///   (the message includes [`BAN_WARNING`] and says the option stays off).
    /// - Warning: an Advanced option is on and accepted (message includes [`BAN_WARNING`]).
    /// - Warning: no target enabled at all.
    /// - Warning: an empty `line_template` or `no_lyrics_template`.
    ///
    /// Issues come in the order above: one per Advanced option that is on and
    /// one per empty template. Text made only of whitespace counts as empty.
    /// The targets are Discord (`discord.enabled`) and the console
    /// (`console.enabled`).
    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut issues = Vec::new();

        if self.general.poll_interval_ms < MIN_POLL_INTERVAL_MS {
            issues.push(ConfigIssue::error(format!(
                "general.poll_interval_ms is {} ms; it must be at least {} ms.",
                self.general.poll_interval_ms, MIN_POLL_INTERVAL_MS
            )));
        }

        if self.discord.enabled && self.discord.client_id.trim().is_empty() {
            issues.push(ConfigIssue::warning(
                "discord.enabled is on but discord.client_id is empty, so Discord Rich \
                 Presence cannot connect. Set discord.client_id to a Discord application id."
                    .to_string(),
            ));
        }

        for key in self.advanced_requested() {
            let message = if self.advanced.accept_ban_risk {
                format!("advanced.{key} is on. {BAN_WARNING}")
            } else {
                format!(
                    "advanced.{key} is on but advanced.accept_ban_risk is false, so it stays \
                     off. {BAN_WARNING}"
                )
            };
            issues.push(ConfigIssue::warning(message));
        }

        if !self.discord.enabled && !self.console.enabled {
            issues.push(ConfigIssue::warning(
                "No target is enabled (discord.enabled and console.enabled are both off), \
                 so lyrics are not shown anywhere."
                    .to_string(),
            ));
        }

        if self.status.line_template.trim().is_empty() {
            issues.push(ConfigIssue::warning(
                "status.line_template is empty, so lyric lines would show as an empty status."
                    .to_string(),
            ));
        }
        if self.status.no_lyrics_template.trim().is_empty() {
            issues.push(ConfigIssue::warning(
                "status.no_lyrics_template is empty, so songs without lyrics would show as \
                 an empty status."
                    .to_string(),
            ));
        }

        issues
    }
}

/// Reading the player more often than this is an error in [`Config::validate`].
const MIN_POLL_INTERVAL_MS: u64 = 100;

impl ConfigIssue {
    fn error(message: String) -> Self {
        Self {
            severity: Severity::Error,
            message,
        }
    }

    fn warning(message: String) -> Self {
        Self {
            severity: Severity::Warning,
            message,
        }
    }
}

/// The folders Lyrix keeps its files in.
struct AppDirs {
    config: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

/// The platform folders for Lyrix, or the current folder when they cannot be
/// determined (no home folder).
fn app_dirs() -> AppDirs {
    match directories::ProjectDirs::from("", "", "Lyrix") {
        Some(project) => AppDirs {
            config: project.config_dir().to_path_buf(),
            cache: project.cache_dir().to_path_buf(),
            data: project.data_dir().to_path_buf(),
        },
        None => {
            let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            AppDirs {
                config: here.clone(),
                cache: here.join("cache"),
                data: here,
            }
        }
    }
}

/// Reads a TOML file. A missing file gives `T::default()`; any other problem
/// is an error that names the file.
fn load_toml<T>(path: &Path, what: &str) -> anyhow::Result<T>
where
    T: Default + serde::de::DeserializeOwned,
{
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("could not read the {what} file {}", path.display()))
        }
    };
    // Some Windows editors start UTF-8 files with a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    toml::from_str(text)
        .with_context(|| format!("could not parse the {what} file {}", path.display()))
}

/// Writes `value` as TOML to `path` with [`write_atomic`].
fn save_toml<T: Serialize>(value: &T, path: &Path, what: &str) -> anyhow::Result<()> {
    let context = || format!("could not save the {what} file {}", path.display());
    let text = toml::to_string_pretty(value).with_context(context)?;
    write_atomic(path, text.as_bytes()).with_context(context)
}

/// Numbers the temporary files of [`write_atomic`] within this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many names [`create_temp_file`] tries before giving up.
const MAX_TEMP_ATTEMPTS: u32 = 1_000;

/// Writes `contents` to a new temporary file in the folder of `path` (creating
/// the folder), flushes it to disk, then renames it over `path`. The temporary
/// file is removed when anything fails.
///
/// When `path` is a symbolic link, the file it points to is replaced and the
/// link is kept.
fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let target = save_target(path);
    let path = target.as_path();
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the path has no file name",
        )
    })?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;

    let (mut file, temp_path) = create_temp_file(&dir, file_name)?;
    let written = file.write_all(contents).and_then(|()| file.sync_all());
    // Close the file before renaming or removing it: Windows cannot rename an
    // open file.
    drop(file);
    let result = written.and_then(|()| std::fs::rename(&temp_path, path));

    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Creates a new, empty temporary file in `dir` named after `file_name`. A
/// name that is already taken (for example by a file a crashed save left
/// behind) is skipped, never reused or deleted.
fn create_temp_file(
    dir: &Path,
    file_name: &std::ffi::OsStr,
) -> std::io::Result<(std::fs::File, PathBuf)> {
    use std::sync::atomic::Ordering;

    let mut attempts: u32 = 0;
    loop {
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(
            ".{}-{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let temp_path = dir.join(temp_name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    && attempts < MAX_TEMP_ATTEMPTS =>
            {
                attempts = attempts.saturating_add(1);
            }
            Err(e) => return Err(e),
        }
    }
}

/// The file a save replaces: `path` itself, or the file `path` links to, so a
/// symbolic link (common for dotfiles) keeps working after a save.
fn save_target(path: &Path) -> PathBuf {
    let is_link = std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false);
    if !is_link {
        return path.to_path_buf();
    }
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    // The link points to a file that does not exist yet: create it there.
    match std::fs::read_link(path) {
        // A relative link is relative to the folder holding the link; joining
        // an absolute one gives the absolute one.
        Ok(points_to) => match path.parent() {
            Some(parent) => parent.join(points_to),
            None => points_to,
        },
        Err(_) => path.to_path_buf(),
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
        load_toml(path, "offsets")
    }

    /// Atomic write, like [`Config::save`].
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        save_toml(self, path, "offsets")
    }

    /// The nudge for a song key, 0 when none.
    pub fn get(&self, key: &str) -> i64 {
        self.songs.get(key).copied().unwrap_or(0)
    }

    /// Sets a nudge; 0 removes the entry.
    pub fn set(&mut self, key: &str, offset_ms: i64) {
        if offset_ms == 0 {
            self.songs.remove(key);
        } else {
            self.songs.insert(key.to_string(), offset_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with every warning silenced: Discord has an id.
    fn clean_config() -> Config {
        let mut config = Config::default();
        config.discord.client_id = "1234567890".into();
        config
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // ---- defaults and TOML ----

    #[test]
    fn defaults_match_documentation() {
        let c = Config::default();
        assert_eq!(c.general.poll_interval_ms, 500);
        assert_eq!(c.general.offset_ms, 0);
        assert_eq!(c.status.line_template, "🎵 {line}");
        assert_eq!(c.status.no_lyrics_template, "{title} · {artist}");
        assert_eq!(c.status.instrumental_text, "♪");
        assert!(!c.status.show_when_paused);
        assert!(c.lyrics.cache);
        assert!(c.lyrics.lrclib);
        assert_eq!(c.lyrics.lrclib_url, "https://lrclib.net");
        assert!(c.discord.enabled);
        assert_eq!(c.discord.client_id, DEFAULT_DISCORD_CLIENT_ID);
        assert_eq!(c.discord.min_interval_ms, 2_000);
        assert!(c.console.enabled);
        assert!(!c.advanced.accept_ban_risk);
        assert!(c.advanced_requested().is_empty());
    }

    #[test]
    fn default_round_trips_through_toml() {
        let text = toml::to_string(&Config::default()).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, Config::default());

        let pretty = toml::to_string_pretty(&Config::default()).unwrap();
        let back: Config = toml::from_str(&pretty).unwrap();
        assert_eq!(back, Config::default());
    }

    #[test]
    fn non_default_values_round_trip_through_toml() {
        let mut c = Config::default();
        c.general.poll_interval_ms = 250;
        c.general.offset_ms = -1_500;
        c.status.line_template = "♫ {line} — {artist} \"quoted\" \\ back".into();
        c.status.no_lyrics_template = "日本語 {title}".into();
        c.status.instrumental_text = String::new();
        c.status.show_when_paused = true;
        c.status.profanity_filter = true;
        c.status.profanity_words = vec!["darn".into(), "héck".into()];
        c.privacy.blocked_apps = vec!["chrome".into(), "firefox".into()];
        c.privacy.blocked_artists = vec!["Some Artist".into()];
        c.privacy.title_only = true;
        c.lyrics.lyrics_dir = Some(PathBuf::from("/home/me/Music/lyrics"));
        c.lyrics.cache = false;
        c.lyrics.lrclib = false;
        c.lyrics.lrclib_url = "http://localhost:3000".into();
        c.sources.preferred_apps = vec!["spotify".into()];
        c.sources.macos_adapter_dir = Some(PathBuf::from("/opt/mediaremote-adapter"));
        c.discord.enabled = false;
        c.discord.client_id = "42".into();
        c.discord.min_interval_ms = i64::MAX as u64;
        c.discord.show_progress = false;
        c.console.enabled = false;
        c.advanced.accept_ban_risk = true;
        c.advanced.discord_custom_status = true;
        c.advanced.spotify_cookie_lyrics = true;

        let text = toml::to_string(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn extreme_numbers_round_trip() {
        let mut c = Config::default();
        c.general.offset_ms = i64::MIN;
        c.general.poll_interval_ms = 0;
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        c.general.offset_ms = i64::MAX;
        let back: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn unsigned_value_beyond_toml_range_does_not_panic() {
        // TOML integers are 64-bit signed; a larger u64 cannot be written.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut c = Config::default();
        c.discord.min_interval_ms = u64::MAX;
        assert!(c.save(&path).is_err());
        assert!(!path.exists());
        assert!(files_in(dir.path()).is_empty());
    }

    #[test]
    fn empty_text_is_default() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[general]\npoll_interval_ms = 250\n\n[discord]\nenabled = false\n",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();

        let mut expected = Config::default();
        expected.general.poll_interval_ms = 250;
        expected.discord.enabled = false;
        assert_eq!(c, expected);
        // Other fields of a partly set section keep their defaults.
        assert_eq!(c.general.offset_ms, 0);
        assert_eq!(c.discord.min_interval_ms, 2_000);
        assert!(c.discord.show_progress);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let text = r#"
            future_top_level = "x"

            [general]
            poll_interval_ms = 300
            brand_new_option = true

            [some_future_section]
            anything = [1, 2, 3]

            [discord.nested_future]
            a = 1
        "#;
        let c: Config = toml::from_str(text).unwrap();
        let mut expected = Config::default();
        expected.general.poll_interval_ms = 300;
        assert_eq!(c, expected);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        assert_eq!(Config::load(&path).unwrap(), expected);
    }

    #[test]
    fn unicode_values_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[status]\nline_template = \"🎶 {line} 🎶\"\ninstrumental_text = \"…間奏…\"\n",
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(c.status.line_template, "🎶 {line} 🎶");
        assert_eq!(c.status.instrumental_text, "…間奏…");
    }

    #[test]
    fn byte_order_mark_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "\u{feff}[general]\npoll_interval_ms = 700\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().general.poll_interval_ms, 700);
    }

    // ---- load ----

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        // Loading never creates the file.
        assert!(!path.exists());
    }

    #[test]
    fn missing_parent_folder_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no").join("such").join("config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn empty_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn bad_toml_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken config.toml");
        std::fs::write(&path, "[general\npoll_interval_ms = ").unwrap();
        let err = Config::load(&path).unwrap_err();
        let shown = path.display().to_string();
        assert!(err.to_string().contains(&shown), "{err}");
        assert!(format!("{err:#}").contains(&shown), "{err:#}");
    }

    #[test]
    fn wrong_type_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\npoll_interval_ms = \"fast\"\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
        // The cause explains what is wrong.
        assert!(format!("{err:#}").contains("poll_interval_ms"), "{err:#}");
    }

    #[test]
    fn negative_unsigned_value_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[discord]\nmin_interval_ms = -5\n").unwrap();
        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn invalid_utf8_is_an_error_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, [0x5b, 0xff, 0xfe, 0x5d]).unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn folder_instead_of_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let err = Config::load(dir.path()).unwrap_err();
        assert!(err.to_string().contains(&dir.path().display().to_string()));
    }

    // ---- save ----

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut c = clean_config();
        c.general.offset_ms = 120;
        c.privacy.blocked_apps = vec!["vlc".into()];
        c.lyrics.lyrics_dir = Some(dir.path().join("my lyrics"));
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
    }

    #[test]
    fn saved_file_is_readable_toml_with_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for section in [
            "[general]",
            "[status]",
            "[privacy]",
            "[lyrics]",
            "[sources]",
            "[discord]",
            "[console]",
            "[advanced]",
        ] {
            assert!(text.contains(section), "missing {section} in:\n{text}");
        }
        assert!(text.contains("poll_interval_ms = 500"));
    }

    #[test]
    fn save_creates_parent_folders() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("c").join("config.toml");
        Config::default().save(&path).unwrap();
        assert!(path.is_file());
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save(&path).unwrap();
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);

        // Overwriting an existing file replaces it completely.
        let mut c = clean_config();
        c.status.line_template = "x".into();
        c.save(&path).unwrap();
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);
        assert_eq!(Config::load(&path).unwrap(), c);

        // A shorter file leaves no trailing bytes from a longer one.
        c.status.profanity_words = (0..200).map(|i| format!("word{i}")).collect();
        c.save(&path).unwrap();
        Config::default().save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);
    }

    #[test]
    fn failed_save_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // The target is a non-empty folder, so the final rename must fail.
        let target = dir.path().join("config.toml");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), "x").unwrap();
        let err = Config::default().save(&target).unwrap_err();
        assert!(err.to_string().contains(&target.display().to_string()));
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);
        assert!(target.join("keep").is_file());
    }

    #[test]
    fn save_to_path_without_file_name_is_an_error() {
        assert!(Config::default().save(Path::new("..")).is_err());
        assert!(Config::default().save(Path::new("/")).is_err());
    }

    #[test]
    fn write_atomic_writes_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.toml");
        write_atomic(&path, b"a = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
        write_atomic(&path, b"").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert_eq!(files_in(dir.path()), vec!["plain.toml".to_string()]);
    }

    #[test]
    fn many_saves_in_a_row_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for i in 0..50u64 {
            let mut c = Config::default();
            c.general.poll_interval_ms = 100 + i;
            c.save(&path).unwrap();
            assert_eq!(
                Config::load(&path).unwrap().general.poll_interval_ms,
                100 + i
            );
        }
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);
    }

    #[test]
    fn concurrent_saves_leave_a_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let handles: Vec<_> = (0..8u64)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let mut c = Config::default();
                    c.general.poll_interval_ms = 1_000 + i;
                    c.save(&path).is_ok()
                })
            })
            .collect();
        let saved = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();
        // Windows may refuse to replace a file another thread is replacing at
        // the same moment; elsewhere every save succeeds. Either way the file
        // is whole and no temporary file is left.
        if cfg!(windows) {
            assert!(saved >= 1);
        } else {
            assert_eq!(saved, 8);
        }
        let loaded = Config::load(&path).unwrap();
        assert!((1_000..1_008).contains(&loaded.general.poll_interval_ms));
        assert_eq!(files_in(dir.path()), vec!["config.toml".to_string()]);
    }

    // ---- paths ----

    #[test]
    fn platform_paths_have_the_documented_names() {
        let config = Config::default_path();
        assert_eq!(config.file_name().unwrap(), "config.toml");
        let offsets = Config::offsets_path();
        assert_eq!(offsets.file_name().unwrap(), "offsets.toml");
        assert_eq!(offsets.parent(), config.parent());
        let paused = Config::pause_marker_path();
        assert_eq!(paused.file_name().unwrap(), "paused");
        assert_eq!(paused.parent(), config.parent());
        let cache = Config::cache_dir();
        assert_eq!(cache.file_name().unwrap(), "lyrics");
        assert!(config.is_absolute());
        assert!(cache.is_absolute());
    }

    #[test]
    fn config_folder_is_named_after_the_app() {
        // Only the platform folders are named after the app; the fallback (no
        // home folder) is the current folder.
        if directories::ProjectDirs::from("", "", "Lyrix").is_none() {
            return;
        }
        let config = Config::default_path();
        let named = config.components().any(|c| {
            c.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("lyrix")
        });
        assert!(named, "{}", config.display());
    }

    #[test]
    fn default_lyrics_dir_is_separate_from_the_cache() {
        let c = Config::default();
        let lyrics = c.lyrics_dir();
        assert_eq!(lyrics.file_name().unwrap(), "lyrics");
        assert!(lyrics.is_absolute());
        // `cache clear` must never touch the user's own lyrics.
        assert_ne!(lyrics, Config::cache_dir());
    }

    #[test]
    fn configured_lyrics_dir_is_used_as_is() {
        let mut c = Config::default();
        c.lyrics.lyrics_dir = Some(PathBuf::from("/srv/my lyrics/日本"));
        assert_eq!(c.lyrics_dir(), PathBuf::from("/srv/my lyrics/日本"));
        c.lyrics.lyrics_dir = Some(PathBuf::from("relative/dir"));
        assert_eq!(c.lyrics_dir(), PathBuf::from("relative/dir"));
    }

    #[test]
    fn lyrics_dir_loads_from_toml() {
        let c: Config = toml::from_str("[lyrics]\nlyrics_dir = \"/music/lrc\"\n").unwrap();
        assert_eq!(c.lyrics_dir(), PathBuf::from("/music/lrc"));
        assert!(c.lyrics.cache);
    }

    // ---- advanced_requested ----

    #[test]
    fn advanced_requested_lists_switched_on_options() {
        let mut c = Config::default();
        assert!(c.advanced_requested().is_empty());
        c.advanced.accept_ban_risk = true;
        assert!(c.advanced_requested().is_empty());

        c.advanced.accept_ban_risk = false;
        c.advanced.discord_custom_status = true;
        assert_eq!(c.advanced_requested(), vec!["discord_custom_status"]);

        c.advanced.discord_custom_status = false;
        c.advanced.spotify_cookie_lyrics = true;
        assert_eq!(c.advanced_requested(), vec!["spotify_cookie_lyrics"]);

        c.advanced.discord_custom_status = true;
        assert_eq!(
            c.advanced_requested(),
            vec!["discord_custom_status", "spotify_cookie_lyrics"]
        );
        c.advanced.accept_ban_risk = true;
        assert_eq!(
            c.advanced_requested(),
            vec!["discord_custom_status", "spotify_cookie_lyrics"]
        );
    }

    #[test]
    fn advanced_key_names_match_toml_keys() {
        let mut c = Config::default();
        c.advanced.discord_custom_status = true;
        c.advanced.spotify_cookie_lyrics = true;
        let text = toml::to_string(&c).unwrap();
        for key in c.advanced_requested() {
            assert!(text.contains(&format!("{key} = true")), "{key} in:\n{text}");
        }
    }

    // ---- validate ----

    #[test]
    fn clean_config_has_no_issues() {
        assert!(clean_config().validate().is_empty());
    }

    #[test]
    fn default_config_only_warns_about_the_missing_discord_id() {
        let issues = Config::default().validate();
        if DEFAULT_DISCORD_CLIENT_ID.is_empty() {
            assert_eq!(issues.len(), 1, "{issues:?}");
            assert_eq!(issues[0].severity, Severity::Warning);
            assert!(issues[0].message.contains("client_id"));
        } else {
            assert!(issues.is_empty(), "{issues:?}");
        }
    }

    #[test]
    fn poll_interval_below_100_is_an_error() {
        for ms in [0, 1, 50, 99] {
            let mut c = clean_config();
            c.general.poll_interval_ms = ms;
            let issues = c.validate();
            assert_eq!(issues.len(), 1, "{ms}: {issues:?}");
            assert_eq!(issues[0].severity, Severity::Error);
            assert!(issues[0].message.contains("poll_interval_ms"));
            assert!(issues[0].message.contains(&ms.to_string()));
        }
        for ms in [100, 101, 500, u64::MAX] {
            let mut c = clean_config();
            c.general.poll_interval_ms = ms;
            assert!(c.validate().is_empty(), "{ms}");
        }
    }

    #[test]
    fn discord_without_client_id_warns() {
        let mut c = clean_config();
        c.discord.client_id = String::new();
        let issues = c.validate();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains("client_id"));

        c.discord.client_id = "   ".into();
        assert_eq!(c.validate().len(), 1);

        // Disabled Discord does not need an id.
        c.discord.enabled = false;
        assert!(c.validate().is_empty());
    }

    #[test]
    fn advanced_option_without_accepting_risk_warns_it_stays_off() {
        let mut c = clean_config();
        c.advanced.discord_custom_status = true;
        let issues = c.validate();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains(BAN_WARNING));
        assert!(issues[0].message.contains("discord_custom_status"));
        assert!(issues[0].message.contains("stays off"));
        assert!(issues[0].message.contains("accept_ban_risk"));
    }

    #[test]
    fn accepted_advanced_option_still_warns() {
        let mut c = clean_config();
        c.advanced.accept_ban_risk = true;
        c.advanced.spotify_cookie_lyrics = true;
        let issues = c.validate();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains(BAN_WARNING));
        assert!(issues[0].message.contains("spotify_cookie_lyrics"));
        assert!(!issues[0].message.contains("stays off"));
    }

    #[test]
    fn each_advanced_option_gets_its_own_warning() {
        let mut c = clean_config();
        c.advanced.discord_custom_status = true;
        c.advanced.spotify_cookie_lyrics = true;
        let issues = c.validate();
        assert_eq!(issues.len(), 2);
        assert!(issues[0].message.contains("discord_custom_status"));
        assert!(issues[1].message.contains("spotify_cookie_lyrics"));
        assert!(issues.iter().all(|i| i.message.contains(BAN_WARNING)));
        assert!(issues.iter().all(|i| i.severity == Severity::Warning));
    }

    #[test]
    fn accepting_risk_alone_is_fine() {
        let mut c = clean_config();
        c.advanced.accept_ban_risk = true;
        assert!(c.validate().is_empty());
    }

    #[test]
    fn no_target_enabled_warns() {
        let mut c = clean_config();
        c.discord.enabled = false;
        c.console.enabled = false;
        let issues = c.validate();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.to_lowercase().contains("target"));

        // Either target alone is enough.
        c.console.enabled = true;
        assert!(c.validate().is_empty());
        c.console.enabled = false;
        c.discord.enabled = true;
        assert!(c.validate().is_empty());
    }

    #[test]
    fn empty_templates_warn() {
        let mut c = clean_config();
        c.status.line_template = String::new();
        let issues = c.validate();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains("line_template"));

        let mut c = clean_config();
        c.status.no_lyrics_template = " \t ".into();
        let issues = c.validate();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Warning);
        assert!(issues[0].message.contains("no_lyrics_template"));

        let mut c = clean_config();
        c.status.line_template = String::new();
        c.status.no_lyrics_template = String::new();
        assert_eq!(c.validate().len(), 2);
    }

    #[test]
    fn empty_instrumental_text_is_allowed() {
        let mut c = clean_config();
        c.status.instrumental_text = String::new();
        assert!(c.validate().is_empty());
    }

    #[test]
    fn every_problem_reported_in_order() {
        let mut c = Config::default();
        c.general.poll_interval_ms = 10;
        c.discord.client_id = String::new();
        c.advanced.discord_custom_status = true;
        c.status.line_template = String::new();
        c.status.no_lyrics_template = String::new();
        let issues = c.validate();
        assert_eq!(issues.len(), 5, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].message.contains("poll_interval_ms"));
        assert!(issues[1].message.contains("client_id"));
        assert!(issues[2].message.contains(BAN_WARNING));
        assert!(issues[3].message.contains("line_template"));
        assert!(issues[4].message.contains("no_lyrics_template"));
        assert_eq!(
            issues.iter().map(|i| i.severity).max(),
            Some(Severity::Error)
        );

        c.discord.enabled = false;
        c.console.enabled = false;
        let issues = c.validate();
        assert_eq!(issues.len(), 5, "{issues:?}");
        assert!(issues[2].message.to_lowercase().contains("target"));
    }

    #[test]
    fn severity_orders_by_importance() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    // ---- offsets ----

    #[test]
    fn offsets_get_set_remove() {
        let mut o = Offsets::default();
        assert_eq!(o.get("artist - title"), 0);
        o.set("artist - title", 250);
        assert_eq!(o.get("artist - title"), 250);
        o.set("artist - title", -1_000);
        assert_eq!(o.get("artist - title"), -1_000);
        assert_eq!(o.songs.len(), 1);
        o.set("artist - title", 0);
        assert_eq!(o.get("artist - title"), 0);
        assert!(o.songs.is_empty());
        // Removing a missing entry is fine.
        o.set("never set", 0);
        assert!(o.songs.is_empty());
    }

    #[test]
    fn offsets_keys_are_exact() {
        let mut o = Offsets::default();
        o.set("a", 1);
        o.set("A", 2);
        o.set("", 3);
        assert_eq!(o.get("a"), 1);
        assert_eq!(o.get("A"), 2);
        assert_eq!(o.get(""), 3);
        assert_eq!(o.get("a "), 0);
    }

    #[test]
    fn offsets_extremes() {
        let mut o = Offsets::default();
        o.set("min", i64::MIN);
        o.set("max", i64::MAX);
        assert_eq!(o.get("min"), i64::MIN);
        assert_eq!(o.get("max"), i64::MAX);
    }

    #[test]
    fn offsets_round_trip_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("offsets.toml");
        let mut o = Offsets::default();
        o.set("beyoncé - halo", 300);
        o.set("宇多田ヒカル - first love", -120);
        o.set("weird \"quoted\" = [key] # not a comment\\", 5);
        o.set("emoji 🎵 - song", i64::MAX);
        o.set("negative", i64::MIN);
        o.set("", 1);
        o.save(&path).unwrap();
        assert_eq!(files_in(path.parent().unwrap()), vec!["offsets.toml"]);
        let back = Offsets::load(&path).unwrap();
        assert_eq!(back, o);
        assert_eq!(back.get("宇多田ヒカル - first love"), -120);
    }

    #[test]
    fn empty_offsets_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        Offsets::default().save(&path).unwrap();
        assert_eq!(Offsets::load(&path).unwrap(), Offsets::default());
    }

    #[test]
    fn offsets_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let o = Offsets::load(&dir.path().join("offsets.toml")).unwrap();
        assert!(o.songs.is_empty());
    }

    #[test]
    fn offsets_hand_written_file_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        std::fs::write(
            &path,
            "[songs]\n\"daft punk - one more time\" = 400\nplain_key = -50\n\n[future]\nx = 1\n",
        )
        .unwrap();
        let o = Offsets::load(&path).unwrap();
        assert_eq!(o.get("daft punk - one more time"), 400);
        assert_eq!(o.get("plain_key"), -50);
        assert_eq!(o.songs.len(), 2);
    }

    #[test]
    fn offsets_bad_file_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        std::fs::write(&path, "[songs]\n\"a\" = \"not a number\"\n").unwrap();
        let err = Offsets::load(&path).unwrap_err();
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "{err}"
        );

        std::fs::write(&path, "[songs\n").unwrap();
        let err = Offsets::load(&path).unwrap_err();
        assert!(
            err.to_string().contains(&path.display().to_string()),
            "{err}"
        );
    }

    #[test]
    fn offsets_save_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        let mut o = Offsets::default();
        o.set("a", 1);
        o.set("b", 2);
        o.save(&path).unwrap();
        o.set("a", 0);
        o.save(&path).unwrap();
        let back = Offsets::load(&path).unwrap();
        assert_eq!(back.get("a"), 0);
        assert_eq!(back.get("b"), 2);
        assert_eq!(back.songs.len(), 1);
        assert_eq!(files_in(dir.path()), vec!["offsets.toml"]);
    }

    #[test]
    fn tricky_strings_survive_save_and_load() {
        // Saving uses pretty TOML (multi-line strings), so quotes, newlines and
        // control characters must still come back exactly.
        let tricky = [
            "a\nb\"",
            "'''",
            "\"\"\"",
            "line\r\nbreak",
            "\t tab",
            "\u{7f}del",
            "trail \\",
            "ends quote\"",
            "multi\nline ends quote\"",
            "multi\nline '''",
            "multi\n\"\"\"\nx",
            "\u{0}nul",
            "\u{1b}[31m",
            "x\n",
            "\n",
            "\r",
            "a\u{2028}b",
            "multi\nline ends backslash\\",
            "multi\nline\\\nx",
            "''' and \"\"\"\n both",
            "a\"\"\"\"\"b\nc",
            "\n\n\"\"",
        ];
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let offsets_path = dir.path().join("offsets.toml");
        for s in tricky {
            let mut c = Config::default();
            c.status.line_template = s.to_string();
            c.status.profanity_words = vec![s.to_string(), "plain".into()];
            c.save(&config_path).unwrap();
            assert_eq!(Config::load(&config_path).unwrap(), c, "{s:?}");

            let mut o = Offsets::default();
            o.set(s, 5);
            o.save(&offsets_path).unwrap();
            assert_eq!(Offsets::load(&offsets_path).unwrap(), o, "{s:?}");
        }
    }

    #[test]
    fn save_skips_a_stale_temp_file_with_the_same_name() {
        // A save that crashed (or a process that had the same id) can leave
        // temporary files behind. They must neither block nor be deleted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let start = TEMP_COUNTER.load(std::sync::atomic::Ordering::Relaxed);
        let stale: Vec<PathBuf> = (start..start + 300)
            .map(|n| {
                dir.path()
                    .join(format!(".config.toml.{}-{n}.tmp", std::process::id()))
            })
            .collect();
        for p in &stale {
            std::fs::write(p, "stale").unwrap();
        }
        let mut c = clean_config();
        c.general.poll_interval_ms = 321;
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        for p in &stale {
            assert_eq!(std::fs::read_to_string(p).unwrap(), "stale");
        }
        assert_eq!(files_in(dir.path()).len(), stale.len() + 1);
    }

    #[cfg(unix)]
    #[test]
    fn save_through_a_symlink_updates_the_linked_file() {
        // Dotfile managers keep the real file elsewhere and link to it.
        let dir = tempfile::tempdir().unwrap();
        let real_dir = dir.path().join("dotfiles");
        std::fs::create_dir(&real_dir).unwrap();
        let real = real_dir.join("lyrix.toml");
        std::fs::write(&real, "[general]\npoll_interval_ms = 900\n").unwrap();
        let link_dir = dir.path().join("config");
        std::fs::create_dir(&link_dir).unwrap();
        let link = link_dir.join("config.toml");
        std::os::unix::fs::symlink("../dotfiles/lyrix.toml", &link).unwrap();
        assert_eq!(Config::load(&link).unwrap().general.poll_interval_ms, 900);

        let mut c = clean_config();
        c.general.poll_interval_ms = 250;
        c.save(&link).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(Config::load(&real).unwrap(), c);
        assert_eq!(Config::load(&link).unwrap(), c);
        assert_eq!(files_in(&link_dir), vec!["config.toml"]);
        assert_eq!(files_in(&real_dir), vec!["lyrix.toml"]);
    }

    #[cfg(unix)]
    #[test]
    fn save_through_a_dangling_symlink_creates_the_linked_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir
            .path()
            .join("dotfiles")
            .join("lyrix")
            .join("offsets.toml");
        let link = dir.path().join("offsets.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // Nothing there yet: loading gives the defaults.
        assert_eq!(Offsets::load(&link).unwrap(), Offsets::default());

        let mut o = Offsets::default();
        o.set("artist - title", 120);
        o.save(&link).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(Offsets::load(&real).unwrap(), o);
        assert_eq!(Offsets::load(&link).unwrap(), o);
    }

    #[test]
    fn empty_lyrics_dir_counts_as_not_set() {
        let c: Config = toml::from_str("[lyrics]\nlyrics_dir = \"\"\n").unwrap();
        assert_eq!(c.lyrics_dir(), Config::default().lyrics_dir());
        assert_eq!(c.lyrics_dir().file_name().unwrap(), "lyrics");
    }

    #[test]
    fn byte_order_mark_alone_is_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "\u{feff}").unwrap();
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        std::fs::write(&path, "\u{feff}[songs]\n\"a\" = 3\n").unwrap();
        assert_eq!(Offsets::load(&path).unwrap().get("a"), 3);
    }
}
