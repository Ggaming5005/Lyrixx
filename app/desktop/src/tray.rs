//! The tray icon (the menu bar on macOS) and its menu:
//!
//! ```text
//! Open Lyrix
//! Never Gonna Give You Up - Rick Astley     (disabled; "Nothing playing")
//! Pause sharing                             ("Resume sharing" while paused)
//! ──────────
//! Quit Lyrix
//! ```
//!
//! The tooltip is `Lyrix` or `Lyrix: <title> - <artist>`. A left click on the
//! icon opens the window on Windows and Linux; on macOS it opens the menu, as
//! menu bar icons do (Linux tray hosts may show the menu instead too).
//!
//! An icon nobody can see (stock GNOME shows none) is no way back into Lyrix.
//! [`Presence`] tells whether it can be seen; when not, the window opens even
//! with `--minimized` and closing it quits Lyrix.

mod host;

use crate::supervisor::Supervisor;
use crate::view::View;
use crate::{actions, window};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::image::Image;
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _, Runtime};

const OPEN_ID: &str = "open";
const SONG_ID: &str = "song";
const PAUSE_ID: &str = "pause";
const QUIT_ID: &str = "quit";

/// Shown in the menu when no song is followed.
const NOTHING_PLAYING: &str = "Nothing playing";
/// The longest song line in the menu, in characters.
const MAX_MENU_SONG_CHARS: usize = 60;
/// The longest tooltip: Windows keeps at most 127 characters.
const MAX_TOOLTIP_CHARS: usize = 120;

/// The texts the tray shows for a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayText {
    pub tooltip: String,
    /// The disabled song line, escaped for menus.
    pub song: String,
    pub pause_label: &'static str,
}

/// What the tray shows for `view`.
pub fn tray_text(view: &View) -> TrayText {
    let song = view
        .engine
        .now
        .as_ref()
        .map(|now| song_label(&now.title, &now.artist))
        .filter(|label| !label.is_empty());
    let pause_label = if view.engine.paused {
        "Resume sharing"
    } else {
        "Pause sharing"
    };
    match song {
        Some(song) => TrayText {
            tooltip: lyrix::template::truncate_chars(&format!("Lyrix: {song}"), MAX_TOOLTIP_CHARS),
            song: menu_text(&lyrix::template::truncate_chars(&song, MAX_MENU_SONG_CHARS)),
            pause_label,
        },
        None => TrayText {
            tooltip: "Lyrix".to_string(),
            song: NOTHING_PLAYING.to_string(),
            pause_label,
        },
    }
}

/// `Title - Artist`, just the title without an artist, or "" without a
/// title. Line breaks and other control characters become spaces.
fn song_label(title: &str, artist: &str) -> String {
    let one_line = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string()
    };
    let (title, artist) = (one_line(title), one_line(artist));
    match (title.is_empty(), artist.is_empty()) {
        (true, _) => String::new(),
        (false, true) => title,
        (false, false) => format!("{title} - {artist}"),
    }
}

/// Menus read `&` as a keyboard shortcut marker; `&&` shows one `&`.
fn menu_text(s: &str) -> String {
    s.replace('&', "&&")
}

/// The menu items whose text changes.
struct Items<R: Runtime> {
    song: MenuItem<R>,
    pause: MenuItem<R>,
}

/// Creates the tray icon and keeps it in step with the view. Errors when the
/// icon cannot be made; whether one that was made can be seen is up to
/// [`Presence`].
pub fn create<R: Runtime>(app: &AppHandle<R>, supervisor: Arc<Supervisor>) -> tauri::Result<()> {
    let text = tray_text(&supervisor.view());
    let open = MenuItem::with_id(app, OPEN_ID, "Open Lyrix", true, None::<&str>)?;
    let song = MenuItem::with_id(app, SONG_ID, &text.song, false, None::<&str>)?;
    let pause = MenuItem::with_id(app, PAUSE_ID, text.pause_label, true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, QUIT_ID, "Quit Lyrix", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &song, &pause, &separator, &quit])?;

    let menu_supervisor = supervisor.clone();
    let builder = TrayIconBuilder::with_id("main")
        .icon(icon()?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(&text.tooltip)
        .menu(&menu)
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(move |app, event| on_menu(app, &menu_supervisor, event))
        .on_tray_icon_event(on_icon);
    let tray = builder.build(app)?;

    let items = Items { song, pause };
    let views = supervisor.subscribe();
    tauri::async_runtime::spawn(follow_views(tray, items, text, views));
    Ok(())
}

/// Whether the tray icon can be seen, and so whether Lyrix may keep running
/// once its window is closed. Managed as `Arc<Presence>`.
pub struct Presence {
    /// Whether [`create`] made the icon.
    created: bool,
    host: host::Host,
    /// The last answer of [`Presence::check`].
    seen: AtomicBool,
}

impl Presence {
    /// `created`: whether [`create`] made the icon. Call it on the main
    /// thread, in `setup`.
    pub fn new(created: bool) -> Self {
        Self {
            created,
            host: host::Host::current(),
            seen: AtomicBool::new(created),
        }
    }

    /// Whether the icon can be seen now (on Linux, whether a tray runs to
    /// show it). The answer is also kept for [`seen`].
    pub async fn check(&self) -> bool {
        let seen = self.created && self.host.shows_icons().await;
        self.seen.store(seen, Ordering::Relaxed);
        seen
    }
}

