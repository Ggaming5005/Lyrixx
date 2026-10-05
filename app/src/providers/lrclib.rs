//! [LRCLIB](https://lrclib.net): a free, open lyrics database with timed lines.
//!
//! API used (no account or key):
//! - `GET {base}/api/get?artist_name=&track_name=&album_name=&duration=` → one
//!   record or 404. `duration` is in whole seconds; LRCLIB matches within ±2 s.
//! - `GET {base}/api/search?track_name=&artist_name=` → array of records.
//!
//! Record fields: `id`, `trackName`, `artistName`, `albumName`, `duration`
//! (seconds, may be fractional), `instrumental` (bool), `plainLyrics` (string or
//! null), `syncedLyrics` (LRC string or null).

use super::LyricsProvider;
use crate::lrc::{from_plain, parse_lrc};
use crate::matcher::{primary_artist, score_candidate};
use crate::types::{Lyrics, Track};
use anyhow::{bail, Context as _};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::time::Duration;

/// Request timeout for each LRCLIB call.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum [`crate::matcher::score_candidate`] for a search result to be used.
pub const MIN_SEARCH_SCORE: f64 = 0.6;

/// Largest response body read; real LRCLIB answers are far smaller, and a
/// server that sends more is not LRCLIB.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

const GET_PATH: &str = "/api/get";
const SEARCH_PATH: &str = "/api/search";

/// One LRCLIB record.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LrclibRecord {
    pub id: Option<i64>,
    pub track_name: Option<String>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub duration: Option<f64>,
    #[serde(default)]
    pub instrumental: bool,
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
}

/// See the module docs.
///
/// Lookup order:
/// 1. When album and duration are both known: `/api/get` with all four fields.
/// 2. When either is missing, or step 1 returned 404: `/api/search` with
///    `track_name` + `artist_name` (the primary artist), and if that returns
///    nothing usable, `/api/search` with `track_name` only.
/// 3. Search results are scored with [`crate::matcher::score_candidate`]; results
///    below [`MIN_SEARCH_SCORE`] are dropped; among the rest a record with synced
///    lyrics beats one without, then the higher score wins.
///
/// A record becomes [`Lyrics`] via [`record_to_lyrics`]. HTTP 404 is "not found";
/// other non-success statuses, timeouts and bad JSON are errors. Every request
/// sends [`crate::USER_AGENT`].
///
/// Details: a blank title finds nothing without asking. Step 1 also needs an
/// artist, and a duration of 0 counts as unknown; the duration is rounded to the
/// nearest second. A record from `/api/get` is used without scoring (LRCLIB
/// already matched it); one without usable lyrics falls through to the search.
/// When there is no artist, only the title-only search runs. A 404 from
/// `/api/search` counts as an empty result, and search records that do not
/// have the expected shape are skipped (a body that is not a JSON array is an
/// error). Ties keep LRCLIB's order. A response body larger than 16 MiB is an
/// error.
pub struct LrclibProvider {
    client: reqwest::Client,
    base_url: String,
}

impl LrclibProvider {
    /// `base_url` without a trailing slash, e.g. `https://lrclib.net`.
    ///
    /// Trailing slashes are removed. The URL must be an absolute `http` or
    /// `https` URL with a host and without a query or fragment; anything else
    /// is an error.
    pub fn new(base_url: impl Into<String>) -> anyhow::Result<Self> {
        Self::build(&base_url.into(), TIMEOUT, true)
    }

    /// The validated base URL requests are made against.
    #[cfg(test)]
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// A provider with its own timeout that ignores proxy settings, for tests
    /// against a local server.
    #[cfg(test)]
    pub(crate) fn for_tests(base_url: &str, timeout: Duration) -> anyhow::Result<Self> {
        Self::build(base_url, timeout, false)
    }

    fn build(base_url: &str, timeout: Duration, use_system_proxy: bool) -> anyhow::Result<Self> {
        let base_url = normalize_base_url(base_url)?;
        let mut builder = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .timeout(timeout);
        if !use_system_proxy {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .context("could not set up the HTTP client for LRCLIB")?;
        Ok(Self { client, base_url })
    }

    /// `/api/get`: `Ok(None)` on 404.
    async fn get_record(
        &self,
        artist: &str,
        title: &str,
        album: &str,
        duration_secs: u64,
    ) -> anyhow::Result<Option<LrclibRecord>> {
        let duration = duration_secs.to_string();
        let params = [
            ("artist_name", artist),
            ("track_name", title),
            ("album_name", album),
            ("duration", duration.as_str()),
        ];
        self.request_json(GET_PATH, &params).await
    }

    /// `/api/search`: the records that parse; empty on 404.
    async fn search(&self, params: &[(&str, &str)]) -> anyhow::Result<Vec<LrclibRecord>> {
        let Some(values) = self
            .request_json::<Vec<serde_json::Value>>(SEARCH_PATH, params)
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(values
            .into_iter()
            .filter_map(
                |value| match serde_json::from_value::<LrclibRecord>(value) {
                    Ok(record) => Some(record),
                    Err(err) => {
                        tracing::debug!("skipping malformed LRCLIB search result: {err}");
                        None
                    }
                },
            )
            .collect())
    }

    /// GETs `path` with `params` and decodes the JSON body. `Ok(None)` on 404.
    async fn request_json<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> anyhow::Result<Option<T>> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .client
            .get(&url)
            .query(params)
            .send()
            .await
            .with_context(|| format!("LRCLIB request to {path} failed"))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            bail!("LRCLIB {path} answered HTTP {status}");
        }
        let body = read_body(response, path).await?;
        let value = serde_json::from_slice(&body)
            .with_context(|| format!("LRCLIB {path} sent an unexpected response"))?;
        Ok(Some(value))
    }
}

