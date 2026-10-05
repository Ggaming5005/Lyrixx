//! Linux: MPRIS players on the D-Bus session bus.
//!
//! Every bus name starting with `org.mpris.MediaPlayer2.` is a player. From the
//! object `/org/mpris/MediaPlayer2`, interface `org.mpris.MediaPlayer2.Player`:
//! - `PlaybackStatus` (`"Playing"`, `"Paused"`, `"Stopped"`)
//! - `Metadata` (a{sv}): `xesam:title` (s), `xesam:artist` (as), `xesam:album` (s),
//!   `mpris:length` (x or t, microseconds), `mpris:trackid` (o or s),
//!   `xesam:url` (s)
//! - `Position` (x, microseconds; read live, may be missing or fail for some players)
//! - `Rate` (d, default 1.0)
//!
//! Spotify exposes its track id as `mpris:trackid = /com/spotify/track/<id>` and
//! `xesam:url = https://open.spotify.com/track/<id>`; either fills `spotify_id`.

use super::NowPlayingSource;
use crate::types::PlaybackSnapshot;
use async_trait::async_trait;

/// See the module docs. Holds one session-bus connection, opened lazily on the
/// first [`snapshot`](NowPlayingSource::snapshot) and reopened after an error.
pub struct MprisSource {
    preferred: Vec<String>,
    blocked: Vec<String>,
}

impl MprisSource {
    pub fn new(preferred: Vec<String>, blocked: Vec<String>) -> Self {
        Self { preferred, blocked }
    }
}

/// Extracts a Spotify track id from an MPRIS track id or URL, if it is one.
pub fn spotify_id_from(trackid_or_url: &str) -> Option<String> {
    let _ = trackid_or_url;
    todo!()
}

#[async_trait]
impl NowPlayingSource for MprisSource {
    fn name(&self) -> &'static str {
        "mpris"
    }

    async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
        let _ = (&self.preferred, &self.blocked);
        todo!()
    }
}
