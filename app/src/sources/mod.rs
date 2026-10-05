//! Reading what is playing from the operating system.
//!
//! - Windows: the system media session (GSMTC), which every app in the media
//!   flyout reports to, browsers included.
//! - Linux: MPRIS over the D-Bus session bus.
//! - macOS: Spotify and Apple Music through AppleScript, or every app in the
//!   Now Playing widget when `mediaremote-adapter` is installed.
//!
//! Every source reads all the players it can see, drops the blocked ones and
//! picks one with [`pick_session`]. When the picked player is stopped, the
//! source reports `Ok(None)`: nothing is playing or paused.

mod artwork;
#[cfg(any(target_os = "macos", test))]
pub mod macos;
#[cfg(target_os = "linux")]
pub mod mpris;
#[cfg(windows)]
pub mod windows;

use crate::config::SourcesConfig;
use crate::types::{PlaybackSnapshot, PlaybackStatus};
use async_trait::async_trait;
#[cfg(any(windows, target_os = "macos", test))]
use std::time::{Duration, Instant, SystemTime};

/// Something that can say what is playing right now.
#[async_trait]
pub trait NowPlayingSource: Send + Sync {
    /// Short name for logs, e.g. `windows-media`, `mpris`, `macos`.
    fn name(&self) -> &'static str;

    /// Reads the current playback. `Ok(None)` means nothing is playing or paused.
    /// Errors are for a broken source (no session bus, API failure); the engine
    /// logs them and tries again on the next poll.
    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>>;

    /// Cover art of the song the last [`snapshot`](Self::snapshot) reported,
    /// as a URL a window can load (`https:`, `http:` or `data:`; local files
    /// are read and returned as `data:` URLs). `Ok(None)` when the player
    /// has none. Called once per track change, off the render path.
    async fn artwork(&self) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

/// Chooses which of several players to follow:
/// 1. drop snapshots whose `app_id` contains a `blocked` entry (case-insensitive),
/// 2. prefer `Playing` over `Paused` over `Stopped`,
/// 3. among equals, prefer the earliest match in `preferred` (substring of `app_id`,
///    case-insensitive), then players with a non-empty title,
/// 4. otherwise keep the original order.
///
/// Entries of `preferred` and `blocked` are trimmed; empty entries are ignored
/// (an empty entry would otherwise match every player). A title made only of
/// whitespace counts as empty.
pub fn pick_session(
    candidates: Vec<PlaybackSnapshot>,
    preferred: &[String],
    blocked: &[String],
) -> Option<PlaybackSnapshot> {
    let blocked = normalized_patterns(blocked);
    let preferred = normalized_patterns(preferred);

    // (status rank, preference rank, title missing): smaller is better.
    let mut best: Option<((u8, usize, bool), PlaybackSnapshot)> = None;
    for snapshot in candidates {
        let app_id = snapshot.app_id.to_lowercase();
        if blocked.iter().any(|b| app_id.contains(b.as_str())) {
            continue;
        }
        let preference = preferred
            .iter()
            .position(|p| app_id.contains(p.as_str()))
            .unwrap_or(usize::MAX);
        let key = (
            status_rank(snapshot.status),
            preference,
            snapshot.track.title.trim().is_empty(),
        );
        // Strictly better only, so the first of several equal candidates wins.
        let better = match &best {
            Some((best_key, _)) => key < *best_key,
            None => true,
        };
        if better {
            best = Some((key, snapshot));
        }
    }
    best.map(|(_, snapshot)| snapshot)
}

/// The source for this operating system.
///
/// Linux → [`mpris::MprisSource`], Windows → [`windows::WindowsMediaSource`],
/// macOS → [`macos::MacSource`]; any other operating system is an error. No
/// source connects to anything until its first snapshot.
pub fn default_source(
    config: &SourcesConfig,
    blocked_apps: &[String],
) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    platform_source(config, blocked_apps.to_vec())
}

#[cfg(target_os = "linux")]
fn platform_source(
    config: &SourcesConfig,
    blocked: Vec<String>,
) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    Ok(Box::new(mpris::MprisSource::new(
        config.preferred_apps.clone(),
        blocked,
    )))
}

#[cfg(windows)]
fn platform_source(
    config: &SourcesConfig,
    blocked: Vec<String>,
) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    Ok(Box::new(windows::WindowsMediaSource::new(
        config.preferred_apps.clone(),
        blocked,
    )))
}

#[cfg(target_os = "macos")]
fn platform_source(
    config: &SourcesConfig,
    blocked: Vec<String>,
) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    Ok(Box::new(macos::MacSource::new(
        config.macos_adapter_dir.clone(),
        config.preferred_apps.clone(),
        blocked,
    )))
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn platform_source(
    _config: &SourcesConfig,
    _blocked: Vec<String>,
) -> anyhow::Result<Box<dyn NowPlayingSource>> {
    Err(unsupported_os(std::env::consts::OS))
}

