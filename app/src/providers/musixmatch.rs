//! [Musixmatch](https://www.musixmatch.com) through its official API, with
//! your own key from [developer.musixmatch.com](https://developer.musixmatch.com).
//! Used only when a key is set (`lyrics.musixmatch_key`); the key stays in
//! your settings file and never appears in errors or logs.
//!
//! Timed lyrics (`track.subtitle.get`) need one of Musixmatch's paid plans.
//! The free plan only returns about 30% of each song's words, without
//! timings. Lyrix does not use those: spread over the whole song, a third of
//! the words would show at the wrong times.
//!
//! API used, with the key as the `apikey` query parameter:
//! - `GET {base}/matcher.track.get?q_track=&q_artist=` → the track Musixmatch
//!   matches: `{"message":{"header":{"status_code":200},"body":{"track":
//!   {"track_id":123,"track_name":"Yellow","artist_name":"Coldplay",
//!   "track_length":269,"instrumental":0,"has_lyrics":1,"has_subtitles":1}}}}`
//!   (length in seconds).
//! - `GET {base}/track.subtitle.get?track_id=&subtitle_format=lrc` →
//!   `{"message":{…,"body":{"subtitle":{"subtitle_body":"[00:33.79] Look at
//!   the stars\n…","pixel_tracking_url":"https://tracking.musixmatch.com/…"}}}}`.
//! - `GET {base}/track.lyrics.get?track_id=` → `{"message":{…,"body":
//!   {"lyrics":{"lyrics_body":"Look at the stars\n…","pixel_tracking_url":
//!   "…"}}}}`. On the free plan the words stop early and end with
//!   `******* This Lyrics is NOT for Commercial use *******`.
//!
//! Musixmatch answers HTTP 200 and puts its own status in
//! `message.header.status_code`: 401 for a key it does not accept, 402 when
//! the key's limit is reached, 403 for something the key's plan does not
//! include, 404 when it has nothing.
//!
//! Musixmatch counts how often its lyrics are shown through the tracking link
//! that comes with them, so Lyrix opens that link each time it gets lyrics
//! from Musixmatch. They are not kept in Lyrix's cache
//! ([`LyricsProvider::may_cache`]): Musixmatch is asked again each time a song
//! plays.

use super::{http, LyricsProvider};
use crate::lrc::{from_plain, parse_lrc};
use crate::matcher::{is_other_version, primary_artist, score_candidate};
use crate::types::{Lyrics, Track};
use anyhow::{bail, Context as _};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Musixmatch's API.
pub const BASE_URL: &str = "https://api.musixmatch.com/ws/1.1";

/// Request timeout for each Musixmatch call.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum [`crate::matcher::score_candidate`] for the matched track to be used.
pub const MIN_SCORE: f64 = 0.7;

/// What the free plan puts after the part of the lyrics it returns.
const PARTIAL_LYRICS_MARKERS: [&str; 2] = [
    "this lyrics is not for commercial use",
    "only 30% of the lyrics",
];

/// See the module docs.
///
/// 1. `matcher.track.get` with the title and the primary artist. The match
///    is checked with [`crate::matcher::score_candidate`] (at least
///    [`MIN_SCORE`]) and must not be a version without the singing
///    ([`crate::matcher::is_other_version`]).
/// 2. An instrumental track is instrumental.
/// 3. With subtitles: `track.subtitle.get` for timed lyrics.
/// 4. Otherwise, or when the key's plan has no subtitles: `track.lyrics.get`
///    for untimed lyrics, unless they are the free plan's partial ones.
///
/// Errors: a key Musixmatch does not accept (401 on the match), a reached
/// limit (402), any other status but 200, 403 and 404, HTTP errors,
/// timeouts and bodies that are not the expected JSON. A 401 or 403 on the
/// subtitles or lyrics means the key's plan does not include them. A blank
/// title finds nothing without asking.
pub struct MusixmatchProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    /// Set once it was logged that the key's plan has no timed lyrics.
    told_about_plan: AtomicBool,
    /// Tests keep the tracking links here instead of opening them.
    #[cfg(test)]
    opened: std::sync::Mutex<Vec<String>>,
}

/// The track Musixmatch matched, as far as Lyrix needs it.
#[derive(Debug, Clone, PartialEq)]
struct Matched {
    id: String,
    name: String,
    artist: String,
    duration_ms: Option<u64>,
    instrumental: bool,
    has_lyrics: bool,
    has_subtitles: bool,
}

