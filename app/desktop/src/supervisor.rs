//! Runs the Lyrix engine with the saved settings, restarts it when they
//! change, and keeps the window's [`View`] up to date.
//!
//! The engine publishes its own view; while it runs, every change is copied
//! into the window's view with `running = true`. When the settings cannot be
//! read or have errors, or the music source cannot start, no engine runs and
//! the view's `error` says why in plain words. Nothing here ever ends the app.

use crate::actions::{self, OffsetChange};
use crate::paths::Paths;
use crate::view::View;
use anyhow::Context;
use lyrix::config::{Config, Secret, Severity, BAN_WARNING};
use lyrix::engine::Engine;
use lyrix::providers::cache::LyricsCache;
use lyrix::providers::kugou::KugouProvider;
use lyrix::providers::local::LocalLrcProvider;
use lyrix::providers::lrclib::LrclibProvider;
use lyrix::providers::musixmatch::MusixmatchProvider;
use lyrix::providers::netease::NeteaseProvider;
use lyrix::providers::{LyricsProvider, ProviderChain};
use lyrix::targets::discord_rpc::DiscordRpcTarget;
use lyrix::targets::StatusTarget;
use lyrix::view::EngineView;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{oneshot, watch, Mutex};
use tokio::task::JoinHandle;

/// Longest wait for the engine to stop. It clears every status first, which
/// takes at most 2 s per target (Discord is the only one).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Owns the engine that runs, if any, and the window's view.
pub struct Supervisor {
    paths: Paths,
    views: watch::Sender<View>,
    /// The engine running now. Locked for a whole stop or restart, so they
    /// happen one at a time.
    engine: Mutex<Option<Running>>,
}

