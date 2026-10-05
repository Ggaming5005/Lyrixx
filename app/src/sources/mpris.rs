//! Linux: MPRIS players on the D-Bus session bus.
//!
//! Every bus name starting with `org.mpris.MediaPlayer2.` is a player. From the
//! object `/org/mpris/MediaPlayer2`, interface `org.mpris.MediaPlayer2.Player`:
//! - `PlaybackStatus` (`"Playing"`, `"Paused"`, `"Stopped"`)
//! - `Metadata` (a{sv}): `xesam:title` (s), `xesam:artist` (as), `xesam:album` (s),
//!   `mpris:length` (x or t, microseconds), `mpris:trackid` (o or s),
//!   `xesam:url` (s), `mpris:artUrl` (s)
//! - `Position` (x, microseconds; read live, may be missing or fail for some players)
//! - `Rate` (d, default 1.0)
//!
//! Spotify exposes its track id as `mpris:trackid = /com/spotify/track/<id>` and
//! `xesam:url = https://open.spotify.com/track/<id>`; either fills `spotify_id`.
//!
//! `org.mpris.MediaPlayer2.playerctld` is skipped: it mirrors another player.
//! Properties are never cached (players do not announce `Position` changes),
//! and each player gets 800 ms to answer, so one hung player cannot stall a poll.
//!
//! Cover art is the `mpris:artUrl` of the player the last snapshot picked:
//! `http(s)` URLs as they are, local `file://` images (browsers and most
//! local players write one) read into a `data:` URL, at most 2 MiB.

use super::artwork::artwork_from_url;
use super::{choose, is_blocked, sanitize_rate, spotify_track_id, NowPlayingSource};
use crate::types::{PlaybackSnapshot, PlaybackStatus, Track};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::PoisonError;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{OwnedValue, Value};
use zbus::Connection;

const PLAYER_PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld re-exports whichever player was active last; reading it too
/// would show every song twice.
const PLAYERCTLD: &str = "org.mpris.MediaPlayer2.playerctld";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
/// How long one player may take to answer all of its property reads.
const PLAYER_TIMEOUT: Duration = Duration::from_millis(800);
/// How long connecting to the bus or listing its names may take.
const BUS_TIMEOUT: Duration = Duration::from_secs(2);

/// See the module docs. Holds one session-bus connection, opened lazily on the
/// first [`snapshot`](NowPlayingSource::snapshot) and reopened after an error.
pub struct MprisSource {
    preferred: Vec<String>,
    blocked: Vec<String>,
    /// Bus address to use instead of the session bus (tests only).
    address: Option<String>,
    connection: Mutex<Option<Connection>>,
    /// `mpris:artUrl` of the player the last snapshot picked.
    art_url: std::sync::Mutex<Option<String>>,
}

impl MprisSource {
    pub fn new(preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self {
            preferred,
            blocked,
            address: None,
            connection: Mutex::new(None),
            art_url: std::sync::Mutex::new(None),
        }
    }

    /// A source reading the bus at `address` instead of the session bus.
    #[cfg(test)]
    fn with_address(address: &str, preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self {
            address: Some(address.to_string()),
            ..Self::new(preferred, blocked)
        }
    }

    /// The open connection, opening it first when there is none.
    async fn connection(&self) -> anyhow::Result<Connection> {
        let mut slot = self.connection.lock().await;
        if let Some(connection) = slot.as_ref() {
            return Ok(connection.clone());
        }
        let connection = tokio::time::timeout(BUS_TIMEOUT, self.open())
            .await
            .map_err(|_| anyhow!("the D-Bus session bus did not answer within 2 s"))??;
        *slot = Some(connection.clone());
        Ok(connection)
    }

    async fn open(&self) -> anyhow::Result<Connection> {
        match &self.address {
            Some(address) => zbus::connection::Builder::address(address.as_str())
                .with_context(|| format!("invalid D-Bus address {address}"))?
                .build()
                .await
                .with_context(|| format!("could not connect to the D-Bus bus at {address}")),
            None => Connection::session()
                .await
                .context("could not connect to the D-Bus session bus"),
        }
    }
}

/// Extracts a Spotify track id from an MPRIS track id or URL, if it is one.
///
/// Accepts `/com/spotify/track/<id>` (and other object paths with a `spotify`
/// segment followed by `track/<id>`), `spotify:track:<id>` and
/// `https://open.spotify.com/track/<id>` (query strings are ignored). The id
/// must be 22 base62 characters.
pub fn spotify_id_from(trackid_or_url: &str) -> Option<String> {
    spotify_track_id(trackid_or_url)
}

#[async_trait]
impl NowPlayingSource for MprisSource {
    fn name(&self) -> &'static str {
        "mpris"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        let connection = self.connection().await?;
        match read_players(&connection, &self.blocked).await {
            Ok(players) => {
                let (picked, art_url) = pick(players, &self.preferred, &self.blocked);
                *self.art_url.lock().unwrap_or_else(PoisonError::into_inner) = art_url;
                Ok(picked)
            }
            Err(err) => {
                // The connection may be broken: open a new one on the next poll.
                *self.connection.lock().await = None;
                Err(err)
            }
        }
    }

    async fn artwork(&self) -> anyhow::Result<Option<String>> {
        let art_url = self
            .art_url
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match art_url {
            Some(url) => artwork_from_url(&url).await,
            None => Ok(None),
        }
    }
}