/// One API answer.
enum Answer {
    /// Status 200, with the message body.
    Body(Value),
    /// Status 404.
    NotFound,
    /// A status the caller counts as "not on this key's plan".
    NotOnPlan,
}

impl MusixmatchProvider {
    /// A provider for `api_key`, which must not be blank.
    pub fn new(api_key: &str) -> anyhow::Result<Self> {
        Self::build(BASE_URL, api_key, TIMEOUT, true)
    }

    /// A provider against a local server, with its own timeout and without
    /// proxy settings.
    #[cfg(test)]
    pub(crate) fn for_tests(
        base_url: &str,
        api_key: &str,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Self::build(base_url, api_key, timeout, false)
    }

    fn build(
        base_url: &str,
        api_key: &str,
        timeout: Duration,
        use_system_proxy: bool,
    ) -> anyhow::Result<Self> {
        let api_key = api_key.trim();
        if api_key.is_empty() {
            bail!("no Musixmatch API key is set");
        }
        let client = http::client(timeout, use_system_proxy)
            .context("could not set up the HTTP client for Musixmatch")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            told_about_plan: AtomicBool::new(false),
            #[cfg(test)]
            opened: Default::default(),
        })
    }

    /// Calls one API method. `not_on_plan` lists the statuses that mean the
    /// key's plan does not include it.
    async fn call(
        &self,
        method: &str,
        params: &[(&str, &str)],
        not_on_plan: &[i64],
    ) -> anyhow::Result<Answer> {
        let what = format!("Musixmatch {method}");
        let url = format!("{}/{method}", self.base_url);
        let mut query = params.to_vec();
        query.push(("apikey", self.api_key.as_str()));
        let response: Value = http::get_json(&self.client, &url, &query, &what).await?;
        let status = response
            .pointer("/message/header/status_code")
            .and_then(Value::as_i64);
        match status {
            Some(200) => Ok(Answer::Body(
                response
                    .pointer("/message/body")
                    .cloned()
                    .unwrap_or_default(),
            )),
            Some(404) => Ok(Answer::NotFound),
            Some(code) if not_on_plan.contains(&code) => Ok(Answer::NotOnPlan),
            Some(401) => bail!(
                "Musixmatch did not accept the API key; it needs the key from your \
                 developer.musixmatch.com account"
            ),
            Some(402) => bail!("Musixmatch says the API key has reached its usage limit"),
            Some(code) => bail!("{what} answered status {code}"),
            None => bail!("{what} sent an unexpected response"),
        }
    }

    /// Timed lyrics for a track, or `None`.
    async fn subtitles(&self, matched: &Matched) -> anyhow::Result<Option<Lyrics>> {
        let params = [
            ("track_id", matched.id.as_str()),
            ("subtitle_format", "lrc"),
        ];
        let body = match self
            .call("track.subtitle.get", &params, &[401, 403])
            .await?
        {
            Answer::Body(body) => body,
            Answer::NotFound => return Ok(None),
            Answer::NotOnPlan => {
                if !self.told_about_plan.swap(true, Ordering::Relaxed) {
                    tracing::info!(
                        "this Musixmatch key's plan has no timed lyrics; they need a paid plan"
                    );
                }
                return Ok(None);
            }
        };
        let Some(subtitle) = body.get("subtitle") else {
            return Ok(None);
        };
        let text = subtitle
            .get("subtitle_body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let lyrics = parse_lrc(text);
        if !lyrics.synced || !lyrics.has_text() {
            return Ok(None);
        }
        self.count_view(subtitle);
        Ok(Some(lyrics))
    }

    /// Untimed lyrics for a track, or `None` (also for the free plan's
    /// partial ones).
    async fn plain_lyrics(&self, matched: &Matched) -> anyhow::Result<Option<Lyrics>> {
        let params = [("track_id", matched.id.as_str())];
        let body = match self.call("track.lyrics.get", &params, &[401, 403]).await? {
            Answer::Body(body) => body,
            Answer::NotFound | Answer::NotOnPlan => return Ok(None),
        };
        let Some(found) = body.get("lyrics") else {
            return Ok(None);
        };
        let text = found
            .get("lyrics_body")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let copyright = found
            .get("lyrics_copyright")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if is_partial(text) || is_partial(copyright) {
            tracing::debug!("Musixmatch sent the free plan's partial lyrics, which are not used");
            return Ok(None);
        }
        let lyrics = from_plain(text);
        if !lyrics.has_text() {
            return Ok(None);
        }
        self.count_view(found);
        Ok(Some(lyrics))
    }

    /// Opens the tracking link that came with lyrics (`pixel_tracking_url`),
    /// in the background, when it is an `https` link to Musixmatch.
    fn count_view(&self, found: &Value) {
        let Some(url) = found
            .get("pixel_tracking_url")
            .and_then(Value::as_str)
            .and_then(tracking_url)
        else {
            return;
        };
        #[cfg(test)]
        self.opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
        #[cfg(not(test))]
        {
            let client = self.client.clone();
            tokio::spawn(async move {
                if let Err(err) = client.get(url).send().await {
                    tracing::debug!(
                        "could not open Musixmatch's tracking link: {}",
                        err.without_url()
                    );
                }
            });
        }
    }

    /// The tracking links opened so far.
    #[cfg(test)]
    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

#[async_trait]
impl LyricsProvider for MusixmatchProvider {
    fn name(&self) -> &'static str {
        "musixmatch"
    }

    /// Musixmatch's lyrics are licensed and counted each time they are shown.
    fn may_cache(&self) -> bool {
        false
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let title = track.title.trim();
        if title.is_empty() {
            return Ok(None);
        }
        let artist = primary_artist(&track.artist);
        let mut params = vec![("q_track", title)];
        if !artist.is_empty() {
            params.push(("q_artist", artist.as_str()));
        }
        let body = match self.call("matcher.track.get", &params, &[]).await? {
            Answer::Body(body) => body,
            Answer::NotFound | Answer::NotOnPlan => return Ok(None),
        };
        let Some(matched) = body.get("track").and_then(matched_from_json) else {
            return Ok(None);
        };
        let score = score_candidate(track, &matched.name, &matched.artist, matched.duration_ms);
        if score < MIN_SCORE || is_other_version(&[track.title.as_str()], &[matched.name.as_str()])
        {
            tracing::debug!(score, "Musixmatch matched a different song");
            return Ok(None);
        }

        let found = if matched.instrumental {
            Some(Lyrics {
                lines: Vec::new(),
                synced: false,
                instrumental: true,
                source: String::new(),
            })
        } else {
            let timed = if matched.has_subtitles {
                self.subtitles(&matched).await?
            } else {
                None
            };
            match timed {
                Some(lyrics) => Some(lyrics),
                None if matched.has_lyrics => self.plain_lyrics(&matched).await?,
                None => None,
            }
        };
        Ok(found.map(|mut lyrics| {
            lyrics.source = "musixmatch".into();
            lyrics
        }))
    }
}

