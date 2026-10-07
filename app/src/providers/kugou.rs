//! [Kugou](https://www.kugou.com): a large catalog, strongest for Chinese and
//! other Asian music, with timed lyrics for most songs.
//!
//! It has no official API. These are the lyrics endpoints its own player
//! uses, which answer without an account, key or cookie; they may change, and
//! a provider that fails is only skipped for a while.
//! - `GET {base}/search?ver=1&man=yes&client=pc&keyword=<artist - title>&duration=<ms>`
//!   → `{"status":200,"candidates":[{"id":"447566027",
//!   "accesskey":"81ED2B4F50C3DA827C11D1537E18390B","singer":"Coldplay",
//!   "song":"Yellow","duration":266000}]}` (duration in ms). The access key
//!   comes with each result; it is not an account or app key.
//! - `GET {base}/download?ver=1&client=pc&id=<id>&accesskey=<key>&fmt=lrc&charset=utf8`
//!   → `{"status":200,"content":"<LRC in base64>"}`.

use super::credits::strip_credits;
use super::{http, LyricsProvider};
use crate::lrc::parse_lrc;
use crate::matcher::{best_scored, is_other_version, primary_artist, score_candidate};
use crate::types::{Lyrics, Track};
use anyhow::{bail, Context as _};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use std::time::Duration;

/// Kugou's lyrics server.
pub const BASE_URL: &str = "https://lyrics.kugou.com";

/// Request timeout for each Kugou call.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum [`crate::matcher::score_candidate`] for a search result to be used.
/// Stricter than LRCLIB's, since Kugou lists many covers and other versions
/// of popular songs.
pub const MIN_SCORE: f64 = 0.7;

/// Search results asked for their lyrics at most, best first.
pub const MAX_CANDIDATES: usize = 3;

const SEARCH_PATH: &str = "/search";
const DOWNLOAD_PATH: &str = "/download";

/// One search result, as far as matching and downloading need it.
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    id: String,
    access_key: String,
    song: String,
    singer: String,
    duration_ms: Option<u64>,
}

/// See the module docs.
///
/// 1. Searches for `<primary artist> - <title>` (the title alone without an
///    artist), with the track's duration when it is known.
/// 2. Scores each result with [`crate::matcher::score_candidate`], keeps
///    those at [`MIN_SCORE`] or more that are not a version without the
///    singing ([`crate::matcher::is_other_version`]), and downloads the best
///    [`MAX_CANDIDATES`] in turn.
/// 3. The first synced or instrumental answer wins. Otherwise the first
///    unsynced one is returned, or `None`.
///
/// Lyrics go through [`super::credits::strip_credits`]. A blank title finds
/// nothing without asking. A search `status` other than 200, HTTP errors,
/// timeouts, bodies that are not the expected JSON and content that is not
/// base64 are errors. A download whose `status` is not 200 or that has no
/// content counts as no lyrics for that result. Results without an id or
/// access key are skipped.
pub struct KugouProvider {
    client: reqwest::Client,
    base_url: String,
}

impl KugouProvider {
    pub fn new() -> anyhow::Result<Self> {
        Self::build(BASE_URL, TIMEOUT, true)
    }

    /// A provider against a local server, with its own timeout and without
    /// proxy settings.
    #[cfg(test)]
    pub(crate) fn for_tests(base_url: &str, timeout: Duration) -> anyhow::Result<Self> {
        Self::build(base_url, timeout, false)
    }