/// What one player reported.
#[derive(Debug, Clone, PartialEq)]
struct Player {
    snapshot: PlaybackSnapshot,
    /// `mpris:artUrl`, when the player gave one.
    art_url: Option<String>,
}

/// [`choose`] among the players, with the picked player's art URL. Bus names
/// are unique, so the app id tells which player was picked.
fn pick(
    players: Vec<Player>,
    preferred: &[String],
    blocked: &[String],
) -> (Option<PlaybackSnapshot>, Option<String>) {
    let snapshots = players.iter().map(|p| p.snapshot.clone()).collect();
    let picked = choose(snapshots, preferred, blocked);
    let art_url = picked.as_ref().and_then(|picked| {
        players
            .into_iter()
            .find(|p| p.snapshot.app_id == picked.app_id)
            .and_then(|p| p.art_url)
    });
    (picked, art_url)
}

/// True for a player's bus name (not playerctld).
fn is_player_name(name: &str) -> bool {
    name.len() > PLAYER_PREFIX.len() && name.starts_with(PLAYER_PREFIX) && name != PLAYERCTLD
}

/// Reads every player that is not blocked, in bus-name order. Players that fail
/// or do not answer in time are skipped; only a failure of the bus itself is
/// an error.
async fn read_players(connection: &Connection, blocked: &[String]) -> anyhow::Result<Vec<Player>> {
    let bus = zbus::fdo::DBusProxy::builder(connection)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .context("could not talk to the D-Bus daemon")?;
    let names = tokio::time::timeout(BUS_TIMEOUT, bus.list_names())
        .await
        .map_err(|_| anyhow!("the D-Bus daemon did not list its names within 2 s"))?
        .context("could not list the D-Bus names")?;

    let mut players: Vec<String> = names
        .iter()
        .map(|name| name.as_str().to_string())
        .filter(|name| is_player_name(name) && !is_blocked(name, blocked))
        .collect();
    // A stable order, so that equal players never take turns between polls.
    players.sort();
    players.dedup();

    let mut reads = tokio::task::JoinSet::new();
    for (index, name) in players.into_iter().enumerate() {
        let connection = connection.clone();
        reads.spawn(async move {
            let result =
                tokio::time::timeout(PLAYER_TIMEOUT, read_player(&connection, &name)).await;
            (index, name, result)
        });
    }

    let mut found = Vec::new();
    while let Some(joined) = reads.join_next().await {
        match joined {
            Ok((index, _, Ok(Ok(player)))) => found.push((index, player)),
            Ok((_, name, Ok(Err(err)))) => {
                tracing::debug!(player = %name, "skipping MPRIS player: {err:#}");
            }
            Ok((_, name, Err(_))) => {
                tracing::debug!(player = %name, "skipping MPRIS player: no answer within 800 ms");
            }
            Err(err) => tracing::debug!("MPRIS player read task failed: {err}"),
        }
    }
    found.sort_by_key(|(index, _)| *index);
    Ok(found.into_iter().map(|(_, player)| player).collect())
}

/// Reads one player. `PlaybackStatus` must be readable; a missing `Metadata`
/// counts as empty, a missing `Position` as 0 and a missing `Rate` as 1.0.
async fn read_player(connection: &Connection, name: &str) -> anyhow::Result<Player> {
    let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(connection)
        .destination(name)?
        .path(PLAYER_PATH)?
        .interface(PLAYER_INTERFACE)?
        // Position changes are not announced, so a cache would go stale.
        .cache_properties(CacheProperties::No)
        .build()
        .await?;

    let status: String = proxy
        .get_property("PlaybackStatus")
        .await
        .context("no PlaybackStatus")?;
    let metadata: HashMap<String, OwnedValue> =
        proxy.get_property("Metadata").await.unwrap_or_default();
    let rate = proxy
        .get_property::<OwnedValue>("Rate")
        .await
        .ok()
        .and_then(|value| number_f64(&value))
        .map(sanitize_rate)
        .unwrap_or(1.0);

    let before = Instant::now();
    let position = proxy.get_property::<OwnedValue>("Position").await;
    let after = Instant::now();
    let position_ms = position
        .ok()
        .and_then(|value| integer(&value))
        .map(micros_to_ms)
        .unwrap_or(0);

    let snapshot = PlaybackSnapshot {
        track: track_from_metadata(&metadata),
        status: status_from_str(&status),
        position_ms,
        // The middle of the Position call is the best guess for when it was true.
        position_at: before + after.saturating_duration_since(before) / 2,
        rate,
        app_id: name.to_string(),
    };
    Ok(Player {
        snapshot,
        art_url: art_url_from_metadata(&metadata),
    })
}

/// `"Playing"` / `"Paused"` (any case, surrounding spaces ignored); anything
/// else is `Stopped`.
fn status_from_str(status: &str) -> PlaybackStatus {
    let status = status.trim();
    if status.eq_ignore_ascii_case("playing") {
        PlaybackStatus::Playing
    } else if status.eq_ignore_ascii_case("paused") {
        PlaybackStatus::Paused
    } else {
        PlaybackStatus::Stopped
    }
}