/// An engine task and the way to stop it.
struct Running {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Supervisor {
    /// No engine runs until [`Supervisor::restart`].
    pub fn new(paths: Paths) -> Self {
        let paused = actions::is_paused(&paths.pause_marker);
        let (views, _) = watch::channel(View::stopped(None, paused));
        Self {
            paths,
            views,
            engine: Mutex::new(None),
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The view now.
    pub fn view(&self) -> View {
        self.views.borrow().clone()
    }

    /// Every change of the view from now on.
    pub fn subscribe(&self) -> watch::Receiver<View> {
        self.views.subscribe()
    }

    /// Stops the engine that runs, if any (clearing its statuses), then starts
    /// one with the saved settings. When that is not possible the view says
    /// why. Call it inside a Tokio runtime.
    pub async fn restart(&self) {
        let mut engine = self.engine.lock().await;
        if let Some(running) = engine.take() {
            self.stop_running(running).await;
        }
        *engine = match prepare(&self.paths) {
            Ok(prepared) => Some(self.launch(prepared)),
            Err(message) => {
                tracing::error!("{message}");
                let paused = actions::is_paused(&self.paths.pause_marker);
                self.views
                    .send_replace(View::stopped(Some(message), paused));
                None
            }
        };
    }

    /// Stops the engine, which clears every status first. Waits at most
    /// [`STOP_TIMEOUT`]; an engine still busy then is cut short.
    pub async fn stop(&self) {
        let mut engine = self.engine.lock().await;
        if let Some(running) = engine.take() {
            self.stop_running(running).await;
        }
    }

    /// Runs `engine` (given without a view sender) on a new task.
    fn launch(&self, engine: Engine) -> Running {
        let (engine_tx, engine_rx) = watch::channel(EngineView::default());
        let (stop_tx, stop_rx) = oneshot::channel();
        self.views.send_modify(|view| {
            view.running = true;
            view.error = None;
        });
        let task = tokio::spawn(run_engine(
            engine.with_view(engine_tx),
            stop_rx,
            engine_rx,
            self.views.clone(),
            self.paths.pause_marker.clone(),
        ));
        Running {
            stop: stop_tx,
            task,
        }
    }

    async fn stop_running(&self, running: Running) {
        let Running { stop, mut task } = running;
        // Already gone when the engine stopped by itself.
        let _ = stop.send(());
        if tokio::time::timeout(STOP_TIMEOUT, &mut task).await.is_err() {
            tracing::warn!(
                "the engine did not stop within {} s, so it was cut short",
                STOP_TIMEOUT.as_secs()
            );
            task.abort();
            let _ = task.await;
        }
        let paused = actions::is_paused(&self.paths.pause_marker);
        self.views.send_replace(View::stopped(None, paused));
    }

    /// Pauses or resumes sharing through the pause marker, which a running
    /// engine notices on its next poll. Without an engine the view shows the
    /// new state at once. Returns whether sharing is paused now.
    pub fn set_paused(&self, paused: bool) -> anyhow::Result<bool> {
        let now = actions::set_paused(&self.paths.pause_marker, paused)?;
        self.views.send_if_modified(|view| {
            let changed = !view.running && view.engine.paused != now;
            if changed {
                view.engine.paused = now;
            }
            changed
        });
        Ok(now)
    }

    /// Changes the offset of the song playing now (the one in the view) and
    /// returns its new value. The running engine re-reads the offsets file.
    pub fn change_offset(&self, change: OffsetChange) -> Result<i64, String> {
        let key = self
            .views
            .borrow()
            .engine
            .now
            .as_ref()
            .map(|now| now.song_key.clone());
        let Some(key) = key else {
            return Err("Nothing is playing, so there is no song to adjust.".to_string());
        };
        if key.trim().is_empty() {
            return Err(
                "The player does not say which song is playing, so there is nothing to adjust."
                    .to_string(),
            );
        }
        actions::change_offset(&self.paths.offsets, &key, change)
            .map_err(|e| actions::error_sentence(&e))
    }
}

/// Runs the engine until `stop` fires (or its sender is dropped), copying
/// its views into `views`.
async fn run_engine(
    engine: Engine,
    stop: oneshot::Receiver<()>,
    engine_views: watch::Receiver<EngineView>,
    views: watch::Sender<View>,
    pause_marker: PathBuf,
) {
    let shutdown = async move {
        let _ = stop.await;
    };
    let (result, ()) = tokio::join!(engine.run(shutdown), copy_views(engine_views, &views));
    match result {
        Ok(()) => tracing::info!("stopped"),
        Err(e) => {
            tracing::error!("the engine stopped: {e:#}");
            let message = format!(
                "Lyrix stopped because of an error: {}.",
                actions::error_reason(&e)
            );
            let paused = actions::is_paused(&pause_marker);
            views.send_replace(View::stopped(Some(message), paused));
        }
    }
}

/// Copies every view the engine publishes into the window's view, until
/// the engine is gone.
async fn copy_views(mut engine_views: watch::Receiver<EngineView>, views: &watch::Sender<View>) {
    while engine_views.changed().await.is_ok() {
        let engine = engine_views.borrow_and_update().clone();
        views.send_modify(|view| view.engine = engine);
    }
}

/// The engine for the saved settings (without a view yet), or why there can
/// be none, in plain words.
pub fn prepare(paths: &Paths) -> Result<Engine, String> {
    let config = Config::load(&paths.config).map_err(|e| {
        format!(
            "Lyrix could not read its settings: {}. Fix the file, or save the settings \
             here to replace it.",
            actions::error_reason(&e)
        )
    })?;
    if let Some(issue) = config
        .validate()
        .into_iter()
        .find(|issue| issue.severity == Severity::Error)
    {
        return Err(format!(
            "Lyrix is not running because of a problem in the settings: {} Change it in the \
             settings and save.",
            issue.message
        ));
    }
    for name in config.advanced_requested() {
        if config.advanced.accept_ban_risk {
            tracing::warn!(
                "{BAN_WARNING} advanced.{name} is not included in this build yet, so it stays off."
            );
        } else {
            tracing::warn!(
                "{BAN_WARNING} advanced.{name} stays off until advanced.accept_ban_risk is on."
            );
        }
    }

    let source = lyrix::sources::default_source(&config.sources, &config.privacy.blocked_apps)
        .map_err(|e| {
            format!(
                "Lyrix cannot read what is playing on this computer: {}.",
                actions::error_reason(&e)
            )
        })?;
    let lyrics = LyricsPlan::from_config(&config, paths.cache_dir.clone());
    let chain = lyrics.build().map_err(|e| {
        format!(
            "Lyrix could not set up the lyrics search: {}.",
            actions::error_reason(&e)
        )
    })?;
    let targets = targets(&config);
    tracing::info!(
        "starting: reading {}; lyrics from {}; showing on {}",
        source.name(),
        lyrics.describe(),
        if targets.is_empty() {
            "the window only"
        } else {
            "Discord and the window"
        }
    );

    Ok(Engine::new(config, source, Arc::new(chain), targets)
        .with_offsets_path(paths.offsets.clone())
        .with_pause_marker(paths.pause_marker.clone()))
}

/// The Discord application to show as, when Discord Rich Presence is on
/// (`discord.enabled` with a `client_id` that is not blank).
pub fn discord_client_id(config: &Config) -> Option<String> {
    let client_id = config.discord.client_id.trim();
    (config.discord.enabled && !client_id.is_empty()).then(|| client_id.to_string())
}

/// The status targets: Discord Rich Presence when it is on, otherwise none
/// (the window still follows the song). The app has no terminal, so the
/// console target is never used.
fn targets(config: &Config) -> Vec<Box<dyn StatusTarget>> {
    match discord_client_id(config) {
        Some(client_id) => vec![Box::new(
            DiscordRpcTarget::new(
                client_id,
                Duration::from_millis(config.discord.min_interval_ms),
                config.discord.show_progress,
            )
            .with_large_image(config.discord.large_image.clone()),
        )],
        None => Vec::new(),
    }
}

/// Where lyrics are looked up, in order, decided from the settings alone.
/// The same as the `lyrix` command's: your own files first, then LRCLIB,
/// Musixmatch (with your key), NetEase and Kugou when each is on, with the
/// cache when it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsPlan {
    /// Your own `.lrc` / `.txt` files.
    pub local_dir: PathBuf,
    /// LRCLIB server, without a trailing slash, when LRCLIB is on.
    pub lrclib_url: Option<String>,
    /// Your own Musixmatch API key, when one is set: Musixmatch after LRCLIB.
    pub musixmatch_key: Option<Secret>,
    /// NetEase Cloud Music, after Musixmatch.
    pub netease: bool,
    /// Kugou, after NetEase.
    pub kugou: bool,
    /// Cache folder, when the cache is on.
    pub cache_dir: Option<PathBuf>,
}

impl LyricsPlan {
    pub fn from_config(config: &Config, cache_dir: PathBuf) -> Self {
        let lrclib_url = config.lyrics.lrclib.then(|| {
            config
                .lyrics
                .lrclib_url
                .trim()
                .trim_end_matches('/')
                .to_string()
        });
        Self {
            local_dir: config.lyrics_dir(),
            lrclib_url,
            musixmatch_key: config.lyrics.musixmatch_key.trimmed().map(Secret::new),
            netease: config.lyrics.netease,
            kugou: config.lyrics.kugou,
            cache_dir: config.lyrics.cache.then_some(cache_dir),
        }
    }

