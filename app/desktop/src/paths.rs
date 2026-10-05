//! Where the desktop app keeps its files. Everything but the log folder is
//! shared with the `lyrix` command (see `lyrix::config::Config`), so the two
//! see the same settings, offsets, pause marker and cache.

use lyrix::config::Config;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The files and folders Lyrix uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// The settings file.
    pub config: PathBuf,
    /// Per-song timing offsets.
    pub offsets: PathBuf,
    /// Sharing is paused while this file exists.
    pub pause_marker: PathBuf,
    /// Cached lyrics (`*.json` files).
    pub cache_dir: PathBuf,
    /// Holds `lyrix.log`.
    pub logs_dir: PathBuf,
}

impl Paths {
    /// The platform folders, the same ones the `lyrix` command uses.
    pub fn platform() -> Self {
        Self {
            config: Config::default_path(),
            offsets: Config::offsets_path(),
            pause_marker: Config::pause_marker_path(),
            cache_dir: Config::cache_dir(),
            logs_dir: logs_dir(),
        }
    }

    /// The folder holding the settings file.
    pub fn config_dir(&self) -> PathBuf {
        match self.config.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }

    /// The folder `open_folder` opens for `folder`. `config` gives the
    /// lyrics folder (`lyrics.lyrics_dir`, or the default one).
    pub fn folder(&self, folder: Folder, config: &Config) -> PathBuf {
        match folder {
            Folder::Config => self.config_dir(),
            Folder::Lyrics => config.lyrics_dir(),
            Folder::Cache => self.cache_dir.clone(),
            Folder::Logs => self.logs_dir.clone(),
        }
    }
}

/// A folder the window can ask to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Folder {
    Config,
    Lyrics,
    Cache,
    Logs,
}

/// `<local data folder>/logs`: `%LOCALAPPDATA%\Lyrix\data\logs` on Windows,
/// `~/Library/Application Support/Lyrix/logs` on macOS and
/// `~/.local/share/lyrix/logs` on Linux. Like `lyrix::config`, it falls back
/// to the current folder when there is no home folder.
pub fn logs_dir() -> PathBuf {
    match directories::ProjectDirs::from("", "", "Lyrix") {
        Some(dirs) => dirs.data_local_dir().join("logs"),
        None => current_dir().join("logs"),
    }
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
        Paths {
            config: root.join("config").join("config.toml"),
            offsets: root.join("config").join("offsets.toml"),
            pause_marker: root.join("config").join("paused"),
            cache_dir: root.join("cache"),
            logs_dir: root.join("logs"),
        }
    }

    #[test]
    fn folders_map_to_the_right_places() {
        let root = Path::new("/lyrix");
        let paths = paths(root);
        let mut config = Config::default();
        config.lyrics.lyrics_dir = Some(root.join("my lyrics"));

        assert_eq!(paths.folder(Folder::Config, &config), root.join("config"));
        assert_eq!(
            paths.folder(Folder::Lyrics, &config),
            root.join("my lyrics")
        );
        assert_eq!(paths.folder(Folder::Cache, &config), root.join("cache"));
        assert_eq!(paths.folder(Folder::Logs, &config), root.join("logs"));
    }

    #[test]
    fn the_default_lyrics_folder_is_used_when_none_is_set() {
        let paths = paths(Path::new("/lyrix"));
        let config = Config::default();
        assert_eq!(paths.folder(Folder::Lyrics, &config), config.lyrics_dir());
    }

    #[test]
    fn a_bare_config_file_name_lives_in_the_current_folder() {
        let mut paths = paths(Path::new("/lyrix"));
        paths.config = PathBuf::from("config.toml");
        assert_eq!(paths.config_dir(), PathBuf::from("."));
    }

    #[test]
    fn folder_names_are_the_contract_ones() {
        let parse = |name: &str| serde_json::from_value::<Folder>(serde_json::json!(name));
        assert_eq!(parse("config").unwrap(), Folder::Config);
        assert_eq!(parse("lyrics").unwrap(), Folder::Lyrics);
        assert_eq!(parse("cache").unwrap(), Folder::Cache);
        assert_eq!(parse("logs").unwrap(), Folder::Logs);
        assert!(parse("home").is_err());
        assert!(parse("Logs").is_err());
    }

    #[test]
    fn platform_paths_match_the_cli() {
        let paths = Paths::platform();
        assert_eq!(paths.config, Config::default_path());
        assert_eq!(paths.offsets, Config::offsets_path());
        assert_eq!(paths.pause_marker, Config::pause_marker_path());
        assert_eq!(paths.cache_dir, Config::cache_dir());
        assert!(paths.logs_dir.ends_with("logs"));
    }
}
