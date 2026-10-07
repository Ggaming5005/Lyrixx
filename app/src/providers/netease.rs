//! [NetEase Cloud Music](https://music.163.com): a large catalog of English,
//! Chinese, Japanese, Korean and other songs, most with timed lyrics.
//!
//! It has no official API. These are the endpoints its web player uses, which
//! answer without an account, key or cookie; they may change, and a provider
//! that fails is only skipped for a while.
//! - `GET {base}/api/search/get?s=<title artist>&type=1&limit=10` →
//!   `{"code":200,"result":{"songs":[{"id":17177324,"name":"Yellow",
//!   "alias":[],"artists":[{"name":"Coldplay","alias":[]}],
//!   "album":{"name":"Parachutes"},"duration":266773}]}}` (duration in ms;
//!   `result` has no `songs` when nothing matches).
//! - `GET {base}/api/song/lyric?id=<id>&lv=1&kv=1&tv=-1` →
//!   `{"code":200,"lrc":{"lyric":"[00:00.000] 作词 : …\n[00:33.790]Look at
//!   the stars\n…"}}`, with `"pureMusic":true` for a song without words, and
//!   `"nolyric":true` or `"uncollected":true` and no `lrc` when NetEase has no
//!   lyrics for it.

use super::credits::strip_credits;
use super::{http, LyricsProvider};
use crate::lrc::parse_lrc;
use crate::matcher::{best_scored, is_other_version, primary_artist, score_candidate};
use crate::types::{Lyrics, Track};
use anyhow::{bail, Context as _};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// The NetEase Cloud Music website.
pub const BASE_URL: &str = "https://music.163.com";

/// Request timeout for each NetEase call.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Minimum [`crate::matcher::score_candidate`] for a search result to be used.
/// Stricter than LRCLIB's, since NetEase lists many covers and other versions
/// of popular songs.
pub const MIN_SCORE: f64 = 0.7;

/// Search results asked for their lyrics at most, best first.
pub const MAX_CANDIDATES: usize = 3;

const SEARCH_PATH: &str = "/api/search/get";
const LYRIC_PATH: &str = "/api/song/lyric";

/// Search results requested.
const SEARCH_LIMIT: &str = "10";

/// Names compared per search result at most (its title and aliases, its
/// artists and theirs), so odd answers cannot make scoring slow.
const MAX_NAMES: usize = 8;

/// One search result, as far as matching needs it.
#[derive(Debug, Clone, PartialEq)]
struct Song {
    id: u64,
    /// The title first, then its aliases and translations.
    titles: Vec<String>,
    /// All artists joined with `, ` first, then each artist's aliases.
    artists: Vec<String>,
    album: String,
    duration_ms: Option<u64>,
}

/// See the module docs.
///
/// 1. Searches for the title and the primary artist (the title alone without
///    an artist).
/// 2. Scores each result's title and aliases against the track with
///    [`crate::matcher::score_candidate`], keeps those at [`MIN_SCORE`] or
///    more that are not a version without the singing
///    ([`crate::matcher::is_other_version`], by title, aliases and album),
///    and asks the best [`MAX_CANDIDATES`] for their lyrics in turn.
/// 3. The first synced or instrumental answer wins. Otherwise the first
///    unsynced one is returned, or `None`.
///
/// Lyrics go through [`super::credits::strip_credits`]. A blank title finds
/// nothing without asking. A `code` other than 200, HTTP errors, timeouts
/// and bodies that are not the expected JSON are errors; search results
/// without an id are skipped.
pub struct NeteaseProvider {
    client: reqwest::Client,
    base_url: String,
}

impl NeteaseProvider {
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
            .context("could not set up the HTTP client for NetEase")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    async fn search(&self, query: &str) -> anyhow::Result<Vec<Song>> {
        let what = format!("NetEase {SEARCH_PATH}");
        let url = format!("{}{SEARCH_PATH}", self.base_url);
        let params = [("s", query), ("type", "1"), ("limit", SEARCH_LIMIT)];
        let response: Value = http::get_json(&self.client, &url, &params, &what).await?;
        check_code(&response, &what)?;
        let songs = response
            .pointer("/result/songs")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        Ok(songs.iter().filter_map(song_from_json).collect())
    }

