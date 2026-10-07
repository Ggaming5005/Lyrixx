//! Whether a created tray icon can be seen.
//!
//! On Windows and macOS it always can. On Linux the icon goes through
//! libayatana-appindicator, which creates it even when nothing will ever show
//! it. It shows when a StatusNotifierWatcher (KDE, most panels and docks, GNOME
//! with the AppIndicator extension) owns its name on the session bus or,
//! failing that, as an old-style (XEmbed) icon when Lyrix draws through X11
//! and an X11 system tray runs (i3bar, older panels). Stock GNOME has neither.

/// What decides whether the tray icon can be seen.
#[derive(Debug, Clone, Default)]
pub struct Host {
    /// The X11 display GTK draws on, when it uses X11 (not Wayland).
    #[cfg(target_os = "linux")]
    x11_display: Option<std::ffi::CString>,
}

#[cfg(not(target_os = "linux"))]
impl Host {
    /// The host Lyrix runs on.
    pub fn current() -> Self {
        Self {}
    }

    /// Whether a tray icon can be seen now: always.
    pub async fn shows_icons(&self) -> bool {
        true
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Host;
    use std::ffi::{CStr, CString};
    use std::time::Duration;

    /// The name libayatana-appindicator looks for on the session bus.
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";

    /// The longest wait for the session bus, then for the X server.
    const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

    impl Host {
        /// The host Lyrix runs on. Call it on the main thread once GTK runs
        /// (in `setup`), to learn whether GTK draws through X11.
        pub fn current() -> Self {
            use gtk::glib::object::ObjectExt as _;
            let x11_display = gtk::is_initialized_main_thread()
                .then(gtk::gdk::Display::default)
                .flatten()
                .filter(|display| display.type_().name() == "GdkX11Display")
                .and_then(|display| CString::new(display.name().as_str()).ok());
            Self { x11_display }
        }

        /// Whether a tray icon can be seen now: a StatusNotifierWatcher runs,
        /// or GTK draws through X11 and an X11 system tray runs. Waits at most
        /// about 4 s; what cannot be asked counts as no tray.
        pub async fn shows_icons(&self) -> bool {
            match tokio::time::timeout(CHECK_TIMEOUT, status_notifier_watcher()).await {
                Ok(Ok(true)) => return true,
                Ok(Ok(false)) => {}
                Ok(Err(e)) => tracing::debug!("could not ask the session bus for a tray: {e}"),
                Err(_) => tracing::debug!("the session bus did not answer about a tray"),
            }
            let Some(display) = self.x11_display.clone() else {
                return false;
            };
            let check = tauri::async_runtime::spawn_blocking(move || x11_system_tray(&display));
            matches!(
                tokio::time::timeout(CHECK_TIMEOUT, check).await,
                Ok(Ok(true))
            )
        }
    }

    /// Whether a StatusNotifierWatcher runs on the session bus.
    async fn status_notifier_watcher() -> zbus::Result<bool> {
        let bus = zbus::Connection::session().await?;
        watcher_on(&bus).await
    }

    /// Whether [`WATCHER`] has an owner on `bus`.
    async fn watcher_on(bus: &zbus::Connection) -> zbus::Result<bool> {
        let dbus = zbus::fdo::DBusProxy::new(bus).await?;
        let watcher = zbus::names::BusName::from_static_str(WATCHER)?;
        Ok(dbus.name_has_owner(watcher).await?)
    }

    /// Whether an X11 system tray (the owner of `_NET_SYSTEM_TRAY_S<screen>`)
    /// runs on `display`. False when libX11 or the display cannot be opened.
    fn x11_system_tray(display: &CStr) -> bool {
        let Ok(xlib) = x11_dl::xlib::Xlib::open() else {
            return false;
        };
        // SAFETY: the connection is our own, used on this thread only and
        // closed before returning; the names are valid C strings.
        unsafe {
            let connection = (xlib.XOpenDisplay)(display.as_ptr());
            if connection.is_null() {
                return false;
            }
            let screen = (xlib.XDefaultScreen)(connection);
            let owned = match CString::new(format!("_NET_SYSTEM_TRAY_S{screen}")) {
                Ok(name) => {
                    // Only if it exists: a selection nobody named has no owner.
                    let atom = (xlib.XInternAtom)(connection, name.as_ptr(), x11_dl::xlib::True);
                    atom != 0 && (xlib.XGetSelectionOwner)(connection, atom) != 0
                }
                Err(_) => false,
            };
            (xlib.XCloseDisplay)(connection);
            owned
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::{BufRead as _, BufReader};
        use std::process::{Child, Command, Stdio};

        /// A private `dbus-daemon`, stopped when dropped.
        struct PrivateBus {
            child: Child,
            address: String,
            _dir: tempfile::TempDir,
        }

        impl PrivateBus {
            /// None without `dbus-daemon`.
            fn start() -> Option<Self> {
                let dir = tempfile::tempdir().unwrap();
                let socket = dir.path().join("bus");
                let mut child = Command::new("dbus-daemon")
                    .arg("--session")
                    .arg("--nofork")
                    .arg("--print-address")
                    .arg(format!("--address=unix:path={}", socket.display()))
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()?;
                let mut line = String::new();
                BufReader::new(child.stdout.take().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let address = line.trim().to_string();
                assert!(address.starts_with("unix:"), "no bus address: {line:?}");
                Some(Self {
                    child,
                    address,
                    _dir: dir,
                })
            }

            async fn connect(&self) -> zbus::Connection {
                zbus::connection::Builder::address(self.address.as_str())
                    .unwrap()
                    .build()
                    .await
                    .unwrap()
            }
        }

        impl Drop for PrivateBus {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        #[tokio::test]
        async fn a_status_notifier_watcher_means_a_tray() {
            let Some(bus) = PrivateBus::start() else {
                eprintln!("note: dbus-daemon is not on PATH, skipping the tray host test");
                return;
            };
            let lyrix = bus.connect().await;
            // Stock GNOME: nothing watches for tray icons.
            assert!(!watcher_on(&lyrix).await.unwrap());

            // A panel that shows them starts...
            let panel = bus.connect().await;
            assert!(panel.request_name(WATCHER).await.is_ok());
            assert!(watcher_on(&lyrix).await.unwrap());

            // ...and goes away.
            assert!(panel.release_name(WATCHER).await.unwrap());
            assert!(!watcher_on(&lyrix).await.unwrap());
        }

        #[test]
        fn without_an_x11_display_there_is_no_x11_tray() {
            let missing = CString::new(":4242").unwrap();
            assert!(!x11_system_tray(&missing));
        }
    }
}