/// The matched track, or `None` without an id.
fn matched_from_json(value: &Value) -> Option<Matched> {
    let id = match value.get("track_id")? {
        Value::Number(n) => n.to_string(),
        Value::String(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return None,
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    // Musixmatch writes flags as 0 or 1.
    let flag = |key: &str| match value.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_u64().is_some_and(|n| n > 0),
        Some(Value::String(s)) => matches!(s.trim(), "1" | "true"),
        _ => false,
    };
    Some(Matched {
        id,
        name: text("track_name"),
        artist: text("artist_name"),
        duration_ms: value
            .get("track_length")
            .and_then(Value::as_u64)
            .filter(|&secs| secs > 0)
            .map(|secs| secs.saturating_mul(1_000)),
        instrumental: flag("instrumental"),
        has_lyrics: flag("has_lyrics"),
        has_subtitles: flag("has_subtitles"),
    })
}

/// The free plan's partial lyrics, or its notice about them.
fn is_partial(text: &str) -> bool {
    let lower = text.to_lowercase();
    PARTIAL_LYRICS_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// `raw` when it is an `https` link to musixmatch.com or one of its
/// subdomains.
fn tracking_url(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let musixmatch = host == "musixmatch.com" || host.ends_with(".musixmatch.com");
    (url.scheme() == "https" && musixmatch).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_server::{ok, MockServer, Reply, Seen};
    use serde_json::json;

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const TRACKING_URL: &str = "https://tracking.musixmatch.com/t1.0/AbCdEf";

    fn yellow() -> Track {
        Track {
            title: "Yellow".into(),
            artist: "Coldplay".into(),
            album: Some("Parachutes".into()),
            duration_ms: Some(269_000),
            spotify_id: None,
        }
    }

    fn answer(status: i64, body: Value) -> Reply {
        ok(json!({
            "message": {
                "header": { "status_code": status, "execute_time": 0.01 },
                "body": body,
            }
        }))
    }

    fn track_json(name: &str, artist: &str, seconds: u64, subtitles: u8) -> Value {
        json!({
            "track_id": 123,
            "track_name": name,
            "artist_name": artist,
            "album_name": "Parachutes",
            "track_length": seconds,
            "instrumental": 0,
            "has_lyrics": 1,
            "has_subtitles": subtitles,
            "has_richsync": 0,
            "commontrack_id": 456,
            "restricted": 0,
        })
    }

    fn matched(track: Value) -> Reply {
        answer(200, json!({ "track": track }))
    }

    fn subtitle(lrc: &str) -> Reply {
        answer(
            200,
            json!({ "subtitle": {
                "subtitle_id": 1,
                "subtitle_body": lrc,
                "subtitle_language": "en",
                "subtitle_length": 269,
                "pixel_tracking_url": TRACKING_URL,
                "lyrics_copyright": "Lyrics powered by www.musixmatch.com",
            }}),
        )
    }

    fn lyrics(text: &str, copyright: &str) -> Reply {
        answer(
            200,
            json!({ "lyrics": {
                "lyrics_id": 1,
                "lyrics_body": text,
                "lyrics_language": "en",
                "pixel_tracking_url": TRACKING_URL,
                "lyrics_copyright": copyright,
            }}),
        )
    }

    fn provider(server: &MockServer) -> MusixmatchProvider {
        MusixmatchProvider::for_tests(&server.base, KEY, TIMEOUT).unwrap()
    }

    fn texts_of(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    fn route(seen: &Seen) -> &str {
        seen.path.rsplit('/').next().unwrap_or_default()
    }

    #[tokio::test]
    async fn finds_timed_lyrics_with_the_key() {
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 1)),
            "track.subtitle.get" => subtitle(
                "[00:33.79] Look at the stars\n[00:37.66] Look how they shine for you\n[00:41.00] ",
            ),
            _ => answer(404, json!([])),
        })
        .await;
        let musixmatch = provider(&server);
        let found = musixmatch.fetch(&yellow()).await.unwrap().unwrap();
        assert!(found.synced);
        assert_eq!(found.source, "musixmatch");
        assert_eq!(
            texts_of(&found),
            vec!["Look at the stars", "Look how they shine for you", ""]
        );
        assert_eq!(musixmatch.opened(), vec![TRACKING_URL]);
        assert!(!musixmatch.may_cache());

        let seen = server.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].path, "/matcher.track.get");
        assert_eq!(seen[0].param("q_track"), Some("Yellow"));
        assert_eq!(seen[0].param("q_artist"), Some("Coldplay"));
        assert_eq!(seen[1].path, "/track.subtitle.get");
        assert_eq!(seen[1].param("track_id"), Some("123"));
        assert_eq!(seen[1].param("subtitle_format"), Some("lrc"));
        for request in &seen {
            assert_eq!(request.param("apikey"), Some(KEY));
            assert_eq!(request.header("user-agent"), Some(crate::USER_AGENT));
        }
    }

    #[tokio::test]
    async fn without_subtitles_on_the_plan_full_untimed_lyrics_are_used() {
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 1)),
            "track.subtitle.get" => answer(403, json!([])),
            "track.lyrics.get" => lyrics(
                "Look at the stars\nLook how they shine for you",
                "Lyrics powered by www.musixmatch.com",
            ),
            _ => answer(404, json!([])),
        })
        .await;
        let musixmatch = provider(&server);
        let found = musixmatch.fetch(&yellow()).await.unwrap().unwrap();
        assert!(!found.synced);
        assert_eq!(
            texts_of(&found),
            vec!["Look at the stars", "Look how they shine for you"]
        );
        assert_eq!(server.seen().len(), 3);
        assert_eq!(musixmatch.opened(), vec![TRACKING_URL]);
    }

    #[tokio::test]
    async fn the_free_plans_partial_lyrics_are_not_used() {
        let partial = "Look at the stars\nLook how they shine for you\n...\n\n\
            ******* This Lyrics is NOT for Commercial use *******\n(1409617829851)";
        for (subtitle_status, body, copyright) in [
            (401, partial, "Lyrics powered by www.musixmatch.com"),
            (
                403,
                "Look at the stars",
                "Lyrics powered by www.musixmatch.com. This Lyrics is NOT for Commercial use and only 30% of the lyrics are returned.",
            ),
        ] {
            let server = MockServer::start(move |seen| match route(seen) {
                "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 1)),
                "track.subtitle.get" => answer(subtitle_status, json!([])),
                _ => lyrics(body, copyright),
            })
            .await;
            let musixmatch = provider(&server);
            assert_eq!(musixmatch.fetch(&yellow()).await.unwrap(), None);
            assert_eq!(server.seen().len(), 3);
            assert!(musixmatch.opened().is_empty());
        }
    }

    #[tokio::test]
    async fn tracks_without_subtitles_skip_straight_to_the_lyrics() {
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 0)),
            "track.lyrics.get" => {
                lyrics("Look at the stars", "Lyrics powered by www.musixmatch.com")
            }
            _ => answer(500, json!([])),
        })
        .await;
        let found = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert_eq!(texts_of(&found), vec!["Look at the stars"]);
        assert_eq!(
            server.paths(),
            vec!["/matcher.track.get", "/track.lyrics.get"]
        );
    }

    #[tokio::test]
    async fn instrumental_tracks_are_instrumental() {
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => {
                let mut track = track_json("Yellow", "Coldplay", 269, 0);
                track["instrumental"] = json!(1);
                track["has_lyrics"] = json!(0);
                matched(track)
            }
            _ => answer(500, json!([])),
        })
        .await;
        let found = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(found.instrumental);
        assert!(found.lines.is_empty());
        assert_eq!(found.source, "musixmatch");
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn a_different_song_or_version_is_not_used() {
        for (name, artist, seconds) in [
            ("Yellow Submarine", "The Beatles", 158),
            ("Yellow", "Someone Else", 200),
            ("Yellow (Instrumental)", "Coldplay", 269),
        ] {
            let server = MockServer::start(move |seen| match route(seen) {
                "matcher.track.get" => matched(track_json(name, artist, seconds, 1)),
                _ => subtitle("[00:10.00] should not be asked\n"),
            })
            .await;
            assert_eq!(
                provider(&server).fetch(&yellow()).await.unwrap(),
                None,
                "{name}"
            );
            assert_eq!(server.seen().len(), 1, "{name}");
        }
    }

    #[tokio::test]
    async fn nothing_found_is_none() {
        let server = MockServer::start(|_| answer(404, json!([]))).await;
        assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
        assert_eq!(server.seen().len(), 1);

        // Subtitles and lyrics that turn out to be missing or empty.
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 1)),
            "track.subtitle.get" => subtitle(""),
            _ => lyrics("", ""),
        })
        .await;
        assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
        assert_eq!(server.seen().len(), 3);

        // A match without an id.
        let server =
            MockServer::start(|_| answer(200, json!({ "track": { "track_name": "Yellow" } })))
                .await;
        assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_refused_key_or_reached_limit_is_an_error_that_names_no_key() {
        for (status, words) in [(401, "did not accept the API key"), (402, "usage limit")] {
            let server = MockServer::start(move |_| answer(status, json!([]))).await;
            let err = provider(&server).fetch(&yellow()).await.unwrap_err();
            let message = format!("{err:#}");
            assert!(message.contains(words), "{message}");
            assert!(!message.contains(KEY), "{message}");
        }

        // The limit can also be reached on the lyrics.
        let server = MockServer::start(|seen| match route(seen) {
            "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 1)),
            _ => answer(402, json!([])),
        })
        .await;
        assert!(provider(&server).fetch(&yellow()).await.is_err());
    }

    #[tokio::test]
    async fn other_failures_are_errors_that_name_no_key() {
        let replies: [fn() -> Reply; 4] = [
            || answer(500, json!([])),
            || ok(json!({ "message": "nope" })),
            || Reply::Json(503, "busy".into()),
            || Reply::Json(200, "<html></html>".into()),
        ];
        for reply in replies {
            let server = MockServer::start(move |_| reply()).await;
            let err = provider(&server).fetch(&yellow()).await.unwrap_err();
            assert!(!format!("{err:#}").contains(KEY), "{err:#}");
        }

        // Timeouts and refused connections carry the URL, which is left out.
        let server = MockServer::start(|_| Reply::Hang).await;
        let slow =
            MusixmatchProvider::for_tests(&server.base, KEY, Duration::from_millis(300)).unwrap();
        let err = tokio::time::timeout(Duration::from_secs(10), slow.fetch(&yellow()))
            .await
            .expect("the provider's own timeout should fire first")
            .unwrap_err();
        assert!(!format!("{err:#}").contains(KEY), "{err:#}");

        let port = {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let closed = MusixmatchProvider::for_tests(
            &format!("http://127.0.0.1:{port}"),
            KEY,
            Duration::from_secs(2),
        )
        .unwrap();
        let err = closed.fetch(&yellow()).await.unwrap_err();
        assert!(!format!("{err:#}").contains(KEY), "{err:#}");
    }

    #[tokio::test]
    async fn a_blank_title_asks_nothing_and_no_artist_sends_the_title_only() {
        let server = MockServer::start(|_| answer(404, json!([]))).await;
        let blank = Track {
            title: " ".into(),
            ..yellow()
        };
        assert_eq!(provider(&server).fetch(&blank).await.unwrap(), None);
        assert!(server.seen().is_empty());

        let no_artist = Track {
            artist: String::new(),
            ..yellow()
        };
        provider(&server).fetch(&no_artist).await.unwrap();
        let duet = Track {
            artist: "Coldplay & Rihanna".into(),
            ..yellow()
        };
        provider(&server).fetch(&duet).await.unwrap();
        let seen = server.seen();
        assert_eq!(seen[0].param("q_artist"), None);
        assert_eq!(seen[1].param("q_artist"), Some("Coldplay"));
    }

    #[test]
    fn a_blank_key_is_refused() {
        for key in ["", "   "] {
            assert!(MusixmatchProvider::new(key).is_err());
        }
        assert!(MusixmatchProvider::new(" key ").is_ok());
    }

    #[tokio::test]
    async fn other_tracking_links_are_not_opened() {
        for link in [
            json!("http://tracking.musixmatch.com/x"),
            json!("https://example.org/x"),
            json!(null),
        ] {
            let server = MockServer::start(move |seen| match route(seen) {
                "matcher.track.get" => matched(track_json("Yellow", "Coldplay", 269, 0)),
                _ => answer(
                    200,
                    json!({ "lyrics": { "lyrics_body": "Look at the stars", "pixel_tracking_url": link } }),
                ),
            })
            .await;
            let musixmatch = provider(&server);
            assert!(musixmatch.fetch(&yellow()).await.unwrap().is_some());
            assert!(musixmatch.opened().is_empty());
        }
    }

    #[test]
    fn only_https_links_to_musixmatch_are_opened() {
        for url in [
            "https://tracking.musixmatch.com/t1.0/AbCdEf",
            "https://musixmatch.com/x",
            "https://TRACKING.MUSIXMATCH.COM/x",
        ] {
            assert!(tracking_url(url).is_some(), "{url}");
        }
        for url in [
            "http://tracking.musixmatch.com/t1.0/AbCdEf",
            "https://musixmatch.com.example.org/x",
            "https://evilmusixmatch.com/x",
            "https://example.org/?musixmatch.com",
            "file:///etc/passwd",
            "not a url",
            "",
        ] {
            assert!(tracking_url(url).is_none(), "{url}");
        }
    }

    #[test]
    fn matched_tracks_are_read_leniently() {
        let read = matched_from_json(&json!({
            "track_id": "77",
            "track_name": "Yellow",
            "artist_name": "Coldplay",
            "track_length": 0,
            "instrumental": true,
            "has_lyrics": 0,
            "has_subtitles": "1",
        }))
        .unwrap();
        assert_eq!(read.id, "77");
        assert_eq!(read.duration_ms, None);
        assert!(read.instrumental);
        assert!(!read.has_lyrics);
        assert!(read.has_subtitles);
        let unset = matched_from_json(&json!({ "track_id": 1, "has_lyrics": "no" })).unwrap();
        assert!(!unset.has_lyrics && !unset.has_subtitles && !unset.instrumental);
        assert_eq!(unset.name, "");
        assert_eq!(matched_from_json(&json!({ "track_id": null })), None);
        assert_eq!(matched_from_json(&json!({ "track_id": " " })), None);
        assert_eq!(matched_from_json(&json!([])), None);
    }
}
