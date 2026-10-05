//! The one window, `main`.
//!
//! Closing it (title bar, Alt+F4, Cmd+W) first only hides it and sends the
//! page [`CLOSING_EVENT`], so the page can save settings it still holds.
//! [`CLOSE_GRACE`] later the window is destroyed (freeing the webview) while
//! Lyrix keeps running in the tray, or, with no tray anyone can see, Lyrix
//! quits like "Quit Lyrix". Opening the window meanwhile reuses it and calls
//! the close off; opening it later creates a new one.

use crate::supervisor::Supervisor;
use crate::tray;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter as _, EventTarget, Manager, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, WindowEvent,
};

/// The window's label, which `capabilities/default.json` names.
pub const MAIN: &str = "main";

/// The flag autostart launches Lyrix with: start in the tray, without the window.
pub const MINIMIZED_FLAG: &str = "--minimized";

/// Sent (without a payload) to the window when it is closed, while it is
/// hidden and still alive for [`CLOSE_GRACE`].
pub const CLOSING_EVENT: &str = "lyrix://closing";

/// How long a closed window stays hidden before it goes: time for the page to
/// send what it still holds.
pub const CLOSE_GRACE: Duration = Duration::from_secs(1);

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
    // A window closed moments ago is still there, hidden: keep it.
    if let Some(closes) = app.try_state::<Closes>() {
        closes.call_off();
    }
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

/// Counts the window's closes and openings, so a close that waits out
/// [`CLOSE_GRACE`] knows whether anything happened to the window since.
/// Managed by the app.
#[derive(Debug, Default)]
pub struct Closes(AtomicU64);

impl Closes {
    /// A new close; returns its number.
    pub fn begin(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The window was opened again: no close begun before goes through.
    pub fn call_off(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether close `number` is still the last thing that happened to the
    /// window.
    pub fn is_pending(&self, number: u64) -> bool {
        self.0.load(Ordering::SeqCst) == number
    }
}

/// How a close ends once the page had its time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseEnd {
    /// The window was opened again meanwhile: it stays.
    Reopened,
    /// The window goes; Lyrix keeps running in the tray.
    Destroy,
    /// No tray anyone can see would be left to get back in or quit, so Lyrix
    /// quits, clearing every status.
    Quit,
}

/// How a close ends: `pending` when the window was not opened again since,
/// `tray_seen` when the tray icon can be seen.
pub fn close_end(pending: bool, tray_seen: bool) -> CloseEnd {
    match (pending, tray_seen) {
        (false, _) => CloseEnd::Reopened,
        (true, true) => CloseEnd::Destroy,
        (true, false) => CloseEnd::Quit,
    }
}

/// Handles the window's own events (`Builder::on_window_event`): a close is
/// held off, see the module documentation.
pub fn on_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        if window.label() == MAIN {
            api.prevent_close();
            close(window.app_handle());
        }
    }
}

/// Hides the window, tells the page, and ends the close after
/// [`CLOSE_GRACE`]. Runs on the main thread.
fn close<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(MAIN) else {
        return;
    };
    let number = match app.try_state::<Closes>() {
        Some(closes) => closes.begin(),
        None => 0,
    };
    if let Err(e) = window.hide() {
        tracing::warn!("could not hide the window: {e}");
    }
    if let Err(e) = window.emit_to(EventTarget::webview_window(MAIN), CLOSING_EVENT, ()) {
        tracing::warn!("could not tell the window it is closing: {e}");
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(CLOSE_GRACE).await;
        let tray_seen = match app.try_state::<Arc<tray::Presence>>() {
            Some(presence) => presence.inner().clone().check().await,
            None => false,
        };
        let handle = app.clone();
        let queued = app.run_on_main_thread(move || finish_close(&handle, number, tray_seen));
        if let Err(e) = queued {
            tracing::error!("could not close the window: {e}");
        }
    });
}

/// Ends close `number`. Runs on the main thread, like [`show_now`], so the
/// window cannot be opened again halfway.
fn finish_close<R: Runtime>(app: &AppHandle<R>, number: u64, tray_seen: bool) {
    let pending = match app.try_state::<Closes>() {
        Some(closes) => closes.is_pending(number),
        None => true,
    };
    match close_end(pending, tray_seen) {
        CloseEnd::Reopened => {}
        CloseEnd::Destroy => {
            if let Some(window) = app.get_webview_window(MAIN) {
                if let Err(e) = window.destroy() {
                    tracing::warn!("could not close the window: {e}");
                }
            }
        }
        CloseEnd::Quit => {
            tracing::info!("the window was closed and no tray icon can be seen");
            match app.try_state::<Arc<Supervisor>>() {
                Some(supervisor) => crate::quit(app.clone(), supervisor.inner().clone()),
                None => app.exit(0),
            }
        }
    }
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

    #[test]
    fn a_close_ends_in_the_tray_or_quits_without_one() {
        assert_eq!(close_end(true, true), CloseEnd::Destroy);
        assert_eq!(close_end(true, false), CloseEnd::Quit);
        assert_eq!(close_end(false, true), CloseEnd::Reopened);
        assert_eq!(close_end(false, false), CloseEnd::Reopened);
    }

    #[test]
    fn opening_the_window_calls_a_close_off() {
        let closes = Closes::default();
        let first = closes.begin();
        assert!(closes.is_pending(first));

        closes.call_off();
        assert!(!closes.is_pending(first));

        // Closed again: only the last close goes through.
        let second = closes.begin();
        assert!(!closes.is_pending(first));
        assert!(closes.is_pending(second));
    }
}
