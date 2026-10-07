//! Lyrics providers and the chain that tries them in order.

pub mod cache;
pub mod credits;
mod http;
pub mod kugou;
pub mod local;
pub mod lrclib;
pub mod musixmatch;
pub mod netease;
#[cfg(test)]
mod test_server;

use crate::types::{Lyrics, Track};
use async_trait::async_trait;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Consecutive errors after which a provider is skipped for [`SKIP_FOR`].
pub const MAX_CONSECUTIVE_ERRORS: u32 = 3;
/// How long a failing provider is skipped.
pub const SKIP_FOR: Duration = Duration::from_secs(10 * 60);

/// A place lyrics can come from.
#[async_trait]
pub trait LyricsProvider: Send + Sync {
    /// Short stable name, stored in [`Lyrics::source`] (e.g. `lrclib`).
    fn name(&self) -> &'static str;

    /// Looks up lyrics for an already-normalized track.
    /// `Ok(None)` means "this provider has nothing for the song";
    /// `Err` means the provider failed (network, bad response) and counts
    /// against its health.
    async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>>;

    /// True for the user's own files: asked before the cache, and never
    /// cached, so a file added for a song that was looked up before is used
    /// right away.
    fn is_local(&self) -> bool {
        false
    }

    /// False when the provider's lyrics may not be kept: they are never
    /// written to the cache, so the song is looked up again each time.
    fn may_cache(&self) -> bool {
        true
    }
}

/// The outcome of a lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Found(Lyrics),
    NotFound,
}

/// Tries the local providers, then the cache, then each other healthy
/// provider in order.
///
/// - The track is normalized with [`crate::matcher::normalize_track`] first.
/// - Local providers ([`LyricsProvider::is_local`], your own files) are asked
///   first and their results are never cached: synced or instrumental lyrics
///   from them return at once; plain ones are kept as the first fallback.
/// - A cache hit (including a fresh "not found" entry) returns without asking
///   the other providers; a local fallback still beats a cached "not found"
///   or cached plain lyrics. A cached "not found" or cached plain lyrics
///   count only when every online provider that is on now was asked for them
///   ([`cache::CacheEntry::providers`]), so turning a provider on looks the
///   song up again.
/// - Synced lyrics win: if a provider returns unsynced lyrics, they are kept as
///   a fallback and the remaining providers are still asked for synced ones.
///   Instrumental results count as final.
/// - Unsynced lyrics are returned with timings spread by
///   [`Lyrics::spread_evenly`] when the duration is known.
/// - The final outcome of the other providers (found or not found) is written
///   to the cache when one is set, unless the lyrics come from a provider
///   whose lyrics may not be kept ([`LyricsProvider::may_cache`]).
/// - [`Lyrics::source`] is set to the provider's name.
/// - Health: an `Err` increments the provider's consecutive error count;
///   `Ok` resets it; at [`MAX_CONSECUTIVE_ERRORS`] the provider is skipped until
///   [`SKIP_FOR`] has passed, then tried again. A provider error never fails
///   the whole lookup.
///
/// Details:
/// - Of several unsynced results, the one from the earliest provider is kept.
/// - A result that has no text and is not instrumental (for example LRC made
///   only of timestamps) counts as "nothing" from that provider.
/// - Unsynced lyrics from the cache are spread again over the track's
///   duration, when it is known.
/// - The outcome is not cached when it may be incomplete: nothing synced or
///   instrumental was found while a provider failed or was skipped. So a
///   network outage is not remembered as "not found", nor plain lyrics as the
///   answer when a failing provider may have synced ones.
/// - When a song is looked up again because a provider was turned on, and
///   that lookup is incomplete and finds nothing, the lyrics cached before
///   are used (and kept in the cache).
/// - A provider that is tried again after [`SKIP_FOR`] and fails once more is
///   skipped again right away; one success resets it.
/// - A cache that cannot be written only logs a warning.
pub struct ProviderChain {
    providers: Vec<Box<dyn LyricsProvider>>,
    cache: Option<cache::LyricsCache>,
    /// One entry per provider, same order. Never held across an `.await`.
    health: Mutex<Vec<Health>>,
    /// The time source for health decisions; tests replace it.
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

/// Health of one provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Health {
    consecutive_errors: u32,
    /// Set when the provider reached [`MAX_CONSECUTIVE_ERRORS`].
    skipped_until: Option<Instant>,
}

impl Health {
    fn is_skipped(&self, now: Instant) -> bool {
        self.consecutive_errors >= MAX_CONSECUTIVE_ERRORS
            && self.skipped_until.is_some_and(|until| now < until)
    }
}

impl ProviderChain {
    pub fn new(providers: Vec<Box<dyn LyricsProvider>>, cache: Option<cache::LyricsCache>) -> Self {
        let health = vec![Health::default(); providers.len()];
        Self {
            providers,
            cache,
            health: Mutex::new(health),
            now: Arc::new(Instant::now),
        }
    }

