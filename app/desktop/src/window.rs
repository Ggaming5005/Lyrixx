//! The one window, `main`. Closing it destroys it (freeing the webview) while
//! Lyrix keeps running in the tray; opening it again creates a new one.

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// The window's label, which `capabilities/default.json` names.
pub const MAIN: &str = "main";

/// The flag autostart launches Lyrix with: start in the tray, without the window.
pub const MINIMIZED_FLAG: &str = "--minimized";

/// Whether a launch with these arguments (the program first) should open the
/// window: not when started with [`MINIMIZED_FLAG`].
pub fn opens_window<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    !args
        .into_iter()
        .skip(1)
        .any(|arg| arg.as_ref() == MINIMIZED_FLAG)
}

/// Shows the window and brings it to the front, creating it when it is
/// closed. Safe to call from any thread: the work runs on the main thread.
pub fn show<R: Runtime>(app: &AppHandle<R>) {
    let handle = app.clone();
    let queued = app.run_on_main_thread(move || {
        if let Err(e) = show_now(&handle) {
            tracing::error!("could not open the window: {e}");
        }
    });
    if let Err(e) = queued {
        tracing::error!("could not open the window: {e}");
    }
}

fn show_now<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let window = match app.get_webview_window(MAIN) {
        Some(window) => window,
        None => build(app)?,
    };
    if window.is_minimized().unwrap_or(false) {
        window.unminimize()?;
    }
    window.show()?;
    window.set_focus()
}

/// Creates the window: 1040×680 (at least 860×580), centered. On macOS the
/// page draws under a transparent title bar (the window adds the room and
/// the drag region itself when `app_info` says `macos`).
fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    let builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
        .title("Lyrix")
        .inner_size(1040.0, 680.0)
        .min_inner_size(860.0, 580.0)
        .center();
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_opens_unless_started_minimized() {
        assert!(opens_window(["lyrix-desktop"]));
        assert!(opens_window(["lyrix-desktop", "--verbose"]));
        assert!(!opens_window(["lyrix-desktop", "--minimized"]));
        assert!(!opens_window(["lyrix-desktop", "--other", "--minimized"]));
        // The program's own path never counts.
        assert!(opens_window(["--minimized"]));
        assert!(opens_window(Vec::<String>::new()));
    }
}