/// Converts a record: instrumental → `Lyrics { instrumental: true, lines: [] }`;
/// synced lyrics → parsed LRC (`synced = true`); else plain lyrics →
/// [`crate::lrc::from_plain`]; nothing usable → `None`. `source` = `"lrclib"`.
///
/// Lyrics without any text (empty, whitespace or tags only) count as missing.
/// `syncedLyrics` without a single timestamp is not synced: plain lyrics are
/// used instead, or, when there are none, its text as unsynced lyrics.
pub fn record_to_lyrics(record: &LrclibRecord) -> Option<Lyrics> {
    let mut lyrics = if record.instrumental {
        Lyrics {
            lines: Vec::new(),
            synced: false,
            instrumental: true,
            source: String::new(),
        }
    } else {
        let from_synced = record
            .synced_lyrics
            .as_deref()
            .map(parse_lrc)
            .filter(Lyrics::has_text);
        let from_plain_text = record
            .plain_lyrics
            .as_deref()
            .map(from_plain)
            .filter(Lyrics::has_text);
        match (from_synced, from_plain_text) {
            (Some(synced), _) if synced.synced => synced,
            (_, Some(plain)) => plain,
            (Some(unsynced), None) => unsynced,
            (None, None) => return None,
        }
    };
    lyrics.source = "lrclib".into();
    Some(lyrics)
}

#[async_trait]
impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &'static str {
        "lrclib"
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let title = track.title.trim();
        if title.is_empty() {
            return Ok(None);
        }
        let artist = track.artist.trim();
        let album = track
            .album
            .as_deref()
            .map(str::trim)
            .filter(|album| !album.is_empty());
        let duration_secs = track
            .duration_ms
            .filter(|&ms| ms > 0)
            .map(|ms| ms.saturating_add(500) / 1_000);

        let exact = match (album, duration_secs) {
            (Some(album), Some(duration_secs)) if !artist.is_empty() => {
                Some((album, duration_secs))
            }
            _ => None,
        };
        if let Some((album, duration_secs)) = exact {
            match self.get_record(artist, title, album, duration_secs).await? {
                Some(record) => match record_to_lyrics(&record) {
                    Some(lyrics) => return Ok(Some(lyrics)),
                    None => tracing::debug!(
                        id = ?record.id,
                        "LRCLIB record has no usable lyrics, searching instead"
                    ),
                },
                None => tracing::debug!("LRCLIB has no exact match, searching instead"),
            }
        }

        let primary = primary_artist(artist);
        if !primary.is_empty() {
            let params = [("track_name", title), ("artist_name", primary.as_str())];
            let records = self.search(&params).await?;
            if let Some(lyrics) = best_match(track, &records) {
                return Ok(Some(lyrics));
            }
        }

        let records = self.search(&[("track_name", title)]).await?;
        Ok(best_match(track, &records))
    }
}