    fn build(base_url: &str, timeout: Duration, use_system_proxy: bool) -> anyhow::Result<Self> {
        let client = http::client(timeout, use_system_proxy)
            .context("could not set up the HTTP client for Kugou")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    async fn search(
        &self,
        keyword: &str,
        duration_ms: Option<u64>,
    ) -> anyhow::Result<Vec<Candidate>> {
        let what = format!("Kugou {SEARCH_PATH}");
        let url = format!("{}{SEARCH_PATH}", self.base_url);
        let duration = duration_ms.map(|ms| ms.to_string());
        let mut params = vec![
            ("ver", "1"),
            ("man", "yes"),
            ("client", "pc"),
            ("keyword", keyword),
        ];
        if let Some(duration) = &duration {
            params.push(("duration", duration.as_str()));
        }
        let response: Value = http::get_json(&self.client, &url, &params, &what).await?;
        match response.get("status").and_then(Value::as_i64) {
            Some(200) => {}
            Some(status) => bail!("{what} answered status {status}"),
            None => bail!("{what} sent an unexpected response"),
        }
        let candidates = response
            .get("candidates")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        Ok(candidates.iter().filter_map(candidate_from_json).collect())
    }

    /// The lyrics Kugou has for one result, tidied, or `None`.
    async fn lyrics(&self, candidate: &Candidate) -> anyhow::Result<Option<Lyrics>> {
        let what = format!("Kugou {DOWNLOAD_PATH}");
        let url = format!("{}{DOWNLOAD_PATH}", self.base_url);
        let params = [
            ("ver", "1"),
            ("client", "pc"),
            ("id", candidate.id.as_str()),
            ("accesskey", candidate.access_key.as_str()),
            ("fmt", "lrc"),
            ("charset", "utf8"),
        ];
        let response: Value = http::get_json(&self.client, &url, &params, &what).await?;
        let status = response.get("status").and_then(Value::as_i64);
        let content = response
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if status != Some(200) || content.is_empty() {
            tracing::debug!(id = %candidate.id, ?status, "Kugou has no lyrics for this result");
            return Ok(None);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .with_context(|| format!("{what} sent lyrics that are not base64"))?;
        let text = String::from_utf8_lossy(&bytes);
        let mut lyrics = strip_credits(parse_lrc(&text), &candidate.song, &candidate.singer);
        if !lyrics.instrumental && !lyrics.has_text() {
            tracing::debug!(id = %candidate.id, "Kugou's lyrics for this result are empty");
            return Ok(None);
        }
        lyrics.source = "kugou".into();
        Ok(Some(lyrics))
    }
}

#[async_trait]
impl LyricsProvider for KugouProvider {
    fn name(&self) -> &'static str {
        "kugou"
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let title = track.title.trim();
        if title.is_empty() {
            return Ok(None);
        }
        let artist = primary_artist(&track.artist);
        let keyword = if artist.is_empty() {
            title.to_string()
        } else {
            format!("{artist} - {title}")
        };
        let duration_ms = track.duration_ms.filter(|&ms| ms > 0);
        let candidates = self.search(&keyword, duration_ms).await?;

        let mut fallback: Option<Lyrics> = None;
        for candidate in pick(track, candidates) {
            match self.lyrics(&candidate).await? {
                Some(lyrics) if lyrics.synced || lyrics.instrumental => return Ok(Some(lyrics)),
                Some(lyrics) => {
                    fallback.get_or_insert(lyrics);
                }
                None => {}
            }
        }
        Ok(fallback)
    }
}

/// The search results worth downloading, best first.
fn pick(track: &Track, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let wanted = [track.title.as_str()];
    let scored = candidates
        .into_iter()
        .filter(|candidate| !is_other_version(&wanted, &[candidate.song.as_str()]))
        .map(|candidate| {
            let score = score_candidate(
                track,
                &candidate.song,
                &candidate.singer,
                candidate.duration_ms,
            );
            (score, candidate)
        })
        .collect();
    best_scored(scored, MIN_SCORE, MAX_CANDIDATES)
}

/// A search result, or `None` without an id or access key.
fn candidate_from_json(value: &Value) -> Option<Candidate> {
    let id = match value.get("id")? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let access_key = value
        .get("accesskey")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if id.is_empty() || access_key.is_empty() {
        return None;
    }
    let text = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    Some(Candidate {
        id,
        access_key,
        song: text("song"),
        singer: text("singer"),
        duration_ms: value
            .get("duration")
            .and_then(Value::as_u64)
            .filter(|&ms| ms > 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::test_server::{ok, MockServer, Reply};
    use serde_json::json;

    fn yellow() -> Track {
        Track {
            title: "Yellow".into(),
            artist: "Coldplay".into(),
            album: Some("Parachutes".into()),
            duration_ms: Some(266_773),
            spotify_id: None,
        }
    }

    fn candidate(id: &str, song: &str, singer: &str, duration: u64) -> Value {
        json!({
            "id": id,
            "accesskey": format!("KEY{id}"),
            "singer": singer,
            "song": song,
            "duration": duration,
            "uid": "1000000010",
            "nickname": "",
            "language": "",
            "krctype": 2,
            "hitlayer": 7,
            "score": 60,
        })
    }

    fn search(candidates: Vec<Value>) -> Reply {
        ok(json!({
            "status": 200,
            "info": "OK",
            "errcode": 200,
            "errmsg": "",
            "keyword": "Coldplay - Yellow",
            "proposal": "447566027",
            "candidates": candidates,
        }))
    }

    fn download(lrc: &str) -> Reply {
        let content = base64::engine::general_purpose::STANDARD.encode(lrc);
        ok(json!({
            "status": 200,
            "info": "OK",
            "error_code": 0,
            "fmt": "lrc",
            "contenttype": 1,
            "_source": "bss",
            "charset": "utf8",
            "content": content,
        }))
    }

    /// The start of Kugou's lyrics for Coldplay's Yellow.
    const YELLOW_LRC: &str = "\u{feff}[ti:Yellow]\r\n[ar:Coldplay]\r\n[al:The Singles 1999-2006]\r\n[by:]\r\n[offset:0]\r\n\
        [00:00.00]Yellow - Coldplay (中文版本)\r\n\
        [00:08.39]Lyrics by:Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\r\n\
        [00:16.79]Composed by:Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\r\n\
        [00:25.18]Produced by:Ken Nelson/Coldplay\r\n\
        [00:33.58]Look at the stars\r\n\
        [00:37.43]Look how they shine for you\r\n";

    fn provider(server: &MockServer) -> KugouProvider {
        KugouProvider::for_tests(&server.base, TIMEOUT).unwrap()
    }

    fn texts_of(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[tokio::test]
    async fn finds_synced_lyrics_without_the_header_and_credits() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![candidate("447566027", "Yellow", "Coldplay", 266_000)]),
            DOWNLOAD_PATH => download(YELLOW_LRC),
            _ => Reply::Json(404, "{}".into()),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "kugou");
        assert_eq!(
            texts_of(&lyrics),
            vec!["Look at the stars", "Look how they shine for you"]
        );

        let seen = server.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].param("keyword"), Some("Coldplay - Yellow"));
        assert_eq!(seen[0].param("duration"), Some("266773"));
        assert_eq!(seen[0].param("client"), Some("pc"));
        assert_eq!(seen[1].param("id"), Some("447566027"));
        assert_eq!(seen[1].param("accesskey"), Some("KEY447566027"));
        assert_eq!(seen[1].param("fmt"), Some("lrc"));
        assert_eq!(seen[1].param("charset"), Some("utf8"));
        for request in &seen {
            assert_eq!(request.header("user-agent"), Some(crate::USER_AGENT));
            assert_eq!(request.header("cookie"), None);
        }
    }