    /// `local, lrclib (<url>), musixmatch, netease, kugou, cache`, for the
    /// log, never with the Musixmatch key.
    pub fn describe(&self) -> String {
        let mut parts = vec!["local".to_string()];
        if let Some(url) = &self.lrclib_url {
            parts.push(format!("lrclib ({url})"));
        }
        if self.musixmatch_key.is_some() {
            parts.push("musixmatch".to_string());
        }
        if self.netease {
            parts.push("netease".to_string());
        }
        if self.kugou {
            parts.push("kugou".to_string());
        }
        if self.cache_dir.is_some() {
            parts.push("cache".to_string());
        }
        parts.join(", ")
    }

    pub fn build(&self) -> anyhow::Result<ProviderChain> {
        let mut providers: Vec<Box<dyn LyricsProvider>> =
            vec![Box::new(LocalLrcProvider::new(self.local_dir.clone()))];
        if let Some(url) = &self.lrclib_url {
            let lrclib = LrclibProvider::new(url.clone())
                .with_context(|| format!("could not set up LRCLIB at {url}"))?;
            providers.push(Box::new(lrclib));
        }
        if let Some(key) = &self.musixmatch_key {
            providers.push(Box::new(MusixmatchProvider::new(key.expose())?));
        }
        if self.netease {
            providers.push(Box::new(NeteaseProvider::new()?));
        }
        if self.kugou {
            providers.push(Box::new(KugouProvider::new()?));
        }
        let cache = self.cache_dir.clone().map(LyricsCache::new);
        Ok(ProviderChain::new(providers, cache))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use lyrix::sources::NowPlayingSource;
    use lyrix::{PlaybackSnapshot, PlaybackStatus, Track};
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    fn paths(root: &Path) -> Paths {
        Paths {
            config: root.join("config.toml"),
            offsets: root.join("offsets.toml"),
            pause_marker: root.join("paused"),
            cache_dir: root.join("cache"),
            logs_dir: root.join("logs"),
        }
    }

    /// A player that plays whatever the test says.
    struct FakeSource {
        playing: Arc<StdMutex<Option<Track>>>,
    }

    #[async_trait]
    impl NowPlayingSource for FakeSource {
        fn name(&self) -> &'static str {
            "fake"
        }

        async fn snapshot(&self) -> anyhow::Result<Option<PlaybackSnapshot>> {
            let track = self.playing.lock().unwrap().clone();
            Ok(track.map(|track| PlaybackSnapshot {
                track,
                status: PlaybackStatus::Playing,
                position_ms: 1_000,
                position_at: Instant::now(),
                rate: 1.0,
                app_id: "fake.player".into(),
            }))
        }
    }