/// The error for an operating system Lyrix cannot read yet.
#[cfg(any(test, not(any(target_os = "linux", windows, target_os = "macos"))))]
fn unsupported_os(os: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "reading what is playing is not supported on {os} yet \
         (Lyrix supports Windows, Linux and macOS)"
    )
}

// ---------------------------------------------------------------------------
// Helpers shared by the platform sources. They are plain functions so that
// they are unit-tested on every operating system.
// ---------------------------------------------------------------------------

/// [`pick_session`] for a source: the chosen player, or `None` when there is
/// none or the chosen one is stopped.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
fn choose(
    candidates: Vec<PlaybackSnapshot>,
    preferred: &[String],
    blocked: &[String],
) -> Option<PlaybackSnapshot> {
    pick_session(candidates, preferred, blocked)
        .filter(|snapshot| snapshot.status != PlaybackStatus::Stopped)
}

/// True when `app_id` contains one of the `blocked` entries (case-insensitive),
/// with the same matching rules as [`pick_session`]. Sources use it to skip
/// blocked players before reading anything else from them.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
fn is_blocked(app_id: &str, blocked: &[String]) -> bool {
    let app_id = app_id.to_lowercase();
    normalized_patterns(blocked)
        .iter()
        .any(|b| app_id.contains(b.as_str()))
}

/// Lowercased, trimmed, non-empty patterns, in their original order.
fn normalized_patterns(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

fn status_rank(status: PlaybackStatus) -> u8 {
    match status {
        PlaybackStatus::Playing => 0,
        PlaybackStatus::Paused => 1,
        PlaybackStatus::Stopped => 2,
    }
}

/// A playback rate that is safe to extrapolate with: finite and positive,
/// otherwise 1.0.
#[cfg_attr(
    not(any(target_os = "linux", windows, target_os = "macos")),
    allow(dead_code)
)]
fn sanitize_rate(rate: f64) -> f64 {
    if rate.is_finite() && rate > 0.0 {
        rate
    } else {
        1.0
    }
}

/// The oldest player timestamp that is still trusted.
#[cfg(any(windows, target_os = "macos", test))]
const MAX_TIMESTAMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Converts the wall-clock moment `at` into an [`Instant`], given the current
/// wall-clock time and `Instant`. The age (`now_sys - at`) must be within
/// `[0, 1 day]`; a timestamp in the future or older than a day is not trusted
/// and gives `now`. The result is never later than `now`.
#[cfg(any(windows, target_os = "macos", test))]
fn instant_at(at: SystemTime, now_sys: SystemTime, now: Instant) -> Instant {
    match now_sys.duration_since(at) {
        Ok(age) if age <= MAX_TIMESTAMP_AGE => now.checked_sub(age).unwrap_or(now),
        _ => now,
    }
}

/// `UNIX_EPOCH + ms`, for negative values too. `None` when not representable.
#[cfg(any(target_os = "macos", test))]
fn system_time_from_unix_ms(ms: i64) -> Option<SystemTime> {
    let magnitude = Duration::from_millis(ms.unsigned_abs());
    if ms >= 0 {
        std::time::UNIX_EPOCH.checked_add(magnitude)
    } else {
        std::time::UNIX_EPOCH.checked_sub(magnitude)
    }
}

/// Extracts a Spotify track id (22 base62 characters) from any of the forms
/// players use:
/// - URI: `spotify:track:<id>`
/// - URL: `https://open.spotify.com/track/<id>?si=…` (also `/intl-xx/track/<id>`)
/// - D-Bus object path: `/com/spotify/track/<id>` (any path with a `spotify`
///   segment followed by `track/<id>` at the end)
///
/// Anything else (episodes, ads, local files, other services) gives `None`.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn spotify_track_id(text: &str) -> Option<String> {
    let text = text.trim();
    let candidate = if let Some(rest) = strip_prefix_ignore_ascii_case(text, "spotify:track:") {
        rest.split([':', '?', '#']).next().unwrap_or("")
    } else if let Some(rest) = strip_prefix_ignore_ascii_case(text, "https://")
        .or_else(|| strip_prefix_ignore_ascii_case(text, "http://"))
    {
        let slash = rest.find('/')?;
        let (host, path) = (rest.get(..slash)?, rest.get(slash..)?);
        let host = host.to_ascii_lowercase();
        if host != "spotify.com" && !host.ends_with(".spotify.com") {
            return None;
        }
        let path = path.split(['?', '#']).next().unwrap_or("");
        let mut segments = path.split('/').filter(|s| !s.is_empty());
        segments.find(|s| s.eq_ignore_ascii_case("track"))?;
        segments.next()?
    } else if text.starts_with('/') {
        let segments: Vec<&str> = text.split('/').filter(|s| !s.is_empty()).collect();
        match segments.as_slice() {
            [before @ .., track, id] if track.eq_ignore_ascii_case("track") => {
                if !before.iter().any(|s| s.eq_ignore_ascii_case("spotify")) {
                    return None;
                }
                *id
            }
            _ => return None,
        }
    } else {
        return None;
    };
    if candidate.len() == 22 && candidate.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Some(candidate.to_string())
    } else {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

/// Lets only one blocking read run at a time. A blocking thread cannot be
/// cancelled: when a read outlives its time limit it keeps running, and a new
/// read on every poll would pile up stuck threads (and starve the blocking
/// pool that `tokio::fs` also uses). The claim moves into the blocking task
/// and is released when that task really ends.
#[cfg(any(windows, test))]
#[derive(Debug, Default)]
struct ReadSlot(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(any(windows, test))]
impl ReadSlot {
    /// The claim, or `None` while an earlier read still runs.
    fn try_claim(&self) -> Option<ReadClaim> {
        use std::sync::atomic::Ordering;
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ReadClaim(std::sync::Arc::clone(&self.0)))
    }
}