    #[tokio::test]
    async fn the_best_match_is_downloaded_first_and_poor_ones_never() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                candidate("1", "Yellow Submarine", "The Beatles", 158_000),
                candidate("2", "Yellow (伴奏)", "Coldplay", 266_000),
                candidate("3", "Yellow", "Coldplay", 266_000),
                candidate("4", "Yellow", "Coldplay", 300_000),
            ]),
            _ => {
                let id = seen.param("id").unwrap_or_default();
                download(&format!("[00:10.00]from {id}\n"))
            }
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert_eq!(texts_of(&lyrics), vec!["from 3"]);
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn results_without_lyrics_move_on_to_the_next() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                candidate("1", "Yellow", "Coldplay", 266_000),
                candidate("2", "Yellow", "Coldplay", 266_500),
                candidate("3", "Yellow", "Coldplay", 267_000),
            ]),
            _ => match seen.param("id") {
                Some("1") => ok(json!({ "status": 404, "info": "not found", "error_code": 1 })),
                Some("2") => ok(json!({ "status": 200, "content": "" })),
                _ => download("[00:33.58]Look at the stars\n"),
            },
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert_eq!(texts_of(&lyrics), vec!["Look at the stars"]);
        assert_eq!(server.seen().len(), 4);
    }

    #[tokio::test]
    async fn a_pure_music_notice_is_instrumental() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![candidate("1", "Yellow", "Coldplay", 266_000)]),
            _ => download("[00:00.00]Yellow - Coldplay\r\n[00:01.00]纯音乐，请欣赏\r\n"),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.instrumental);
        assert_eq!(lyrics.source, "kugou");
    }

    #[tokio::test]
    async fn plain_lyrics_are_kept_until_synced_ones_turn_up() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                candidate("1", "Yellow", "Coldplay", 266_000),
                candidate("2", "Yellow", "Coldplay", 266_500),
            ]),
            _ => match seen.param("id") {
                Some("1") => download("Look at the stars\nLook how they shine for you"),
                _ => download("[00:33.58]Look at the stars\n"),
            },
        })
        .await;
        assert!(
            provider(&server)
                .fetch(&yellow())
                .await
                .unwrap()
                .unwrap()
                .synced
        );
    }

    #[tokio::test]
    async fn nothing_found_is_none() {
        for body in [
            json!({ "status": 200, "info": "OK", "candidates": [] }),
            json!({ "status": 200, "info": "OK" }),
            json!({ "status": 200, "candidates": [{ "id": "1", "song": "Yellow" }, { "accesskey": "K" }, 5] }),
        ] {
            let server = MockServer::start(move |_| ok(body.clone())).await;
            assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
            assert_eq!(server.seen().len(), 1);
        }
    }

    #[tokio::test]
    async fn failures_are_errors() {
        let replies: [fn() -> Reply; 4] = [
            || ok(json!({ "status": 0, "info": "busy" })),
            || ok(json!({ "candidates": [] })),
            || Reply::Json(502, "bad gateway".into()),
            || Reply::Json(200, "<html></html>".into()),
        ];
        for reply in replies {
            let server = MockServer::start(move |_| reply()).await;
            assert!(provider(&server).fetch(&yellow()).await.is_err());
        }

        // Lyrics that are not base64.
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![candidate("1", "Yellow", "Coldplay", 266_000)]),
            _ => ok(json!({ "status": 200, "content": "not base64 at all!" })),
        })
        .await;
        let err = provider(&server).fetch(&yellow()).await.unwrap_err();
        assert!(format!("{err:#}").contains("base64"), "{err:#}");
    }

    #[tokio::test]
    async fn timeouts_are_errors() {
        let server = MockServer::start(|_| Reply::Hang).await;
        let provider = KugouProvider::for_tests(&server.base, Duration::from_millis(300)).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), provider.fetch(&yellow()))
            .await
            .expect("the provider's own timeout should fire first");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn the_keyword_and_duration_follow_the_track() {
        let server = MockServer::start(|_| search(Vec::new())).await;
        let blank = Track {
            title: "\t".into(),
            ..yellow()
        };
        assert_eq!(provider(&server).fetch(&blank).await.unwrap(), None);
        assert!(server.seen().is_empty());

        let bare = Track {
            artist: " ".into(),
            duration_ms: Some(0),
            ..yellow()
        };
        provider(&server).fetch(&bare).await.unwrap();
        let duet = Track {
            artist: "Coldplay feat. Rihanna".into(),
            duration_ms: None,
            ..yellow()
        };
        provider(&server).fetch(&duet).await.unwrap();
        let seen = server.seen();
        assert_eq!(seen[0].param("keyword"), Some("Yellow"));
        assert_eq!(seen[0].param("duration"), None);
        assert_eq!(seen[1].param("keyword"), Some("Coldplay - Yellow"));
        assert_eq!(seen[1].param("duration"), None);
    }

    #[test]
    fn search_results_are_read_leniently() {
        let read = candidate_from_json(&json!({
            "id": 447_566_027,
            "accesskey": " K ",
            "song": " Yellow ",
            "duration": 0,
        }))
        .unwrap();
        assert_eq!(read.id, "447566027");
        assert_eq!(read.access_key, "K");
        assert_eq!(read.song, "Yellow");
        assert_eq!(read.singer, "");
        assert_eq!(read.duration_ms, None);
        assert_eq!(
            candidate_from_json(&json!({ "id": "", "accesskey": "K" })),
            None
        );
        assert_eq!(
            candidate_from_json(&json!({ "id": [1], "accesskey": "K" })),
            None
        );
        assert_eq!(candidate_from_json(&json!(null)), None);
    }
}