    /// Replaces the time source used for provider health.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, now: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// See the type docs. Safe to call from several tasks at once.
    pub async fn resolve(&self, track: &Track) -> Resolved {
        let track = crate::matcher::normalize_track(track);
        let duration_ms = track.duration_ms.filter(|&d| d > 0);

        // Your own files first. A failing one is logged and does not make the
        // other providers' outcome incomplete (it is never cached anyway).
        let mut local_fallback: Option<Lyrics> = None;
        for (index, provider) in self.providers.iter().enumerate() {
            if !provider.is_local() {
                continue;
            }
            match self.ask(index, provider.as_ref(), &track).await {
                Answer::Final(mut lyrics) => {
                    spread(&mut lyrics, duration_ms);
                    return Resolved::Found(lyrics);
                }
                Answer::Plain(lyrics) => {
                    local_fallback.get_or_insert(lyrics);
                }
                Answer::Nothing | Answer::Failed => {}
            }
        }

        // Lyrics cached by a lookup that did not ask every provider that is
        // on now: used only when this lookup cannot finish.
        let mut stale: Option<Lyrics> = None;
        if let Some(cache) = &self.cache {
            if let Some(entry) = cache.get(&track).await {
                let final_hit = entry.lyrics.as_ref().is_some_and(is_final);
                let asked_all = self
                    .online_names()
                    .all(|name| entry.providers.iter().any(|asked| asked == name));
                if final_hit || asked_all {
                    let best = match entry.lyrics {
                        Some(lyrics) if final_hit => Some(lyrics),
                        cached => local_fallback.or(cached),
                    };
                    return match best {
                        Some(mut lyrics) => {
                            spread(&mut lyrics, duration_ms);
                            Resolved::Found(lyrics)
                        }
                        None => Resolved::NotFound,
                    };
                }
                stale = entry.lyrics;
            }
        }

        // Synced or instrumental lyrics: nothing better can come.
        let mut final_result: Option<Lyrics> = None;
        // The first unsynced lyrics, used when nobody has synced ones.
        let mut fallback: Option<Lyrics> = None;
        // A provider failed or was skipped, so a better result may exist.
        let mut incomplete = false;

        for (index, provider) in self.providers.iter().enumerate() {
            if provider.is_local() {
                continue;
            }
            match self.ask(index, provider.as_ref(), &track).await {
                Answer::Final(lyrics) => {
                    final_result = Some(lyrics);
                    break;
                }
                Answer::Plain(lyrics) => {
                    fallback.get_or_insert(lyrics);
                }
                Answer::Nothing => {}
                Answer::Failed => incomplete = true,
            }
        }

        let found_final = final_result.is_some();
        let outcome = final_result.or(fallback).map(|mut lyrics| {
            spread(&mut lyrics, duration_ms);
            lyrics
        });

        let may_cache = outcome
            .as_ref()
            .map_or(true, |lyrics| self.may_cache(&lyrics.source));
        if let Some(cache) = &self.cache {
            if (found_final || !incomplete) && may_cache {
                let entry = cache::CacheEntry {
                    lyrics: outcome.clone(),
                    fetched_at: unix_now_secs(),
                    duration_ms: track.duration_ms,
                    providers: self.online_names().map(str::to_string).collect(),
                };
                if let Err(e) = cache.put(&track, &entry).await {
                    tracing::warn!("could not write to the lyrics cache: {e:#}");
                }
            }
        }

        // Nothing new because a provider failed: what was cached before is
        // still better than nothing.
        let outcome = match (outcome, stale) {
            (None, Some(mut lyrics)) if incomplete => {
                spread(&mut lyrics, duration_ms);
                Some(lyrics)
            }
            (outcome, _) => outcome,
        };

        let best = if found_final {
            outcome
        } else {
            local_fallback
                .map(|mut lyrics| {
                    spread(&mut lyrics, duration_ms);
                    lyrics
                })
                .or(outcome)
        };
        match best {
            Some(lyrics) => Resolved::Found(lyrics),
            None => Resolved::NotFound,
        }
    }

    /// Whether lyrics from the provider named `source` may be cached.
    fn may_cache(&self, source: &str) -> bool {
        self.providers
            .iter()
            .find(|provider| provider.name() == source)
            .map_or(true, |provider| provider.may_cache())
    }