/// Frees its [`ReadSlot`] when dropped.
#[cfg(any(windows, test))]
#[derive(Debug)]
struct ReadClaim(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(any(windows, test))]
impl Drop for ReadClaim {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Pure conversions for the Windows media session (GSMTC), kept here so they
/// are tested on every operating system.
#[cfg(any(windows, test))]
mod gsmtc {
    use crate::types::PlaybackStatus;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// 100 ns ticks per millisecond.
    const TICKS_PER_MS: i64 = 10_000;
    /// Seconds from 1601-01-01 (the Windows epoch) to 1970-01-01.
    const UNIX_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
    const TICKS_PER_SEC: i64 = 10_000_000;

    /// `GlobalSystemMediaTransportControlsSessionPlaybackStatus` (its raw value)
    /// → status: Playing (4) and Paused (5) map to themselves; Stopped (3),
    /// Closed (0), Opened (1), Changing (2) and unknown values are `Stopped`.
    pub(super) fn status_from_raw(raw: i32) -> PlaybackStatus {
        match raw {
            4 => PlaybackStatus::Playing,
            5 => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        }
    }

    /// A `TimeSpan` (100 ns ticks) in whole milliseconds; negative spans are 0.
    pub(super) fn ticks_to_ms(ticks: i64) -> u64 {
        u64::try_from(ticks / TICKS_PER_MS).unwrap_or(0)
    }

    /// The song position: `position - start` when the timeline starts later
    /// than 0 (it normally starts at 0), never negative.
    pub(super) fn position_ms(start_ticks: i64, position_ticks: i64) -> u64 {
        let relative = if start_ticks > 0 {
            position_ticks.saturating_sub(start_ticks)
        } else {
            position_ticks
        };
        ticks_to_ms(relative)
    }

    /// `end - start` in ms when it is positive (at least 1 ms), else `None`.
    pub(super) fn duration_ms(start_ticks: i64, end_ticks: i64) -> Option<u64> {
        let span = end_ticks.checked_sub(start_ticks)?;
        match ticks_to_ms(span) {
            0 => None,
            ms => Some(ms),
        }
    }

