//! Windows: the Global System Media Transport Controls (GSMTC) session manager,
//! the same data the volume flyout shows.
//!
//! `GlobalSystemMediaTransportControlsSessionManager::RequestAsync()` gives the
//! manager; `GetSessions()` lists every app's session. Per session:
//! - `SourceAppUserModelId()` → `app_id`
//! - `TryGetMediaPropertiesAsync()` → `Title`, `Artist`, `AlbumTitle`
//! - `GetTimelineProperties()` → `Position`, `EndTime`, `StartTime` (TimeSpan,
//!   100 ns units) and `LastUpdatedTime` (DateTime, 100 ns since 1601-01-01 UTC);
//!   the position was true at `LastUpdatedTime`, so it is converted to an
//!   `Instant` via the current system time, never later than now.
//! - `GetPlaybackInfo()` → `PlaybackStatus` (Playing/Paused/Stopped/Closed/Opened/
//!   Changing) and `PlaybackRate` (optional, default 1.0)
//!
//! Duration is `EndTime - StartTime` when positive. The WinRT calls block, so
//! they run on a blocking thread.
//!
//! Details: Closed/Opened/Changing count as Stopped. The position is measured
//! from `StartTime` (normally 0). A `LastUpdatedTime` in the future or more
//! than a day old is not trusted and counts as "now". When `Artist` is empty,
//! `AlbumArtist` is used. Each WinRT wait has a time limit, so a hung app cannot
//! stall the poll, and the manager is kept between polls (dropped after an error).
//! A blocking read cannot be cancelled, so while one is still running (after
//! its snapshot gave up waiting) the next snapshot is an error instead of a
//! second read.

use super::{choose, gsmtc, instant_at, is_blocked, sanitize_rate, NowPlayingSource, ReadSlot};
use crate::types::{PlaybackSnapshot, Track};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use std::cell::Cell;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use windows::core::RuntimeType;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows_future::{AsyncOperationCompletedHandler, AsyncStatus, IAsyncOperation};

/// How long `RequestAsync` may take.
const MANAGER_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one app may take to hand over its media properties.
const PROPERTIES_TIMEOUT: Duration = Duration::from_millis(800);
/// How long a whole snapshot may take.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// See the module docs.
pub struct WindowsMediaSource {
    preferred: Vec<String>,
    blocked: Vec<String>,
    /// The session manager, requested on the first snapshot and kept.
    manager: Arc<Mutex<Option<SessionManager>>>,
    /// Held by the blocking read while it runs, even after the snapshot gave
    /// up waiting for it, so stuck reads never pile up.
    reading: ReadSlot,
}

impl WindowsMediaSource {
    pub fn new(preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self {
            preferred,
            blocked,
            manager: Arc::new(Mutex::new(None)),
            reading: ReadSlot::default(),
        }
    }
}

#[async_trait]
impl NowPlayingSource for WindowsMediaSource {
    fn name(&self) -> &'static str {
        "windows-media"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        let claim = self.reading.try_claim().ok_or_else(|| {
            anyhow!("the previous read of the Windows media sessions is still running")
        })?;
        let manager = Arc::clone(&self.manager);
        let blocked = self.blocked.clone();
        let read = tokio::task::spawn_blocking(move || {
            // Released when this read really ends, not when the snapshot stops waiting.
            let _claim = claim;
            read_all(&manager, &blocked)
        });
        let candidates = match tokio::time::timeout(SNAPSHOT_TIMEOUT, read).await {
            Ok(Ok(result)) => result?,
            Ok(Err(join_error)) => {
                return Err(anyhow!(
                    "reading the Windows media sessions failed: {join_error}"
                ))
            }
            Err(_) => {
                return Err(anyhow!(
                    "the Windows media sessions did not answer within 5 s"
                ))
            }
        };
        Ok(choose(candidates, &self.preferred, &self.blocked))
    }
}

thread_local! {
    static WINRT_READY: Cell<bool> = const { Cell::new(false) };
}

/// Initializes WinRT once on this (blocking-pool) thread. "Already
/// initialized" (S_FALSE) and "changed mode" (the thread is already in a
/// single-threaded apartment) results are fine for these calls.
fn init_winrt() {
    WINRT_READY.with(|ready| {
        if !ready.get() {
            // SAFETY: RoInitialize has no preconditions; it only sets up the
            // calling thread's apartment. The thread stays initialized for as
            // long as it lives, which is what a pooled worker thread wants.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            ready.set(true);
        }
    });
}

