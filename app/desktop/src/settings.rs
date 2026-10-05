//! The settings as the window sees them (`get_settings`) and how a save from
//! the window is checked before it is written (`save_settings`).

use crate::paths::Paths;
use lyrix::config::{Config, ConfigIssue, Severity};
use serde::Serialize;
use std::path::Path;

/// A problem in the settings, for the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    pub severity: IssueSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueSeverity {
    Error,
    Warning,
}

impl Issue {
    fn error(message: String) -> Self {
        Self {
            severity: IssueSeverity::Error,
            message,
        }
    }

    pub fn is_error(&self) -> bool {
        self.severity == IssueSeverity::Error
    }
}

impl From<&ConfigIssue> for Issue {
    /// The window knows errors and warnings; a notice counts as a warning.
    fn from(issue: &ConfigIssue) -> Self {
        let severity = match issue.severity {
            Severity::Error => IssueSeverity::Error,
            Severity::Warning | Severity::Info => IssueSeverity::Warning,
        };
        Self {
            severity,
            message: issue.message.clone(),
        }
    }
}

/// [`Config::validate`] in the window's terms.
pub fn issues(config: &Config) -> Vec<Issue> {
    config.validate().iter().map(Issue::from).collect()
}

/// What `get_settings` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// The saved settings (the defaults when there is no file yet, or when
    /// the file cannot be read; `issues` then says why).
    pub config: Config,
    pub defaults: Config,
    pub issues: Vec<Issue>,
    pub paths: SettingsPaths,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPaths {
    pub config: String,
    pub lyrics_dir: String,
    pub cache_dir: String,
    pub logs: String,
}

/// The saved settings, or the defaults with an error when the file cannot be
/// read. Saving from the window then replaces the broken file.
pub fn load_config(path: &Path) -> (Config, Vec<Issue>) {
    match Config::load(path) {
        Ok(config) => {
            let issues = issues(&config);
            (config, issues)
        }
        Err(e) => (
            Config::default(),
            vec![Issue::error(format!(
                "{} Lyrix shows the default settings instead; saving replaces the file.",
                crate::actions::error_sentence(&e)
            ))],
        ),
    }
}

/// Everything the settings page shows.
pub fn settings(paths: &Paths) -> Settings {
    let (config, issues) = load_config(&paths.config);
    let lyrics_dir = config.lyrics_dir();
    Settings {
        config,
        defaults: Config::default(),
        issues,
        paths: SettingsPaths {
            config: paths.config.display().to_string(),
            lyrics_dir: lyrics_dir.display().to_string(),
            cache_dir: paths.cache_dir.display().to_string(),
            logs: paths.logs_dir.display().to_string(),
        },
    }
}

/// What `save_settings` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaveOutcome {
    pub saved: bool,
    pub issues: Vec<Issue>,
}

