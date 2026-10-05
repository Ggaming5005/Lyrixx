//! The Lyrix desktop app: the engine runs in the background, the tray icon
//! (the menu bar on macOS) keeps it at hand, and a window shows what it is
//! doing and holds the settings.
//!
//! - Lyrix starts with its window, or only in the tray with `--minimized`
//!   (what starting at login uses). A second launch opens the window of the
//!   one already running.
//! - Closing the window keeps Lyrix running in the tray; "Quit Lyrix" (tray,
//!   window or the macOS app menu) clears every status and ends it.
//! - The window talks to Lyrix only through the commands and the event in
//!   `CONTRACT.md`.
//! - The log is `lyrix.log` in the logs folder, replaced on every start.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod commands;
mod logging;
mod paths;
mod settings;
mod supervisor;
mod tray;
mod view;
mod window;

use paths::Paths;
use std::sync::Arc;
use supervisor::Supervisor;
use tauri::{AppHandle, Emitter as _, Manager as _, RunEvent, Runtime};

fn main() {
    logging::log_panics();
    let opens_window = window::opens_window(std::env::args());
    let paths = Paths::platform();

    let app = tauri::Builder::default()
        // First, so a second launch hands over and ends before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            tracing::info!("Lyrix was launched again");
            if window::opens_window(args) {
                window::show(app);
            }
        }))
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .args([window::MINIMIZED_FLAG])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_view,
            commands::get_settings,
            commands::save_settings,
            commands::preview_status,
            commands::set_paused,
            commands::adjust_offset,
            commands::reset_offset,
            commands::clear_cache,
            commands::open_folder,
            commands::open_url,
            commands::get_autostart,
            commands::set_autostart,
            commands::app_info,
            commands::quit,
        ])
        .setup(move |app| {
            // Only now: a second launch has ended in the single-instance
            // plugin by this point, so it never replaces the running log.
            let log_file = logging::init(&paths.logs_dir);
            tracing::info!(
                "Lyrix {} starting; log file {}",
                env!("CARGO_PKG_VERSION"),
                log_file.display()
            );
            start(app.handle(), paths, opens_window);
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => {
            tracing::error!("Lyrix could not start: {e}");
            eprintln!("Lyrix could not start: {e}");
            std::process::exit(1);
        }
    };

    app.run(|app, event| match event {
        // The last window closed: keep running in the tray. `app.exit` (Quit)
        // comes with an exit code and goes through.
        RunEvent::ExitRequested {
            code: None, api, ..
        } => api.prevent_exit(),
        // Also after macOS's own Quit (Cmd+Q), which skips ExitRequested.
        RunEvent::Exit => {
            if let Some(supervisor) = app.try_state::<Arc<Supervisor>>() {
                let supervisor = supervisor.inner().clone();
                tauri::async_runtime::block_on(stop_engine(&supervisor));
            }
        }
        // The Dock icon was clicked.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => window::show(app),
        _ => {}
    });
}

/// Starts everything that runs while Lyrix does: the engine, the tray icon,
/// the `lyrix://view` event, and the window unless started minimized.
fn start<R: Runtime>(app: &AppHandle<R>, paths: Paths, opens_window: bool) {
    let supervisor = Arc::new(Supervisor::new(paths));
    app.manage(supervisor.clone());

    let tray = tray::create(app, supervisor.clone());
    if let Err(e) = &tray {
        tracing::error!("could not create the tray icon: {e}");
    }

    let handle = app.clone();
    tauri::async_runtime::spawn(view::forward_throttled(
        supervisor.subscribe(),
        view::VIEW_EVENT_EVERY,
        move |view: &view::View| {
            // A closed window loads the view itself when it opens again.
            if handle.get_webview_window(window::MAIN).is_none() {
                return;
            }
            if let Err(e) = handle.emit(view::VIEW_EVENT, view) {
                tracing::warn!("could not send the view to the window: {e}");
            }
        },
    ));

    let engine = supervisor.clone();
    tauri::async_runtime::spawn(async move { engine.restart().await });

    #[cfg(unix)]
    tauri::async_runtime::spawn(quit_on_signal(app.clone(), supervisor.clone()));

    // Without a tray icon the window is the only way in.
    if opens_window || tray.is_err() {
        window::show(app);
    }
}

/// Stops the engine, clearing every status, within about
/// [`supervisor::STOP_TIMEOUT`].
async fn stop_engine(supervisor: &Supervisor) {
    let limit = supervisor::STOP_TIMEOUT + std::time::Duration::from_secs(1);
    if tokio::time::timeout(limit, supervisor.stop())
        .await
        .is_err()
    {
        tracing::warn!("Lyrix did not stop in time; quitting anyway");
    }
}

/// Quits like "Quit Lyrix" on SIGTERM (logging out, `kill`) or SIGINT
/// (Ctrl+C in a terminal), so statuses are cleared. A signal that cannot be
/// watched is logged and ignored.
#[cfg(unix)]
async fn quit_on_signal<R: Runtime>(app: AppHandle<R>, supervisor: Arc<Supervisor>) {
    use tokio::signal::unix::{signal, SignalKind};
    let signals = signal(SignalKind::terminate())
        .and_then(|terminate| Ok((terminate, signal(SignalKind::interrupt())?)));
    let (mut terminate, mut interrupt) = match signals {
        Ok(signals) => signals,
        Err(e) => {
            tracing::warn!("cannot watch for SIGTERM and SIGINT: {e}");
            return;
        }
    };
    tokio::select! {
        _ = terminate.recv() => tracing::info!("asked to stop (SIGTERM)"),
        _ = interrupt.recv() => tracing::info!("asked to stop (SIGINT)"),
    }
    quit(app, supervisor);
}

/// Clears every status, then ends Lyrix. Returns at once; the work runs in
/// the background.
pub fn quit<R: Runtime>(app: AppHandle<R>, supervisor: Arc<Supervisor>) {
    tauri::async_runtime::spawn(async move {
        tracing::info!("quitting");
        stop_engine(&supervisor).await;
        app.exit(0);
    });
}
