//! Logs go to `lyrix.log` in the logs folder, replaced on every start, and in
//! debug builds also to stderr.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::EnvFilter;

/// The log file's name inside the logs folder.
pub const LOG_FILE: &str = "lyrix.log";

/// The filter when `RUST_LOG` is not set: Lyrix's own messages from info
/// level, everything else (Tauri, the webview, D-Bus) from warnings.
const DEFAULT_FILTER: &str = "warn,lyrix=info,lyrix_desktop=info";

/// Starts logging to `<dir>/lyrix.log` (creating the folder and replacing the
/// file) and, in debug builds, to stderr. `RUST_LOG` replaces the default
/// filter. A log file that cannot be created is reported in the log that
/// remains; logging never stops the app. Returns the log file's path.
pub fn init(dir: &Path) -> PathBuf {
    let path = dir.join(LOG_FILE);
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

    if let Some(e) = file_error {
        tracing::warn!("could not create the log file {}: {e}", path.display());
    }
    path
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
}