/// Whether the tray icon could be seen when last checked; false before
/// there is a tray at all.
pub fn seen<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<Arc<Presence>>()
        .is_some_and(|presence| presence.seen.load(Ordering::Relaxed))
}

/// The tray image: in color, or on macOS white on transparent, which the menu
/// bar recolors to match its theme (a template image).
fn icon() -> tauri::Result<Image<'static>> {
    let png: &[u8] = if cfg!(target_os = "macos") {
        include_bytes!("../icons/tray-mono.png")
    } else {
        include_bytes!("../icons/tray.png")
    };
    Image::from_bytes(png)
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, supervisor: &Arc<Supervisor>, event: MenuEvent) {
    match event.id().as_ref() {
        OPEN_ID => window::show(app),
        PAUSE_ID => {
            // The marker file is the truth, not the view, which a running
            // engine updates on its next poll.
            let paused = actions::is_paused(&supervisor.paths().pause_marker);
            if let Err(e) = supervisor.set_paused(!paused) {
                tracing::error!("could not change the pause state: {e:#}");
            }
        }
        QUIT_ID => crate::quit(app.clone(), supervisor.clone()),
        _ => {}
    }
}

fn on_icon<R: Runtime>(tray: &TrayIcon<R>, event: TrayIconEvent) {
    if cfg!(target_os = "macos") {
        return;
    }
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        window::show(tray.app_handle());
    }
}

/// Updates the tooltip and the menu whenever what they show changes.
async fn follow_views<R: Runtime>(
    tray: TrayIcon<R>,
    items: Items<R>,
    mut shown: TrayText,
    mut views: tokio::sync::watch::Receiver<View>,
) {
    while views.changed().await.is_ok() {
        let text = tray_text(&views.borrow_and_update());
        if text == shown {
            continue;
        }
        let result = tray
            .set_tooltip(Some(&text.tooltip))
            .and_then(|()| items.song.set_text(&text.song))
            .and_then(|()| items.pause.set_text(text.pause_label));
        if let Err(e) = result {
            tracing::warn!("could not update the tray icon: {e}");
        }
        shown = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyrix::view::{EngineView, LyricsView, NowView};

    fn now(title: &str, artist: &str) -> NowView {
        NowView {
            title: title.into(),
            artist: artist.into(),
            album: None,
            duration_ms: None,
            app: "spotify".into(),
            playing: true,
            position_ms: 0,
            position_at_unix_ms: 0,
            rate: 1.0,
            artwork: None,
            song_key: "key".into(),
            song_offset_ms: 0,
            global_offset_ms: 0,
            lyrics: LyricsView::Searching,
        }
    }

    fn view(now: Option<NowView>, paused: bool) -> View {
        View {
            running: true,
            error: None,
            engine: EngineView {
                source: "mpris".into(),
                paused,
                now,
                status: None,
                targets: Vec::new(),
            },
        }
    }

    #[test]
    fn nothing_playing() {
        assert_eq!(
            tray_text(&view(None, false)),
            TrayText {
                tooltip: "Lyrix".into(),
                song: "Nothing playing".into(),
                pause_label: "Pause sharing",
            }
        );
        assert_eq!(tray_text(&View::default()).song, "Nothing playing");
    }

    #[test]
    fn a_song_shows_in_the_tooltip_and_the_menu() {
        let text = tray_text(&view(
            Some(now("Never Gonna Give You Up", "Rick Astley")),
            false,
        ));
        assert_eq!(text.tooltip, "Lyrix: Never Gonna Give You Up - Rick Astley");
        assert_eq!(text.song, "Never Gonna Give You Up - Rick Astley");
    }

    #[test]
    fn paused_offers_to_resume() {
        let text = tray_text(&view(Some(now("Song", "Artist")), true));
        assert_eq!(text.pause_label, "Resume sharing");
        assert_eq!(tray_text(&view(None, true)).pause_label, "Resume sharing");
    }

    #[test]
    fn missing_parts_and_odd_characters() {
        assert_eq!(tray_text(&view(Some(now("Song", " ")), false)).song, "Song");
        assert_eq!(
            tray_text(&view(Some(now(" ", "Artist")), false)).song,
            "Nothing playing"
        );
        assert_eq!(
            tray_text(&view(Some(now("A\nB\tC", "D")), false)).song,
            "A B C - D"
        );
        let text = tray_text(&view(Some(now("Bonnie & Clyde", "Jay-Z & Beyoncé")), false));
        assert_eq!(text.song, "Bonnie && Clyde - Jay-Z && Beyoncé");
        assert_eq!(text.tooltip, "Lyrix: Bonnie & Clyde - Jay-Z & Beyoncé");
    }

    #[tokio::test]
    async fn an_icon_that_was_not_made_is_never_seen() {
        let presence = Presence {
            created: false,
            host: host::Host::default(),
            seen: AtomicBool::new(true),
        };
        assert!(!presence.check().await);
        assert!(!presence.seen.load(Ordering::Relaxed));
    }

    #[test]
    fn long_songs_are_shortened() {
        let title = "a".repeat(300);
        let text = tray_text(&view(Some(now(&title, "Artist")), false));
        assert!(text.song.chars().count() <= MAX_MENU_SONG_CHARS);
        assert!(text.song.ends_with('…'));
        assert!(text.tooltip.chars().count() <= MAX_TOOLTIP_CHARS);
        assert!(text.tooltip.starts_with("Lyrix: aaa"));
    }
}