/// Converts an MPRIS `Metadata` map into a [`Track`].
///
/// - `xesam:title`: a string (missing → empty).
/// - `xesam:artist`: a string array joined with `", "` (empty entries skipped)
///   or a single string; when it gives nothing, `xesam:albumArtist` is used.
/// - `xesam:album`: a string; empty → `None`.
/// - `mpris:length`: microseconds as any integer type (or a double); zero,
///   negative or missing → `None`.
/// - `spotify_id`: from `mpris:trackid` (object path or string), else from `xesam:url`.
///
/// Values wrapped in extra variants are unwrapped. Values of unexpected types
/// are ignored.
fn track_from_metadata(metadata: &HashMap<String, OwnedValue>) -> Track {
    let get = |key: &str| metadata.get(key).map(|value| plain(value));

    let title = get("xesam:title")
        .and_then(text)
        .unwrap_or_default()
        .to_string();
    let mut artist = get("xesam:artist").map(artists).unwrap_or_default();
    if artist.is_empty() {
        artist = get("xesam:albumArtist").map(artists).unwrap_or_default();
    }
    let album = get("xesam:album")
        .and_then(text)
        .filter(|album| !album.trim().is_empty())
        .map(str::to_string);
    let duration_ms = get("mpris:length")
        .and_then(integer)
        .filter(|micros| *micros > 0)
        .map(micros_to_ms)
        .filter(|ms| *ms > 0);
    let spotify_id = get("mpris:trackid")
        .and_then(text)
        .and_then(spotify_track_id)
        .or_else(|| get("xesam:url").and_then(text).and_then(spotify_track_id));

    Track {
        title,
        artist,
        album,
        duration_ms,
        spotify_id,
    }
}

/// `mpris:artUrl` without surrounding spaces; missing, blank or not a string → `None`.
fn art_url_from_metadata(metadata: &HashMap<String, OwnedValue>) -> Option<String> {
    let url = text(metadata.get("mpris:artUrl")?)?.trim();
    (!url.is_empty()).then(|| url.to_string())
}

