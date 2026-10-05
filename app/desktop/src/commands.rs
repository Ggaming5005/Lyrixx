//! The commands the window calls (see `CONTRACT.md`). Errors reach the
//! window as rejected promises carrying a message for the user.
//!
//! Commands that touch other programs or many files are `async`, so they run
//! off the main thread and never hold up the window.

use crate::actions::{self, OffsetChange};
use crate::paths::Folder;
use crate::settings::{self, SaveOutcome, Settings};
use crate::supervisor::Supervisor;
use crate::view::View;
use lyrix::config::Config;
use lyrix::template::TemplateContext;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Runtime, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_opener::OpenerExt as _;

type Shared<'a> = State<'a, Arc<Supervisor>>;

/// What the engine is doing now.
#[tauri::command]
pub fn get_view(supervisor: Shared<'_>) -> View {
    supervisor.view()
}

#[tauri::command]
pub fn get_settings(supervisor: Shared<'_>) -> Settings {
    settings::settings(supervisor.paths())
}

/// Saves the settings unless they have an error, then restarts the engine
/// with them (the old one clears its statuses first).
#[tauri::command]
pub async fn save_settings(supervisor: Shared<'_>, config: Config) -> Result<SaveOutcome, String> {
    let outcome = settings::save(&config, &supervisor.paths().config)
        .map_err(|e| actions::error_sentence(&e))?;
    if outcome.saved {
        tracing::info!("settings saved; restarting");
        supervisor.restart().await;
    }
    Ok(outcome)
}

/// Renders a status template exactly like the engine does.
#[tauri::command]
pub fn preview_status(
    template: String,
    line: Option<String>,
    next: Option<String>,
    title: String,
    artist: String,
    album: Option<String>,
) -> String {
    let context = TemplateContext {
        line: line.as_deref(),
        next: next.as_deref(),
        title: &title,
        artist: &artist,
        album: album.as_deref(),
    };
    lyrix::template::render(&template, &context)
}

#[tauri::command]
pub fn set_paused(supervisor: Shared<'_>, paused: bool) -> Result<bool, String> {
    supervisor
        .set_paused(paused)
        .map_err(|e| actions::error_sentence(&e))
}

#[tauri::command]
pub fn adjust_offset(supervisor: Shared<'_>, delta_ms: i64) -> Result<i64, String> {
    supervisor.change_offset(OffsetChange::By(delta_ms))
}

#[tauri::command]
pub fn reset_offset(supervisor: Shared<'_>) -> Result<i64, String> {
    supervisor.change_offset(OffsetChange::Reset)
}

#[tauri::command]
pub async fn clear_cache(supervisor: Shared<'_>) -> Result<usize, String> {
    let dir = &supervisor.paths().cache_dir;
    let removed = actions::clear_cache_dir(dir).map_err(|e| actions::error_sentence(&e))?;
    tracing::info!("removed {removed} cached lyrics");
    Ok(removed)
}

/// Opens one of Lyrix's folders in the file manager, creating it first.
#[tauri::command]
pub async fn open_folder<R: Runtime>(
    app: AppHandle<R>,
    supervisor: Shared<'_>,
    which: Folder,
) -> Result<(), String> {
    let paths = supervisor.paths();
    let (config, _) = settings::load_config(&paths.config);
    let dir = paths.folder(which, &config);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create the folder {}: {e}", dir.display()))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("Could not open the folder {}: {e}", dir.display()))
}

/// Opens an `https://` link in the browser; anything else is refused.
#[tauri::command]
pub async fn open_url<R: Runtime>(app: AppHandle<R>, url: String) -> Result<(), String> {
    let url = actions::https_url(&url)?;
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|e| format!("Could not open {url}: {e}"))
}

#[tauri::command]
pub async fn get_autostart<R: Runtime>(app: AppHandle<R>) -> Result<bool, String> {
    app.autolaunch()
        .is_enabled()
        .map_err(|e| format!("Could not tell whether Lyrix starts at login: {e}"))
}

#[tauri::command]
pub async fn set_autostart<R: Runtime>(app: AppHandle<R>, enabled: bool) -> Result<bool, String> {
    let autolaunch = app.autolaunch();
    let changed = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    changed.map_err(|e| format!("Could not change whether Lyrix starts at login: {e}"))?;
    autolaunch
        .is_enabled()
        .map_err(|e| format!("Could not tell whether Lyrix starts at login: {e}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: &'static str,
    pub os: &'static str,
    pub discord_default_client_id: &'static str,
}

pub fn app_info_now() -> AppInfo {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        os,
        discord_default_client_id: lyrix::config::DEFAULT_DISCORD_CLIENT_ID,
    }
}

#[tauri::command]
pub fn app_info() -> AppInfo {
    app_info_now()
}

/// Clears every status, then ends Lyrix.
#[tauri::command]
pub fn quit<R: Runtime>(app: AppHandle<R>, supervisor: Shared<'_>) {
    crate::quit(app, supervisor.inner().clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_render_like_the_engine() {
        assert_eq!(
            preview_status(
                "🎵 {line}".into(),
                Some("We're no strangers to love".into()),
                None,
                "Never Gonna Give You Up".into(),
                "Rick Astley".into(),
                None,
            ),
            "🎵 We're no strangers to love"
        );
        assert_eq!(
            preview_status(
                "{title} · {album} · {artist}".into(),
                None,
                None,
                "Song".into(),
                "Artist".into(),
                None,
            ),
            "Song · Artist"
        );
    }

    #[test]
    fn app_info_names_the_os_and_the_default_discord_app() {
        let info = app_info_now();
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(["windows", "macos", "linux"].contains(&info.os));
        assert_eq!(info.discord_default_client_id, "1556752305653809272");
        let json = serde_json::to_value(&info).unwrap();
        assert!(json["discordDefaultClientId"].is_string());
        assert!(json["version"].is_string());
        assert!(json["os"].is_string());
    }
}