/// Checks `config` and writes it to `path` when nothing is an error.
/// Warnings are returned either way. Errors writing the file are errors.
pub fn save(config: &Config, path: &Path) -> anyhow::Result<SaveOutcome> {
    let issues = issues(config);
    if issues.iter().any(Issue::is_error) {
        return Ok(SaveOutcome {
            saved: false,
            issues,
        });
    }
    config.save(path)?;
    Ok(SaveOutcome {
        saved: true,
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn paths(root: &Path) -> Paths {
        Paths {
            config: root.join("config.toml"),
            offsets: root.join("offsets.toml"),
            pause_marker: root.join("paused"),
            cache_dir: root.join("cache"),
            logs_dir: root.join("logs"),
        }
    }

    #[test]
    fn a_missing_file_gives_the_defaults_without_issues_or_writing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let settings = settings(&paths);
        assert_eq!(settings.config, Config::default());
        assert_eq!(settings.defaults, Config::default());
        assert!(settings.issues.is_empty());
        assert!(!paths.config.exists());
        assert_eq!(settings.paths.config, paths.config.display().to_string());
        assert_eq!(settings.paths.logs, paths.logs_dir.display().to_string());
        assert_eq!(
            settings.paths.cache_dir,
            paths.cache_dir.display().to_string()
        );
        assert_eq!(
            settings.paths.lyrics_dir,
            Config::default().lyrics_dir().display().to_string()
        );
    }

    #[test]
    fn a_broken_file_gives_the_defaults_and_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.config, "general = [[ nope").unwrap();
        let settings = settings(&paths);
        assert_eq!(settings.config, Config::default());
        assert_eq!(settings.issues.len(), 1);
        assert!(settings.issues[0].is_error());
        assert!(settings.issues[0].message.contains("config.toml"));
    }

    #[test]
    fn saved_warnings_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let mut config = Config::default();
        config.status.line_template = " ".into();
        config.lyrics.lyrics_dir = Some(PathBuf::from("/music/lyrics"));
        config.save(&paths.config).unwrap();

        let settings = settings(&paths);
        assert_eq!(settings.config, config);
        assert_eq!(settings.paths.lyrics_dir, "/music/lyrics".to_string());
        assert_eq!(settings.issues.len(), 1);
        assert_eq!(settings.issues[0].severity, IssueSeverity::Warning);
    }

    #[test]
    fn errors_are_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# mine\n").unwrap();
        let mut config = Config::default();
        config.general.poll_interval_ms = 10;

        let outcome = save(&config, &path).unwrap();
        assert!(!outcome.saved);
        assert!(outcome.issues.iter().any(Issue::is_error));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# mine\n");
    }

    #[test]
    fn warnings_are_saved_and_returned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings").join("config.toml");
        let mut config = Config::default();
        config.discord.client_id = String::new();
        config.general.offset_ms = 120;

        let outcome = save(&config, &path).unwrap();
        assert!(outcome.saved);
        assert_eq!(outcome.issues.len(), 1);
        assert_eq!(outcome.issues[0].severity, IssueSeverity::Warning);
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn a_save_that_cannot_be_written_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // The settings file's folder is a file.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let error = save(&Config::default(), &blocker.join("config.toml")).unwrap_err();
        assert!(format!("{error:#}").contains("config"));
    }

    #[test]
    fn settings_serialize_to_the_contract_shape() {
        let dir = tempfile::tempdir().unwrap();
        let json = serde_json::to_value(settings(&paths(dir.path()))).unwrap();
        // The config keeps the settings file's own snake_case keys.
        assert_eq!(json["config"]["general"]["poll_interval_ms"], 500);
        assert_eq!(json["defaults"]["discord"]["min_interval_ms"], 2000);
        assert!(json["config"]["lyrics"]["lyrics_dir"].is_null());
        assert!(json["paths"]["lyricsDir"].is_string());
        assert!(json["paths"]["cacheDir"].is_string());
        assert!(json["paths"]["logs"].is_string());
        assert!(json["issues"].as_array().unwrap().is_empty());

        let issue = serde_json::to_value(Issue {
            severity: IssueSeverity::Warning,
            message: "m".into(),
        })
        .unwrap();
        assert_eq!(
            issue,
            serde_json::json!({"severity": "warning", "message": "m"})
        );
    }

    #[test]
    fn the_window_can_send_the_config_back() {
        // What get_settings sends, as the window sends it back.
        let mut sent = serde_json::to_value(Config::default()).unwrap();
        sent["status"]["line_template"] = "♪ {line}".into();
        sent["lyrics"]["lyrics_dir"] = "/music".into();
        let config: Config = serde_json::from_value(sent).unwrap();
        assert_eq!(config.status.line_template, "♪ {line}");
        assert_eq!(config.lyrics.lyrics_dir, Some(PathBuf::from("/music")));

        // Missing sections keep their defaults.
        let partial: Config =
            serde_json::from_value(serde_json::json!({"general": {"offset_ms": -40}})).unwrap();
        assert_eq!(partial.general.offset_ms, -40);
        assert_eq!(partial.status, Config::default().status);
    }
}