    /// The lyrics NetEase has for one song, tidied, or `None`.
    async fn lyrics(&self, song: &Song) -> anyhow::Result<Option<Lyrics>> {
        let what = format!("NetEase {LYRIC_PATH}");
        let url = format!("{}{LYRIC_PATH}", self.base_url);
        let id = song.id.to_string();
        let params = [("id", id.as_str()), ("lv", "1"), ("kv", "1"), ("tv", "-1")];
        let response: Value = http::get_json(&self.client, &url, &params, &what).await?;
        check_code(&response, &what)?;
        if response.get("pureMusic").and_then(Value::as_bool) == Some(true) {
            return Ok(Some(Lyrics {
                lines: Vec::new(),
                synced: false,
                instrumental: true,
                source: "netease".into(),
            }));
        }
        let text = response
            .pointer("/lrc/lyric")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = song.titles.first().map_or("", String::as_str);
        let artist = song.artists.first().map_or("", String::as_str);
        let mut lyrics = strip_credits(parse_lrc(text), title, artist);
        if !lyrics.instrumental && !lyrics.has_text() {
            tracing::debug!(id = song.id, "NetEase has no lyrics for this song");
            return Ok(None);
        }
        lyrics.source = "netease".into();
        Ok(Some(lyrics))
    }
}