/// Reads every session, reusing the cached manager. Runs on a blocking thread.
fn read_all(
    cache: &Mutex<Option<SessionManager>>,
    blocked: &[String],
) -> anyhow::Result<Vec<PlaybackSnapshot>> {
    init_winrt();
    let cached = cache.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let manager = match cached {
        Some(manager) => manager,
        None => {
            let request = SessionManager::RequestAsync()
                .context("could not ask Windows for the media session manager")?;
            let manager = wait(&request, MANAGER_TIMEOUT)
                .context("Windows did not hand over the media session manager")?;
            *cache.lock().unwrap_or_else(PoisonError::into_inner) = Some(manager.clone());
            manager
        }
    };
    let result = read_sessions(&manager, blocked);
    if result.is_err() {
        // Ask for a fresh manager next time.
        *cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    result
}

/// Reads all sessions; a session that cannot be read is skipped.
fn read_sessions(
    manager: &SessionManager,
    blocked: &[String],
) -> anyhow::Result<Vec<PlaybackSnapshot>> {
    let sessions = manager
        .GetSessions()
        .context("could not list the media sessions")?;
    let count = sessions
        .Size()
        .context("could not count the media sessions")?;
    let mut snapshots = Vec::new();
    for index in 0..count {
        let Ok(session) = sessions.GetAt(index) else {
            continue;
        };
        match read_session(&session, blocked) {
            Ok(Some(snapshot)) => snapshots.push(snapshot),
            Ok(None) => {}
            Err(err) => tracing::debug!("skipping a media session: {err:#}"),
        }
    }
    Ok(snapshots)
}

/// Reads one session. `None` for a blocked app.
fn read_session(session: &Session, blocked: &[String]) -> anyhow::Result<Option<PlaybackSnapshot>> {
    let app_id = session
        .SourceAppUserModelId()
        .map(|id| id.to_string_lossy())
        .unwrap_or_default();
    if is_blocked(&app_id, blocked) {
        // Never read anything else from a blocked app.
        return Ok(None);
    }

    let info = session.GetPlaybackInfo().context("no playback info")?;
    let status = gsmtc::status_from_raw(info.PlaybackStatus().context("no playback status")?.0);
    let rate = info
        .PlaybackRate()
        .and_then(|rate| rate.Value())
        .map(sanitize_rate)
        .unwrap_or(1.0);

    let request = session
        .TryGetMediaPropertiesAsync()
        .context("could not ask for media properties")?;
    let properties = wait(&request, PROPERTIES_TIMEOUT).context("no media properties")?;
    let text = |value: windows::core::Result<windows::core::HSTRING>| {
        value.map(|text| text.to_string_lossy()).unwrap_or_default()
    };
    let title = text(properties.Title());
    let mut artist = text(properties.Artist());
    if artist.trim().is_empty() {
        artist = text(properties.AlbumArtist());
    }
    let album = Some(text(properties.AlbumTitle())).filter(|album| !album.trim().is_empty());

    let timeline = session
        .GetTimelineProperties()
        .context("no timeline properties")?;
    let start = timeline.StartTime().map(|span| span.Duration).unwrap_or(0);
    let end = timeline.EndTime().map(|span| span.Duration).unwrap_or(0);
    let position = timeline.Position().map(|span| span.Duration).unwrap_or(0);
    let last_updated = timeline
        .LastUpdatedTime()
        .map(|time| time.UniversalTime)
        .ok();

    let now = Instant::now();
    let now_sys = SystemTime::now();
    let position_at = last_updated
        .and_then(gsmtc::system_time_from_ticks)
        .map(|at| instant_at(at, now_sys, now))
        .unwrap_or(now);

    Ok(Some(PlaybackSnapshot {
        track: Track {
            title,
            artist,
            album,
            duration_ms: gsmtc::duration_ms(start, end),
            spotify_id: None,
        },
        status,
        position_ms: gsmtc::position_ms(start, position),
        position_at,
        rate,
        app_id,
    }))
}

/// Waits for a WinRT async operation, at most `timeout`. On timeout the
/// operation is cancelled and an error returned.
fn wait<T>(operation: &IAsyncOperation<T>, timeout: Duration) -> anyhow::Result<T>
where
    T: RuntimeType + 'static,
{
    if operation.Status()? != AsyncStatus::Started {
        return Ok(operation.GetResults()?);
    }
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
    // The handler is called even when the operation finished in the meantime.
    operation.SetCompleted(&AsyncOperationCompletedHandler::new(move |_, _| {
        let _ = done_tx.try_send(());
        Ok(())
    }))?;
    match done_rx.recv_timeout(timeout) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => Ok(operation.GetResults()?),
        Err(RecvTimeoutError::Timeout) => {
            let _ = operation.Cancel();
            Err(anyhow!("no answer within {} ms", timeout.as_millis()))
        }
    }
}