    /// Names of the providers that are not your own files, in order.
    fn online_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.providers
            .iter()
            .filter(|provider| !provider.is_local())
            .map(|provider| provider.name())
    }

    /// Asks one provider (unless it is skipped), keeping its health.
    async fn ask(&self, index: usize, provider: &dyn LyricsProvider, track: &Track) -> Answer {
        let name = provider.name();
        if self.is_skipped(index) {
            tracing::debug!(provider = name, "skipping a failing lyrics provider");
            return Answer::Failed;
        }
        match provider.fetch(track).await {
            Ok(found) => {
                self.record_ok(index);
                let Some(mut lyrics) = found else {
                    return Answer::Nothing;
                };
                lyrics.source = name.to_string();
                if is_final(&lyrics) {
                    Answer::Final(lyrics)
                } else if lyrics.has_text() {
                    Answer::Plain(lyrics)
                } else {
                    Answer::Nothing
                }
            }
            Err(e) => {
                if self.record_err(index) {
                    tracing::warn!(
                        provider = name,
                        "lyrics lookup failed {MAX_CONSECUTIVE_ERRORS} times in a row, \
                         not asking this provider for {} minutes: {e:#}",
                        SKIP_FOR.as_secs() / 60
                    );
                } else {
                    tracing::warn!(provider = name, "lyrics lookup failed: {e:#}");
                }
                Answer::Failed
            }
        }
    }

    /// Names of the providers, in order, with whether each is currently skipped.
    pub fn health(&self) -> Vec<(&'static str, bool)> {
        let now = (self.now)();
        let health = self.lock_health();
        self.providers
            .iter()
            .enumerate()
            .map(|(index, provider)| {
                let skipped = health.get(index).is_some_and(|h| h.is_skipped(now));
                (provider.name(), skipped)
            })
            .collect()
    }

    fn lock_health(&self) -> MutexGuard<'_, Vec<Health>> {
        // A panic while the lock was held leaves plain counters behind, which
        // are still fine to use.
        self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_skipped(&self, index: usize) -> bool {
        let now = (self.now)();
        self.lock_health()
            .get(index)
            .is_some_and(|h| h.is_skipped(now))
    }

    fn record_ok(&self, index: usize) {
        if let Some(h) = self.lock_health().get_mut(index) {
            *h = Health::default();
        }
    }

    /// Counts an error. Returns true when the provider is skipped from now on.
    fn record_err(&self, index: usize) -> bool {
        let now = (self.now)();
        let mut health = self.lock_health();
        let Some(h) = health.get_mut(index) else {
            return false;
        };
        h.consecutive_errors = h.consecutive_errors.saturating_add(1);
        if h.consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
            h.skipped_until = Some(now.checked_add(SKIP_FOR).unwrap_or(now));
            true
        } else {
            false
        }
    }
}

/// Spreads unsynced lyrics over a known, non-zero duration.
/// What one provider gave.
enum Answer {
    /// Synced lyrics with text, or an instrumental song: nothing better can come.
    Final(Lyrics),
    /// Unsynced lyrics with text.
    Plain(Lyrics),
    /// Nothing usable.
    Nothing,
    /// The provider failed or is skipped.
    Failed,
}

/// Synced lyrics with text, or instrumental.
fn is_final(lyrics: &Lyrics) -> bool {
    lyrics.instrumental || (lyrics.synced && lyrics.has_text())
}

fn spread(lyrics: &mut Lyrics, duration_ms: Option<u64>) {
    if let Some(duration) = duration_ms {
        if !lyrics.synced {
            lyrics.spread_evenly(duration);
        }
    }
}