    /// An engine with the fake player, no lyrics and no targets.
    fn fake_engine(paths: &Paths, playing: Arc<StdMutex<Option<Track>>>) -> Engine {
        let mut config = Config::default();
        config.general.poll_interval_ms = 100;
        Engine::new(
            config,
            Box::new(FakeSource { playing }),
            Arc::new(ProviderChain::new(Vec::new(), None)),
            Vec::new(),
        )
        .with_offsets_path(paths.offsets.clone())
        .with_pause_marker(paths.pause_marker.clone())
    }

    fn song() -> Track {
        Track {
            title: "Never Gonna Give You Up".into(),
            artist: "Rick Astley".into(),
            album: None,
            duration_ms: Some(213_000),
            spotify_id: None,
        }
    }

    /// Waits (at most 5 s) until the view passes `check`.
    async fn wait_for(supervisor: &Supervisor, check: impl Fn(&View) -> bool) -> View {
        let mut views = supervisor.subscribe();
        let wait = views.wait_for(|view| check(view));
        let view = tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .expect("the view never got there")
            .expect("the view is gone")
            .clone();
        view
    }

    #[test]
    fn discord_is_used_only_when_enabled_with_an_id() {
        let mut config = Config::default();
        assert_eq!(
            discord_client_id(&config).as_deref(),
            Some(lyrix::config::DEFAULT_DISCORD_CLIENT_ID)
        );
        config.discord.client_id = "  123  ".into();
        assert_eq!(discord_client_id(&config).as_deref(), Some("123"));
        config.discord.client_id = " ".into();
        assert_eq!(discord_client_id(&config), None);
        config.discord.client_id = "123".into();
        config.discord.enabled = false;
        assert_eq!(discord_client_id(&config), None);
        assert!(targets(&config).is_empty());
        // The console setting never adds a target to the app.
        config.console.enabled = true;
        assert!(targets(&config).is_empty());
    }