    /// A `DateTime` (100 ns ticks since 1601-01-01 UTC) as a `SystemTime`.
    /// `None` when it cannot be represented.
    pub(super) fn system_time_from_ticks(ticks: i64) -> Option<SystemTime> {
        let unix_ticks = ticks.checked_sub(UNIX_EPOCH_OFFSET_SECS.checked_mul(TICKS_PER_SEC)?)?;
        let magnitude = unix_ticks.unsigned_abs();
        let span = Duration::new(
            magnitude / TICKS_PER_SEC as u64,
            // Below 10^7, times 100 stays below 10^9: always valid nanoseconds.
            u32::try_from((magnitude % TICKS_PER_SEC as u64) * 100).ok()?,
        );
        if unix_ticks >= 0 {
            UNIX_EPOCH.checked_add(span)
        } else {
            UNIX_EPOCH.checked_sub(span)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Track;
    use std::time::UNIX_EPOCH;

    fn snap(app_id: &str, status: PlaybackStatus, title: &str) -> PlaybackSnapshot {
        PlaybackSnapshot {
            track: Track {
                title: title.to_string(),
                artist: "Artist".to_string(),
                ..Track::default()
            },
            status,
            position_ms: 0,
            position_at: Instant::now(),
            rate: 1.0,
            app_id: app_id.to_string(),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn picked_app(
        candidates: Vec<PlaybackSnapshot>,
        preferred: &[&str],
        blocked: &[&str],
    ) -> Option<String> {
        pick_session(candidates, &strings(preferred), &strings(blocked)).map(|s| s.app_id)
    }

    use PlaybackStatus::{Paused, Playing, Stopped};

    // ---- pick_session ----------------------------------------------------

    #[test]
    fn pick_empty_is_none() {
        assert_eq!(picked_app(vec![], &[], &[]), None);
        assert_eq!(picked_app(vec![], &["spotify"], &["chrome"]), None);
    }

    #[test]
    fn pick_single_candidate() {
        assert_eq!(
            picked_app(vec![snap("spotify", Paused, "Song")], &[], &[]),
            Some("spotify".into())
        );
    }

    #[test]
    fn pick_single_stopped_candidate_is_still_returned() {
        let picked = pick_session(vec![snap("vlc", Stopped, "")], &[], &[]).unwrap();
        assert_eq!(picked.status, Stopped);
    }

    #[test]
    fn pick_drops_blocked_case_insensitive_substring() {
        let candidates = vec![
            snap(
                "org.mpris.MediaPlayer2.Chromium.instance42",
                Playing,
                "Video",
            ),
            snap("org.mpris.MediaPlayer2.spotify", Paused, "Song"),
        ];
        assert_eq!(
            picked_app(candidates, &[], &["CHROMIUM"]),
            Some("org.mpris.MediaPlayer2.spotify".into())
        );
    }

    #[test]
    fn pick_all_blocked_is_none() {
        let candidates = vec![
            snap("chrome.exe", Playing, "A"),
            snap("Chrome", Paused, "B"),
        ];
        assert_eq!(picked_app(candidates, &[], &["chrome"]), None);
    }

    #[test]
    fn pick_blocked_wins_over_preferred() {
        let candidates = vec![snap("Spotify.exe", Playing, "A"), snap("vlc", Paused, "B")];
        assert_eq!(
            picked_app(candidates, &["spotify"], &["spotify"]),
            Some("vlc".into())
        );
    }

    #[test]
    fn pick_empty_and_blank_patterns_are_ignored() {
        let candidates = vec![snap("a", Paused, "A"), snap("b", Playing, "B")];
        // An empty blocked entry must not block everything.
        assert_eq!(picked_app(candidates.clone(), &[], &[""]), Some("b".into()));
        assert_eq!(
            picked_app(candidates.clone(), &[], &["   "]),
            Some("b".into())
        );
        // An empty preferred entry must not count as a match.
        let equal = vec![snap("a", Playing, "A"), snap("b", Playing, "B")];
        assert_eq!(picked_app(equal, &["", "b"], &[]), Some("b".into()));
    }

    #[test]
    fn pick_patterns_are_trimmed() {
        let candidates = vec![snap("firefox", Playing, "A"), snap("spotify", Playing, "B")];
        assert_eq!(
            picked_app(candidates.clone(), &["  spotify "], &[]),
            Some("spotify".into())
        );
        assert_eq!(
            picked_app(candidates, &[], &[" FIREFOX\t"]),
            Some("spotify".into())
        );
    }

    #[test]
    fn pick_playing_over_paused_over_stopped() {
        let candidates = vec![
            snap("stopped", Stopped, "S"),
            snap("paused", Paused, "P"),
            snap("playing", Playing, "Y"),
        ];
        assert_eq!(picked_app(candidates, &[], &[]), Some("playing".into()));

        let candidates = vec![snap("stopped", Stopped, "S"), snap("paused", Paused, "P")];
        assert_eq!(picked_app(candidates, &[], &[]), Some("paused".into()));
    }

    #[test]
    fn pick_status_beats_preference() {
        let candidates = vec![snap("spotify", Paused, "A"), snap("firefox", Playing, "B")];
        assert_eq!(
            picked_app(candidates, &["spotify"], &[]),
            Some("firefox".into())
        );
    }

    #[test]
    fn pick_status_beats_title() {
        let candidates = vec![snap("named", Paused, "Song"), snap("untitled", Playing, "")];
        assert_eq!(picked_app(candidates, &[], &[]), Some("untitled".into()));
    }

    #[test]
    fn pick_preferred_among_equals() {
        let candidates = vec![
            snap("org.mpris.MediaPlayer2.firefox", Playing, "A"),
            snap("org.mpris.MediaPlayer2.spotify", Playing, "B"),
        ];
        assert_eq!(
            picked_app(candidates, &["Spotify"], &[]),
            Some("org.mpris.MediaPlayer2.spotify".into())
        );
    }

    #[test]
    fn pick_earliest_preferred_entry_wins() {
        let candidates = vec![
            snap("spotify", Playing, "A"),
            snap("vlc", Playing, "B"),
            snap("firefox", Playing, "C"),
        ];
        assert_eq!(
            picked_app(candidates.clone(), &["vlc", "spotify"], &[]),
            Some("vlc".into())
        );
        assert_eq!(
            picked_app(candidates.clone(), &["firefox", "vlc"], &[]),
            Some("firefox".into())
        );
        // A preferred entry that matches nothing does not disturb the order.
        assert_eq!(
            picked_app(candidates, &["mpv", "vlc"], &[]),
            Some("vlc".into())
        );
    }

    #[test]
    fn pick_preference_beats_title() {
        let candidates = vec![snap("vlc", Playing, "Song"), snap("spotify", Playing, "")];
        assert_eq!(
            picked_app(candidates, &["spotify"], &[]),
            Some("spotify".into())
        );
    }

    #[test]
    fn pick_non_empty_title_among_equals() {
        let candidates = vec![
            snap("first", Playing, ""),
            snap("second", Playing, "   "),
            snap("third", Playing, "Song"),
        ];
        assert_eq!(picked_app(candidates, &[], &[]), Some("third".into()));
    }

    #[test]
    fn pick_keeps_original_order_for_ties() {
        let candidates = vec![
            snap("one", Playing, "A"),
            snap("two", Playing, "B"),
            snap("three", Playing, "C"),
        ];
        assert_eq!(picked_app(candidates, &[], &[]), Some("one".into()));

        let candidates = vec![snap("one", Paused, ""), snap("two", Paused, "")];
        assert_eq!(picked_app(candidates, &[], &[]), Some("one".into()));

        // Two candidates matching the same preferred entry: first one wins.
        let candidates = vec![
            snap("spotify-a", Playing, "A"),
            snap("spotify-b", Playing, "B"),
        ];
        assert_eq!(
            picked_app(candidates, &["spotify"], &[]),
            Some("spotify-a".into())
        );
    }

    #[test]
    fn pick_returns_the_whole_snapshot_unchanged() {
        let mut wanted = snap("spotify", Playing, "Song");
        wanted.position_ms = 12_345;
        wanted.rate = 1.5;
        wanted.track.album = Some("Album".into());
        let candidates = vec![snap("other", Paused, "X"), wanted.clone()];
        assert_eq!(pick_session(candidates, &[], &[]), Some(wanted));
    }

    #[test]
    fn pick_unicode_app_ids_and_patterns() {
        let candidates = vec![
            snap("Плеер.ÄPP", Playing, "Песня"),
            snap("音乐播放器", Playing, "歌"),
        ];
        assert_eq!(
            picked_app(candidates.clone(), &["音乐"], &[]),
            Some("音乐播放器".into())
        );
        assert_eq!(
            picked_app(candidates.clone(), &["плеер.äpp"], &[]),
            Some("Плеер.ÄPP".into())
        );
        assert_eq!(
            picked_app(candidates, &[], &["ПЛЕЕР"]),
            Some("音乐播放器".into())
        );
    }

    #[test]
    fn pick_empty_app_id() {
        let candidates = vec![snap("", Playing, "Song")];
        assert_eq!(picked_app(candidates.clone(), &[], &["x"]), Some("".into()));
        assert_eq!(picked_app(candidates, &["x"], &[]), Some("".into()));
    }

    #[test]
    fn pick_many_candidates() {
        let mut candidates: Vec<PlaybackSnapshot> = (0..1000)
            .map(|i| snap(&format!("player{i}"), Paused, "T"))
            .collect();
        candidates.push(snap("player-last", Playing, "T"));
        assert_eq!(picked_app(candidates, &[], &[]), Some("player-last".into()));
    }

    #[test]
    fn pick_realistic_linux_desktop() {
        // Spotify paused, a browser playing a video, playerctld already filtered by the source.
        let candidates = vec![
            snap(
                "org.mpris.MediaPlayer2.firefox.instance_1_42",
                Playing,
                "Some video",
            ),
            snap("org.mpris.MediaPlayer2.spotify", Paused, "Song"),
            snap("org.mpris.MediaPlayer2.vlc", Stopped, ""),
        ];
        assert_eq!(
            picked_app(candidates.clone(), &["spotify"], &[]),
            Some("org.mpris.MediaPlayer2.firefox.instance_1_42".into())
        );
        assert_eq!(
            picked_app(candidates, &["spotify"], &["firefox"]),
            Some("org.mpris.MediaPlayer2.spotify".into())
        );
    }

    // ---- choose / is_blocked / sanitize_rate -------------------------------

    #[test]
    fn choose_drops_a_stopped_pick() {
        assert!(choose(vec![snap("vlc", Stopped, "Song")], &[], &[]).is_none());
        assert!(choose(vec![], &[], &[]).is_none());
        let picked = choose(
            vec![snap("vlc", Stopped, "A"), snap("mpv", Paused, "B")],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(picked.app_id, "mpv");
        let picked = choose(vec![snap("mpv", Playing, "B")], &[], &[]).unwrap();
        assert_eq!(picked.status, Playing);
    }

    #[test]
    fn choose_applies_blocking() {
        assert!(choose(
            vec![snap("chrome", Playing, "A")],
            &[],
            &strings(&["Chrome"])
        )
        .is_none());
    }

    #[test]
    fn is_blocked_rules() {
        assert!(is_blocked(
            "org.mpris.MediaPlayer2.Chromium",
            &strings(&["chromium"])
        ));
        assert!(is_blocked("Chrome.EXE", &strings(&[" chrome "])));
        assert!(!is_blocked("spotify", &strings(&["", "  "])));
        assert!(!is_blocked("spotify", &[]));
        assert!(!is_blocked("", &strings(&["x"])));
        assert!(is_blocked("ÄPFEL", &strings(&["äpfel"])));
    }

    #[test]
    fn sanitize_rate_rules() {
        assert_eq!(sanitize_rate(1.0), 1.0);
        assert_eq!(sanitize_rate(1.25), 1.25);
        assert_eq!(sanitize_rate(0.5), 0.5);
        assert_eq!(sanitize_rate(0.0), 1.0);
        assert_eq!(sanitize_rate(-1.0), 1.0);
        assert_eq!(sanitize_rate(f64::NAN), 1.0);
        assert_eq!(sanitize_rate(f64::INFINITY), 1.0);
        assert_eq!(sanitize_rate(f64::NEG_INFINITY), 1.0);
    }

    // ---- default_source / unsupported_os ----------------------------------

    #[test]
    fn unsupported_os_error_names_the_os() {
        let message = unsupported_os("plan9").to_string();
        assert!(message.contains("plan9"), "{message}");
        assert!(message.contains("not supported"), "{message}");
        assert!(message.contains("yet"), "{message}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn default_source_on_linux_is_mpris() {
        let config = SourcesConfig {
            preferred_apps: strings(&["spotify"]),
            macos_adapter_dir: None,
        };
        let source = default_source(&config, &strings(&["chrome"])).unwrap();
        assert_eq!(source.name(), "mpris");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_source_on_macos() {
        let source = default_source(&SourcesConfig::default(), &[]).unwrap();
        assert_eq!(source.name(), "macos");
    }

    #[cfg(windows)]
    #[test]
    fn default_source_on_windows() {
        let source = default_source(&SourcesConfig::default(), &[]).unwrap();
        assert_eq!(source.name(), "windows-media");
    }

    // ---- time helpers ------------------------------------------------------

    #[test]
    fn instant_at_recent_past() {
        let now = Instant::now();
        let now_sys = SystemTime::now();
        let at = now_sys - Duration::from_millis(1_500);
        let instant = instant_at(at, now_sys, now);
        assert_eq!(now.duration_since(instant), Duration::from_millis(1_500));
    }

    #[test]
    fn instant_at_exactly_now_and_exactly_one_day() {
        let now = Instant::now();
        let now_sys = SystemTime::now();
        assert_eq!(instant_at(now_sys, now_sys, now), now);
        let day_ago = now_sys - MAX_TIMESTAMP_AGE;
        let instant = instant_at(day_ago, now_sys, now);
        // Either a day earlier, or `now` when the platform's Instant cannot go that far back.
        assert!(instant == now || now.duration_since(instant) == MAX_TIMESTAMP_AGE);
    }

    #[test]
    fn instant_at_future_or_too_old_is_now() {
        let now = Instant::now();
        let now_sys = SystemTime::now();
        assert_eq!(
            instant_at(now_sys + Duration::from_secs(5), now_sys, now),
            now
        );
        assert_eq!(
            instant_at(
                now_sys - MAX_TIMESTAMP_AGE - Duration::from_millis(1),
                now_sys,
                now
            ),
            now
        );
        assert_eq!(instant_at(UNIX_EPOCH, now_sys, now), now);
    }

    #[test]
    fn system_time_from_unix_ms_values() {
        assert_eq!(system_time_from_unix_ms(0), Some(UNIX_EPOCH));
        assert_eq!(
            system_time_from_unix_ms(1_700_000_000_123),
            Some(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123))
        );
        assert_eq!(
            system_time_from_unix_ms(-1_000),
            UNIX_EPOCH.checked_sub(Duration::from_secs(1))
        );
        // Extreme values never panic.
        let _ = system_time_from_unix_ms(i64::MAX);
        let _ = system_time_from_unix_ms(i64::MIN);
    }

    // ---- spotify_track_id ----------------------------------------------------

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    #[test]
    fn spotify_id_from_every_form() {
        let forms = [
            format!("spotify:track:{ID}"),
            format!("SPOTIFY:TRACK:{ID}"),
            format!("/com/spotify/track/{ID}"),
            format!("/org/ncspot/spotify/track/{ID}"),
            format!("https://open.spotify.com/track/{ID}"),
            format!("https://open.spotify.com/track/{ID}?si=abc123"),
            format!("https://open.spotify.com/track/{ID}#x"),
            format!("http://open.spotify.com/track/{ID}/"),
            format!("https://open.spotify.com/intl-de/track/{ID}?si=1"),
            format!("HTTPS://OPEN.SPOTIFY.COM/track/{ID}"),
            format!("https://play.spotify.com/track/{ID}"),
            format!("  spotify:track:{ID}  "),
        ];
        for form in forms {
            assert_eq!(spotify_track_id(&form), Some(ID.to_string()), "{form}");
        }
    }

    #[test]
    fn spotify_id_rejects_other_things() {
        let rejected = [
            String::new(),
            "   ".to_string(),
            "/".to_string(),
            "spotify:track:".to_string(),
            "spotify:episode:4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "/com/spotify/ad/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "/com/spotify/episode/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "/org/mpris/MediaPlayer2/Track/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_string(),
            "/com/spotify/track/short".to_string(),
            "/com/spotify/track/4uLU6hMCjMI75M1A2tKUQCX".to_string(),
            "/com/spotify/track/4uLU6hMCjMI75M1A2tKUQ_".to_string(),
            "/com/spotify/track/4uLU6hMCjMI75M1A2tKUQC/extra".to_string(),
            "https://open.spotify.com/album/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "https://open.spotify.com/track/".to_string(),
            "https://open.spotify.com".to_string(),
            "https://evil.com/track/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "https://notspotify.com/track/4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "file:///home/me/music/song.mp3".to_string(),
            "4uLU6hMCjMI75M1A2tKUQC".to_string(),
            "spotify:local:Artist:Album:Title:215".to_string(),
        ];
        for text in rejected {
            assert_eq!(spotify_track_id(&text), None, "{text}");
        }
    }

    #[test]
    fn spotify_id_unicode_never_panics() {
        let inputs = [
            "spotify:track:ééééééééééééééééééééé",
            "spotify:trac",
            "ſpotify:track:4uLU6hMCjMI75M1A2tKUQC",
            "https://open.spotify.com/track/４uLU6hMCjMI75M1A2tKUQ",
            "https://ö",
            "/spotify/track/🎵🎵🎵🎵🎵🎵",
            "🎵",
            "h",
        ];
        for input in inputs {
            assert_eq!(spotify_track_id(input), None, "{input}");
        }
    }

    #[test]
    fn strip_prefix_ignore_ascii_case_boundaries() {
        assert_eq!(strip_prefix_ignore_ascii_case("ABCdef", "abc"), Some("def"));
        assert_eq!(strip_prefix_ignore_ascii_case("ab", "abc"), None);
        assert_eq!(strip_prefix_ignore_ascii_case("äbc", "abc"), None);
        assert_eq!(strip_prefix_ignore_ascii_case("", ""), Some(""));
    }

    // ---- ReadSlot ------------------------------------------------------------

    #[test]
    fn read_slot_allows_one_read_at_a_time() {
        let slot = ReadSlot::default();
        let claim = slot.try_claim().expect("a free slot");
        assert!(slot.try_claim().is_none());
        assert!(slot.try_claim().is_none());
        drop(claim);
        let again = slot.try_claim().expect("free again after the read ended");
        drop(again);
        assert!(slot.try_claim().is_some());
    }

    #[test]
    fn read_slot_claim_released_on_another_thread() {
        let slot = ReadSlot::default();
        let claim = slot.try_claim().unwrap();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let _claim = claim;
            go_rx.recv().unwrap();
        });
        // The read is still running on its thread: no second read.
        assert!(slot.try_claim().is_none());
        go_tx.send(()).unwrap();
        worker.join().unwrap();
        assert!(slot.try_claim().is_some());
    }

    #[tokio::test]
    async fn read_slot_survives_a_timed_out_blocking_task() {
        let slot = ReadSlot::default();
        let claim = slot.try_claim().unwrap();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let mut task = tokio::task::spawn_blocking(move || {
            let _claim = claim;
            go_rx.recv().ok();
        });
        // The caller gives up waiting (as a snapshot does after its time
        // limit), but the blocking task keeps its claim until it really ends.
        let waited = tokio::time::timeout(Duration::from_millis(20), &mut task).await;
        assert!(waited.is_err());
        assert!(slot.try_claim().is_none());
        go_tx.send(()).unwrap();
        task.await.unwrap();
        assert!(slot.try_claim().is_some());
    }

    // ---- gsmtc ---------------------------------------------------------------

    #[test]
    fn gsmtc_status_mapping() {
        assert_eq!(gsmtc::status_from_raw(0), Stopped); // Closed
        assert_eq!(gsmtc::status_from_raw(1), Stopped); // Opened
        assert_eq!(gsmtc::status_from_raw(2), Stopped); // Changing
        assert_eq!(gsmtc::status_from_raw(3), Stopped); // Stopped
        assert_eq!(gsmtc::status_from_raw(4), Playing);
        assert_eq!(gsmtc::status_from_raw(5), Paused);
        assert_eq!(gsmtc::status_from_raw(6), Stopped);
        assert_eq!(gsmtc::status_from_raw(-1), Stopped);
        assert_eq!(gsmtc::status_from_raw(i32::MAX), Stopped);
    }

    #[test]
    fn gsmtc_ticks_to_ms() {
        assert_eq!(gsmtc::ticks_to_ms(0), 0);
        assert_eq!(gsmtc::ticks_to_ms(9_999), 0);
        assert_eq!(gsmtc::ticks_to_ms(10_000), 1);
        assert_eq!(gsmtc::ticks_to_ms(2_150_000_000), 215_000);
        assert_eq!(gsmtc::ticks_to_ms(-1), 0);
        assert_eq!(gsmtc::ticks_to_ms(-10_000_000), 0);
        assert_eq!(gsmtc::ticks_to_ms(i64::MIN), 0);
        assert_eq!(gsmtc::ticks_to_ms(i64::MAX), (i64::MAX / 10_000) as u64);
    }

    #[test]
    fn gsmtc_position() {
        assert_eq!(gsmtc::position_ms(0, 615_000_000), 61_500);
        assert_eq!(gsmtc::position_ms(10_000_000, 25_000_000), 1_500);
        assert_eq!(gsmtc::position_ms(10_000_000, 5_000_000), 0);
        assert_eq!(gsmtc::position_ms(-10_000_000, 5_000_000), 500);
        assert_eq!(gsmtc::position_ms(0, -5), 0);
        assert_eq!(gsmtc::position_ms(i64::MAX, i64::MIN), 0);
        assert_eq!(
            gsmtc::position_ms(1, i64::MAX),
            ((i64::MAX - 1) / 10_000) as u64
        );
    }

    #[test]
    fn gsmtc_duration() {
        assert_eq!(gsmtc::duration_ms(0, 2_150_000_000), Some(215_000));
        assert_eq!(gsmtc::duration_ms(10_000_000, 30_000_000), Some(2_000));
        assert_eq!(gsmtc::duration_ms(0, 0), None);
        assert_eq!(gsmtc::duration_ms(0, 9_999), None);
        assert_eq!(gsmtc::duration_ms(0, 10_000), Some(1));
        assert_eq!(gsmtc::duration_ms(30_000_000, 10_000_000), None);
        assert_eq!(gsmtc::duration_ms(i64::MIN, i64::MAX), None);
        assert_eq!(gsmtc::duration_ms(i64::MAX, i64::MIN), None);
    }

    #[test]
    fn gsmtc_datetime_conversion() {
        // 1970-01-01 in Windows ticks.
        let epoch_ticks = 11_644_473_600 * 10_000_000;
        assert_eq!(gsmtc::system_time_from_ticks(epoch_ticks), Some(UNIX_EPOCH));
        // 2024-01-01T00:00:00.1234567Z
        let unix_secs = 1_704_067_200_i64;
        let ticks = epoch_ticks + unix_secs * 10_000_000 + 1_234_567;
        assert_eq!(
            gsmtc::system_time_from_ticks(ticks),
            Some(
                UNIX_EPOCH
                    + Duration::from_secs(unix_secs as u64)
                    + Duration::from_nanos(123_456_700)
            )
        );
        // Before 1970 (e.g. the zero value, 1601-01-01).
        assert_eq!(
            gsmtc::system_time_from_ticks(0),
            UNIX_EPOCH.checked_sub(Duration::from_secs(11_644_473_600))
        );
        assert_eq!(
            gsmtc::system_time_from_ticks(epoch_ticks - 5),
            UNIX_EPOCH.checked_sub(Duration::from_nanos(500))
        );
        // Extremes never panic.
        let _ = gsmtc::system_time_from_ticks(i64::MAX);
        assert_eq!(gsmtc::system_time_from_ticks(i64::MIN), None);
    }

    #[test]
    fn gsmtc_last_updated_to_instant() {
        let now = Instant::now();
        let now_sys = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let ticks_for = |unix_ms: i64| {
            (11_644_473_600 + unix_ms / 1000) * 10_000_000 + (unix_ms % 1000) * 10_000
        };

        // Updated 2.5 s ago.
        let at = gsmtc::system_time_from_ticks(ticks_for(1_800_000_000_000 - 2_500)).unwrap();
        assert_eq!(
            now.duration_since(instant_at(at, now_sys, now)),
            Duration::from_millis(2_500)
        );
        // A LastUpdatedTime in the future → now.
        let at = gsmtc::system_time_from_ticks(ticks_for(1_800_000_000_000 + 60_000)).unwrap();
        assert_eq!(instant_at(at, now_sys, now), now);
        // More than a day old → now.
        let at = gsmtc::system_time_from_ticks(ticks_for(1_800_000_000_000 - 86_400_001)).unwrap();
        assert_eq!(instant_at(at, now_sys, now), now);
        // Never set (0 ticks, year 1601) → now.
        let at = gsmtc::system_time_from_ticks(0).unwrap();
        assert_eq!(instant_at(at, now_sys, now), now);
    }
}