/// Unix seconds now, 0 when the system clock is before 1970.
fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LyricLine;
    use std::collections::VecDeque;

    /// What a fake provider answers.
    #[derive(Debug, Clone)]
    enum Reply {
        Found(Lyrics),
        Nothing,
        Fail(&'static str),
    }

    /// A provider that answers from a script and records every track it saw.
    struct FakeProvider {
        name: &'static str,
        replies: Mutex<VecDeque<Reply>>,
        /// Answer once the script is used up.
        default: Reply,
        calls: Arc<Mutex<Vec<Track>>>,
        delay: Duration,
        local: bool,
        keep: bool,
    }

    impl FakeProvider {
        fn new(name: &'static str, default: Reply) -> Self {
            Self {
                name,
                replies: Mutex::new(VecDeque::new()),
                default,
                calls: Arc::new(Mutex::new(Vec::new())),
                delay: Duration::ZERO,
                local: false,
                keep: true,
            }
        }

        /// Makes this provider's lyrics ones that may not be cached.
        fn not_cached(mut self) -> Self {
            self.keep = false;
            self
        }

        /// Makes this provider stand for your own files.
        fn local(mut self) -> Self {
            self.local = true;
            self
        }

        fn script(self, replies: Vec<Reply>) -> Self {
            *self.replies.lock().unwrap() = replies.into();
            self
        }

        fn delayed(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn calls(&self) -> Arc<Mutex<Vec<Track>>> {
            Arc::clone(&self.calls)
        }
    }

    #[async_trait]
    impl LyricsProvider for FakeProvider {
        fn name(&self) -> &'static str {
            self.name
        }

        fn is_local(&self) -> bool {
            self.local
        }

        fn may_cache(&self) -> bool {
            self.keep
        }

        async fn fetch(&self, track: &Track) -> anyhow::Result<Option<Lyrics>> {
            self.calls.lock().unwrap().push(track.clone());
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| self.default.clone());
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            match reply {
                Reply::Found(lyrics) => Ok(Some(lyrics)),
                Reply::Nothing => Ok(None),
                Reply::Fail(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    /// A clock tests move by hand.
    #[derive(Clone)]
    struct TestClock {
        base: Instant,
        offset: Arc<Mutex<Duration>>,
    }

    impl TestClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset: Arc::new(Mutex::new(Duration::ZERO)),
            }
        }

        fn advance(&self, by: Duration) {
            let mut offset = self.offset.lock().unwrap();
            *offset += by;
        }

        fn source(&self) -> Arc<dyn Fn() -> Instant + Send + Sync> {
            let clock = self.clone();
            Arc::new(move || clock.base + *clock.offset.lock().unwrap())
        }
    }

    fn track() -> Track {
        Track {
            title: "Song".into(),
            artist: "Artist".into(),
            album: Some("Album".into()),
            duration_ms: Some(200_000),
            spotify_id: None,
        }
    }

    fn track_without_duration() -> Track {
        Track {
            duration_ms: None,
            ..track()
        }
    }

    fn synced(text: &str) -> Lyrics {
        Lyrics {
            lines: vec![
                LyricLine {
                    start_ms: 1_000,
                    text: text.into(),
                },
                LyricLine {
                    start_ms: 4_000,
                    text: format!("{text} again"),
                },
            ],
            synced: true,
            instrumental: false,
            source: "made up".into(),
        }
    }

    fn plain(lines: &[&str]) -> Lyrics {
        crate::lrc::from_plain(&lines.join("\n"))
    }

    fn instrumental() -> Lyrics {
        Lyrics {
            lines: Vec::new(),
            synced: false,
            instrumental: true,
            source: String::new(),
        }
    }

    fn chain(providers: Vec<FakeProvider>) -> ProviderChain {
        let boxed = providers
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn LyricsProvider>)
            .collect();
        ProviderChain::new(boxed, None)
    }

    fn found(resolved: Resolved) -> Lyrics {
        match resolved {
            Resolved::Found(lyrics) => lyrics,
            Resolved::NotFound => panic!("expected lyrics, got NotFound"),
        }
    }

    fn texts(lyrics: &Lyrics) -> Vec<&str> {
        lyrics.lines.iter().map(|l| l.text.as_str()).collect()
    }

    fn starts(lyrics: &Lyrics) -> Vec<u64> {
        lyrics.lines.iter().map(|l| l.start_ms).collect()
    }

    fn call_count(calls: &Arc<Mutex<Vec<Track>>>) -> usize {
        calls.lock().unwrap().len()
    }

    // ---- order and preference ----

    #[tokio::test]
    async fn providers_are_asked_in_order_until_synced_lyrics_are_found() {
        let first = FakeProvider::new("first", Reply::Nothing);
        let second = FakeProvider::new("second", Reply::Found(synced("Two")));
        let third = FakeProvider::new("third", Reply::Found(synced("Three")));
        let (c1, c2, c3) = (first.calls(), second.calls(), third.calls());
        let chain = chain(vec![first, second, third]);

        let lyrics = found(chain.resolve(&track()).await);

        assert_eq!(texts(&lyrics), ["Two", "Two again"]);
        assert_eq!(lyrics.source, "second");
        assert!(lyrics.synced);
        assert_eq!(call_count(&c1), 1);
        assert_eq!(call_count(&c2), 1);
        assert_eq!(call_count(&c3), 0, "synced lyrics end the search");
    }

    #[tokio::test]
    async fn synced_lyrics_beat_earlier_unsynced_ones() {
        let first = FakeProvider::new("first", Reply::Found(plain(&["a", "b"])));
        let second = FakeProvider::new("second", Reply::Found(synced("Synced")));
        let chain = chain(vec![first, second]);

        let lyrics = found(chain.resolve(&track()).await);

        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "second");
        assert_eq!(starts(&lyrics), [1_000, 4_000], "synced timings are kept");
    }

    #[tokio::test]
    async fn unsynced_lyrics_are_the_fallback_and_the_earliest_one_wins() {
        let first = FakeProvider::new("first", Reply::Found(plain(&["first a", "first b"])));
        let second = FakeProvider::new("second", Reply::Nothing);
        let third = FakeProvider::new("third", Reply::Found(plain(&["third"])));
        let (c2, c3) = (second.calls(), third.calls());
        let chain = chain(vec![first, second, third]);

        let lyrics = found(chain.resolve(&track()).await);

        assert_eq!(texts(&lyrics), ["first a", "first b"]);
        assert_eq!(lyrics.source, "first");
        assert!(!lyrics.synced);
        assert_eq!(
            call_count(&c2),
            1,
            "later providers are asked for synced lyrics"
        );
        assert_eq!(call_count(&c3), 1);
    }

    #[tokio::test]
    async fn instrumental_results_are_final() {
        let first = FakeProvider::new("first", Reply::Found(instrumental()));
        let second = FakeProvider::new("second", Reply::Found(synced("Words")));
        let c2 = second.calls();
        let chain = chain(vec![first, second]);

        let lyrics = found(chain.resolve(&track()).await);

        assert!(lyrics.instrumental);
        assert_eq!(lyrics.source, "first");
        assert_eq!(call_count(&c2), 0);
    }

    #[tokio::test]
    async fn instrumental_after_an_unsynced_fallback_wins() {
        let first = FakeProvider::new("first", Reply::Found(plain(&["words"])));
        let second = FakeProvider::new("second", Reply::Found(instrumental()));
        let chain = chain(vec![first, second]);

        let lyrics = found(chain.resolve(&track()).await);

        assert!(lyrics.instrumental);
        assert_eq!(lyrics.source, "second");
    }

    #[tokio::test]
    async fn nothing_found_anywhere_is_not_found() {
        let chain = chain(vec![
            FakeProvider::new("first", Reply::Nothing),
            FakeProvider::new("second", Reply::Nothing),
        ]);
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
    }

    #[tokio::test]
    async fn an_empty_chain_finds_nothing() {
        let chain = chain(Vec::new());
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        assert!(chain.health().is_empty());
    }

    #[tokio::test]
    async fn results_without_text_count_as_nothing() {
        let empty_synced = Lyrics {
            lines: vec![LyricLine {
                start_ms: 1_000,
                text: String::new(),
            }],
            synced: true,
            instrumental: false,
            source: String::new(),
        };
        let first = FakeProvider::new("first", Reply::Found(empty_synced));
        let second = FakeProvider::new("second", Reply::Found(Lyrics::default()));
        let third = FakeProvider::new("third", Reply::Found(plain(&["real words"])));
        let chain = chain(vec![first, second, third]);

        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.source, "third");
        assert_eq!(texts(&lyrics), ["real words"]);

        let only_empty = self::chain(vec![FakeProvider::new(
            "first",
            Reply::Found(Lyrics::default()),
        )]);
        assert_eq!(only_empty.resolve(&track()).await, Resolved::NotFound);
    }

    #[tokio::test]
    async fn source_is_set_to_the_provider_name() {
        let mut lyrics = synced("x");
        lyrics.source = "something else".into();
        let chain = chain(vec![FakeProvider::new("mine", Reply::Found(lyrics))]);
        assert_eq!(found(chain.resolve(&track()).await).source, "mine");
    }

    // ---- unsynced timing ----

    #[tokio::test]
    async fn unsynced_lyrics_are_spread_over_a_known_duration() {
        let mut t = track();
        t.duration_ms = Some(100_000);
        let chain = chain(vec![FakeProvider::new(
            "plain",
            Reply::Found(plain(&["a", "b", "c"])),
        )]);

        let lyrics = found(chain.resolve(&t).await);

        assert_eq!(starts(&lyrics), [5_000, 47_500, 90_000]);
        assert!(!lyrics.synced, "spreading keeps synced = false");
    }

    #[tokio::test]
    async fn unsynced_lyrics_stay_at_zero_without_a_duration() {
        for duration in [None, Some(0)] {
            let mut t = track();
            t.duration_ms = duration;
            let chain = chain(vec![FakeProvider::new(
                "plain",
                Reply::Found(plain(&["a", "b"])),
            )]);
            let lyrics = found(chain.resolve(&t).await);
            assert_eq!(starts(&lyrics), [0, 0], "duration {duration:?}");
        }
    }

    #[tokio::test]
    async fn synced_lyrics_are_not_spread() {
        let chain = chain(vec![FakeProvider::new("s", Reply::Found(synced("x")))]);
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(starts(&lyrics), [1_000, 4_000]);
    }

    // ---- normalization ----

    #[tokio::test]
    async fn the_track_is_normalized_before_any_provider_sees_it() {
        let provider = FakeProvider::new("p", Reply::Nothing);
        let calls = provider.calls();
        let chain = chain(vec![provider]);
        let raw = Track {
            title: "Never Gonna Give You Up (Remastered 2011)".into(),
            artist: "RickAstleyVEVO".into(),
            album: Some("Whenever You Need Somebody".into()),
            duration_ms: Some(213_000),
            spotify_id: Some("4PTG3Z6ehGkBFwjybzWkR8".into()),
        };

        chain.resolve(&raw).await;

        let seen = calls.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0], crate::matcher::normalize_track(&raw));
        assert_eq!(seen[0].title, "Never Gonna Give You Up");
        assert_eq!(seen[0].artist, "RickAstley");
        assert_eq!(seen[0].album, raw.album);
        assert_eq!(seen[0].duration_ms, raw.duration_ms);
        assert_eq!(seen[0].spotify_id, raw.spotify_id);
    }

    #[tokio::test]
    async fn video_titles_are_split_into_artist_and_title() {
        let provider = FakeProvider::new("p", Reply::Nothing);
        let calls = provider.calls();
        let chain = chain(vec![provider]);
        let raw = Track {
            title: "Rick Astley - Never Gonna Give You Up (Official Video)".into(),
            artist: "YouTube".into(),
            ..Track::default()
        };

        chain.resolve(&raw).await;

        let seen = calls.lock().unwrap().clone();
        assert_eq!(seen[0].artist, "Rick Astley");
        assert_eq!(seen[0].title, "Never Gonna Give You Up");
    }

    // ---- errors and health ----

    #[tokio::test]
    async fn a_provider_error_never_fails_the_lookup() {
        let first = FakeProvider::new("broken", Reply::Fail("connection refused"));
        let second = FakeProvider::new("works", Reply::Found(synced("Hello")));
        let chain = chain(vec![first, second]);

        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.source, "works");

        let all_broken = self::chain(vec![
            FakeProvider::new("a", Reply::Fail("boom")),
            FakeProvider::new("b", Reply::Fail("boom")),
        ]);
        assert_eq!(all_broken.resolve(&track()).await, Resolved::NotFound);
    }

    #[tokio::test]
    async fn errors_are_counted_and_the_provider_is_skipped_at_the_limit() {
        let clock = TestClock::new();
        let broken = FakeProvider::new("broken", Reply::Fail("timeout"));
        let other = FakeProvider::new("other", Reply::Nothing);
        let (broken_calls, other_calls) = (broken.calls(), other.calls());
        let chain = chain(vec![broken, other]).with_clock(clock.source());

        assert_eq!(chain.health(), [("broken", false), ("other", false)]);
        for round in 1..MAX_CONSECUTIVE_ERRORS {
            chain.resolve(&track()).await;
            assert_eq!(
                chain.health(),
                [("broken", false), ("other", false)],
                "after {round} errors"
            );
        }
        chain.resolve(&track()).await;
        assert_eq!(chain.health(), [("broken", true), ("other", false)]);
        assert_eq!(call_count(&broken_calls), MAX_CONSECUTIVE_ERRORS as usize);

        // Skipped: not asked any more, the others still are.
        chain.resolve(&track()).await;
        chain.resolve(&track()).await;
        assert_eq!(call_count(&broken_calls), MAX_CONSECUTIVE_ERRORS as usize);
        assert_eq!(
            call_count(&other_calls),
            MAX_CONSECUTIVE_ERRORS as usize + 2
        );
    }

    #[tokio::test]
    async fn a_skipped_provider_is_tried_again_after_skip_for() {
        let clock = TestClock::new();
        let broken = FakeProvider::new("flaky", Reply::Found(synced("Back")))
            .script(vec![Reply::Fail("down"); MAX_CONSECUTIVE_ERRORS as usize]);
        let calls = broken.calls();
        let chain = chain(vec![broken]).with_clock(clock.source());

        for _ in 0..MAX_CONSECUTIVE_ERRORS {
            assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        }
        assert_eq!(chain.health(), [("flaky", true)]);

        clock.advance(SKIP_FOR - Duration::from_secs(1));
        assert_eq!(chain.health(), [("flaky", true)]);
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        assert_eq!(call_count(&calls), MAX_CONSECUTIVE_ERRORS as usize);

        clock.advance(Duration::from_secs(1));
        assert_eq!(chain.health(), [("flaky", false)]);
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.source, "flaky");
        assert_eq!(call_count(&calls), MAX_CONSECUTIVE_ERRORS as usize + 1);
        assert_eq!(chain.health(), [("flaky", false)]);
    }

    #[tokio::test]
    async fn a_retried_provider_that_fails_again_is_skipped_again() {
        let clock = TestClock::new();
        let broken = FakeProvider::new("broken", Reply::Fail("still down"));
        let calls = broken.calls();
        let chain = chain(vec![broken]).with_clock(clock.source());

        for _ in 0..MAX_CONSECUTIVE_ERRORS {
            chain.resolve(&track()).await;
        }
        clock.advance(SKIP_FOR);
        chain.resolve(&track()).await;
        assert_eq!(call_count(&calls), MAX_CONSECUTIVE_ERRORS as usize + 1);
        assert_eq!(chain.health(), [("broken", true)]);

        chain.resolve(&track()).await;
        assert_eq!(call_count(&calls), MAX_CONSECUTIVE_ERRORS as usize + 1);
    }

    #[tokio::test]
    async fn a_success_resets_the_error_count() {
        let flaky = FakeProvider::new("flaky", Reply::Nothing).script(vec![
            Reply::Fail("1"),
            Reply::Fail("2"),
            Reply::Nothing,
            Reply::Fail("1"),
            Reply::Fail("2"),
            Reply::Found(synced("ok")),
            Reply::Fail("1"),
            Reply::Fail("2"),
            Reply::Fail("3"),
        ]);
        let calls = flaky.calls();
        let chain = chain(vec![flaky]);

        for _ in 0..8 {
            chain.resolve(&track()).await;
            assert_eq!(chain.health(), [("flaky", false)]);
        }
        chain.resolve(&track()).await;
        assert_eq!(chain.health(), [("flaky", true)]);
        assert_eq!(call_count(&calls), 9);
    }

    #[tokio::test]
    async fn health_lists_providers_in_order() {
        let chain = chain(vec![
            FakeProvider::new("local", Reply::Nothing),
            FakeProvider::new("lrclib", Reply::Nothing),
        ]);
        assert_eq!(chain.health(), [("local", false), ("lrclib", false)]);
    }

    // ---- concurrency ----

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resolve_can_run_from_many_tasks_at_once() {
        let slow = FakeProvider::new("slow", Reply::Fail("down")).delayed(Duration::from_millis(5));
        let fast = FakeProvider::new("fast", Reply::Found(synced("Hi")));
        let (slow_calls, fast_calls) = (slow.calls(), fast.calls());
        let chain = Arc::new(chain(vec![slow, fast]));

        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let chain = Arc::clone(&chain);
                tokio::spawn(async move { chain.resolve(&track_without_duration()).await })
            })
            .collect();
        for task in tasks {
            let lyrics = found(task.await.unwrap());
            assert_eq!(lyrics.source, "fast");
        }

        // Every lookup reached the fast provider; the failing one was asked
        // until its failures got it skipped (lookups already past the check
        // when that happened still asked it).
        assert_eq!(call_count(&fast_calls), 16);
        let slow_count = call_count(&slow_calls);
        assert!(
            slow_count >= MAX_CONSECUTIVE_ERRORS as usize && slow_count <= 16,
            "slow provider called {slow_count} times"
        );
        assert_eq!(chain.health(), [("slow", true), ("fast", false)]);
    }

    #[test]
    fn health_skip_needs_both_the_count_and_the_deadline() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        assert!(!Health::default().is_skipped(now));
        let counted_only = Health {
            consecutive_errors: MAX_CONSECUTIVE_ERRORS,
            skipped_until: None,
        };
        assert!(!counted_only.is_skipped(now));
        let skipped = Health {
            consecutive_errors: MAX_CONSECUTIVE_ERRORS,
            skipped_until: Some(later),
        };
        assert!(skipped.is_skipped(now));
        assert!(
            !skipped.is_skipped(later),
            "the deadline itself ends the skip"
        );
        let below = Health {
            consecutive_errors: MAX_CONSECUTIVE_ERRORS - 1,
            skipped_until: Some(later),
        };
        assert!(!below.is_skipped(now));
    }

    #[test]
    fn unix_now_is_after_2020() {
        assert!(unix_now_secs() > 1_577_836_800);
    }

    // ---- your own files and the cache ----

    fn chain_with_cache(providers: Vec<FakeProvider>, dir: &std::path::Path) -> ProviderChain {
        let boxed = providers
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn LyricsProvider>)
            .collect();
        ProviderChain::new(boxed, Some(cache::LyricsCache::new(dir.to_path_buf())))
    }

    #[tokio::test]
    async fn your_own_file_beats_lyrics_cached_earlier_and_is_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let normalized = crate::matcher::normalize_track(&track());

        // Looked up online first: the cache now holds LRCLIB's lyrics.
        let remote = chain_with_cache(
            vec![FakeProvider::new("lrclib", Reply::Found(synced("online")))],
            dir.path(),
        );
        assert_eq!(
            found(remote.resolve(&track()).await).lines[0].text,
            "online"
        );

        // Then the user adds a file: it wins, and LRCLIB is not asked.
        let local = FakeProvider::new("local", Reply::Found(synced("mine"))).local();
        let lrclib = FakeProvider::new("lrclib", Reply::Found(synced("online")));
        let lrclib_calls = lrclib.calls();
        let chain = chain_with_cache(vec![local, lrclib], dir.path());
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.lines[0].text, "mine");
        assert_eq!(lyrics.source, "local");
        assert!(lrclib_calls.lock().unwrap().is_empty());

        // The cache still holds the online lyrics, not the file's.
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        let entry = cache.get(&normalized).await.unwrap();
        assert_eq!(entry.lyrics.unwrap().lines[0].text, "online");
    }

    #[tokio::test]
    async fn a_plain_file_beats_a_cached_not_found_and_plain_online_lyrics() {
        let dir = tempfile::tempdir().unwrap();
        let mine = plain(&["my words", "more of mine"]);

        // Nothing online: the plain file is used, and "not found" is cached
        // for the online providers.
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Found(mine.clone())).local(),
                FakeProvider::new("lrclib", Reply::Nothing),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "local");
        let normalized = crate::matcher::normalize_track(&track());
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        assert_eq!(cache.get(&normalized).await.unwrap().lyrics, None);

        // The cached "not found" does not hide the file next time.
        let lrclib = FakeProvider::new("lrclib", Reply::Nothing);
        let lrclib_calls = lrclib.calls();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Found(mine.clone())).local(),
                lrclib,
            ],
            dir.path(),
        );
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.source, "local");
        assert!(lyrics.lines.iter().any(|l| l.start_ms > 0), "spread");
        assert!(
            lrclib_calls.lock().unwrap().is_empty(),
            "answered by the cache"
        );

        // Plain lyrics online: the file is still the earlier, preferred one.
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Found(mine)).local(),
                FakeProvider::new("lrclib", Reply::Found(plain(&["online words"]))),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "local");
    }

    #[tokio::test]
    async fn synced_lyrics_online_beat_a_plain_file() {
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Found(plain(&["my words"]))).local(),
                FakeProvider::new("lrclib", Reply::Found(synced("online"))),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "lrclib");
        // And from the cache next time.
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Found(plain(&["my words"]))).local(),
                FakeProvider::new("lrclib", Reply::Fail("offline")),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "lrclib");
    }

    #[tokio::test]
    async fn a_failing_folder_does_not_stop_caching_the_online_answer() {
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("local", Reply::Fail("permission denied")).local(),
                FakeProvider::new("lrclib", Reply::Nothing),
            ],
            dir.path(),
        );
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        let normalized = crate::matcher::normalize_track(&track());
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        assert!(cache.get(&normalized).await.is_some());
    }

    #[tokio::test]
    async fn turning_a_provider_on_looks_cached_songs_up_again() {
        let dir = tempfile::tempdir().unwrap();
        let normalized = crate::matcher::normalize_track(&track());
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());

        // LRCLIB alone has nothing: "not found" is cached for it.
        let chain = chain_with_cache(
            vec![FakeProvider::new("lrclib", Reply::Nothing)],
            dir.path(),
        );
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        let entry = cache.get(&normalized).await.unwrap();
        assert_eq!(entry.providers, vec!["lrclib"]);

        // NetEase is turned on: the song is looked up again and found.
        let lrclib = FakeProvider::new("lrclib", Reply::Nothing);
        let lrclib_calls = lrclib.calls();
        let netease = FakeProvider::new("netease", Reply::Found(synced("from netease")));
        let chain = chain_with_cache(vec![lrclib, netease], dir.path());
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(lyrics.source, "netease");
        assert_eq!(call_count(&lrclib_calls), 1);

        // Next time it comes from the cache.
        let lrclib = FakeProvider::new("lrclib", Reply::Nothing);
        let lrclib_calls = lrclib.calls();
        let netease = FakeProvider::new("netease", Reply::Fail("offline"));
        let netease_calls = netease.calls();
        let chain = chain_with_cache(vec![lrclib, netease], dir.path());
        assert_eq!(found(chain.resolve(&track()).await).source, "netease");
        assert_eq!(call_count(&lrclib_calls), 0);
        assert_eq!(call_count(&netease_calls), 0);
    }

    #[tokio::test]
    async fn plain_lyrics_cached_before_are_looked_up_again_for_synced_ones() {
        let dir = tempfile::tempdir().unwrap();
        // An entry from before providers were recorded.
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        let normalized = crate::matcher::normalize_track(&track());
        let old = cache::CacheEntry {
            lyrics: Some(Lyrics {
                source: "lrclib".into(),
                ..plain(&["old words"])
            }),
            fetched_at: unix_now_secs(),
            duration_ms: normalized.duration_ms,
            providers: Vec::new(),
        };
        cache.put(&normalized, &old).await.unwrap();

        let chain = chain_with_cache(
            vec![
                FakeProvider::new("lrclib", Reply::Found(plain(&["old words"]))),
                FakeProvider::new("kugou", Reply::Found(synced("timed words"))),
            ],
            dir.path(),
        );
        let lyrics = found(chain.resolve(&track()).await);
        assert!(lyrics.synced);
        assert_eq!(lyrics.source, "kugou");
        let entry = cache.get(&normalized).await.unwrap();
        assert_eq!(entry.lyrics.unwrap().source, "kugou");
    }

    #[tokio::test]
    async fn cached_lyrics_are_used_when_the_new_lookup_fails() {
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![FakeProvider::new(
                "lrclib",
                Reply::Found(plain(&["cached words"])),
            )],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "lrclib");

        // NetEase is turned on while offline: the cached lyrics still show,
        // spread over the song, and stay cached for the next try.
        let netease = FakeProvider::new("netease", Reply::Fail("offline"));
        let netease_calls = netease.calls();
        let chain = chain_with_cache(
            vec![FakeProvider::new("lrclib", Reply::Fail("offline")), netease],
            dir.path(),
        );
        let lyrics = found(chain.resolve(&track()).await);
        assert_eq!(texts(&lyrics), vec!["cached words"]);
        assert_eq!(lyrics.source, "lrclib");
        assert!(!lyrics.synced);
        assert_eq!(call_count(&netease_calls), 1);
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        let normalized = crate::matcher::normalize_track(&track());
        assert_eq!(
            cache.get(&normalized).await.unwrap().providers,
            vec!["lrclib"]
        );

        // When every provider answers and none has lyrics, that is the answer.
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("lrclib", Reply::Nothing),
                FakeProvider::new("netease", Reply::Nothing),
            ],
            dir.path(),
        );
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
    }

    #[tokio::test]
    async fn turning_a_provider_off_keeps_what_it_found() {
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("lrclib", Reply::Nothing),
                FakeProvider::new("netease", Reply::Found(plain(&["from netease"]))),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "netease");

        let lrclib = FakeProvider::new("lrclib", Reply::Fail("should not be asked"));
        let lrclib_calls = lrclib.calls();
        let chain = chain_with_cache(vec![lrclib], dir.path());
        assert_eq!(found(chain.resolve(&track()).await).source, "netease");
        assert_eq!(call_count(&lrclib_calls), 0);
    }

    #[tokio::test]
    async fn lyrics_that_may_not_be_kept_are_looked_up_each_time() {
        let normalized = crate::matcher::normalize_track(&track());
        for lyrics in [synced("licensed"), plain(&["licensed"])] {
            let dir = tempfile::tempdir().unwrap();
            let musixmatch =
                FakeProvider::new("musixmatch", Reply::Found(lyrics.clone())).not_cached();
            let calls = musixmatch.calls();
            let chain = chain_with_cache(
                vec![
                    FakeProvider::new("lrclib", Reply::Nothing),
                    musixmatch,
                    FakeProvider::new("netease", Reply::Nothing),
                ],
                dir.path(),
            );
            for _ in 0..2 {
                let found = found(chain.resolve(&track()).await);
                assert_eq!(found.source, "musixmatch");
                assert_eq!(found.lines[0].text, "licensed");
            }
            assert_eq!(call_count(&calls), 2);
            let cache = cache::LyricsCache::new(dir.path().to_path_buf());
            assert!(cache.get(&normalized).await.is_none());
        }

        // What other providers find, and "not found", are still kept.
        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![
                FakeProvider::new("musixmatch", Reply::Found(plain(&["licensed"]))).not_cached(),
                FakeProvider::new("netease", Reply::Found(synced("from netease"))),
            ],
            dir.path(),
        );
        assert_eq!(found(chain.resolve(&track()).await).source, "netease");
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        let entry = cache.get(&normalized).await.unwrap();
        assert_eq!(entry.lyrics.unwrap().source, "netease");

        let dir = tempfile::tempdir().unwrap();
        let chain = chain_with_cache(
            vec![FakeProvider::new("musixmatch", Reply::Nothing).not_cached()],
            dir.path(),
        );
        assert_eq!(chain.resolve(&track()).await, Resolved::NotFound);
        let cache = cache::LyricsCache::new(dir.path().to_path_buf());
        let entry = cache.get(&normalized).await.unwrap();
        assert_eq!(entry.lyrics, None);
        assert_eq!(entry.providers, vec!["musixmatch".to_string()]);
    }
}