    #[test]
    fn the_target_is_discord() {
        let names: Vec<&str> = targets(&Config::default())
            .iter()
            .map(|target| target.name())
            .collect();
        assert_eq!(names, vec!["discord"]);
    }

    #[test]
    fn the_lyrics_plan_follows_the_settings_like_the_cli() {
        let cache = PathBuf::from("/cache");
        let mut config = Config::default();
        config.lyrics.lrclib_url = " https://lrclib.example/// ".into();
        config.lyrics.lyrics_dir = Some(PathBuf::from("/mine"));
        let plan = LyricsPlan::from_config(&config, cache.clone());
        assert_eq!(
            plan,
            LyricsPlan {
                local_dir: PathBuf::from("/mine"),
                lrclib_url: Some("https://lrclib.example".into()),
                musixmatch_key: None,
                netease: true,
                kugou: true,
                cache_dir: Some(cache.clone()),
            }
        );
        assert_eq!(
            plan.describe(),
            "local, lrclib (https://lrclib.example), netease, kugou, cache"
        );
        let chain = plan.build().unwrap();
        let names: Vec<&str> = chain.health().into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, vec!["local", "lrclib", "netease", "kugou"]);

        let key = "0123456789abcdef0123456789abcdef";
        config.lyrics.musixmatch_key = Secret::new(format!("{key}\n"));
        let plan = LyricsPlan::from_config(&config, cache.clone());
        assert_eq!(plan.musixmatch_key, Some(Secret::new(key)));
        assert_eq!(
            plan.describe(),
            "local, lrclib (https://lrclib.example), musixmatch, netease, kugou, cache"
        );
        assert!(!format!("{plan:?}").contains(key));
        let chain = plan.build().unwrap();
        let names: Vec<&str> = chain.health().into_iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            vec!["local", "lrclib", "musixmatch", "netease", "kugou"]
        );

        config.lyrics.musixmatch_key = Secret::new(" ");
        config.lyrics.netease = false;
        let plan = LyricsPlan::from_config(&config, cache.clone());
        assert_eq!(plan.musixmatch_key, None);
        assert_eq!(
            plan.describe(),
            "local, lrclib (https://lrclib.example), kugou, cache"
        );

