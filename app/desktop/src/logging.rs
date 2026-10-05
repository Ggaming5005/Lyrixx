//! Logs go to `lyrix.log` in the logs folder and, in debug builds, also to
//! stderr. Every start begins a new `lyrix.log` and keeps the last one as
//! `lyrix.old.log`, so the message of a crash survives the next start.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::EnvFilter;

/// The log file's name inside the logs folder.
pub const LOG_FILE: &str = "lyrix.log";

/// The last start's log, next to [`LOG_FILE`].
pub const OLD_LOG_FILE: &str = "lyrix.old.log";

/// The filter when `RUST_LOG` is not set: Lyrix's own messages from info
/// level, everything else (Tauri, the webview, D-Bus) from warnings.
const DEFAULT_FILTER: &str = "warn,lyrix=info,lyrix_desktop=info";

/// Starts logging to a new `<dir>/lyrix.log` (creating the folder and keeping
/// the last log as `lyrix.old.log`) and, in debug builds, to stderr.
/// `RUST_LOG` replaces the default filter. A log file that cannot be created
/// or kept is reported in the log that remains; logging never stops the app.
/// Returns the log file's path.
pub fn init(dir: &Path) -> PathBuf {
    let path = dir.join(LOG_FILE);
    let kept = keep_last_log(dir);
    let file = std::fs::create_dir_all(dir).and_then(|()| std::fs::File::create(&path));
    let (file_layer, file_error) = match file {
        Ok(file) => (
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(file)),
            ),
            None,
        ),
        Err(e) => (None, Some(e)),
    };
    let stderr_layer = cfg!(debug_assertions)
        .then(|| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));

    // A second logger (never the case here) is not worth failing over.
    let _ = tracing_subscriber::registry()
        .with(filter())
        .with(file_layer)
        .with(stderr_layer)
        .try_init();

    if let Err(e) = kept {
        tracing::warn!("could not keep the last log as {OLD_LOG_FILE}: {e}");
    }
    if let Some(e) = file_error {
        tracing::warn!("could not create the log file {}: {e}", path.display());
    }
    path
}

/// Moves `<dir>/lyrix.log` to `<dir>/lyrix.old.log`, replacing the older one
/// (`rename` replaces it on Windows too). Without a log there is nothing to
/// keep.
fn keep_last_log(dir: &Path) -> std::io::Result<()> {
    match std::fs::rename(dir.join(LOG_FILE), dir.join(OLD_LOG_FILE)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        kept => kept,
    }
}

/// `RUST_LOG` when it is set and valid, otherwise [`DEFAULT_FILTER`].
fn filter() -> EnvFilter {
    match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => {
            EnvFilter::try_new(&spec).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
        }
        _ => EnvFilter::new(DEFAULT_FILTER),
    }
}

/// Writes panics to the log before the default handler runs (which prints
/// them to stderr, invisible in a release build, and ends the process: the
/// release profile uses `panic = "abort"`).
pub fn log_panics() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("Lyrix crashed: {info}");
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_filter_parses() {
        assert!(EnvFilter::try_new(DEFAULT_FILTER).is_ok());
    }

    #[test]
    fn the_last_log_is_kept_once() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("logs");
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();

        // No folder and no log yet: nothing to keep.
        keep_last_log(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        keep_last_log(&dir).unwrap();
        assert_eq!(read(OLD_LOG_FILE), None);

        // A crash's message survives the next start.
        write(LOG_FILE, "Lyrix crashed: boom");
        keep_last_log(&dir).unwrap();
        assert_eq!(read(LOG_FILE), None);
        assert_eq!(read(OLD_LOG_FILE).as_deref(), Some("Lyrix crashed: boom"));

        // Only the last log is kept.
        write(LOG_FILE, "a quiet run");
        keep_last_log(&dir).unwrap();
        assert_eq!(read(OLD_LOG_FILE).as_deref(), Some("a quiet run"));
    }
}