/// The value inside any number of variant wrappers.
fn plain<'a, 'b>(mut value: &'a Value<'b>) -> &'a Value<'b> {
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

/// A string or object path.
fn text<'a>(value: &'a Value<'_>) -> Option<&'a str> {
    match plain(value) {
        Value::Str(text) => Some(text.as_str()),
        Value::ObjectPath(path) => Some(path.as_str()),
        _ => None,
    }
}

/// A list of artists (or one artist) as one string joined with `", "`.
fn artists(value: &Value<'_>) -> String {
    match plain(value) {
        Value::Array(list) => list
            .iter()
            .filter_map(text)
            .filter(|artist| !artist.trim().is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        other => text(other)
            .filter(|artist| !artist.trim().is_empty())
            .unwrap_or_default()
            .to_string(),
    }
}

/// Any integer type, or a finite double rounded to the nearest integer.
fn integer(value: &Value<'_>) -> Option<i128> {
    match plain(value) {
        Value::U8(n) => Some(i128::from(*n)),
        Value::I16(n) => Some(i128::from(*n)),
        Value::U16(n) => Some(i128::from(*n)),
        Value::I32(n) => Some(i128::from(*n)),
        Value::U32(n) => Some(i128::from(*n)),
        Value::I64(n) => Some(i128::from(*n)),
        Value::U64(n) => Some(i128::from(*n)),
        // `as` saturates for floats; the value is finite here.
        Value::F64(n) if n.is_finite() => Some(n.round() as i128),
        _ => None,
    }
}

/// A double, or any integer type.
fn number_f64(value: &Value<'_>) -> Option<f64> {
    match plain(value) {
        Value::F64(n) => Some(*n),
        // Precision loss above 2^53 does not matter for a playback rate.
        other => integer(other).map(|n| n as f64),
    }
}

/// Microseconds → milliseconds, never negative, saturating at `u64::MAX`.
fn micros_to_ms(micros: i128) -> u64 {
    u64::try_from((micros / 1000).max(0)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{Array, ObjectPath, Str};

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    fn ov(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).unwrap()
    }

    fn string(text: &str) -> OwnedValue {
        OwnedValue::from(Str::from(text.to_string()))
    }

    fn string_list(items: &[&str]) -> OwnedValue {
        ov(Value::from(
            items.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        ))
    }

    fn object_path(path: &str) -> OwnedValue {
        OwnedValue::from(ObjectPath::try_from(path.to_string()).unwrap())
    }

    fn map(entries: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    // ---- spotify_id_from ---------------------------------------------------

    #[test]
    fn spotify_id_from_trackid_path() {
        assert_eq!(
            spotify_id_from(&format!("/com/spotify/track/{ID}")),
            Some(ID.into())
        );
    }

    #[test]
    fn spotify_id_from_url() {
        assert_eq!(
            spotify_id_from(&format!("https://open.spotify.com/track/{ID}")),
            Some(ID.into())
        );
        assert_eq!(
            spotify_id_from(&format!(
                "https://open.spotify.com/track/{ID}?si=0123456789abcdef"
            )),
            Some(ID.into())
        );
    }

    #[test]
    fn spotify_id_from_uri() {
        assert_eq!(
            spotify_id_from(&format!("spotify:track:{ID}")),
            Some(ID.into())
        );
    }

    #[test]
    fn spotify_id_from_non_spotify() {
        for text in [
            "",
            "/org/mpris/MediaPlayer2/TrackList/NoTrack",
            "/org/videolan/vlc/playlist/3",
            "/com/spotify/ad/4uLU6hMCjMI75M1A2tKUQC",
            "/com/spotify/episode/4uLU6hMCjMI75M1A2tKUQC",
            "/com/spotify/track/tooShort",
            "file:///home/me/Music/song.flac",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://open.spotify.com/episode/4uLU6hMCjMI75M1A2tKUQC",
            "🎵/com/spotify/track/🎵",
        ] {
            assert_eq!(spotify_id_from(text), None, "{text}");
        }
    }

    // ---- status --------------------------------------------------------------

    #[test]
    fn status_strings() {
        assert_eq!(status_from_str("Playing"), PlaybackStatus::Playing);
        assert_eq!(status_from_str("Paused"), PlaybackStatus::Paused);
        assert_eq!(status_from_str("Stopped"), PlaybackStatus::Stopped);
        assert_eq!(status_from_str(" playing\n"), PlaybackStatus::Playing);
        assert_eq!(status_from_str("PAUSED"), PlaybackStatus::Paused);
        assert_eq!(status_from_str(""), PlaybackStatus::Stopped);
        assert_eq!(status_from_str("Buffering"), PlaybackStatus::Stopped);
        assert_eq!(status_from_str("Плей"), PlaybackStatus::Stopped);
    }

    #[test]
    fn player_names() {
        assert!(is_player_name("org.mpris.MediaPlayer2.spotify"));
        assert!(is_player_name(
            "org.mpris.MediaPlayer2.firefox.instance_1_42"
        ));
        assert!(!is_player_name("org.mpris.MediaPlayer2.playerctld"));
        assert!(!is_player_name("org.mpris.MediaPlayer2."));
        assert!(!is_player_name("org.mpris.MediaPlayer2"));
        assert!(!is_player_name("org.freedesktop.DBus"));
        assert!(!is_player_name(":1.42"));
        assert!(!is_player_name(""));
    }

    // ---- metadata --------------------------------------------------------------

    #[test]
    fn metadata_spotify_full() {
        let metadata = map(vec![
            (
                "mpris:trackid",
                object_path(&format!("/com/spotify/track/{ID}")),
            ),
            ("mpris:length", OwnedValue::from(215_672_000_u64)),
            ("mpris:artUrl", string("https://i.scdn.co/image/abc")),
            ("xesam:album", string("Album Name")),
            ("xesam:albumArtist", string_list(&["Album Artist"])),
            ("xesam:artist", string_list(&["Artist One", "Artist Two"])),
            ("xesam:autoRating", OwnedValue::from(0.5_f64)),
            ("xesam:discNumber", OwnedValue::from(1_i32)),
            ("xesam:title", string("Song Title")),
            ("xesam:trackNumber", OwnedValue::from(3_i32)),
            (
                "xesam:url",
                string(&format!("https://open.spotify.com/track/{ID}")),
            ),
        ]);
        assert_eq!(
            track_from_metadata(&metadata),
            Track {
                title: "Song Title".into(),
                artist: "Artist One, Artist Two".into(),
                album: Some("Album Name".into()),
                duration_ms: Some(215_672),
                spotify_id: Some(ID.into()),
            }
        );
        assert_eq!(
            art_url_from_metadata(&metadata).as_deref(),
            Some("https://i.scdn.co/image/abc")
        );
    }

    #[test]
    fn metadata_empty_map() {
        assert_eq!(track_from_metadata(&HashMap::new()), Track::default());
    }

    #[test]
    fn metadata_artist_as_single_string() {
        let metadata = map(vec![("xesam:artist", string("Solo Artist"))]);
        assert_eq!(track_from_metadata(&metadata).artist, "Solo Artist");
    }

    #[test]
    fn metadata_artist_list_skips_empty_entries() {
        let metadata = map(vec![("xesam:artist", string_list(&["", "A", "  ", "B"]))]);
        assert_eq!(track_from_metadata(&metadata).artist, "A, B");
        let metadata = map(vec![("xesam:artist", string_list(&[]))]);
        assert_eq!(track_from_metadata(&metadata).artist, "");
    }

    #[test]
    fn metadata_artist_falls_back_to_album_artist() {
        let metadata = map(vec![
            ("xesam:artist", string_list(&[""])),
            ("xesam:albumArtist", string_list(&["Band"])),
        ]);
        assert_eq!(track_from_metadata(&metadata).artist, "Band");
        let metadata = map(vec![("xesam:albumArtist", string("Band"))]);
        assert_eq!(track_from_metadata(&metadata).artist, "Band");
        // A real artist wins.
        let metadata = map(vec![
            ("xesam:artist", string("Singer")),
            ("xesam:albumArtist", string("Band")),
        ]);
        assert_eq!(track_from_metadata(&metadata).artist, "Singer");
    }

    #[test]
    fn metadata_length_integer_types() {
        let cases: Vec<(OwnedValue, Option<u64>)> = vec![
            (OwnedValue::from(215_000_000_i64), Some(215_000)),
            (OwnedValue::from(215_000_000_u64), Some(215_000)),
            (OwnedValue::from(215_000_000_i32), Some(215_000)),
            (OwnedValue::from(215_000_000_u32), Some(215_000)),
            (OwnedValue::from(215_000_000.4_f64), Some(215_000)),
            (OwnedValue::from(999_i64), None),
            (OwnedValue::from(0_i64), None),
            (OwnedValue::from(-5_000_000_i64), None),
            (OwnedValue::from(f64::NAN), None),
            (OwnedValue::from(f64::INFINITY), None),
            (OwnedValue::from(i64::MAX), Some(i64::MAX as u64 / 1000)),
            (OwnedValue::from(u64::MAX), Some(u64::MAX / 1000)),
            (OwnedValue::from(1e300_f64), Some(u64::MAX)),
            (string("215000000"), None),
            (OwnedValue::from(true), None),
        ];
        for (value, expected) in cases {
            let metadata = map(vec![("mpris:length", value)]);
            assert_eq!(track_from_metadata(&metadata).duration_ms, expected);
        }
    }

    #[test]
    fn metadata_trackid_as_string() {
        let metadata = map(vec![(
            "mpris:trackid",
            string(&format!("spotify:track:{ID}")),
        )]);
        assert_eq!(track_from_metadata(&metadata).spotify_id, Some(ID.into()));
        let metadata = map(vec![(
            "mpris:trackid",
            string(&format!("/com/spotify/track/{ID}")),
        )]);
        assert_eq!(track_from_metadata(&metadata).spotify_id, Some(ID.into()));
    }

    #[test]
    fn metadata_spotify_id_from_url_when_trackid_is_not_spotify() {
        let metadata = map(vec![
            (
                "mpris:trackid",
                object_path("/org/mpris/MediaPlayer2/Track/1"),
            ),
            (
                "xesam:url",
                string(&format!("https://open.spotify.com/track/{ID}")),
            ),
        ]);
        assert_eq!(track_from_metadata(&metadata).spotify_id, Some(ID.into()));
    }

    #[test]
    fn metadata_non_spotify_player() {
        let metadata = map(vec![
            ("mpris:trackid", object_path("/org/videolan/vlc/playlist/7")),
            (
                "xesam:url",
                string("file:///home/me/Music/Bj%C3%B6rk%20-%20Joga.flac"),
            ),
            ("xesam:title", string("Jóga")),
            ("xesam:artist", string_list(&["Björk"])),
            ("xesam:album", string("Homogenic")),
            ("mpris:length", OwnedValue::from(305_000_000_i64)),
        ]);
        assert_eq!(
            track_from_metadata(&metadata),
            Track {
                title: "Jóga".into(),
                artist: "Björk".into(),
                album: Some("Homogenic".into()),
                duration_ms: Some(305_000),
                spotify_id: None,
            }
        );
    }

    #[test]
    fn metadata_blank_album_is_none() {
        let metadata = map(vec![("xesam:album", string(""))]);
        assert_eq!(track_from_metadata(&metadata).album, None);
        let metadata = map(vec![("xesam:album", string("   "))]);
        assert_eq!(track_from_metadata(&metadata).album, None);
    }

    #[test]
    fn metadata_unicode_and_whitespace_kept_as_reported() {
        let metadata = map(vec![
            ("xesam:title", string("  夜に駆ける 🎵 ")),
            ("xesam:artist", string_list(&["YOASOBI", "Ayase"])),
        ]);
        let track = track_from_metadata(&metadata);
        assert_eq!(track.title, "  夜に駆ける 🎵 ");
        assert_eq!(track.artist, "YOASOBI, Ayase");
    }

    #[test]
    fn metadata_values_wrapped_in_variants() {
        let metadata = map(vec![
            ("xesam:title", ov(Value::new(Value::new("Wrapped")))),
            ("mpris:length", ov(Value::new(Value::I64(1_000_000)))),
            (
                "xesam:artist",
                ov(Value::new(Value::from(vec!["A".to_string()]))),
            ),
        ]);
        let track = track_from_metadata(&metadata);
        assert_eq!(track.title, "Wrapped");
        assert_eq!(track.duration_ms, Some(1_000));
        assert_eq!(track.artist, "A");
    }

    #[test]
    fn metadata_artist_array_of_variants() {
        let mut array = Array::new(&zbus::zvariant::Signature::Variant);
        array.append(Value::new(Value::from("X"))).unwrap();
        array.append(Value::new(Value::from(7_i32))).unwrap();
        array.append(Value::new(Value::from("Y"))).unwrap();
        let metadata = map(vec![("xesam:artist", ov(Value::Array(array)))]);
        assert_eq!(track_from_metadata(&metadata).artist, "X, Y");
    }

    #[test]
    fn metadata_wrong_types_are_ignored() {
        let metadata = map(vec![
            ("xesam:title", OwnedValue::from(42_i32)),
            ("xesam:artist", OwnedValue::from(true)),
            ("xesam:album", string_list(&["not", "a", "string"])),
            ("mpris:trackid", OwnedValue::from(1_u8)),
            ("xesam:url", OwnedValue::from(2.5_f64)),
            ("mpris:artUrl", OwnedValue::from(3_i32)),
        ]);
        assert_eq!(track_from_metadata(&metadata), Track::default());
        assert_eq!(art_url_from_metadata(&metadata), None);
    }

    // ---- cover art -------------------------------------------------------------

    #[test]
    fn art_url_from_metadata_values() {
        let art = |value: OwnedValue| art_url_from_metadata(&map(vec![("mpris:artUrl", value)]));
        assert_eq!(
            art(string("https://i.scdn.co/image/ab67616d0000b273")).as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273")
        );
        assert_eq!(
            art(string(" file:///tmp/.org.chromium.Chromium.abc \n")).as_deref(),
            Some("file:///tmp/.org.chromium.Chromium.abc")
        );
        assert_eq!(
            art(ov(Value::new(Value::new("file:///a.png")))).as_deref(),
            Some("file:///a.png")
        );
        assert_eq!(art(string("")), None);
        assert_eq!(art(string("   ")), None);
        assert_eq!(art(string_list(&["https://a/b.png"])), None);
        assert_eq!(art_url_from_metadata(&HashMap::new()), None);
    }

    fn player(app_id: &str, status: PlaybackStatus, art_url: Option<&str>) -> Player {
        Player {
            snapshot: PlaybackSnapshot {
                track: Track {
                    title: format!("{app_id} song"),
                    ..Track::default()
                },
                status,
                position_ms: 0,
                position_at: Instant::now(),
                rate: 1.0,
                app_id: app_id.to_string(),
            },
            art_url: art_url.map(str::to_string),
        }
    }

    #[test]
    fn pick_takes_the_art_url_of_the_picked_player() {
        let players = vec![
            player(
                "org.mpris.MediaPlayer2.aaa",
                PlaybackStatus::Paused,
                Some("https://a"),
            ),
            player(
                "org.mpris.MediaPlayer2.vlc",
                PlaybackStatus::Playing,
                Some("file:///v"),
            ),
            player("org.mpris.MediaPlayer2.zzz", PlaybackStatus::Playing, None),
        ];
        let (picked, art) = pick(players.clone(), &[], &[]);
        assert_eq!(picked.unwrap().app_id, "org.mpris.MediaPlayer2.vlc");
        assert_eq!(art.as_deref(), Some("file:///v"));

        // The preferred player has no art: none, not another player's.
        let (picked, art) = pick(players.clone(), &["zzz".to_string()], &[]);
        assert_eq!(picked.unwrap().app_id, "org.mpris.MediaPlayer2.zzz");
        assert_eq!(art, None);

        let blocked = ["vlc".to_string(), "zzz".to_string()];
        let (picked, art) = pick(players, &[], &blocked);
        assert_eq!(picked.unwrap().app_id, "org.mpris.MediaPlayer2.aaa");
        assert_eq!(art.as_deref(), Some("https://a"));

        let stopped = vec![player(
            "org.mpris.MediaPlayer2.vlc",
            PlaybackStatus::Stopped,
            Some("https://s"),
        )];
        assert_eq!(pick(stopped, &[], &[]), (None, None));
        assert_eq!(pick(Vec::new(), &[], &[]), (None, None));
    }

    #[tokio::test]
    async fn artwork_follows_the_last_pick() {
        let source = MprisSource::new(vec![], vec![]);
        assert_eq!(source.artwork().await.unwrap(), None, "no snapshot yet");
        *source.art_url.lock().unwrap() = Some("https://i.scdn.co/image/abc".into());
        assert_eq!(
            source.artwork().await.unwrap().as_deref(),
            Some("https://i.scdn.co/image/abc")
        );
        *source.art_url.lock().unwrap() = Some("file:///definitely/not/here.png".into());
        assert!(source.artwork().await.is_err());
        *source.art_url.lock().unwrap() = Some("spotify:image:abc".into());
        assert_eq!(source.artwork().await.unwrap(), None);
    }

    // ---- number helpers --------------------------------------------------------

    #[test]
    fn micros_to_ms_bounds() {
        assert_eq!(micros_to_ms(0), 0);
        assert_eq!(micros_to_ms(999), 0);
        assert_eq!(micros_to_ms(1_000), 1);
        assert_eq!(micros_to_ms(61_500_000), 61_500);
        assert_eq!(micros_to_ms(-1), 0);
        assert_eq!(micros_to_ms(i128::from(i64::MIN)), 0);
        assert_eq!(micros_to_ms(i128::MAX), u64::MAX);
        assert_eq!(micros_to_ms(i128::MIN), 0);
    }

    #[test]
    fn number_helpers() {
        assert_eq!(number_f64(&Value::F64(1.5)), Some(1.5));
        assert_eq!(number_f64(&Value::I32(2)), Some(2.0));
        assert_eq!(number_f64(&Value::new(Value::F64(0.75))), Some(0.75));
        assert_eq!(number_f64(&Value::from("1.0")), None);
        assert_eq!(integer(&Value::U8(7)), Some(7));
        assert_eq!(integer(&Value::I16(-7)), Some(-7));
        assert_eq!(integer(&Value::U16(7)), Some(7));
        assert_eq!(integer(&Value::F64(-2.6)), Some(-3));
        assert_eq!(integer(&Value::F64(f64::NEG_INFINITY)), None);
        assert_eq!(integer(&Value::Bool(true)), None);
    }

    #[test]
    fn text_and_plain() {
        assert_eq!(text(&Value::from("a")), Some("a"));
        let path = ObjectPath::try_from("/a/b").unwrap();
        assert_eq!(text(&Value::ObjectPath(path)), Some("/a/b"));
        assert_eq!(text(&Value::U32(1)), None);
        let nested = Value::new(Value::new(Value::new("deep")));
        assert_eq!(text(&nested), Some("deep"));
    }

    // ---- the source without a bus ------------------------------------------------

    #[tokio::test]
    async fn unreachable_bus_is_an_error_and_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let address = format!("unix:path={}", dir.path().join("no-bus").display());
        let source = MprisSource::with_address(&address, vec![], vec![]);
        assert_eq!(source.name(), "mpris");
        assert!(source.snapshot().await.is_err());
        assert!(source.connection.lock().await.is_none());
        // Still an error (and no panic) on the next poll.
        assert!(source.snapshot().await.is_err());
    }

    #[tokio::test]
    async fn invalid_bus_address_is_an_error() {
        let source = MprisSource::with_address("this is not an address", vec![], vec![]);
        assert!(source.snapshot().await.is_err());
    }

    #[test]
    fn new_does_not_connect() {
        let source = MprisSource::new(vec!["spotify".into()], vec!["chrome".into()]);
        assert!(source.address.is_none());
        assert_eq!(source.preferred, vec!["spotify".to_string()]);
        assert_eq!(source.blocked, vec!["chrome".to_string()]);
    }
}

/// Integration test against a private `dbus-daemon`. Ignored by default because
/// it needs the `dbus-daemon` binary. Run it with:
///
/// ```text
/// cd app && cargo test --lib sources::mpris -- --ignored
/// ```
#[cfg(test)]
mod bus_tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    /// A private bus that is stopped when dropped.
    struct PrivateBus {
        child: Child,
        address: String,
        _dir: tempfile::TempDir,
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn start_bus() -> PrivateBus {
        start_bus_in(tempfile::tempdir().unwrap())
    }

    /// A bus listening on `<dir>/bus`.
    fn start_bus_in(dir: tempfile::TempDir) -> PrivateBus {
        let socket = dir.path().join("bus");
        let mut child = Command::new("dbus-daemon")
            .arg("--session")
            .arg("--nofork")
            .arg("--print-address")
            .arg(format!("--address=unix:path={}", socket.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dbus-daemon must be installed for this test");
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        let address = line.trim().to_string();
        assert!(
            address.starts_with("unix:"),
            "unexpected address {address:?}"
        );
        PrivateBus {
            child,
            address,
            _dir: dir,
        }
    }

    struct FakePlayer {
        status: &'static str,
        title: &'static str,
        artists: Vec<String>,
        length_us: i64,
        trackid: &'static str,
        position_us: i64,
        started: Instant,
        hang: bool,
        art_url: Option<String>,
    }

    impl FakePlayer {
        fn new(status: &'static str, title: &'static str) -> Self {
            Self {
                status,
                title,
                artists: vec!["Fake Artist".into()],
                length_us: 180_000_000,
                trackid: "/org/mpris/MediaPlayer2/Track/1",
                position_us: 0,
                started: Instant::now(),
                hang: false,
                art_url: None,
            }
        }
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl FakePlayer {
        #[zbus(property)]
        async fn playback_status(&self) -> String {
            if self.hang {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            self.status.to_string()
        }

        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            let mut metadata = HashMap::new();
            metadata.insert(
                "xesam:title".to_string(),
                OwnedValue::from(zbus::zvariant::Str::from(self.title.to_string())),
            );
            metadata.insert(
                "xesam:artist".to_string(),
                OwnedValue::try_from(Value::from(self.artists.clone())).unwrap(),
            );
            metadata.insert("mpris:length".to_string(), OwnedValue::from(self.length_us));
            if let Some(url) = &self.art_url {
                metadata.insert(
                    "mpris:artUrl".to_string(),
                    OwnedValue::from(zbus::zvariant::Str::from(url.clone())),
                );
            }
            metadata.insert(
                "mpris:trackid".to_string(),
                OwnedValue::from(
                    zbus::zvariant::ObjectPath::try_from(self.trackid.to_string()).unwrap(),
                ),
            );
            metadata
        }

        /// Advances in real time, like a playing player (and never announced).
        #[zbus(property)]
        fn position(&self) -> i64 {
            let elapsed = i64::try_from(self.started.elapsed().as_micros()).unwrap_or(i64::MAX);
            self.position_us.saturating_add(elapsed)
        }

        #[zbus(property)]
        fn rate(&self) -> f64 {
            1.0
        }
    }

    async fn serve(address: &str, name: &str, player: FakePlayer) -> Connection {
        zbus::connection::Builder::address(address)
            .unwrap()
            .name(name.to_string())
            .unwrap()
            .serve_at(PLAYER_PATH, player)
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs dbus-daemon; run with --ignored"]
    async fn reads_players_from_a_private_bus() {
        let bus = start_bus();

        let mut spotify = FakePlayer::new("Playing", "Song Title");
        spotify.artists = vec!["Artist One".into(), "Artist Two".into()];
        spotify.length_us = 215_000_000;
        spotify.trackid = "/com/spotify/track/4uLU6hMCjMI75M1A2tKUQC";
        spotify.position_us = 61_500_000;
        spotify.art_url = Some("https://i.scdn.co/image/ab67616d0000b273".into());
        let _spotify = serve(&bus.address, "org.mpris.MediaPlayer2.spotify", spotify).await;

        // Sorts first, but paused: the playing player wins. Its cover is a
        // local file, like browsers and local players write.
        let art_dir = tempfile::tempdir().unwrap();
        let cover = art_dir.path().join("cover art.png");
        std::fs::write(&cover, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
        let mut paused = FakePlayer::new("Paused", "Paused Song");
        paused.art_url = Some(format!(
            "file://{}",
            cover.to_str().unwrap().replace(' ', "%20")
        ));
        let _paused = serve(&bus.address, "org.mpris.MediaPlayer2.aaa", paused).await;
        // playerctld mirrors another player and must be ignored, even when preferred.
        let _playerctld = serve(
            &bus.address,
            "org.mpris.MediaPlayer2.playerctld",
            FakePlayer::new("Playing", "Mirror"),
        )
        .await;
        // A player that never answers must not stall the poll.
        let mut hung = FakePlayer::new("Playing", "Hung");
        hung.hang = true;
        let _hung = serve(&bus.address, "org.mpris.MediaPlayer2.hung", hung).await;
        // Not a player at all.
        let _other = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.example.NotAPlayer")
            .unwrap()
            .build()
            .await
            .unwrap();

        let source = MprisSource::with_address(
            &bus.address,
            vec!["playerctld".into(), "hung".into()],
            vec![],
        );

        let started = Instant::now();
        let first = source.snapshot().await.unwrap().expect("a player");
        let took = started.elapsed();
        assert!(took < Duration::from_millis(2_000), "took {took:?}");
        assert_eq!(first.app_id, "org.mpris.MediaPlayer2.spotify");
        assert_eq!(first.status, PlaybackStatus::Playing);
        assert_eq!(
            first.track,
            Track {
                title: "Song Title".into(),
                artist: "Artist One, Artist Two".into(),
                album: None,
                duration_ms: Some(215_000),
                spotify_id: Some(ID.into()),
            }
        );
        assert!(first.position_ms >= 61_500, "{}", first.position_ms);
        assert!(first.position_ms < 61_500 + 5_000, "{}", first.position_ms);
        assert_eq!(first.rate, 1.0);
        // The picked player's cover, a web URL handed over as it is.
        assert_eq!(
            source.artwork().await.unwrap().as_deref(),
            Some("https://i.scdn.co/image/ab67616d0000b273")
        );

        // The position is read live every time (no stale cached value).
        tokio::time::sleep(Duration::from_millis(400)).await;
        let second = source.snapshot().await.unwrap().expect("a player");
        let advanced = second.position_ms.saturating_sub(first.position_ms);
        assert!(advanced >= 300, "position advanced only {advanced} ms");
        assert!(second.position_at > first.position_at);

        // Blocking the playing player falls back to the paused one.
        let blocked =
            MprisSource::with_address(&bus.address, vec![], vec!["SPOTIFY".into(), "hung".into()]);
        let picked = blocked.snapshot().await.unwrap().expect("a player");
        assert_eq!(picked.app_id, "org.mpris.MediaPlayer2.aaa");
        assert_eq!(picked.status, PlaybackStatus::Paused);
        assert_eq!(picked.track.title, "Paused Song");
        // Its local cover is read into a data: URL.
        assert_eq!(
            blocked.artwork().await.unwrap().as_deref(),
            Some("data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==")
        );

        // A stopped player alone means nothing is playing.
        let stopped_bus = start_bus();
        let _stopped = serve(
            &stopped_bus.address,
            "org.mpris.MediaPlayer2.vlc",
            FakePlayer::new("Stopped", ""),
        )
        .await;
        let source = MprisSource::with_address(&stopped_bus.address, vec![], vec![]);
        assert_eq!(source.snapshot().await.unwrap(), None);
        assert_eq!(source.artwork().await.unwrap(), None);

        // When the bus goes away the source reports an error and drops the connection,
        // so the next poll opens a new one.
        drop(stopped_bus);
        let mut saw_error = false;
        for _ in 0..20 {
            if source.snapshot().await.is_err() {
                saw_error = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(saw_error, "a dead bus must be reported");
        assert!(source.connection.lock().await.is_none());
    }

    /// The connection is opened lazily: a bus that is not there yet is an
    /// error, and the next poll connects once it is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "needs dbus-daemon; run with --ignored"]
    async fn connects_once_the_bus_appears() {
        let dir = tempfile::tempdir().unwrap();
        let address = format!("unix:path={}", dir.path().join("bus").display());
        let source = MprisSource::with_address(&address, vec![], vec![]);
        assert!(source.connection.lock().await.is_none(), "new() connects");
        assert!(source.snapshot().await.is_err());
        assert!(source.connection.lock().await.is_none());

        let bus = start_bus_in(dir);
        let player = serve(
            &bus.address,
            "org.mpris.MediaPlayer2.mpv",
            FakePlayer::new("Paused", "Later Song"),
        )
        .await;
        let snapshot = source.snapshot().await.unwrap().expect("a player");
        assert_eq!(snapshot.app_id, "org.mpris.MediaPlayer2.mpv");
        assert_eq!(snapshot.status, PlaybackStatus::Paused);
        assert_eq!(snapshot.track.title, "Later Song");
        assert!(source.connection.lock().await.is_some());

        // An empty bus (no players) is "nothing playing", not an error.
        drop(player);
        let mut empty = None;
        for _ in 0..20 {
            match source.snapshot().await {
                Ok(None) => {
                    empty = Some(());
                    break;
                }
                Ok(Some(_)) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(err) => panic!("an empty bus is not an error: {err:#}"),
            }
        }
        assert!(empty.is_some(), "the player never went away");
    }
}