        config.lyrics.lrclib = false;
        config.lyrics.kugou = false;
        config.lyrics.cache = false;
        let plan = LyricsPlan::from_config(&config, cache);
        assert_eq!(plan.lrclib_url, None);
        assert!(!plan.netease);
        assert!(!plan.kugou);
        assert_eq!(plan.cache_dir, None);
        assert_eq!(plan.describe(), "local");
        let chain = plan.build().unwrap();
        let names: Vec<&str> = chain.health().into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, vec!["local"]);
    }

    #[test]
    fn unreadable_settings_explain_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.config, "general = [[ nope").unwrap();
        let message = prepare(&paths).err().expect("no engine");
        assert!(message.starts_with("Lyrix could not read its settings"));
        assert!(message.contains("config.toml"), "{message}");
    }

    #[test]
    fn settings_with_errors_explain_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let mut config = Config::default();
        config.general.poll_interval_ms = 20;
        config.save(&paths.config).unwrap();
        let message = prepare(&paths).err().expect("no engine");
        assert!(message.contains("poll_interval_ms is 20 ms"), "{message}");
    }

    #[test]
    fn missing_or_warned_settings_still_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        assert!(prepare(&paths).is_ok());
        assert!(!paths.config.exists(), "the defaults are not written");

        let mut config = Config::default();
        config.discord.client_id = String::new();
        config.advanced.discord_custom_status = true;
        config.save(&paths.config).unwrap();
        assert!(prepare(&paths).is_ok());
    }

    #[tokio::test]
    async fn a_failed_start_is_in_the_view_and_a_fixed_one_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.config, "general = [[ nope").unwrap();
        let supervisor = Supervisor::new(paths.clone());

        supervisor.restart().await;
        let view = supervisor.view();
        assert!(!view.running);
        assert!(view
            .error
            .as_deref()
            .is_some_and(|e| e.contains("could not read its settings")));

        // Stopping with nothing running is fine.
        supervisor.stop().await;
        assert!(!supervisor.view().running);
    }

    #[tokio::test]
    async fn the_engine_view_is_copied_and_cleared_on_stop() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let supervisor = Supervisor::new(paths.clone());
        let playing = Arc::new(StdMutex::new(Some(song())));

        let running = supervisor.launch(fake_engine(&paths, playing.clone()));
        *supervisor.engine.lock().await = Some(running);
        assert!(supervisor.view().running);

        let view = wait_for(&supervisor, |view| view.engine.now.is_some()).await;
        assert!(view.running);
        assert_eq!(view.error, None);
        assert_eq!(view.engine.source, "fake");
        let now = view.engine.now.unwrap();
        assert_eq!(now.title, "Never Gonna Give You Up");
        assert_eq!(now.app, "fake.player");
        assert!(view.engine.targets.is_empty());

        supervisor.stop().await;
        let view = supervisor.view();
        assert_eq!(view, View::stopped(None, false));
    }

    #[tokio::test]
    async fn offsets_follow_the_song_in_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let supervisor = Supervisor::new(paths.clone());
        assert_eq!(
            supervisor.change_offset(OffsetChange::By(100)),
            Err("Nothing is playing, so there is no song to adjust.".to_string())
        );

        let playing = Arc::new(StdMutex::new(Some(song())));
        let running = supervisor.launch(fake_engine(&paths, playing.clone()));
        *supervisor.engine.lock().await = Some(running);
        let view = wait_for(&supervisor, |view| view.engine.now.is_some()).await;
        let key = view.engine.now.unwrap().song_key;
        assert!(!key.is_empty());

        assert_eq!(supervisor.change_offset(OffsetChange::By(250)), Ok(250));
        assert_eq!(supervisor.change_offset(OffsetChange::By(-50)), Ok(200));
        let offsets = lyrix::config::Offsets::load(&paths.offsets).unwrap();
        assert_eq!(offsets.get(&key), 200);
        // The engine picks the file up and shows the song's offset.
        wait_for(&supervisor, |view| {
            view.engine
                .now
                .as_ref()
                .is_some_and(|now| now.song_offset_ms == 200)
        })
        .await;

        assert_eq!(supervisor.change_offset(OffsetChange::Reset), Ok(0));
        supervisor.stop().await;
    }

    #[tokio::test]
    async fn pausing_shows_at_once_without_an_engine_and_through_the_engine_with_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let supervisor = Supervisor::new(paths.clone());

        assert!(supervisor.set_paused(true).unwrap());
        assert!(supervisor.view().engine.paused);
        assert!(!supervisor.set_paused(false).unwrap());
        assert!(!supervisor.view().engine.paused);

        let playing = Arc::new(StdMutex::new(Some(song())));
        let running = supervisor.launch(fake_engine(&paths, playing));
        *supervisor.engine.lock().await = Some(running);
        wait_for(&supervisor, |view| view.engine.now.is_some()).await;

        assert!(supervisor.set_paused(true).unwrap());
        assert!(paths.pause_marker.exists());
        wait_for(&supervisor, |view| view.engine.paused).await;
        supervisor.stop().await;
        // Stopped, the view still knows sharing is paused.
        assert!(supervisor.view().engine.paused);
        assert!(!supervisor.view().running);
    }

    #[tokio::test]
    async fn a_new_supervisor_reads_the_pause_marker() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        std::fs::write(&paths.pause_marker, "").unwrap();
        let supervisor = Supervisor::new(paths);
        assert!(supervisor.view().engine.paused);
        assert!(!supervisor.view().running);
    }
}