/// Reads a response body of at most [`MAX_BODY_BYTES`].
async fn read_body(mut response: reqwest::Response, path: &str) -> anyhow::Result<Vec<u8>> {
    let too_big = || anyhow::anyhow!("LRCLIB {path} sent more than {MAX_BODY_BYTES} bytes");
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY_BYTES as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("could not read the LRCLIB {path} response"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The base URL with trailing slashes removed, after checking it.
fn normalize_base_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url =
        reqwest::Url::parse(trimmed).with_context(|| format!("invalid LRCLIB URL {raw:?}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("invalid LRCLIB URL {raw:?}: only http and https are supported");
    }
    if url.host_str().map_or(true, str::is_empty) {
        bail!("invalid LRCLIB URL {raw:?}: no host");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("invalid LRCLIB URL {raw:?}: must not have a query or fragment");
    }
    Ok(trimmed.to_string())
}

/// How well a record matches the wanted track.
fn score_record(track: &Track, record: &LrclibRecord) -> f64 {
    let duration_ms = record
        .duration
        .filter(|secs| secs.is_finite() && *secs > 0.0)
        // `as` saturates, so absurd durations cannot wrap.
        .map(|secs| (secs * 1_000.0).round() as u64);
    score_candidate(
        track,
        record.track_name.as_deref().unwrap_or(""),
        record.artist_name.as_deref().unwrap_or(""),
        duration_ms,
    )
}

/// The best usable search result: score at least [`MIN_SEARCH_SCORE`], synced
/// before unsynced, then the highest score; ties keep the earlier record.
fn best_match(track: &Track, records: &[LrclibRecord]) -> Option<Lyrics> {
    let mut best: Option<(f64, Lyrics)> = None;
    for record in records {
        let score = score_record(track, record);
        if score < MIN_SEARCH_SCORE {
            continue;
        }
        let Some(lyrics) = record_to_lyrics(record) else {
            continue;
        };
        let better = match &best {
            None => true,
            Some((best_score, best_lyrics)) => {
                (lyrics.synced && !best_lyrics.synced)
                    || (lyrics.synced == best_lyrics.synced && score > *best_score)
            }
        };
        if better {
            best = Some((score, lyrics));
        }
    }
    best.map(|(_, lyrics)| lyrics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    // ----- mock LRCLIB server ----------------------------------------------

    /// One request the mock server received.
    #[derive(Debug, Clone)]
    struct Seen {
        path: String,
        /// Decoded query pairs, in order.
        query: Vec<(String, String)>,
        /// The raw request target (path and encoded query).
        target: String,
        /// Header names lowercased.
        headers: Vec<(String, String)>,
    }

    impl Seen {
        fn param(&self, name: &str) -> Option<&str> {
            self.query
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        }
    }

    enum Reply {
        Json(u16, String),
        /// Bytes written as they are, then the connection is closed.
        Raw(Vec<u8>),
        /// Read the request, then never answer.
        Hang,
    }

    fn ok(body: Value) -> Reply {
        Reply::Json(200, body.to_string())
    }

    fn not_found() -> Reply {
        Reply::Json(
            404,
            json!({"code": 404, "name": "TrackNotFound", "message": "Failed to find specified track"})
                .to_string(),
        )
    }

    type Handler = dyn Fn(&Seen) -> Reply + Send + Sync;

    struct MockServer {
        base: String,
        seen: Arc<Mutex<Vec<Seen>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockServer {
        async fn start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let handler: Arc<Handler> = Arc::new(handler);
            let task = tokio::spawn({
                let seen = Arc::clone(&seen);
                async move {
                    while let Ok((stream, _)) = listener.accept().await {
                        tokio::spawn(serve(stream, Arc::clone(&seen), Arc::clone(&handler)));
                    }
                }
            });
            Self {
                base: format!("http://{addr}"),
                seen,
                task,
            }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }

        fn paths(&self) -> Vec<String> {
            self.seen().into_iter().map(|s| s.path).collect()
        }

        fn provider(&self) -> LrclibProvider {
            LrclibProvider::for_tests(&self.base, TIMEOUT).unwrap()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve(mut stream: TcpStream, seen: Arc<Mutex<Vec<Seen>>>, handler: Arc<Handler>) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let head = String::from_utf8_lossy(&buf).into_owned();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let target = request_line
            .split(' ')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let headers = lines
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let absolute = if target.starts_with("http") {
            target.clone()
        } else {
            format!("http://mock{target}")
        };
        let url = reqwest::Url::parse(&absolute).unwrap();
        let request = Seen {
            path: url.path().to_string(),
            query: url
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            target,
            headers,
        };
        seen.lock().unwrap().push(request.clone());
        match handler(&request) {
            Reply::Json(status, body) => {
                let reason = match status {
                    200 => "OK",
                    404 => "Not Found",
                    500 => "Internal Server Error",
                    503 => "Service Unavailable",
                    _ => "Whatever",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
            Reply::Raw(bytes) => {
                let _ = stream.write_all(&bytes).await;
                let _ = stream.shutdown().await;
            }
            Reply::Hang => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        }
    }

    // ----- fixtures --------------------------------------------------------

    const SYNCED: &str = "[00:18.50] We're no strangers to love\n[00:22.80] You know the rules and so do I\n[00:27.00] ";
    const PLAIN: &str = "We're no strangers to love\nYou know the rules and so do I";

    fn rick() -> Track {
        Track {
            title: "Never Gonna Give You Up".into(),
            artist: "Rick Astley".into(),
            album: Some("Whenever You Need Somebody".into()),
            duration_ms: Some(212_600),
            spotify_id: None,
        }
    }

    fn without_album(mut track: Track) -> Track {
        track.album = None;
        track
    }

    fn record(
        artist: &str,
        title: &str,
        duration: f64,
        synced: Option<&str>,
        plain: Option<&str>,
    ) -> Value {
        json!({
            "id": 12345,
            "name": title,
            "trackName": title,
            "artistName": artist,
            "albumName": "Whenever You Need Somebody",
            "duration": duration,
            "instrumental": false,
            "plainLyrics": plain,
            "syncedLyrics": synced,
        })
    }

    fn rick_record() -> Value {
        record(
            "Rick Astley",
            "Never Gonna Give You Up",
            213.0,
            Some(SYNCED),
            Some(PLAIN),
        )
    }

    /// Synced lyrics whose single line is `marker`, to tell records apart.
    fn marked(marker: &str) -> String {
        format!("[00:10.00]{marker}\n")
    }

    fn first_text(lyrics: &Lyrics) -> &str {
        lyrics.lines.first().map(|l| l.text.as_str()).unwrap_or("")
    }

    // ----- construction ----------------------------------------------------

    #[test]
    fn new_accepts_http_and_https_and_strips_trailing_slash() {
        let cases = [
            ("https://lrclib.net", "https://lrclib.net"),
            ("https://lrclib.net/", "https://lrclib.net"),
            ("https://lrclib.net///", "https://lrclib.net"),
            ("  http://127.0.0.1:8080/ ", "http://127.0.0.1:8080"),
            (
                "https://example.com/mirror/lrclib/",
                "https://example.com/mirror/lrclib",
            ),
            ("HTTPS://LRCLIB.NET", "HTTPS://LRCLIB.NET"),
        ];
        for (input, expected) in cases {
            let provider = LrclibProvider::new(input).unwrap();
            assert_eq!(provider.base_url(), expected, "{input}");
        }
    }

    #[test]
    fn new_rejects_invalid_urls() {
        for input in [
            "",
            "   ",
            "/",
            "lrclib.net",
            "not a url",
            "ftp://lrclib.net",
            "file:///tmp/lrclib",
            "mailto:someone@example.com",
            "https://",
            "http:///",
            "https://lrclib.net?x=1",
            "https://lrclib.net/#top",
            "https://exa mple.com",
        ] {
            assert!(
                LrclibProvider::new(input).is_err(),
                "{input:?} should be rejected"
            );
        }
    }

    #[test]
    fn name_is_lrclib() {
        assert_eq!(
            LrclibProvider::new("https://lrclib.net").unwrap().name(),
            "lrclib"
        );
    }

    // ----- record_to_lyrics ------------------------------------------------

    fn rec(value: Value) -> LrclibRecord {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn record_with_synced_lyrics() {
        let lyrics = record_to_lyrics(&rec(rick_record())).unwrap();
        assert!(lyrics.synced);
        assert!(!lyrics.instrumental);
        assert_eq!(lyrics.source, "lrclib");
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].start_ms, 18_500);
        assert_eq!(lyrics.lines[0].text, "We're no strangers to love");
        assert_eq!(lyrics.lines[2].text, "");
    }

    #[test]
    fn record_with_plain_lyrics_only() {
        let lyrics = record_to_lyrics(&rec(record("A", "B", 200.0, None, Some(PLAIN)))).unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.source, "lrclib");
        assert_eq!(lyrics.lines.len(), 2);
        assert!(lyrics.lines.iter().all(|l| l.start_ms == 0));
    }

    #[test]
    fn instrumental_record() {
        let value = json!({
            "id": 1, "trackName": "Your Hand in Mine", "artistName": "Explosions in the Sky",
            "albumName": "The Earth Is Not a Cold Dead Place", "duration": 497.0,
            "instrumental": true, "plainLyrics": null, "syncedLyrics": null
        });
        let lyrics = record_to_lyrics(&rec(value)).unwrap();
        assert_eq!(
            lyrics,
            Lyrics {
                lines: vec![],
                synced: false,
                instrumental: true,
                source: "lrclib".into(),
            }
        );
    }

    #[test]
    fn instrumental_flag_wins_over_lyrics() {
        let mut value = rick_record();
        value["instrumental"] = json!(true);
        let lyrics = record_to_lyrics(&rec(value)).unwrap();
        assert!(lyrics.instrumental);
        assert!(lyrics.lines.is_empty());
    }

    #[test]
    fn empty_or_whitespace_synced_lyrics_count_as_missing() {
        for synced in [
            "",
            "   ",
            "\n\r\n\t",
            "[ar:Rick Astley]\n[ti:Song]",
            "[00:01.00]\n[00:02.00] ",
        ] {
            let lyrics =
                record_to_lyrics(&rec(record("A", "B", 200.0, Some(synced), Some(PLAIN)))).unwrap();
            assert!(!lyrics.synced, "{synced:?}");
            assert_eq!(lyrics.lines.len(), 2, "{synced:?}");
            assert!(record_to_lyrics(&rec(record("A", "B", 200.0, Some(synced), None))).is_none());
        }
    }

    #[test]
    fn synced_field_without_timestamps() {
        // Plain lyrics are preferred over untimed text in the synced field.
        let lyrics = record_to_lyrics(&rec(record(
            "A",
            "B",
            200.0,
            Some("only words"),
            Some(PLAIN),
        )))
        .unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines[0].text, "We're no strangers to love");
        // Without plain lyrics, the untimed text is still better than nothing.
        let lyrics =
            record_to_lyrics(&rec(record("A", "B", 200.0, Some("only words"), None))).unwrap();
        assert!(!lyrics.synced);
        assert_eq!(lyrics.lines[0].text, "only words");
        assert_eq!(lyrics.source, "lrclib");
    }

    #[test]
    fn record_without_lyrics_is_none() {
        assert!(record_to_lyrics(&rec(record("A", "B", 200.0, None, None))).is_none());
        assert!(record_to_lyrics(&rec(record("A", "B", 200.0, None, Some("  \n ")))).is_none());
        assert!(record_to_lyrics(&rec(json!({}))).is_none());
    }

    #[test]
    fn record_deserializes_real_lrclib_json() {
        let text = r#"{"id":3396226,"name":"I Want to Live","trackName":"I Want to Live","artistName":"Borislav Slavov","albumName":"Baldur's Gate 3 (Original Game Soundtrack)","duration":233.0,"instrumental":false,"plainLyrics":"I feel your breath upon my neck\nA soft caress as cold as death","syncedLyrics":"[00:17.12] I feel your breath upon my neck\n[00:20.45] A soft caress as cold as death"}"#;
        let record: LrclibRecord = serde_json::from_str(text).unwrap();
        assert_eq!(record.id, Some(3_396_226));
        assert_eq!(record.track_name.as_deref(), Some("I Want to Live"));
        assert_eq!(record.artist_name.as_deref(), Some("Borislav Slavov"));
        assert_eq!(record.duration, Some(233.0));
        assert!(!record.instrumental);
        let lyrics = record_to_lyrics(&record).unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines[1].start_ms, 20_450);

        // Missing `instrumental` defaults to false; nulls are fine.
        let record: LrclibRecord =
            serde_json::from_str(r#"{"id":null,"trackName":null,"duration":null}"#).unwrap();
        assert!(!record.instrumental);
        assert_eq!(record.duration, None);
    }

    // ----- scoring ---------------------------------------------------------

    #[test]
    fn score_record_uses_fractional_duration() {
        let t = rick();
        let exact = rec(record(
            "Rick Astley",
            "Never Gonna Give You Up",
            212.6,
            None,
            None,
        ));
        assert!(score_record(&t, &exact) > 0.99);
        let far = rec(record(
            "Rick Astley",
            "Never Gonna Give You Up",
            400.0,
            None,
            None,
        ));
        assert!(score_record(&t, &far) < score_record(&t, &exact));
        for weird in [f64::NAN, f64::INFINITY, -5.0, 0.0, 1e300] {
            let r = LrclibRecord {
                duration: Some(weird),
                ..rec(record(
                    "Rick Astley",
                    "Never Gonna Give You Up",
                    1.0,
                    None,
                    None,
                ))
            };
            let score = score_record(&t, &r);
            assert!((0.0..=1.0).contains(&score), "{weird}: {score}");
        }
        let empty = rec(json!({}));
        assert!(score_record(&t, &empty) < MIN_SEARCH_SCORE);
    }

    #[test]
    fn best_match_prefers_synced_then_score() {
        let t = without_album(rick());
        let records: Vec<LrclibRecord> = [
            // Wrong artist (a cover with another length): rejected.
            record(
                "Some Cover Band",
                "Never Gonna Give You Up",
                180.0,
                Some(&marked("cover")),
                None,
            ),
            // Right song, plain only, perfect score.
            record(
                "Rick Astley",
                "Never Gonna Give You Up",
                213.0,
                None,
                Some("plain perfect"),
            ),
            // Right song, synced, a bit off in length.
            record(
                "Rick Astley",
                "Never Gonna Give You Up",
                216.5,
                Some(&marked("synced off")),
                None,
            ),
            // Right song, synced, exact length: the winner.
            record(
                "Rick Astley",
                "Never Gonna Give You Up",
                212.6,
                Some(&marked("synced exact")),
                None,
            ),
            // Synced again with the same score: the earlier one stays.
            record(
                "Rick Astley",
                "Never Gonna Give You Up",
                212.6,
                Some(&marked("synced later")),
                None,
            ),
        ]
        .into_iter()
        .map(rec)
        .collect();
        let lyrics = best_match(&t, &records).unwrap();
        assert_eq!(first_text(&lyrics), "synced exact");

        let only_plain_and_cover = [records[0].clone(), records[1].clone()];
        let lyrics = best_match(&t, &only_plain_and_cover).unwrap();
        assert_eq!(first_text(&lyrics), "plain perfect");

        assert!(best_match(&t, &records[..1]).is_none());
        assert!(best_match(&t, &[]).is_none());
    }

    // ----- fetch: /api/get -------------------------------------------------

    #[tokio::test]
    async fn get_hit_with_synced_lyrics() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            "/api/get" => ok(rick_record()),
            _ => Reply::Json(500, "unexpected".into()),
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "lrclib");
        assert_eq!(lyrics.lines[0].text, "We're no strangers to love");

        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        let get = &seen[0];
        assert_eq!(get.path, "/api/get");
        assert_eq!(get.param("artist_name"), Some("Rick Astley"));
        assert_eq!(get.param("track_name"), Some("Never Gonna Give You Up"));
        assert_eq!(get.param("album_name"), Some("Whenever You Need Somebody"));
        // 212.6 s rounds to 213.
        assert_eq!(get.param("duration"), Some("213"));
        assert_eq!(get.query.len(), 4);
    }

    #[tokio::test]
    async fn get_duration_rounds_to_nearest_second() {
        for (ms, expected) in [
            (212_499, "212"),
            (212_500, "213"),
            (1, "0"),
            (499, "0"),
            (500, "1"),
        ] {
            let server = MockServer::start(|_| ok(rick_record())).await;
            let mut t = rick();
            t.duration_ms = Some(ms);
            server.provider().fetch(&t).await.unwrap().unwrap();
            assert_eq!(server.seen()[0].param("duration"), Some(expected), "{ms}");
        }
    }

    #[tokio::test]
    async fn get_record_is_used_without_scoring() {
        // LRCLIB already matched it, even though the names look different.
        let server = MockServer::start(|seen| match seen.path.as_str() {
            "/api/get" => ok(record(
                "RICK ASTLEY!!",
                "Totally Different",
                999.0,
                Some(&marked("from get")),
                None,
            )),
            _ => ok(json!([])),
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert_eq!(first_text(&lyrics), "from get");
        assert_eq!(server.paths(), vec!["/api/get"]);
    }

    #[tokio::test]
    async fn get_instrumental() {
        let server = MockServer::start(|_| {
            ok(json!({
                "id": 7, "trackName": "Never Gonna Give You Up", "artistName": "Rick Astley",
                "albumName": "Whenever You Need Somebody", "duration": 213.0,
                "instrumental": true, "plainLyrics": null, "syncedLyrics": null
            }))
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert!(lyrics.instrumental);
        assert!(lyrics.lines.is_empty());
        assert_eq!(lyrics.source, "lrclib");
        assert_eq!(server.paths(), vec!["/api/get"]);
    }

    #[tokio::test]
    async fn get_plain_only() {
        let server = MockServer::start(|_| {
            ok(record(
                "Rick Astley",
                "Never Gonna Give You Up",
                213.0,
                None,
                Some(PLAIN),
            ))
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert!(!lyrics.synced);
        assert!(!lyrics.instrumental);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(server.paths(), vec!["/api/get"]);
    }

    #[tokio::test]
    async fn get_404_then_search_success() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            "/api/get" => not_found(),
            "/api/search" => ok(json!([rick_record()])),
            _ => Reply::Json(500, "{}".into()),
        })
        .await;
        let t = Track {
            artist: "Rick Astley, Someone Else".into(),
            ..rick()
        };
        let lyrics = server.provider().fetch(&t).await.unwrap().unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "lrclib");

        let seen = server.seen();
        assert_eq!(server.paths(), vec!["/api/get", "/api/search"]);
        // The exact lookup uses the full artist, the search the primary one.
        assert_eq!(
            seen[0].param("artist_name"),
            Some("Rick Astley, Someone Else")
        );
        assert_eq!(seen[1].param("track_name"), Some("Never Gonna Give You Up"));
        assert_eq!(seen[1].param("artist_name"), Some("Rick Astley"));
        assert_eq!(seen[1].query.len(), 2);
    }

    #[tokio::test]
    async fn get_record_without_lyrics_falls_back_to_search() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            "/api/get" => ok(record(
                "Rick Astley",
                "Never Gonna Give You Up",
                213.0,
                None,
                None,
            )),
            _ => ok(json!([rick_record()])),
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert!(lyrics.synced);
        assert_eq!(server.paths(), vec!["/api/get", "/api/search"]);
    }

    #[tokio::test]
    async fn no_album_or_duration_skips_get() {
        let server = MockServer::start(|_| ok(json!([rick_record()]))).await;
        server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap()
            .unwrap();
        let mut no_duration = rick();
        no_duration.duration_ms = None;
        server
            .provider()
            .fetch(&no_duration)
            .await
            .unwrap()
            .unwrap();
        let mut zero_duration = rick();
        zero_duration.duration_ms = Some(0);
        server
            .provider()
            .fetch(&zero_duration)
            .await
            .unwrap()
            .unwrap();
        let mut blank_album = rick();
        blank_album.album = Some("  ".into());
        server
            .provider()
            .fetch(&blank_album)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(server.paths(), vec!["/api/search"; 4]);
    }

    // ----- fetch: /api/search ----------------------------------------------

    #[tokio::test]
    async fn search_picks_the_best_record() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            "/api/search" => ok(json!([
                record(
                    "Some Cover Band",
                    "Never Gonna Give You Up",
                    180.0,
                    Some(&marked("cover")),
                    None
                ),
                record(
                    "Rick Astley",
                    "Never Gonna Give You Up (Live)",
                    260.0,
                    Some(&marked("live")),
                    None
                ),
                record(
                    "Rick Astley",
                    "Never Gonna Give You Up",
                    213.0,
                    None,
                    Some("plain perfect")
                ),
                record(
                    "Rick Astley",
                    "Never Gonna Give You Up",
                    214.0,
                    Some(&marked("synced")),
                    None
                ),
            ])),
            _ => not_found(),
        })
        .await;
        let lyrics = server.provider().fetch(&rick()).await.unwrap().unwrap();
        assert_eq!(first_text(&lyrics), "synced");
        assert!(lyrics.synced);
        assert_eq!(server.paths(), vec!["/api/get", "/api/search"]);
    }

    #[tokio::test]
    async fn search_rejects_wrong_artist() {
        let server = MockServer::start(|seen| {
            if seen.path == "/api/search" {
                ok(json!([record(
                    "Some Cover Band",
                    "Never Gonna Give You Up",
                    180.0,
                    Some(SYNCED),
                    None
                )]))
            } else {
                not_found()
            }
        })
        .await;
        assert_eq!(server.provider().fetch(&rick()).await.unwrap(), None);
        // Both searches ran, neither had a usable record.
        assert_eq!(
            server.paths(),
            vec!["/api/get", "/api/search", "/api/search"]
        );
    }

    #[tokio::test]
    async fn second_search_by_title_only() {
        let server = MockServer::start(|seen| {
            if seen.param("artist_name").is_some() {
                ok(json!([]))
            } else {
                ok(json!([record(
                    "Rick Astley",
                    "Never Gonna Give You Up",
                    213.0,
                    Some(&marked("title only")),
                    None
                )]))
            }
        })
        .await;
        let t = without_album(rick());
        let lyrics = server.provider().fetch(&t).await.unwrap().unwrap();
        assert_eq!(first_text(&lyrics), "title only");
        let seen = server.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].param("artist_name"), Some("Rick Astley"));
        assert_eq!(seen[1].path, "/api/search");
        assert_eq!(seen[1].param("track_name"), Some("Never Gonna Give You Up"));
        assert_eq!(seen[1].param("artist_name"), None);
        assert_eq!(seen[1].query.len(), 1);
    }

    #[tokio::test]
    async fn second_search_runs_when_first_has_only_poor_matches() {
        let server = MockServer::start(|seen| {
            if seen.param("artist_name").is_some() {
                ok(json!([
                    record(
                        "Rick Astley",
                        "Together Forever",
                        213.0,
                        Some(&marked("wrong song")),
                        None
                    ),
                    record("Rick Astley", "Never Gonna Give You Up", 213.0, None, None),
                ]))
            } else {
                ok(json!([rick_record()]))
            }
        })
        .await;
        let lyrics = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lyrics.lines[0].text, "We're no strangers to love");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn second_search_is_skipped_when_first_finds_something() {
        let server = MockServer::start(|_| {
            ok(json!([record(
                "Rick Astley",
                "Never Gonna Give You Up",
                213.0,
                None,
                Some(PLAIN)
            )]))
        })
        .await;
        let lyrics = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap()
            .unwrap();
        // Plain lyrics are usable, so there is no title-only search for synced ones.
        assert!(!lyrics.synced);
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn search_instrumental() {
        let server = MockServer::start(|_| {
            ok(json!([{
                "id": 9, "trackName": "Your Hand in Mine", "artistName": "Explosions in the Sky",
                "albumName": null, "duration": 497.4, "instrumental": true,
                "plainLyrics": null, "syncedLyrics": null
            }]))
        })
        .await;
        let t = Track {
            title: "Your Hand in Mine".into(),
            artist: "Explosions in the Sky".into(),
            album: None,
            duration_ms: Some(497_000),
            spotify_id: None,
        };
        let lyrics = server.provider().fetch(&t).await.unwrap().unwrap();
        assert!(lyrics.instrumental);
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn search_404_counts_as_empty() {
        let server = MockServer::start(|_| not_found()).await;
        assert_eq!(server.provider().fetch(&rick()).await.unwrap(), None);
        assert_eq!(
            server.paths(),
            vec!["/api/get", "/api/search", "/api/search"]
        );
    }

    #[tokio::test]
    async fn malformed_search_records_are_skipped() {
        let server = MockServer::start(|_| {
            ok(json!([
                {"id": "not a number", "trackName": "Never Gonna Give You Up", "artistName": "Rick Astley"},
                42,
                "text",
                rick_record(),
            ]))
        })
        .await;
        let lyrics = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap()
            .unwrap();
        assert!(lyrics.synced);
    }

    #[tokio::test]
    async fn empty_artist_searches_by_title_only() {
        let server = MockServer::start(|_| ok(json!([rick_record()]))).await;
        let t = Track {
            artist: "  ".into(),
            ..rick()
        };
        let lyrics = server.provider().fetch(&t).await.unwrap().unwrap();
        assert!(lyrics.synced);
        let seen = server.seen();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "/api/search");
        assert_eq!(seen[0].param("artist_name"), None);
    }

    #[tokio::test]
    async fn empty_title_asks_nothing() {
        let server = MockServer::start(|_| ok(json!([rick_record()]))).await;
        let t = Track {
            title: " \t ".into(),
            ..rick()
        };
        assert_eq!(server.provider().fetch(&t).await.unwrap(), None);
        assert!(server.seen().is_empty());
    }

    // ----- errors ----------------------------------------------------------

    #[tokio::test]
    async fn server_error_on_get_is_an_error() {
        let server = MockServer::start(|_| Reply::Json(500, "oops".into())).await;
        let err = server.provider().fetch(&rick()).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("500"), "{message}");
        assert!(message.contains("/api/get"), "{message}");
        // No search after a failure.
        assert_eq!(server.paths(), vec!["/api/get"]);
    }

    #[tokio::test]
    async fn server_error_on_search_is_an_error() {
        let server = MockServer::start(|seen| {
            if seen.param("artist_name").is_some() {
                ok(json!([]))
            } else {
                Reply::Json(503, "busy".into())
            }
        })
        .await;
        let err = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("503"), "{err:#}");
    }

    #[tokio::test]
    async fn other_client_errors_are_errors() {
        let server = MockServer::start(|_| Reply::Json(400, "{\"code\":400}".into())).await;
        let err = server.provider().fetch(&rick()).await.unwrap_err();
        assert!(format!("{err:#}").contains("400"), "{err:#}");
    }

    #[tokio::test]
    async fn bad_json_is_an_error() {
        for body in [
            "<html>Bad gateway</html>",
            "{\"id\":",
            "",
            "[1,2]",
            "\"just a string\"",
        ] {
            let server = MockServer::start(move |_| Reply::Json(200, body.into())).await;
            assert!(
                server.provider().fetch(&rick()).await.is_err(),
                "get body {body:?}"
            );
        }
        for body in ["<html></html>", "{\"trackName\":\"x\"}", "[", "null"] {
            let server = MockServer::start(move |_| Reply::Json(200, body.into())).await;
            let result = server.provider().fetch(&without_album(rick())).await;
            assert!(result.is_err(), "search body {body:?}");
        }
    }

    #[tokio::test]
    async fn timeout_is_an_error() {
        let server = MockServer::start(|_| Reply::Hang).await;
        let provider = LrclibProvider::for_tests(&server.base, Duration::from_millis(300)).unwrap();
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(10), provider.fetch(&rick()))
            .await
            .expect("the provider's own timeout should fire first");
        let err = result.unwrap_err();
        assert!(format!("{err:#}").contains("/api/get"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(server.seen().len(), 1);
    }

    #[tokio::test]
    async fn oversized_bodies_are_errors() {
        // Announced as too big: refused before reading.
        let announced = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n[]",
            MAX_BODY_BYTES + 1
        );
        let server = MockServer::start(move |_| Reply::Raw(announced.clone().into_bytes())).await;
        let err = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("more than"), "{err:#}");

        // No length given: reading stops once the limit is passed.
        let mut streamed =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n["
                .to_vec();
        streamed.resize(streamed.len() + MAX_BODY_BYTES + 1, b' ');
        let streamed = Arc::new(streamed);
        let server = MockServer::start(move |_| Reply::Raw(streamed.to_vec())).await;
        let err = server.provider().fetch(&rick()).await.unwrap_err();
        assert!(format!("{err:#}").contains("more than"), "{err:#}");
        assert_eq!(server.paths(), vec!["/api/get"]);
    }

    #[tokio::test]
    async fn body_without_length_is_read_to_the_end() {
        let body = json!([rick_record()]).to_string();
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
        );
        let server = MockServer::start(move |_| Reply::Raw(raw.clone().into_bytes())).await;
        let lyrics = server
            .provider()
            .fetch(&without_album(rick()))
            .await
            .unwrap()
            .unwrap();
        assert!(lyrics.synced);
    }

    #[tokio::test]
    async fn connection_refused_is_an_error() {
        // Bind and drop a listener to get a port nobody listens on.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let provider =
            LrclibProvider::for_tests(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2))
                .unwrap();
        assert!(provider.fetch(&rick()).await.is_err());
    }

    // ----- request details -------------------------------------------------

    #[tokio::test]
    async fn query_values_are_encoded() {
        let server = MockServer::start(|seen| {
            if seen.path == "/api/get" {
                not_found()
            } else {
                ok(json!([]))
            }
        })
        .await;
        let t = Track {
            title: "Déjà Vu & Me + You = 100% #1?".into(),
            artist: "Beyoncé & Jay-Z".into(),
            album: Some("B'Day / 宇多田ヒカル".into()),
            duration_ms: Some(240_000),
            spotify_id: None,
        };
        assert_eq!(server.provider().fetch(&t).await.unwrap(), None);
        let seen = server.seen();
        assert_eq!(seen.len(), 3);
        let get = &seen[0];
        assert_eq!(
            get.param("track_name"),
            Some("Déjà Vu & Me + You = 100% #1?")
        );
        assert_eq!(get.param("artist_name"), Some("Beyoncé & Jay-Z"));
        assert_eq!(get.param("album_name"), Some("B'Day / 宇多田ヒカル"));
        assert_eq!(get.param("duration"), Some("240"));
        assert_eq!(get.query.len(), 4);
        // Nothing that would split or end the query is sent raw.
        let raw_query = get.target.split_once('?').map(|(_, q)| q).unwrap_or("");
        assert_eq!(raw_query.matches('&').count(), 3, "{raw_query}");
        assert!(
            !raw_query.contains('#') && !raw_query.contains(' '),
            "{raw_query}"
        );
        assert!(raw_query.is_ascii(), "{raw_query}");
        // The search uses the primary artist.
        assert_eq!(seen[1].param("artist_name"), Some("Beyoncé"));
        assert_eq!(
            seen[1].param("track_name"),
            Some("Déjà Vu & Me + You = 100% #1?")
        );
        assert_eq!(seen[2].query.len(), 1);
    }

    #[tokio::test]
    async fn user_agent_is_sent() {
        let server = MockServer::start(|seen| {
            if seen.path == "/api/get" {
                not_found()
            } else {
                ok(json!([]))
            }
        })
        .await;
        // `new` and `for_tests` share `build`, which sets the User-Agent; the
        // test constructor keeps proxy settings of the machine out of the way.
        let provider = LrclibProvider::for_tests(&format!("{}/", server.base), TIMEOUT).unwrap();
        assert_eq!(provider.fetch(&rick()).await.unwrap(), None);
        let seen = server.seen();
        assert_eq!(seen.len(), 3);
        for request in &seen {
            assert_eq!(request.header("user-agent"), Some(crate::USER_AGENT));
        }
        assert!(crate::USER_AGENT.starts_with("Lyrix/"));
    }

    #[tokio::test]
    async fn base_path_prefix_is_kept() {
        let server = MockServer::start(|_| ok(rick_record())).await;
        let provider =
            LrclibProvider::for_tests(&format!("{}/mirror/", server.base), TIMEOUT).unwrap();
        provider.fetch(&rick()).await.unwrap().unwrap();
        assert_eq!(server.paths(), vec!["/mirror/api/get"]);
    }
}