#[async_trait]
impl LyricsProvider for NeteaseProvider {
    fn name(&self) -> &'static str {
        "netease"
    }

    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
        let title = track.title.trim();
        if title.is_empty() {
            return Ok(None);
        }
        let artist = primary_artist(&track.artist);
        let query = if artist.is_empty() {
            title.to_string()
        } else {
            format!("{title} {artist}")
        };
        let songs = self.search(&query).await?;

        let mut fallback: Option<Lyrics> = None;
        for song in pick(track, songs) {
            match self.lyrics(&song).await? {
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

/// The search results worth asking for lyrics, best first.
fn pick(track: &Track, songs: Vec<Song>) -> Vec<Song> {
    let wanted = [track.title.as_str(), track.album.as_deref().unwrap_or("")];
    let scored = songs
        .into_iter()
        .filter(|song| {
            let mut texts: Vec<&str> = song.titles.iter().map(String::as_str).collect();
            texts.push(&song.album);
            !is_other_version(&wanted, &texts)
        })
        .map(|song| (score(track, &song), song))
        .collect();
    best_scored(scored, MIN_SCORE, MAX_CANDIDATES)
}

/// The best score over the song's titles and artist names.
fn score(track: &Track, song: &Song) -> f64 {
    song.titles
        .iter()
        .flat_map(|title| {
            song.artists
                .iter()
                .map(move |artist| score_candidate(track, title, artist, song.duration_ms))
        })
        .fold(0.0, f64::max)
}

/// A search result, or `None` without a usable id.
fn song_from_json(value: &Value) -> Option<Song> {
    let id = value.get("id").and_then(as_id)?;
    let titles = names(
        std::iter::once(text(value, "name"))
            .chain(texts(value, "alias"))
            .chain(texts(value, "transNames")),
    );
    let artist_values = value
        .get("artists")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let joined = artist_values
        .iter()
        .map(|artist| text(artist, "name"))
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let artists = names(
        std::iter::once(joined.as_str()).chain(
            artist_values
                .iter()
                .flat_map(|artist| texts(artist, "alias").chain(texts(artist, "trans"))),
        ),
    );
    let album = value
        .get("album")
        .map(|album| text(album, "name"))
        .unwrap_or_default()
        .to_string();
    let duration_ms = value
        .get("duration")
        .or_else(|| value.get("dt"))
        .and_then(Value::as_u64)
        .filter(|&ms| ms > 0);
    Some(Song {
        id,
        titles,
        artists,
        album,
        duration_ms,
    })
}

/// Distinct, non-blank names, at most [`MAX_NAMES`]. Keeps one empty name
/// when there is none, so a song without artists can still be scored.
fn names<'a>(candidates: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in candidates.map(str::trim).filter(|name| !name.is_empty()) {
        if out.len() == MAX_NAMES {
            break;
        }
        if !out.iter().any(|seen| seen == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// A string field, or `""`.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The strings in an array field (or the field itself when it is a string).
fn texts<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a str> {
    let (list, single) = match value.get(key) {
        Some(Value::Array(items)) => (items.as_slice(), None),
        Some(Value::String(s)) => (&[][..], Some(s.as_str())),
        _ => (&[][..], None),
    };
    list.iter().filter_map(Value::as_str).chain(single)
}

/// An id written as a number or as a string of digits.
fn as_id(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// NetEase puts its own status in `code`; anything but 200 is a failure.
fn check_code(response: &Value, what: &str) -> anyhow::Result<()> {
    match response.get("code").and_then(Value::as_i64) {
        Some(200) => Ok(()),
        Some(code) => bail!("{what} answered code {code}"),
        None => bail!("{what} sent an unexpected response"),
    }
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
            duration_ms: Some(266_000),
            spotify_id: None,
        }
    }

    fn song(id: u64, name: &str, artist: &str, duration: u64) -> Value {
        json!({
            "id": id,
            "name": name,
            "alias": [],
            "artists": [{ "id": 89365, "name": artist, "alias": [] }],
            "album": { "id": 1, "name": "Parachutes" },
            "duration": duration,
            "mvid": 0,
            "fee": 8,
        })
    }

    fn search(songs: Vec<Value>) -> Reply {
        let count = songs.len();
        ok(json!({ "result": { "songs": songs, "songCount": count }, "code": 200 }))
    }

    fn lyric(lrc: &str) -> Reply {
        ok(json!({
            "sgc": false,
            "sfy": false,
            "qfy": false,
            "lrc": { "version": 80, "lyric": lrc },
            "klyric": { "version": 0, "lyric": "" },
            "tlyric": { "version": 0, "lyric": "" },
            "code": 200,
        }))
    }

    const YELLOW_LRC: &str =
        "[00:00.000] 作词 : Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\n\
        [00:01.000] 作曲 : Guy Berryman/Jonny Buckland/Chris Martin/Will Champion\n\
        [00:33.790]Look at the stars\n\
        [00:37.660]Look how they shine for you\n\
        [04:21.773] 电吉他 : Jonny Buckland\n";

    fn provider(server: &MockServer) -> NeteaseProvider {
        NeteaseProvider::for_tests(&server.base, TIMEOUT).unwrap()
    }

    fn texts_of(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[tokio::test]
    async fn finds_synced_lyrics_without_the_credits() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![song(17_177_324, "Yellow", "Coldplay", 266_773)]),
            LYRIC_PATH => lyric(YELLOW_LRC),
            _ => Reply::Json(404, "{}".into()),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "netease");
        assert_eq!(
            texts_of(&lyrics),
            vec!["Look at the stars", "Look how they shine for you", ""]
        );

        let seen = server.seen();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].param("s"), Some("Yellow Coldplay"));
        assert_eq!(seen[0].param("type"), Some("1"));
        assert_eq!(seen[0].param("limit"), Some("10"));
        assert_eq!(seen[1].param("id"), Some("17177324"));
        assert_eq!(seen[1].param("lv"), Some("1"));
        assert_eq!(seen[1].param("tv"), Some("-1"));
        for request in &seen {
            assert_eq!(request.header("user-agent"), Some(crate::USER_AGENT));
            assert_eq!(request.header("cookie"), None);
        }
    }

    #[tokio::test]
    async fn the_best_match_is_asked_first_and_poor_ones_never() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                song(1, "Yellow Submarine", "The Beatles", 158_000),
                song(2, "Yellow (Live)", "Coldplay", 290_000),
                song(3, "Yellow", "Coldplay", 266_773),
                song(4, "Yellow", "Someone Else", 200_000),
            ]),
            LYRIC_PATH => {
                let id = seen.param("id").unwrap_or_default();
                lyric(&format!("[00:10.00]from {id}\n"))
            }
            _ => Reply::Json(404, "{}".into()),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert_eq!(texts_of(&lyrics), vec!["from 3"]);
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn versions_without_singing_are_skipped() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                song(1, "Yellow (Instrumental)", "Coldplay", 266_773),
                song(2, "Yellow (伴奏)", "Coldplay", 266_773),
            ]),
            _ => lyric("[00:10.00]should not be asked\n"),
        })
        .await;
        assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
        assert_eq!(server.paths(), vec![SEARCH_PATH]);
    }

    #[tokio::test]
    async fn aliases_count_as_titles_and_artists() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![json!({
                "id": 186_016,
                "name": "晴天",
                "alias": ["Sunny Day"],
                "artists": [{ "name": "周杰伦", "alias": ["Jay Chou"] }],
                "album": { "name": "叶惠美" },
                "duration": 269_000,
            })]),
            _ => lyric("[00:29.00]故事的小黄花\n"),
        })
        .await;
        let track = Track {
            title: "Sunny Day".into(),
            artist: "Jay Chou".into(),
            album: None,
            duration_ms: Some(269_500),
            spotify_id: None,
        };
        let lyrics = provider(&server).fetch(&track).await.unwrap().unwrap();
        assert_eq!(texts_of(&lyrics), vec!["故事的小黄花"]);
    }

    #[tokio::test]
    async fn synced_lyrics_from_a_later_result_beat_plain_ones() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                song(1, "Yellow", "Coldplay", 266_773),
                song(2, "Yellow", "Coldplay", 266_000),
            ]),
            _ => match seen.param("id") {
                Some("1") => lyric("Look at the stars\nLook how they shine for you"),
                _ => lyric("[00:33.79]Look at the stars\n"),
            },
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.synced);

        // Only plain lyrics anywhere: the first ones.
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                song(1, "Yellow", "Coldplay", 266_773),
                song(2, "Yellow", "Coldplay", 266_000),
            ]),
            _ => lyric(&format!("plain {}", seen.param("id").unwrap_or_default())),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(!lyrics.synced);
        assert_eq!(texts_of(&lyrics), vec!["plain 1"]);
    }

    #[tokio::test]
    async fn pure_music_is_instrumental() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![song(1, "Yellow", "Coldplay", 266_773)]),
            _ => ok(json!({
                "sgc": false, "sfy": true, "qfy": true, "needDesc": true,
                "pureMusic": true, "lrc": { "version": 1, "lyric": "" },
                "code": 200, "briefDesc": null,
            })),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.instrumental);
        assert!(lyrics.lines.is_empty());
        assert_eq!(lyrics.source, "netease");

        // A "pure music" notice instead of the flag.
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![song(1, "Yellow", "Coldplay", 266_773)]),
            _ => lyric("[00:00.00] 纯音乐，请欣赏\n"),
        })
        .await;
        let lyrics = provider(&server).fetch(&yellow()).await.unwrap().unwrap();
        assert!(lyrics.instrumental);
    }

    #[tokio::test]
    async fn songs_without_lyrics_move_on_to_the_next_result() {
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![
                song(1, "Yellow", "Coldplay", 266_773),
                song(2, "Yellow", "Coldplay", 266_000),
                song(3, "Yellow", "Coldplay", 266_500),
                song(4, "Yellow", "Coldplay", 266_200),
            ]),
            _ => match seen.param("id") {
                Some("1") => ok(json!({ "sgc": true, "nolyric": true, "code": 200 })),
                Some("2") => ok(json!({ "uncollected": true, "code": 200 })),
                Some("3") => lyric("[00:00.00] 作词 : Someone\n"),
                _ => lyric("[00:33.79]Look at the stars\n"),
            },
        })
        .await;
        // Only the best three are asked, and none has lyrics.
        assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
        assert_eq!(server.seen().len(), 4);
    }

    #[tokio::test]
    async fn nothing_found_is_none() {
        for body in [
            json!({ "result": { "songCount": 0 }, "code": 200 }),
            json!({ "result": {}, "code": 200 }),
            json!({ "code": 200 }),
            json!({ "result": { "songs": [{ "name": "no id" }, 7, null] }, "code": 200 }),
        ] {
            let server = MockServer::start(move |_| ok(body.clone())).await;
            assert_eq!(provider(&server).fetch(&yellow()).await.unwrap(), None);
            assert_eq!(server.seen().len(), 1);
        }
    }

    #[tokio::test]
    async fn failures_are_errors() {
        // NetEase's own refusal.
        let server =
            MockServer::start(|_| ok(json!({ "code": -460, "message": "Cheating" }))).await;
        let err = provider(&server).fetch(&yellow()).await.unwrap_err();
        assert!(format!("{err:#}").contains("-460"), "{err:#}");

        // HTTP errors and bodies that are not NetEase's JSON.
        let replies: [fn() -> Reply; 3] = [
            || Reply::Json(503, "busy".into()),
            || Reply::Json(200, "<html>blocked</html>".into()),
            || Reply::Json(200, "[]".into()),
        ];
        for reply in replies {
            let server = MockServer::start(move |_| reply()).await;
            assert!(provider(&server).fetch(&yellow()).await.is_err());
        }

        // A failing lyrics request after a good search.
        let server = MockServer::start(|seen| match seen.path.as_str() {
            SEARCH_PATH => search(vec![song(1, "Yellow", "Coldplay", 266_773)]),
            _ => Reply::Json(500, "oops".into()),
        })
        .await;
        let err = provider(&server).fetch(&yellow()).await.unwrap_err();
        assert!(format!("{err:#}").contains(LYRIC_PATH), "{err:#}");
    }

    #[tokio::test]
    async fn timeouts_are_errors() {
        let server = MockServer::start(|_| Reply::Hang).await;
        let provider =
            NeteaseProvider::for_tests(&server.base, Duration::from_millis(300)).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), provider.fetch(&yellow()))
            .await
            .expect("the provider's own timeout should fire first");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_blank_title_asks_nothing_and_no_artist_searches_the_title() {
        let server = MockServer::start(|_| search(Vec::new())).await;
        let blank = Track {
            title: "  ".into(),
            ..yellow()
        };
        assert_eq!(provider(&server).fetch(&blank).await.unwrap(), None);
        assert!(server.seen().is_empty());

        let no_artist = Track {
            artist: String::new(),
            ..yellow()
        };
        assert_eq!(provider(&server).fetch(&no_artist).await.unwrap(), None);
        assert_eq!(server.seen()[0].param("s"), Some("Yellow"));

        // Several artists: the first one.
        let duet = Track {
            artist: "Coldplay & Rihanna".into(),
            ..yellow()
        };
        provider(&server).fetch(&duet).await.unwrap();
        assert_eq!(server.seen()[1].param("s"), Some("Yellow Coldplay"));
    }

    #[test]
    fn search_results_are_read_leniently() {
        let song = song_from_json(&json!({
            "id": "42",
            "name": " Yellow ",
            "alias": ["Yellow", "黄色", null, 3],
            "transNames": "Huang",
            "artists": [{ "name": "Coldplay", "alias": null, "trans": "酷玩乐队" }, { "name": "" }],
            "album": null,
            "dt": 266_773,
        }))
        .unwrap();
        assert_eq!(song.id, 42);
        assert_eq!(song.titles, vec!["Yellow", "黄色", "Huang"]);
        assert_eq!(song.artists, vec!["Coldplay", "酷玩乐队"]);
        assert_eq!(song.album, "");
        assert_eq!(song.duration_ms, Some(266_773));

        let bare = song_from_json(&json!({ "id": 7 })).unwrap();
        assert_eq!(bare.titles, vec![""]);
        assert_eq!(bare.artists, vec![""]);
        assert_eq!(bare.duration_ms, None);

        assert_eq!(song_from_json(&json!({ "id": -1 })), None);
        assert_eq!(song_from_json(&json!({ "name": "x" })), None);
        assert_eq!(song_from_json(&json!("x")), None);
    }

    #[test]
    fn names_are_capped() {
        let many: Vec<String> = (0..20).map(|i| format!("name {i}")).collect();
        let out = names(many.iter().map(String::as_str));
        assert_eq!(out.len(), MAX_NAMES);
        assert_eq!(out[0], "name 0");
    }
}
