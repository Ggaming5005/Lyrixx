//! The `lyrix` command.
//!
//! ```text
//! lyrix [--config <file>] [-v…] [COMMAND]
//!
//! run (default)            Show lyrics as your status until Ctrl+C.
//!     --no-discord         Don't use Discord Rich Presence this time.
//!     --quiet              Don't print statuses in the terminal.
//! now                      Print what is playing, once.
//! lyrics                   Look up lyrics and print them as LRC.
//!     --artist <a> --title <t> [--album <al>] [--duration <seconds>]
//!     (without --artist/--title: the song playing now)
//! config init [--force]    Write a config file with every setting and its default.
//! config path              Print where the config file is.
//! config show              Print the settings in use.
//! config check             Report problems with the settings.
//! pause / resume           Clear the status and stop / start updating a running lyrix.
//! offset <ms>              Nudge the song playing now: positive shows lines later.
//! offset reset             Remove the nudge for the song playing now.
//! cache clear              Delete cached lyrics.
//! ```
//!
//! Advanced mode options print `BAN_WARNING` on `run` and `config check`. In this
//! build their connectors are not included yet, so `run` says so and continues
//! without them.

use anyhow::{bail, Context};
use clap::{ArgAction, Args, Parser, Subcommand};
use lyrix::config::{Config, ConfigIssue, Offsets, Severity, BAN_WARNING};
use lyrix::providers::cache::LyricsCache;
use lyrix::providers::local::LocalLrcProvider;
use lyrix::providers::lrclib::LrclibProvider;
use lyrix::providers::{LyricsProvider, ProviderChain, Resolved};
use lyrix::sources::NowPlayingSource;
use lyrix::targets::console::ConsoleTarget;
use lyrix::targets::discord_rpc::DiscordRpcTarget;
use lyrix::targets::StatusTarget;
use lyrix::{Lyrics, PlaybackSnapshot, PlaybackStatus, Track};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// Turns whatever you're listening to into a live status, with synced lyrics.
#[derive(Debug, Parser)]
#[command(name = "lyrix", version)]
struct Cli {
    /// Use this config file instead of the default one.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Log more: -v for debug messages, -vv for everything.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum Command {
    /// Show lyrics as your status until Ctrl+C (the default).
    Run(RunArgs),
    /// Print what is playing, once.
    Now,
    /// Look up lyrics and print them as LRC.
    Lyrics(LyricsArgs),
    /// Create, find, show or check the config file.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Clear the status and stop updating a running lyrix.
    Pause,
    /// Start updating the status of a running lyrix again.
    Resume,
    /// Nudge the timing of the song playing now.
    #[command(allow_negative_numbers = true)]
    Offset {
        /// Milliseconds, e.g. +250 or -100 (positive shows lines later), or `reset`.
        #[arg(value_name = "MS|reset", value_parser = parse_offset)]
        change: OffsetChange,
    },
    /// Manage the lyrics cache.
    #[command(subcommand)]
    Cache(CacheCommand),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
struct RunArgs {
    /// Don't use Discord Rich Presence this time.
    #[arg(long)]
    no_discord: bool,
    /// Don't print statuses in the terminal.
    #[arg(long)]
    quiet: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Args)]
struct LyricsArgs {
    /// The song's artist (needs --title).
    #[arg(long, requires = "title")]
    artist: Option<String>,
    /// The song's title (needs --artist).
    #[arg(long, requires = "artist")]
    title: Option<String>,
    /// The album, for a better match (needs --artist and --title).
    #[arg(long, requires = "artist")]
    album: Option<String>,
    /// The song's length in seconds (`215`, `215.5`) or as `m:ss` (`3:35`),
    /// for a better match (needs --artist and --title).
    #[arg(
        long = "duration",
        value_name = "SECONDS",
        requires = "artist",
        value_parser = parse_duration_ms
    )]
    duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum ConfigCommand {
    /// Write a config file with every setting and its default.
    Init {
        /// Replace an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print where the config file is.
    Path,
    /// Print the settings in use.
    Show,
    /// Report problems with the settings.
    Check,
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
enum CacheCommand {
    /// Delete cached lyrics.
    Clear,
}

/// What `lyrix offset` does to the song playing now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OffsetChange {
    /// Set the nudge to this many ms (positive shows lines later).
    Set(i64),
    /// Remove the nudge.
    Reset,
}

impl OffsetChange {
    /// The nudge to store: [`OffsetChange::Reset`] is 0, which removes the entry.
    fn value(self) -> i64 {
        match self {
            OffsetChange::Set(ms) => ms,
            OffsetChange::Reset => 0,
        }
    }
}

/// Reads `+250`, `-100`, `250` or `reset` (any case). Anything else, such as
/// `1.5`, `250ms`, `+` or `++1`, is rejected with a message saying what is accepted.
fn parse_offset(s: &str) -> Result<OffsetChange, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("reset") {
        return Ok(OffsetChange::Reset);
    }
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!(
            "`{s}` is not an offset. Use whole milliseconds such as +250 (lines later), \
             -100 (lines earlier) or 250, or `reset`."
        ));
    }
    s.parse::<i64>()
        .map(OffsetChange::Set)
        .map_err(|_| format!("`{s}` is too large for an offset in milliseconds."))
}

/// The longest song length `--duration` accepts: a day.
const MAX_DURATION_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Reads a song length given as seconds (`215`, `215.5`) or `m:ss` (`3:35`)
/// and returns milliseconds.
fn parse_duration_ms(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let invalid = || {
        format!(
            "`{s}` is not a song length. Use seconds such as 215 or 215.5, or m:ss such as 3:35."
        )
    };
    let secs = match s.split_once(':') {
        Some((minutes, seconds)) => {
            let minutes: u64 = parse_plain_number(minutes)
                .and_then(|m| m.parse().ok())
                .ok_or_else(invalid)?;
            let seconds = parse_plain_number(seconds)
                .filter(|sec| sec.len() == 2)
                .and_then(|sec| sec.parse::<u64>().ok())
                .filter(|sec| *sec < 60)
                .ok_or_else(invalid)?;
            (minutes as f64) * 60.0 + seconds as f64
        }
        None => {
            let valid = !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && s.bytes().filter(|b| *b == b'.').count() <= 1
                && s.bytes().any(|b| b.is_ascii_digit());
            if !valid {
                return Err(invalid());
            }
            s.parse::<f64>().map_err(|_| invalid())?
        }
    };
    if !secs.is_finite() || !(0.0..=MAX_DURATION_SECS).contains(&secs) {
        return Err(format!(
            "`{s}` is longer than a day; give the song length in seconds."
        ));
    }
    // In range, so the conversion neither overflows nor loses whole milliseconds.
    Ok((secs * 1000.0).round() as u64)
}

/// `s` when it is a non-empty run of ASCII digits.
fn parse_plain_number(s: &str) -> Option<&str> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        Some(s)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let code = match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            print_err(&format!("error: {error:#}"));
            1
        }
    };
    exit_now(code);
}

/// Ends the process with `code` without dropping the async runtime. Dropping
/// it waits, with no time limit, for blocking work that is still running: a
/// Windows media-session read that never answers keeps its blocking thread
/// after the source gives up on it, and `lyrix` would then hang after Ctrl+C
/// or after `lyrix now` printed its error. Standard output is flushed first.
fn exit_now(code: u8) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    std::process::exit(i32::from(code))
}

/// Logs to stderr. `RUST_LOG` sets the filter (default `lyrix=info`); `-v` and
/// `-vv` set Lyrix's own messages to debug and trace on top of it (replacing
/// any `lyrix=` level that `RUST_LOG` gave).
fn init_logging(verbose: u8) {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;

    let filter = match std::env::var("RUST_LOG") {
        Ok(spec) if !spec.trim().is_empty() => {
            EnvFilter::try_new(&spec).unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG))
        }
        _ => EnvFilter::new(DEFAULT_LOG),
    };
    let filter = match verbose_directive(verbose).and_then(|d| d.parse().ok()) {
        Some(directive) => filter.add_directive(directive),
        None => filter,
    };
    let ansi = std::io::stderr().is_terminal();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi);
    // A second logger (never the case here) is not worth failing over.
    let _ = if verbose == 0 {
        builder.without_time().with_target(false).try_init()
    } else {
        builder.try_init()
    };
}

/// The log filter when `RUST_LOG` is not set.
const DEFAULT_LOG: &str = "lyrix=info";

/// The extra filter directive for `-v` / `-vv`.
fn verbose_directive(verbose: u8) -> Option<&'static str> {
    match verbose {
        0 => None,
        1 => Some("lyrix=debug"),
        _ => Some("lyrix=trace"),
    }
}

/// Runs the command and returns the process exit code. Errors are printed by
/// `main` as `error: <message chain>`.
async fn run(cli: Cli) -> anyhow::Result<u8> {
    let config_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let command = cli.command.unwrap_or(Command::Run(RunArgs::default()));
    match command {
        Command::Run(args) => cmd_run(&config_path, &args).await,
        Command::Now => cmd_now(&config_path).await,
        Command::Lyrics(args) => cmd_lyrics(&config_path, &args).await,
        Command::Config(ConfigCommand::Init { force }) => {
            let message = config_init(&config_path, force)?;
            print_out(&message)?;
            Ok(0)
        }
        Command::Config(ConfigCommand::Path) => {
            print_out(&config_path.display().to_string())?;
            Ok(0)
        }
        Command::Config(ConfigCommand::Show) => cmd_config_show(&config_path),
        Command::Config(ConfigCommand::Check) => cmd_config_check(&config_path),
        Command::Pause => {
            print_out(&pause(&Config::pause_marker_path())?)?;
            Ok(0)
        }
        Command::Resume => {
            print_out(&resume(&Config::pause_marker_path())?)?;
            Ok(0)
        }
        Command::Offset { change } => cmd_offset(&config_path, change).await,
        Command::Cache(CacheCommand::Clear) => {
            let dir = Config::cache_dir();
            let removed = clear_cache_dir(&dir)?;
            print_out(&cache_cleared_message(removed, &dir))?;
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Writes `text` and a newline to stdout. A closed pipe (`lyrix lyrics | head`)
/// is not an error.
fn print_out(text: &str) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{text}").and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e).context("could not write to standard output"),
    }
}

/// Writes `text` and a newline to stderr, ignoring write errors (there is
/// nowhere left to report them).
fn print_err(text: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{text}");
}

/// Replaces control characters (line breaks, tabs, terminal escapes) with
/// spaces, so text from players or files cannot restyle the terminal.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `Title — Artist`, or just the title when the artist is empty.
fn song_label(track: &Track) -> String {
    let title = one_line(track.title.trim());
    let artist = one_line(track.artist.trim());
    let title = if title.is_empty() {
        "(untitled)".to_string()
    } else {
        title
    };
    if artist.is_empty() {
        title
    } else {
        format!("{title} — {artist}")
    }
}

/// `m:ss`, e.g. `3:07`; minutes keep counting past an hour (`62:03`).
fn format_mss(ms: u64) -> String {
    let total_secs = ms / 1000;
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// `error: …`, `warning: …` or `info: …`.
fn format_issue(issue: &ConfigIssue) -> String {
    let label = match issue.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    };
    format!("{label}: {}", issue.message)
}

/// `lyrix <args>` as the user should type it to act on `config_path`: with
/// `--config "<path>"` when that is not the default config file, so a
/// suggested command never touches a different file.
fn lyrix_command(config_path: &Path, args: &str) -> String {
    if config_path == Config::default_path() {
        format!("lyrix {args}")
    } else {
        format!("lyrix --config \"{}\" {args}", config_path.display())
    }
}

/// Prints every issue to stderr. Returns true when one of them is an error.
fn report_issues(issues: &[ConfigIssue]) -> bool {
    for issue in issues {
        print_err(&format_issue(issue));
    }
    issues.iter().any(|i| i.severity == Severity::Error)
}

// ---------------------------------------------------------------------------
// Lyrics providers
// ---------------------------------------------------------------------------

/// Where lyrics are looked up, in order, decided from the config alone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderPlan {
    /// Your own `.lrc` / `.txt` files, always first.
    local_dir: PathBuf,
    /// LRCLIB server, without a trailing slash, when LRCLIB is on.
    lrclib_url: Option<String>,
    /// Cache folder, when the cache is on.
    cache_dir: Option<PathBuf>,
}

impl ProviderPlan {
    fn from_config(config: &Config, cache_dir: PathBuf) -> Self {
        let lrclib_url = if config.lyrics.lrclib {
            Some(
                config
                    .lyrics
                    .lrclib_url
                    .trim()
                    .trim_end_matches('/')
                    .to_string(),
            )
        } else {
            None
        };
        Self {
            local_dir: config.lyrics_dir(),
            lrclib_url,
            cache_dir: config.lyrics.cache.then_some(cache_dir),
        }
    }

    /// `local (<dir>), lrclib (<url>), cache`.
    fn describe(&self) -> String {
        let mut parts = vec![format!(
            "local ({})",
            one_line(&self.local_dir.display().to_string())
        )];
        if let Some(url) = &self.lrclib_url {
            parts.push(format!("lrclib ({})", one_line(url)));
        }
        if self.cache_dir.is_some() {
            parts.push("cache".to_string());
        }
        parts.join(", ")
    }

    /// Builds the chain: local files, then LRCLIB when on, with the cache when on.
    fn build(&self) -> anyhow::Result<ProviderChain> {
        let mut providers: Vec<Box<dyn LyricsProvider>> =
            vec![Box::new(LocalLrcProvider::new(self.local_dir.clone()))];
        if let Some(url) = &self.lrclib_url {
            let lrclib = LrclibProvider::new(url.clone())
                .with_context(|| format!("could not set up LRCLIB at {url}"))?;
            providers.push(Box::new(lrclib));
        }
        let cache = self.cache_dir.clone().map(LyricsCache::new);
        Ok(ProviderChain::new(providers, cache))
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// Which targets `run` uses, decided from the config and flags alone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetPlan {
    console: bool,
    /// The Discord client id, when Discord Rich Presence is used.
    discord_client_id: Option<String>,
    /// Why each target that is off is off, and what would switch it on.
    off: Vec<(String, String)>,
    /// A line to print before starting (an empty Discord client id).
    notes: Vec<String>,
}

impl TargetPlan {
    fn from_config(config: &Config, args: &RunArgs, config_path: &Path) -> Self {
        let mut off = Vec::new();
        let mut notes = Vec::new();

        let console = if !config.console.enabled {
            off.push((
                "the terminal: console.enabled is off".to_string(),
                "set console.enabled = true".to_string(),
            ));
            false
        } else if args.quiet {
            off.push((
                "the terminal: --quiet".to_string(),
                "leave out --quiet".to_string(),
            ));
            false
        } else {
            true
        };

        let client_id = config.discord.client_id.trim();
        let discord_client_id = if !config.discord.enabled {
            let fix = if client_id.is_empty() {
                "set discord.enabled = true and discord.client_id"
            } else {
                "set discord.enabled = true"
            };
            off.push((
                "Discord: discord.enabled is off".to_string(),
                fix.to_string(),
            ));
            None
        } else if args.no_discord {
            off.push((
                "Discord: --no-discord".to_string(),
                "leave out --no-discord".to_string(),
            ));
            None
        } else if client_id.is_empty() {
            off.push((
                "Discord: discord.client_id is empty".to_string(),
                "set discord.client_id".to_string(),
            ));
            notes.push(format!(
                "Discord Rich Presence stays off because discord.client_id is empty. Create an \
                 application at https://discord.com/developers/applications and put its \
                 application id in {} as client_id = \"<id>\" under [discord] (run `{}` first if \
                 the file does not exist).",
                config_path.display(),
                lyrix_command(config_path, "config init")
            ));
            None
        } else {
            Some(client_id.to_string())
        };

        Self {
            console,
            discord_client_id,
            off,
            notes,
        }
    }

    /// Target names in the order they are built.
    fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.console {
            names.push("console");
        }
        if self.discord_client_id.is_some() {
            names.push("discord");
        }
        names
    }

    /// The error when no target is on: why each one is off, and only the
    /// changes that would switch one on.
    fn nothing_on_message(&self, config_path: &Path) -> String {
        let why: Vec<&str> = self.off.iter().map(|(why, _)| why.as_str()).collect();
        let fixes: Vec<&str> = self.off.iter().map(|(_, fix)| fix.as_str()).collect();
        format!(
            "there is nowhere to show lyrics ({}). To show them: {} (settings file: {}).",
            why.join("; "),
            fixes.join(", or "),
            config_path.display()
        )
    }
}

/// The lines `run` prints for each Advanced option that is switched on.
fn advanced_notes(config: &Config) -> Vec<String> {
    let mut lines = Vec::new();
    for name in config.advanced_requested() {
        lines.push(BAN_WARNING.to_string());
        if config.advanced.accept_ban_risk {
            lines.push(format!(
                "{name} is not included in this build yet, so it stays off."
            ));
        } else {
            lines.push(format!(
                "{name} is switched on, but stays off until you set advanced.accept_ban_risk = true."
            ));
        }
    }
    lines
}

/// The line `run` prints when it starts.
fn startup_line(source: &str, providers: &ProviderPlan, targets: &[&str], paused: bool) -> String {
    let mut line = format!(
        "Lyrix is running. Reading {}; lyrics from {}; showing on {}.",
        source,
        providers.describe(),
        targets.join(", ")
    );
    if paused {
        line.push_str(" Paused: nothing is shown until you run `lyrix resume`.");
    }
    line.push_str(" Press Ctrl+C to stop.");
    line
}

async fn cmd_run(config_path: &Path, args: &RunArgs) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    if report_issues(&config.validate()) {
        bail!(
            "the settings in {} have errors; fix them and try again",
            config_path.display()
        );
    }
    for line in advanced_notes(&config) {
        print_err(&line);
    }

    let source = lyrix::sources::default_source(&config.sources, &config.privacy.blocked_apps)?;
    let providers = ProviderPlan::from_config(&config, Config::cache_dir());
    let plan = TargetPlan::from_config(&config, args, config_path);
    for note in &plan.notes {
        print_err(note);
    }

    let mut targets: Vec<Box<dyn StatusTarget>> = Vec::new();
    if plan.console {
        targets.push(Box::new(ConsoleTarget::stdout()));
    }
    if let Some(client_id) = &plan.discord_client_id {
        targets.push(Box::new(DiscordRpcTarget::new(
            client_id.clone(),
            Duration::from_millis(config.discord.min_interval_ms),
            config.discord.show_progress,
        )));
    }
    if targets.is_empty() {
        bail!(plan.nothing_on_message(config_path));
    }

    let chain = providers.build()?;
    let pause_marker = Config::pause_marker_path();
    let paused = std::fs::symlink_metadata(&pause_marker).is_ok();
    print_err(&startup_line(
        source.name(),
        &providers,
        &plan.names(),
        paused,
    ));

    lyrix::engine::Engine::new(config, source, Arc::new(chain), targets)
        .with_offsets_path(Config::offsets_path())
        .with_pause_marker(pause_marker)
        .run(shutdown_signal())
        .await?;
    Ok(0)
}

/// Completes on Ctrl+C, or on Unix also on SIGTERM. A signal that cannot be
/// watched is logged and never fires (the other one still can). Once it has
/// fired, the engine clears the statuses (a few seconds at most); a second
/// Ctrl+C in that time ends the process at once with exit code 130, since
/// tokio's handler has replaced the default one that would have.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("cannot watch for Ctrl+C: {e}");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                tracing::warn!("cannot watch for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("stopping");
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            print_err("Stopped right away; the status may not have been cleared.");
            exit_now(130);
        }
    });
}

// ---------------------------------------------------------------------------
// now
// ---------------------------------------------------------------------------

/// Reads the source for this computer once.
async fn current_snapshot(config: &Config) -> anyhow::Result<Option<PlaybackSnapshot>> {
    let source: Box<dyn NowPlayingSource> =
        lyrix::sources::default_source(&config.sources, &config.privacy.blocked_apps)?;
    source
        .snapshot()
        .await
        .with_context(|| format!("could not read what is playing ({})", source.name()))
}

/// The position at `now`, extrapolated from the snapshot while playing.
fn position_now(snapshot: &PlaybackSnapshot, now: Instant) -> u64 {
    let mut clock = lyrix::clock::SyncClock::new();
    clock.update(snapshot);
    clock.position_ms(now).unwrap_or(snapshot.position_ms)
}

/// What `lyrix now` prints for a snapshot.
fn describe_snapshot(snapshot: &PlaybackSnapshot, position_ms: u64) -> String {
    let track = &snapshot.track;
    let or_unknown = |s: &str| {
        let s = one_line(s.trim());
        if s.is_empty() {
            "unknown".to_string()
        } else {
            s
        }
    };
    let duration = match track.duration_ms {
        Some(ms) if ms > 0 => format_mss(ms),
        _ => "unknown".to_string(),
    };
    let status = match snapshot.status {
        PlaybackStatus::Playing => "playing",
        PlaybackStatus::Paused => "paused",
        PlaybackStatus::Stopped => "stopped",
    };
    [
        format!("Title:    {}", or_unknown(&track.title)),
        format!("Artist:   {}", or_unknown(&track.artist)),
        format!(
            "Album:    {}",
            or_unknown(track.album.as_deref().unwrap_or(""))
        ),
        format!("Duration: {duration}"),
        format!("Position: {}", format_mss(position_ms)),
        format!("Status:   {status}"),
        format!("App:      {}", or_unknown(&snapshot.app_id)),
    ]
    .join("\n")
}

async fn cmd_now(config_path: &Path) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    match current_snapshot(&config).await? {
        None => print_out("Nothing is playing.")?,
        Some(snapshot) => {
            let position = position_now(&snapshot, Instant::now());
            print_out(&describe_snapshot(&snapshot, position))?;
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// lyrics
// ---------------------------------------------------------------------------

/// The song named on the command line, when `--artist` and `--title` are given.
fn track_from_args(args: &LyricsArgs) -> Option<Track> {
    match (&args.artist, &args.title) {
        (Some(artist), Some(title)) => Some(Track {
            title: title.clone(),
            artist: artist.clone(),
            album: args.album.clone().filter(|album| !album.trim().is_empty()),
            duration_ms: args.duration_ms.filter(|ms| *ms > 0),
            spotify_id: None,
        }),
        _ => None,
    }
}

/// The stderr line after lyrics were found: where they came from and how they are timed.
fn found_message(lyrics: &Lyrics, track: &Track) -> String {
    let source = one_line(&lyrics.source);
    if lyrics.instrumental {
        format!("From {source}: {} is instrumental.", song_label(track))
    } else if lyrics.synced {
        format!("From {source} (synced).")
    } else if lyrics.lines.iter().any(|line| line.start_ms > 0) {
        format!("From {source} (estimated timing).")
    } else {
        format!("From {source} (not synced).")
    }
}

async fn cmd_lyrics(config_path: &Path, args: &LyricsArgs) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    let track = match track_from_args(args) {
        Some(track) => track,
        None => match current_snapshot(&config).await? {
            Some(snapshot) => snapshot.track,
            None => {
                bail!("nothing is playing. Play a song, or name one with --artist and --title.")
            }
        },
    };
    if track.title.trim().is_empty() {
        bail!("the song has no title, so its lyrics cannot be looked up");
    }

    let chain = ProviderPlan::from_config(&config, Config::cache_dir()).build()?;
    match chain.resolve(&track).await {
        Resolved::Found(lyrics) => {
            let text = lyrix::lrc::to_lrc(&lyrics);
            if !text.is_empty() {
                print_out(&text)?;
            }
            print_err(&found_message(&lyrics, &track));
            Ok(0)
        }
        Resolved::NotFound => {
            print_err(&format!("No lyrics found for {}.", song_label(&track)));
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

/// The comment at the top of a file written by `lyrix config init`.
const CONFIG_HEADER: &str = "\
# Lyrix settings.
#
# Lyrix reads what your music player is playing, finds synced lyrics and shows
# the current line as your status (Discord Rich Presence and the terminal).
#
# Every setting is listed with its default value. Delete a line to go back to
# the default; settings Lyrix does not know are ignored. After editing, run
# `lyrix config check` to look for mistakes, and restart `lyrix` to use them.
";

/// The comment written above a section's `[header]`.
fn section_comment(section: &str) -> Option<String> {
    let text = match section {
        "general" => "# How often the player is read (ms), and a timing nudge for every song\n\
                      # (ms; positive shows lines later).",
        "status" => "# What the status says. Placeholders: {line} {next} {title} {artist} {album}.",
        "privacy" => "# Players to ignore (matched inside the app id, e.g. \"chrome\"), artists\n\
                      # to never show, and title_only to show the song but never lyric lines.",
        "lyrics" => "# Where lyrics come from: your own .lrc/.txt files first, then LRCLIB\n\
                     # (free, no account). Found lyrics are cached on disk when cache = true.",
        "sources" => "# Players to prefer when several are playing (matched like blocked_apps).",
        "discord" => "# Discord Rich Presence (needs the Discord desktop app). client_id is the\n\
                      # id of a Discord application (https://discord.com/developers/applications);\n\
                      # its name is shown as \"Listening to <name>\".",
        "console" => "# Print each status change in the terminal.",
        "advanced" => {
            return Some(format!(
                "# Advanced mode: options that use your own account in ways the services do\n\
                 # not allow. {BAN_WARNING}\n\
                 # Nothing in this section runs unless accept_ban_risk = true, and this build\n\
                 # does not include these options yet."
            ))
        }
        _ => return None,
    };
    Some(text.to_string())
}

/// Commented-out settings written right below a section's `[header]`: the
/// optional ones that have no value by default. `lyrics_dir` is the default
/// lyrics folder, shown as the example value.
fn section_extras(section: &str, lyrics_dir: &Path) -> Vec<String> {
    match section {
        "lyrics" => {
            let dir = lyrics_dir.display().to_string();
            vec![
                "# Folder of your own lyric files (default shown):".to_string(),
                format!("# lyrics_dir = {}", one_line_toml_string(&dir)),
            ]
        }
        "sources" => vec![
            "# macOS only: folder of an installed mediaremote-adapter, to read every app in"
                .to_string(),
            "# the Now Playing widget:".to_string(),
            "# macos_adapter_dir = \"/path/to/mediaremote-adapter\"".to_string(),
        ],
        _ => Vec::new(),
    }
}

/// `value` as a TOML string that stays on one line, so it can follow a `# `
/// comment marker and is still valid once the marker is removed. The `toml`
/// crate writes text with line breaks as a multi-line string, whose later
/// lines would end up outside the comment.
fn one_line_toml_string(value: &str) -> String {
    let rendered = toml::Value::String(value.to_string()).to_string();
    if !rendered.contains(['\n', '\r']) {
        return rendered;
    }
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The section name of a `[name]` header line.
fn section_header(line: &str) -> Option<&str> {
    let name = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    if name.is_empty() || name.starts_with('[') {
        None
    } else {
        Some(name.trim())
    }
}

/// The text of a new config file: a header comment, then the defaults as
/// TOML with a comment above each section (the `[advanced]` one includes
/// [`BAN_WARNING`]).
fn default_config_text() -> anyhow::Result<String> {
    let defaults = Config::default();
    let body = toml::to_string_pretty(&defaults)
        .context("could not write the default settings as TOML")?;

    let lyrics_dir = defaults.lyrics_dir();

    let mut out = String::from(CONFIG_HEADER);
    out.push('\n');
    let mut warned = false;
    for line in body.lines() {
        let section = section_header(line);
        if let Some(name) = section {
            if let Some(comment) = section_comment(name) {
                out.push_str(&comment);
                out.push('\n');
                warned |= name == "advanced";
            }
        }
        out.push_str(line);
        out.push('\n');
        if let Some(name) = section {
            for extra in section_extras(name, &lyrics_dir) {
                out.push_str(&extra);
                out.push('\n');
            }
        }
    }
    if !warned {
        // The defaults always have an [advanced] section; keep the warning anyway.
        if let Some(comment) = section_comment("advanced") {
            out.push('\n');
            out.push_str(&comment);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Writes a new config file with every setting and its default. An existing
/// file (or anything else at `path`) is kept unless `force` is set. Returns
/// the message to print.
fn config_init(path: &Path, force: bool) -> anyhow::Result<String> {
    let text = default_config_text()?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create the folder {}", parent.display()))?;
        }
    }
    if force {
        std::fs::write(path, &text)
            .with_context(|| format!("could not write the config file {}", path.display()))?;
    } else {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path);
        let mut file = match file {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => bail!(
                "{} already exists. Run `{}` to replace it with the defaults.",
                path.display(),
                lyrix_command(path, "config init --force")
            ),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("could not create the config file {}", path.display())
                })
            }
        };
        file.write_all(text.as_bytes())
            .and_then(|()| file.flush())
            .with_context(|| format!("could not write the config file {}", path.display()))?;
    }
    Ok(format!("Wrote {}", path.display()))
}

fn cmd_config_show(config_path: &Path) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    if std::fs::symlink_metadata(config_path).is_err() {
        print_err(&format!(
            "There is no config file at {} yet, so these are the defaults.",
            config_path.display()
        ));
    }
    let text = toml::to_string_pretty(&config).context("could not write the settings as TOML")?;
    print_out(text.trim_end())?;
    Ok(0)
}

fn cmd_config_check(config_path: &Path) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    let issues = config.validate();
    if issues.is_empty() {
        print_out(&format!("No problems found in {}.", config_path.display()))?;
        return Ok(0);
    }
    Ok(if report_issues(&issues) { 1 } else { 0 })
}

// ---------------------------------------------------------------------------
// pause / resume
// ---------------------------------------------------------------------------

/// Creates the pause marker (and its folder). Returns the message to print.
fn pause(marker: &Path) -> anyhow::Result<String> {
    if std::fs::symlink_metadata(marker).is_ok() {
        return Ok("Already paused. Run `lyrix resume` to show lyrics again.".to_string());
    }
    if let Some(parent) = marker.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create the folder {}", parent.display()))?;
        }
    }
    std::fs::write(
        marker,
        "Lyrix is paused while this file exists. Run `lyrix resume` or delete it.\n",
    )
    .with_context(|| format!("could not create the pause marker {}", marker.display()))?;
    Ok(
        "Paused. A running lyrix clears its status and stops updating until you run \
         `lyrix resume`."
            .to_string(),
    )
}

/// Removes the pause marker. Returns the message to print.
fn resume(marker: &Path) -> anyhow::Result<String> {
    match std::fs::remove_file(marker) {
        Ok(()) => Ok("Resumed. A running lyrix shows lyrics again.".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok("Lyrix was not paused, so there is nothing to resume.".to_string())
        }
        Err(e) => Err(e)
            .with_context(|| format!("could not remove the pause marker {}", marker.display())),
    }
}

// ---------------------------------------------------------------------------
// offset
// ---------------------------------------------------------------------------

/// Stores the nudge for `key` in the offsets file and returns the stored value.
fn apply_offset(offsets_path: &Path, key: &str, change: OffsetChange) -> anyhow::Result<i64> {
    let mut offsets = Offsets::load(offsets_path)?;
    let value = change.value();
    offsets.set(key, value);
    offsets.save(offsets_path)?;
    Ok(offsets.get(key))
}

/// What `lyrix offset` prints after saving.
fn offset_message(track: &Track, offset_ms: i64) -> String {
    let song = song_label(track);
    let what = match offset_ms {
        0 => format!("{song}: no offset."),
        ms if ms > 0 => format!("{song}: +{ms} ms (lines show later)."),
        ms => format!("{song}: {ms} ms (lines show earlier)."),
    };
    format!("{what} A running lyrix uses it from the next time the song starts.")
}

async fn cmd_offset(config_path: &Path, change: OffsetChange) -> anyhow::Result<u8> {
    let config = Config::load(config_path)?;
    let snapshot = match current_snapshot(&config).await? {
        Some(snapshot) => snapshot,
        None => bail!("nothing is playing. Play the song you want to nudge, then run this again."),
    };
    let Some((track, key)) = offset_key(&snapshot.track) else {
        bail!("the player does not say which song is playing, so there is nothing to nudge");
    };
    let value = apply_offset(&Config::offsets_path(), &key, change)?;
    print_out(&offset_message(&track, value))?;
    Ok(0)
}

/// The normalized song and its offsets key (the one the engine looks up), or
/// `None` when the song has no usable title.
fn offset_key(track: &Track) -> Option<(Track, String)> {
    use lyrix::matcher::{clean_title, normalize_key, normalize_track, song_key};
    let track = normalize_track(track);
    if normalize_key(&clean_title(&track.title)).is_empty() {
        return None;
    }
    let key = song_key(&track);
    Some((track, key))
}

// ---------------------------------------------------------------------------
// cache clear
// ---------------------------------------------------------------------------

/// Deletes the `*.json` files directly inside `dir` (not in subfolders, and
/// nothing else). A missing folder counts as empty. Returns how many were removed.
fn clear_cache_dir(dir: &Path) -> anyhow::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("could not read the cache folder {}", dir.display()))
        }
    };
    let mut removed: usize = 0;
    let mut failed: Vec<String> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                failed.push(e.to_string());
                continue;
            }
        };
        let path = entry.path();
        let is_json = path.extension().is_some_and(|ext| ext == "json");
        // The entry's own type: a link is removed as a link, never followed.
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(true);
        if !is_json || is_dir {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed.push(format!("{}: {e}", path.display())),
        }
    }
    if let Some(first) = failed.first() {
        bail!(
            "removed {removed} cached lyrics from {}, but {} could not be removed ({first})",
            dir.display(),
            failed.len()
        );
    }
    Ok(removed)
}

fn cache_cleared_message(removed: usize, dir: &Path) -> String {
    let what = if removed == 1 {
        "1 cached song".to_string()
    } else {
        format!("{removed} cached songs")
    };
    format!("Removed {what} from {}.", dir.display())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("lyrix").chain(args.iter().copied()))
    }

    fn command(args: &[&str]) -> Command {
        let cli = parse(args).unwrap_or_else(|e| panic!("{args:?} should parse: {e}"));
        cli.command.unwrap_or(Command::Run(RunArgs::default()))
    }

    fn parse_err(args: &[&str]) -> ErrorKind {
        match parse(args) {
            Ok(cli) => panic!("{args:?} should not parse, got {cli:?}"),
            Err(e) => e.kind(),
        }
    }

    fn snapshot(track: Track, status: PlaybackStatus, position_ms: u64) -> PlaybackSnapshot {
        PlaybackSnapshot {
            track,
            status,
            position_ms,
            position_at: Instant::now(),
            rate: 1.0,
            app_id: "spotify".into(),
        }
    }

    fn song() -> Track {
        Track {
            title: "Never Gonna Give You Up".into(),
            artist: "Rick Astley".into(),
            album: Some("Whenever You Need Somebody".into()),
            duration_ms: Some(213_000),
            spotify_id: None,
        }
    }

    // ---- parsing: commands and flags ----

    #[test]
    fn no_command_is_run() {
        let cli = parse(&[]).unwrap();
        assert_eq!(cli.command, None);
        assert_eq!(cli.config, None);
        assert_eq!(cli.verbose, 0);
        assert_eq!(command(&[]), Command::Run(RunArgs::default()));
    }

    #[test]
    fn run_command_and_flags() {
        assert_eq!(command(&["run"]), Command::Run(RunArgs::default()));
        assert_eq!(
            command(&["run", "--no-discord"]),
            Command::Run(RunArgs {
                no_discord: true,
                quiet: false
            })
        );
        assert_eq!(
            command(&["run", "--quiet"]),
            Command::Run(RunArgs {
                no_discord: false,
                quiet: true
            })
        );
        assert_eq!(
            command(&["run", "--quiet", "--no-discord"]),
            Command::Run(RunArgs {
                no_discord: true,
                quiet: true
            })
        );
    }

    #[test]
    fn run_flags_belong_to_the_run_command() {
        assert_eq!(parse_err(&["--quiet"]), ErrorKind::UnknownArgument);
        assert_eq!(
            parse_err(&["--no-discord", "run"]),
            ErrorKind::UnknownArgument
        );
        assert_eq!(parse_err(&["now", "--quiet"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn global_config_flag_before_or_after_the_command() {
        let cli = parse(&["--config", "/tmp/x.toml", "now"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/x.toml")));
        assert_eq!(cli.command, Some(Command::Now));

        let cli = parse(&["now", "--config", "/tmp/y.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/y.toml")));

        let cli = parse(&["config", "show", "--config=/tmp/z.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/z.toml")));
        assert_eq!(cli.command, Some(Command::Config(ConfigCommand::Show)));

        let cli = parse(&["--config", "c.toml"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("c.toml")));
        assert_eq!(cli.command, None);

        let cli = parse(&["--config", "c.toml", "run", "--quiet"]).unwrap();
        assert_eq!(cli.config, Some(PathBuf::from("c.toml")));
        assert_eq!(
            cli.command,
            Some(Command::Run(RunArgs {
                no_discord: false,
                quiet: true
            }))
        );

        assert_eq!(parse_err(&["--config"]), ErrorKind::InvalidValue);
    }

    #[test]
    fn verbose_counts() {
        assert_eq!(parse(&["-v"]).unwrap().verbose, 1);
        assert_eq!(parse(&["-vv"]).unwrap().verbose, 2);
        assert_eq!(parse(&["-v", "-v", "-v"]).unwrap().verbose, 3);
        assert_eq!(parse(&["--verbose", "now"]).unwrap().verbose, 1);
        assert_eq!(parse(&["now", "-vv"]).unwrap().verbose, 2);
        assert_eq!(parse(&["run", "-v", "--quiet"]).unwrap().verbose, 1);
    }

    #[test]
    fn verbose_directives() {
        assert_eq!(verbose_directive(0), None);
        assert_eq!(verbose_directive(1), Some("lyrix=debug"));
        assert_eq!(verbose_directive(2), Some("lyrix=trace"));
        assert_eq!(verbose_directive(u8::MAX), Some("lyrix=trace"));
        // Every directive is valid for the filter.
        for v in 0..=3 {
            if let Some(d) = verbose_directive(v) {
                assert!(d.parse::<tracing_subscriber::filter::Directive>().is_ok());
            }
        }
        assert!(tracing_subscriber::EnvFilter::try_new(DEFAULT_LOG).is_ok());
    }

    #[test]
    fn now_command() {
        assert_eq!(command(&["now"]), Command::Now);
        assert_eq!(parse_err(&["now", "extra"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn lyrics_command_without_a_song() {
        assert_eq!(command(&["lyrics"]), Command::Lyrics(LyricsArgs::default()));
    }

    #[test]
    fn lyrics_command_with_every_option() {
        assert_eq!(
            command(&[
                "lyrics",
                "--artist",
                "Rick Astley",
                "--title",
                "Never Gonna Give You Up",
                "--album",
                "Whenever You Need Somebody",
                "--duration",
                "213",
            ]),
            Command::Lyrics(LyricsArgs {
                artist: Some("Rick Astley".into()),
                title: Some("Never Gonna Give You Up".into()),
                album: Some("Whenever You Need Somebody".into()),
                duration_ms: Some(213_000),
            })
        );
        assert_eq!(
            command(&["lyrics", "--title=Song", "--artist=Band", "--duration=3:35"]),
            Command::Lyrics(LyricsArgs {
                artist: Some("Band".into()),
                title: Some("Song".into()),
                album: None,
                duration_ms: Some(215_000),
            })
        );
    }

    #[test]
    fn lyrics_needs_artist_and_title_together() {
        assert_eq!(
            parse_err(&["lyrics", "--artist", "Band"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["lyrics", "--title", "Song"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["lyrics", "--album", "Album"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["lyrics", "--duration", "200"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["lyrics", "--artist", "Band", "--album", "Album"]),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn lyrics_rejects_a_bad_duration() {
        for bad in [
            "abc", "-1", "1:2", "1:60", "", "1e3", "inf", "NaN", "100000",
        ] {
            let arg = format!("--duration={bad}");
            assert_eq!(
                parse_err(&["lyrics", "--artist", "A", "--title", "T", &arg]),
                ErrorKind::ValueValidation,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn config_commands() {
        assert_eq!(
            command(&["config", "init"]),
            Command::Config(ConfigCommand::Init { force: false })
        );
        assert_eq!(
            command(&["config", "init", "--force"]),
            Command::Config(ConfigCommand::Init { force: true })
        );
        assert_eq!(
            command(&["config", "path"]),
            Command::Config(ConfigCommand::Path)
        );
        assert_eq!(
            command(&["config", "show"]),
            Command::Config(ConfigCommand::Show)
        );
        assert_eq!(
            command(&["config", "check"]),
            Command::Config(ConfigCommand::Check)
        );
        assert_eq!(
            parse_err(&["config"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(parse_err(&["config", "edit"]), ErrorKind::InvalidSubcommand);
        assert_eq!(
            parse_err(&["config", "path", "--force"]),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn pause_and_resume_commands() {
        assert_eq!(command(&["pause"]), Command::Pause);
        assert_eq!(command(&["resume"]), Command::Resume);
    }

    #[test]
    fn offset_commands() {
        assert_eq!(
            command(&["offset", "+250"]),
            Command::Offset {
                change: OffsetChange::Set(250)
            }
        );
        assert_eq!(
            command(&["offset", "-100"]),
            Command::Offset {
                change: OffsetChange::Set(-100)
            }
        );
        assert_eq!(
            command(&["offset", "250"]),
            Command::Offset {
                change: OffsetChange::Set(250)
            }
        );
        assert_eq!(
            command(&["offset", "reset"]),
            Command::Offset {
                change: OffsetChange::Reset
            }
        );
        // Flags still work around a negative number.
        let cli = parse(&["offset", "-100", "-v"]).unwrap();
        assert_eq!(cli.verbose, 1);
        assert_eq!(
            cli.command,
            Some(Command::Offset {
                change: OffsetChange::Set(-100)
            })
        );
        assert_eq!(parse_err(&["offset", "abc"]), ErrorKind::ValueValidation);
        assert_eq!(parse_err(&["offset", "1.5"]), ErrorKind::ValueValidation);
        assert_eq!(parse_err(&["offset"]), ErrorKind::MissingRequiredArgument);
        assert_eq!(parse_err(&["offset", "1", "2"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn offset_error_message_explains_the_format() {
        let err = parse(&["offset", "soon"]).unwrap_err().to_string();
        assert!(err.contains("+250"), "{err}");
        assert!(err.contains("reset"), "{err}");
    }

    #[test]
    fn cache_commands() {
        assert_eq!(
            command(&["cache", "clear"]),
            Command::Cache(CacheCommand::Clear)
        );
        assert_eq!(
            parse_err(&["cache"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(parse_err(&["cache", "purge"]), ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn unknown_commands_and_flags_are_errors() {
        assert_eq!(parse_err(&["dance"]), ErrorKind::InvalidSubcommand);
        assert_eq!(parse_err(&["--loud"]), ErrorKind::UnknownArgument);
        assert_eq!(parse_err(&["run", "--force"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse_err(&["--help"]), ErrorKind::DisplayHelp);
        assert_eq!(parse_err(&["lyrics", "--help"]), ErrorKind::DisplayHelp);
        assert_eq!(parse_err(&["--version"]), ErrorKind::DisplayVersion);
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    // ---- offset and duration values ----

    #[test]
    fn parse_offset_accepts_signed_whole_milliseconds() {
        assert_eq!(parse_offset("+250"), Ok(OffsetChange::Set(250)));
        assert_eq!(parse_offset("-100"), Ok(OffsetChange::Set(-100)));
        assert_eq!(parse_offset("250"), Ok(OffsetChange::Set(250)));
        assert_eq!(parse_offset("0"), Ok(OffsetChange::Set(0)));
        assert_eq!(parse_offset("-0"), Ok(OffsetChange::Set(0)));
        assert_eq!(parse_offset("007"), Ok(OffsetChange::Set(7)));
        assert_eq!(parse_offset(" 40 "), Ok(OffsetChange::Set(40)));
        assert_eq!(
            parse_offset("9223372036854775807"),
            Ok(OffsetChange::Set(i64::MAX))
        );
        assert_eq!(
            parse_offset("-9223372036854775808"),
            Ok(OffsetChange::Set(i64::MIN))
        );
    }

    #[test]
    fn parse_offset_accepts_reset() {
        assert_eq!(parse_offset("reset"), Ok(OffsetChange::Reset));
        assert_eq!(parse_offset("RESET"), Ok(OffsetChange::Reset));
        assert_eq!(OffsetChange::Reset.value(), 0);
        assert_eq!(OffsetChange::Set(-5).value(), -5);
    }

    #[test]
    fn parse_offset_rejects_everything_else() {
        for bad in [
            "", " ", "+", "-", "++1", "+-1", "--1", "1.5", "250ms", "1_000", "abc", "1e3", "٣",
            "0x10", "1 2", "resets",
        ] {
            let err = parse_offset(bad).expect_err(bad);
            assert!(
                err.contains("+250") && err.contains("reset"),
                "{bad:?}: {err}"
            );
        }
        let err = parse_offset("99999999999999999999").unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn parse_duration_reads_seconds_and_minutes() {
        assert_eq!(parse_duration_ms("215"), Ok(215_000));
        assert_eq!(parse_duration_ms("215.5"), Ok(215_500));
        assert_eq!(parse_duration_ms("0.25"), Ok(250));
        assert_eq!(parse_duration_ms(".5"), Ok(500));
        assert_eq!(parse_duration_ms("3:35"), Ok(215_000));
        assert_eq!(parse_duration_ms("0:07"), Ok(7_000));
        assert_eq!(parse_duration_ms("62:03"), Ok(3_723_000));
        assert_eq!(parse_duration_ms(" 10 "), Ok(10_000));
        assert_eq!(parse_duration_ms("86400"), Ok(86_400_000));
    }

    #[test]
    fn parse_duration_rejects_nonsense() {
        for bad in [
            "",
            ".",
            "-1",
            "+5",
            "1e3",
            "inf",
            "NaN",
            "3:5",
            "3:60",
            ":30",
            "3:",
            "1:2:3",
            "a:bc",
            "1..2",
            "1.2.3",
            "86401",
            "99999999999999999999999",
        ] {
            assert!(parse_duration_ms(bad).is_err(), "{bad:?}");
        }
    }

    // ---- small formatting helpers ----

    #[test]
    fn format_mss_examples() {
        assert_eq!(format_mss(0), "0:00");
        assert_eq!(format_mss(999), "0:00");
        assert_eq!(format_mss(59_999), "0:59");
        assert_eq!(format_mss(60_000), "1:00");
        assert_eq!(format_mss(213_000), "3:33");
        assert_eq!(format_mss(3_723_000), "62:03");
        assert!(!format_mss(u64::MAX).is_empty());
    }

    #[test]
    fn song_label_examples() {
        assert_eq!(song_label(&song()), "Never Gonna Give You Up — Rick Astley");
        let mut t = song();
        t.artist = "  ".into();
        assert_eq!(song_label(&t), "Never Gonna Give You Up");
        t.title = String::new();
        assert_eq!(song_label(&t), "(untitled)");
        t.title = "Line\nbreak\x1b[31m".into();
        t.artist = "Tab\tby".into();
        assert_eq!(song_label(&t), "Line break [31m — Tab by");
    }

    #[test]
    fn issues_are_labelled_by_severity() {
        let issue = |severity, message: &str| ConfigIssue {
            severity,
            message: message.into(),
        };
        assert_eq!(format_issue(&issue(Severity::Error, "bad")), "error: bad");
        assert_eq!(
            format_issue(&issue(Severity::Warning, "hmm")),
            "warning: hmm"
        );
        assert_eq!(format_issue(&issue(Severity::Info, "fyi")), "info: fyi");
        assert!(!report_issues(&[]));
        assert!(!report_issues(&[issue(Severity::Warning, "w")]));
        assert!(report_issues(&[
            issue(Severity::Warning, "w"),
            issue(Severity::Error, "e")
        ]));
    }

    // ---- now ----

    #[test]
    fn describe_snapshot_lists_every_field() {
        let s = snapshot(song(), PlaybackStatus::Playing, 62_000);
        let text = describe_snapshot(&s, 62_400);
        assert_eq!(
            text,
            "Title:    Never Gonna Give You Up\n\
             Artist:   Rick Astley\n\
             Album:    Whenever You Need Somebody\n\
             Duration: 3:33\n\
             Position: 1:02\n\
             Status:   playing\n\
             App:      spotify"
        );
    }

    #[test]
    fn describe_snapshot_with_missing_details() {
        let track = Track {
            title: "Radio\nStream".into(),
            ..Track::default()
        };
        let mut s = snapshot(track, PlaybackStatus::Paused, 0);
        s.app_id = String::new();
        let text = describe_snapshot(&s, 0);
        assert!(text.contains("Title:    Radio Stream"), "{text}");
        assert!(text.contains("Artist:   unknown"), "{text}");
        assert!(text.contains("Album:    unknown"), "{text}");
        assert!(text.contains("Duration: unknown"), "{text}");
        assert!(text.contains("Position: 0:00"), "{text}");
        assert!(text.contains("Status:   paused"), "{text}");
        assert!(text.contains("App:      unknown"), "{text}");

        let mut zero = song();
        zero.duration_ms = Some(0);
        let s = snapshot(zero, PlaybackStatus::Stopped, 0);
        let text = describe_snapshot(&s, 0);
        assert!(text.contains("Duration: unknown"), "{text}");
        assert!(text.contains("Status:   stopped"), "{text}");
    }

    #[test]
    fn position_now_extrapolates_only_while_playing() {
        let start = Instant::now();
        let mut s = snapshot(song(), PlaybackStatus::Playing, 10_000);
        s.position_at = start;
        let later = start + Duration::from_millis(2_500);
        assert_eq!(position_now(&s, later), 12_500);

        s.status = PlaybackStatus::Paused;
        assert_eq!(position_now(&s, later), 10_000);

        // Never past the end of the song.
        s.status = PlaybackStatus::Playing;
        s.position_ms = 212_000;
        assert_eq!(position_now(&s, start + Duration::from_secs(60)), 213_000);
    }

    // ---- lyrics ----

    #[test]
    fn track_from_args_builds_the_song() {
        assert_eq!(track_from_args(&LyricsArgs::default()), None);
        let args = LyricsArgs {
            artist: Some("Rick Astley".into()),
            title: Some("Never Gonna Give You Up".into()),
            album: Some("Whenever You Need Somebody".into()),
            duration_ms: Some(213_000),
        };
        assert_eq!(track_from_args(&args), Some(song()));

        let args = LyricsArgs {
            artist: Some("A".into()),
            title: Some("T".into()),
            album: Some("  ".into()),
            duration_ms: Some(0),
        };
        let track = track_from_args(&args).unwrap();
        assert_eq!(track.album, None);
        assert_eq!(track.duration_ms, None);
        assert_eq!(track.spotify_id, None);
    }

    #[test]
    fn track_from_parsed_command_line() {
        let Command::Lyrics(args) = command(&[
            "lyrics",
            "--artist",
            "Daft Punk",
            "--title",
            "One More Time",
            "--duration",
            "320.4",
        ]) else {
            panic!("not lyrics");
        };
        let track = track_from_args(&args).unwrap();
        assert_eq!(track.artist, "Daft Punk");
        assert_eq!(track.title, "One More Time");
        assert_eq!(track.duration_ms, Some(320_400));
        assert_eq!(track.album, None);
    }

    #[test]
    fn found_message_names_source_and_timing() {
        let line = |start_ms, text: &str| lyrix::LyricLine {
            start_ms,
            text: text.into(),
        };
        let synced = Lyrics {
            lines: vec![line(1_000, "a")],
            synced: true,
            instrumental: false,
            source: "lrclib".into(),
        };
        assert_eq!(found_message(&synced, &song()), "From lrclib (synced).");

        let estimated = Lyrics {
            lines: vec![line(10_650, "a"), line(191_700, "b")],
            synced: false,
            instrumental: false,
            source: "local".into(),
        };
        assert_eq!(
            found_message(&estimated, &song()),
            "From local (estimated timing)."
        );

        let plain = Lyrics {
            lines: vec![line(0, "a"), line(0, "b")],
            synced: false,
            instrumental: false,
            source: "local".into(),
        };
        assert_eq!(found_message(&plain, &song()), "From local (not synced).");

        let instrumental = Lyrics {
            lines: Vec::new(),
            synced: false,
            instrumental: true,
            source: "lrclib".into(),
        };
        assert_eq!(
            found_message(&instrumental, &song()),
            "From lrclib: Never Gonna Give You Up — Rick Astley is instrumental."
        );
    }

    // ---- providers and targets ----

    #[test]
    fn provider_plan_defaults() {
        let config = Config::default();
        let plan = ProviderPlan::from_config(&config, PathBuf::from("/c/lyrics"));
        assert_eq!(plan.local_dir, config.lyrics_dir());
        assert_eq!(plan.lrclib_url.as_deref(), Some("https://lrclib.net"));
        assert_eq!(plan.cache_dir, Some(PathBuf::from("/c/lyrics")));
        let text = plan.describe();
        assert!(text.starts_with("local ("), "{text}");
        assert!(
            text.ends_with("lrclib (https://lrclib.net), cache"),
            "{text}"
        );
    }

    #[test]
    fn provider_plan_follows_the_settings() {
        let mut config = Config::default();
        config.lyrics.lyrics_dir = Some(PathBuf::from("/music/lrc"));
        config.lyrics.lrclib = false;
        config.lyrics.cache = false;
        let plan = ProviderPlan::from_config(&config, PathBuf::from("/c"));
        assert_eq!(
            plan,
            ProviderPlan {
                local_dir: PathBuf::from("/music/lrc"),
                lrclib_url: None,
                cache_dir: None,
            }
        );
        assert_eq!(plan.describe(), "local (/music/lrc)");

        config.lyrics.lrclib = true;
        config.lyrics.lrclib_url = " http://localhost:3000// ".into();
        let plan = ProviderPlan::from_config(&config, PathBuf::from("/c"));
        assert_eq!(plan.lrclib_url.as_deref(), Some("http://localhost:3000"));
        assert_eq!(
            plan.describe(),
            "local (/music/lrc), lrclib (http://localhost:3000)"
        );
    }

    #[test]
    fn target_plan_without_discord_client_id() {
        let config = Config::default();
        let path = Path::new("/home/me/.config/lyrix/config.toml");
        let plan = TargetPlan::from_config(&config, &RunArgs::default(), path);
        assert!(plan.console);
        assert_eq!(plan.discord_client_id, None);
        assert_eq!(plan.names(), vec!["console"]);
        assert_eq!(plan.notes.len(), 1);
        let note = &plan.notes[0];
        assert!(note.contains("discord.client_id"), "{note}");
        assert!(note.contains("client_id = "), "{note}");
        assert!(note.contains(&path.display().to_string()), "{note}");
    }

    #[test]
    fn target_plan_with_discord() {
        let mut config = Config::default();
        config.discord.client_id = " 1234567890 ".into();
        let plan = TargetPlan::from_config(&config, &RunArgs::default(), Path::new("c.toml"));
        assert!(plan.console);
        assert_eq!(plan.discord_client_id.as_deref(), Some("1234567890"));
        assert_eq!(plan.names(), vec!["console", "discord"]);
        assert!(plan.notes.is_empty());
        assert!(plan.off.is_empty());
    }

    #[test]
    fn target_plan_flags_switch_targets_off() {
        let mut config = Config::default();
        config.discord.client_id = "1234567890".into();
        let quiet = RunArgs {
            no_discord: false,
            quiet: true,
        };
        let plan = TargetPlan::from_config(&config, &quiet, Path::new("c.toml"));
        assert_eq!(plan.names(), vec!["discord"]);

        let no_discord = RunArgs {
            no_discord: true,
            quiet: false,
        };
        let plan = TargetPlan::from_config(&config, &no_discord, Path::new("c.toml"));
        assert_eq!(plan.names(), vec!["console"]);
        // --no-discord means no note about the client id either.
        config.discord.client_id = String::new();
        let plan = TargetPlan::from_config(&config, &no_discord, Path::new("c.toml"));
        assert!(plan.notes.is_empty());
    }

    #[test]
    fn target_plan_with_nothing_on_explains_why() {
        let mut config = Config::default();
        config.console.enabled = false;
        config.discord.enabled = false;
        let plan = TargetPlan::from_config(&config, &RunArgs::default(), Path::new("c.toml"));
        assert!(plan.names().is_empty());
        let message = plan.nothing_on_message(Path::new("c.toml"));
        assert!(message.contains("console.enabled is off"), "{message}");
        assert!(message.contains("discord.enabled is off"), "{message}");
        assert!(message.contains("c.toml"), "{message}");

        let config = Config::default();
        let both = RunArgs {
            no_discord: true,
            quiet: true,
        };
        let plan = TargetPlan::from_config(&config, &both, Path::new("c.toml"));
        assert!(plan.names().is_empty());
        let message = plan.nothing_on_message(Path::new("c.toml"));
        assert!(message.contains("--quiet"), "{message}");
        assert!(message.contains("--no-discord"), "{message}");
    }

    #[test]
    fn advanced_notes_warn_for_each_option() {
        assert!(advanced_notes(&Config::default()).is_empty());

        let mut config = Config::default();
        config.advanced.discord_custom_status = true;
        assert_eq!(
            advanced_notes(&config),
            vec![
                BAN_WARNING.to_string(),
                "discord_custom_status is switched on, but stays off until you set \
                 advanced.accept_ban_risk = true."
                    .to_string(),
            ]
        );

        config.advanced.spotify_cookie_lyrics = true;
        config.advanced.accept_ban_risk = true;
        assert_eq!(
            advanced_notes(&config),
            vec![
                BAN_WARNING.to_string(),
                "discord_custom_status is not included in this build yet, so it stays off."
                    .to_string(),
                BAN_WARNING.to_string(),
                "spotify_cookie_lyrics is not included in this build yet, so it stays off."
                    .to_string(),
            ]
        );
    }

    #[test]
    fn startup_line_names_everything_in_use() {
        let plan = ProviderPlan {
            local_dir: PathBuf::from("/music/lrc"),
            lrclib_url: Some("https://lrclib.net".into()),
            cache_dir: Some(PathBuf::from("/c")),
        };
        assert_eq!(
            startup_line("mpris", &plan, &["console", "discord"], false),
            "Lyrix is running. Reading mpris; lyrics from local (/music/lrc), lrclib \
             (https://lrclib.net), cache; showing on console, discord. Press Ctrl+C to stop."
        );
        let line = startup_line("windows-media", &plan, &["discord"], true);
        assert!(line.contains("Paused"), "{line}");
        assert!(line.contains("lyrix resume"), "{line}");
    }

    // ---- config init / show / check ----

    #[test]
    fn default_config_text_reads_back_as_the_defaults() {
        let text = default_config_text().unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed, Config::default());
        assert!(text.starts_with("# Lyrix settings."));
    }

    #[test]
    fn default_config_text_warns_above_advanced() {
        let text = default_config_text().unwrap();
        assert!(text.contains(BAN_WARNING));
        let lines: Vec<&str> = text.lines().collect();
        let header = lines
            .iter()
            .position(|l| l.trim() == "[advanced]")
            .expect("an [advanced] section");
        // The comment block right above the header contains the warning.
        let comment: Vec<&str> = lines[..header]
            .iter()
            .rev()
            .take_while(|l| l.starts_with('#'))
            .copied()
            .collect();
        assert!(!comment.is_empty());
        assert!(
            comment.iter().any(|l| l.contains(BAN_WARNING)),
            "{comment:?}"
        );
        // The warning appears only there.
        assert_eq!(text.matches(BAN_WARNING).count(), 1);
    }

    #[test]
    fn default_config_text_comments_every_section() {
        let text = default_config_text().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if section_header(line).is_some() {
                assert!(
                    i > 0 && lines[i - 1].starts_with('#'),
                    "no comment above {line}"
                );
            }
        }
        for section in [
            "general", "status", "privacy", "lyrics", "sources", "discord", "console", "advanced",
        ] {
            assert!(text.contains(&format!("[{section}]")), "{section}");
        }
    }

    #[test]
    fn commented_out_options_are_valid_when_uncommented() {
        let text = default_config_text().unwrap();
        let uncommented = text
            .replace("# lyrics_dir = ", "lyrics_dir = ")
            .replace("# macos_adapter_dir = ", "macos_adapter_dir = ");
        let parsed: Config = toml::from_str(&uncommented).unwrap();
        assert_eq!(
            parsed.lyrics.lyrics_dir,
            Some(Config::default().lyrics_dir())
        );
        assert_eq!(
            parsed.sources.macos_adapter_dir,
            Some(PathBuf::from("/path/to/mediaremote-adapter"))
        );
    }

    #[test]
    fn section_header_examples() {
        assert_eq!(section_header("[advanced]"), Some("advanced"));
        assert_eq!(section_header("  [general]  "), Some("general"));
        assert_eq!(section_header("[[array]]"), None);
        assert_eq!(section_header("[]"), None);
        assert_eq!(section_header("key = [1]"), None);
        assert_eq!(section_header("profanity_words = []"), None);
    }

    #[tokio::test]
    async fn config_init_writes_defaults_through_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        let path_arg = path.to_str().unwrap();
        let cli = parse(&["--config", path_arg, "config", "init"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 0);

        assert!(path.exists());
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(BAN_WARNING));
        assert_eq!(text, default_config_text().unwrap());
    }

    #[tokio::test]
    async fn config_init_refuses_to_overwrite_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[general]\npoll_interval_ms = 250\n").unwrap();
        let path_arg = path.to_str().unwrap();

        let cli = parse(&["--config", path_arg, "config", "init"]).unwrap();
        let err = run(cli).await.unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("already exists"), "{message}");
        assert!(message.contains("--force"), "{message}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[general]\npoll_interval_ms = 250\n"
        );

        let cli = parse(&["--config", path_arg, "config", "init", "--force"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 0);
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(BAN_WARNING));
    }

    #[test]
    fn config_init_reports_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let message = config_init(&path, false).unwrap();
        assert!(message.contains(&path.display().to_string()), "{message}");
    }

    #[test]
    fn config_init_refuses_a_folder_in_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::create_dir(&path).unwrap();
        assert!(config_init(&path, false).is_err());
        assert!(config_init(&path, true).is_err());
        assert!(path.is_dir());
    }

    #[tokio::test]
    async fn config_path_show_and_check_through_the_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let path_arg = path.to_str().unwrap();

        let cli = parse(&["--config", path_arg, "config", "path"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 0);

        // No file yet: the defaults are shown and checked.
        let cli = parse(&["--config", path_arg, "config", "show"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 0);
        let cli = parse(&["--config", path_arg, "config", "check"]).unwrap();
        assert_eq!(
            run(cli).await.unwrap(),
            0,
            "warnings alone are not a failure"
        );

        // An error-level issue fails the check.
        std::fs::write(&path, "[general]\npoll_interval_ms = 10\n").unwrap();
        let cli = parse(&["--config", path_arg, "config", "check"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 1);

        // A clean config passes.
        std::fs::write(&path, "[discord]\nclient_id = \"1234567890\"\n").unwrap();
        let cli = parse(&["--config", path_arg, "config", "check"]).unwrap();
        assert_eq!(run(cli).await.unwrap(), 0);

        // A file that is not TOML is an error naming the file.
        std::fs::write(&path, "this is = = not toml").unwrap();
        for sub in ["show", "check"] {
            let cli = parse(&["--config", path_arg, "config", sub]).unwrap();
            let err = format!("{:#}", run(cli).await.unwrap_err());
            assert!(err.contains("config.toml"), "{err}");
        }
    }

    #[tokio::test]
    async fn run_stops_on_config_errors_before_starting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let path_arg = path.to_str().unwrap();
        std::fs::write(&path, "[general]\npoll_interval_ms = 10\n").unwrap();
        let cli = parse(&["--config", path_arg]).unwrap();
        let err = format!("{:#}", run(cli).await.unwrap_err());
        assert!(err.contains("errors"), "{err}");

        std::fs::write(&path, "not toml at all [").unwrap();
        let cli = parse(&["--config", path_arg, "run"]).unwrap();
        assert!(run(cli).await.is_err());
    }

    // ---- pause / resume ----

    #[test]
    fn pause_creates_the_marker_and_resume_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("deep").join("folder").join("paused");

        let message = resume(&marker).unwrap();
        assert!(message.contains("not paused"), "{message}");

        let message = pause(&marker).unwrap();
        assert!(message.starts_with("Paused."), "{message}");
        assert!(marker.is_file());

        let message = pause(&marker).unwrap();
        assert!(message.starts_with("Already paused"), "{message}");
        assert!(marker.is_file());

        let message = resume(&marker).unwrap();
        assert!(message.starts_with("Resumed."), "{message}");
        assert!(!marker.exists());

        let message = resume(&marker).unwrap();
        assert!(message.contains("not paused"), "{message}");
    }

    #[test]
    fn resume_reports_a_marker_it_cannot_remove() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("paused");
        std::fs::create_dir(&marker).unwrap();
        assert!(resume(&marker).is_err());
    }

    // ---- offset ----

    #[test]
    fn apply_offset_sets_and_resets_one_song() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("offsets.toml");

        assert_eq!(
            apply_offset(
                &path,
                "rick astley - never gonna give you up",
                OffsetChange::Set(250)
            )
            .unwrap(),
            250
        );
        assert_eq!(
            apply_offset(&path, "daft punk - one more time", OffsetChange::Set(-100)).unwrap(),
            -100
        );
        let offsets = Offsets::load(&path).unwrap();
        assert_eq!(offsets.get("rick astley - never gonna give you up"), 250);
        assert_eq!(offsets.get("daft punk - one more time"), -100);

        // Setting again replaces the value, it does not add.
        assert_eq!(
            apply_offset(
                &path,
                "rick astley - never gonna give you up",
                OffsetChange::Set(400)
            )
            .unwrap(),
            400
        );

        assert_eq!(
            apply_offset(
                &path,
                "rick astley - never gonna give you up",
                OffsetChange::Reset
            )
            .unwrap(),
            0
        );
        let offsets = Offsets::load(&path).unwrap();
        assert!(!offsets
            .songs
            .contains_key("rick astley - never gonna give you up"));
        assert_eq!(offsets.get("daft punk - one more time"), -100);

        assert_eq!(
            apply_offset(&path, "daft punk - one more time", OffsetChange::Set(0)).unwrap(),
            0
        );
        assert!(Offsets::load(&path).unwrap().songs.is_empty());
    }

    #[test]
    fn apply_offset_keeps_a_broken_offsets_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offsets.toml");
        std::fs::write(&path, "songs = not valid").unwrap();
        assert!(apply_offset(&path, "a - b", OffsetChange::Set(1)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "songs = not valid");
    }

    #[test]
    fn offset_key_matches_the_song_key_of_the_normalized_track() {
        let (track, key) = offset_key(&song()).unwrap();
        assert_eq!(track, lyrix::matcher::normalize_track(&song()));
        assert_eq!(key, lyrix::matcher::song_key(&track));
        assert!(key.contains(" - "), "{key}");

        // A video title from a channel names the real song.
        let video = Track {
            title: "Rick Astley - Never Gonna Give You Up (Official Video)".into(),
            artist: "Rick Astley - Topic".into(),
            ..Track::default()
        };
        let (_, video_key) = offset_key(&video).unwrap();
        assert_eq!(video_key, key);

        // Nothing to key on.
        for title in ["", "   ", "!!!"] {
            let t = Track {
                title: title.into(),
                artist: "Someone".into(),
                ..Track::default()
            };
            assert_eq!(offset_key(&t), None, "{title:?}");
        }
    }

    #[test]
    fn offset_message_examples() {
        let track = song();
        let later = offset_message(&track, 250);
        assert!(
            later.starts_with("Never Gonna Give You Up — Rick Astley: +250 ms (lines show later)."),
            "{later}"
        );
        let earlier = offset_message(&track, -100);
        assert!(
            earlier.contains(": -100 ms (lines show earlier)."),
            "{earlier}"
        );
        let none = offset_message(&track, 0);
        assert!(none.contains(": no offset."), "{none}");
        assert!(!offset_message(&track, i64::MIN).is_empty());
    }

    // ---- cache clear ----

    #[test]
    fn clear_cache_removes_only_json_files_directly_inside() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("lyrics");
        std::fs::create_dir_all(cache.join("sub")).unwrap();
        std::fs::create_dir_all(cache.join("folder.json")).unwrap();
        for name in ["a1b2c3d4e5f60718.json", "0000000000000000.json", "x.json"] {
            std::fs::write(cache.join(name), "{}").unwrap();
        }
        for name in [
            "notes.txt",
            "song.lrc",
            ".a1b2.json.123-0.tmp",
            "json",
            "x.json.bak",
            // Distinct stem: on case-insensitive filesystems (macOS,
            // Windows) "x.JSON" would be the same file as "x.json".
            "y.JSON",
        ] {
            std::fs::write(cache.join(name), "keep").unwrap();
        }
        std::fs::write(cache.join("sub").join("nested.json"), "{}").unwrap();

        assert_eq!(clear_cache_dir(&cache).unwrap(), 3);

        let mut left: Vec<String> = std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                ".a1b2.json.123-0.tmp",
                "folder.json",
                "json",
                "notes.txt",
                "song.lrc",
                "sub",
                "x.json.bak",
                "y.JSON",
            ]
        );
        assert!(cache.join("sub").join("nested.json").exists());

        // Nothing left to remove.
        assert_eq!(clear_cache_dir(&cache).unwrap(), 0);
    }

    #[test]
    fn clear_cache_of_a_missing_folder_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(clear_cache_dir(&dir.path().join("missing")).unwrap(), 0);
    }

    #[test]
    fn clear_cache_of_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lyrics");
        std::fs::write(&file, "not a folder").unwrap();
        assert!(clear_cache_dir(&file).is_err());
        assert!(file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn clear_cache_removes_links_but_not_their_targets() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("lyrics");
        std::fs::create_dir_all(&cache).unwrap();
        let outside = dir.path().join("precious.json");
        std::fs::write(&outside, "{}").unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("link.json")).unwrap();
        let outside_dir = dir.path().join("dir.json");
        std::fs::create_dir(&outside_dir).unwrap();
        std::os::unix::fs::symlink(&outside_dir, cache.join("dirlink.json")).unwrap();

        assert_eq!(clear_cache_dir(&cache).unwrap(), 2);
        assert!(outside.exists());
        assert!(outside_dir.is_dir());
        assert!(std::fs::read_dir(&cache).unwrap().next().is_none());
    }

    #[test]
    fn cache_cleared_message_examples() {
        let dir = Path::new("/c/lyrics");
        assert_eq!(
            cache_cleared_message(0, dir),
            format!("Removed 0 cached songs from {}.", dir.display())
        );
        assert_eq!(
            cache_cleared_message(1, dir),
            format!("Removed 1 cached song from {}.", dir.display())
        );
        assert_eq!(
            cache_cleared_message(12, dir),
            format!("Removed 12 cached songs from {}.", dir.display())
        );
    }

    // ---- review: hints, advice and odd paths ----

    #[tokio::test]
    async fn overwrite_hint_keeps_the_config_file_in_use() {
        // Following the hint must replace this file, not the default one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my config.toml");
        std::fs::write(&path, "").unwrap();
        let path_arg = path.to_str().unwrap();
        let cli = parse(&["--config", path_arg, "config", "init"]).unwrap();
        let message = format!("{:#}", run(cli).await.unwrap_err());
        assert!(
            message.contains(&format!(
                "`lyrix --config \"{}\" config init --force`",
                path.display()
            )),
            "{message}"
        );
    }

    #[test]
    fn lyrix_command_adds_config_only_for_another_file() {
        assert_eq!(
            lyrix_command(&Config::default_path(), "config init"),
            "lyrix config init"
        );
        assert_eq!(
            lyrix_command(Path::new("/x/c.toml"), "config init --force"),
            format!(
                "lyrix --config \"{}\" config init --force",
                Path::new("/x/c.toml").display()
            )
        );
    }

    #[test]
    fn discord_note_names_the_config_file_in_use() {
        let path = Path::new("/somewhere/else.toml");
        let plan = TargetPlan::from_config(&Config::default(), &RunArgs::default(), path);
        let note = &plan.notes[0];
        assert!(note.contains(&lyrix_command(path, "config init")), "{note}");
        assert!(!note.contains('\n'), "one line: {note}");

        let default = Config::default_path();
        let plan = TargetPlan::from_config(&Config::default(), &RunArgs::default(), &default);
        assert!(plan.notes[0].contains("`lyrix config init`"), "{plan:?}");
    }

    #[test]
    fn nothing_on_advice_matches_why_each_target_is_off() {
        // Console off; Discord on but without a client id; no flags given.
        let mut config = Config::default();
        config.console.enabled = false;
        let plan = TargetPlan::from_config(&config, &RunArgs::default(), Path::new("c.toml"));
        assert!(plan.names().is_empty());
        let message = plan.nothing_on_message(Path::new("c.toml"));
        assert!(message.contains("console.enabled = true"), "{message}");
        assert!(message.contains("discord.client_id"), "{message}");
        assert!(!message.contains("--quiet"), "no flag was given: {message}");
        assert!(!message.contains("--no-discord"), "{message}");
        assert!(
            !message.contains("discord.enabled = true"),
            "Discord is already enabled: {message}"
        );
        assert!(message.contains("c.toml"), "{message}");

        // Flags only: the advice is to leave them out.
        config.console.enabled = true;
        config.discord.client_id = "1234567890".into();
        let both = RunArgs {
            no_discord: true,
            quiet: true,
        };
        let plan = TargetPlan::from_config(&config, &both, Path::new("c.toml"));
        let message = plan.nothing_on_message(Path::new("c.toml"));
        assert!(message.contains("leave out --quiet"), "{message}");
        assert!(message.contains("leave out --no-discord"), "{message}");
        assert!(!message.contains("console.enabled = true"), "{message}");

        // Discord switched off without a client id: both settings are needed.
        let mut config = Config::default();
        config.console.enabled = false;
        config.discord.enabled = false;
        let plan = TargetPlan::from_config(&config, &RunArgs::default(), Path::new("c.toml"));
        let message = plan.nothing_on_message(Path::new("c.toml"));
        assert!(
            message.contains("discord.enabled = true and discord.client_id"),
            "{message}"
        );
    }

    #[test]
    fn commented_lyrics_dir_stays_one_line_for_any_folder() {
        for dir in [
            "/home/me/.local/share/lyrix/lyrics",
            "C:\\Users\\me\\AppData\\Roaming\\Lyrix\\data\\lyrics",
            "/odd\nfolder/with \"quotes\" and 'apostrophes'\r\tand\u{7f}more",
            "/x'''y\nz",
        ] {
            let lines = section_extras("lyrics", Path::new(dir));
            let setting = lines
                .iter()
                .find(|l| l.starts_with("# lyrics_dir = "))
                .expect("a lyrics_dir line");
            assert!(!setting.contains(['\n', '\r']), "{setting:?}");
            let uncommented = setting.trim_start_matches("# ");
            let parsed: Config = toml::from_str(&format!("[lyrics]\n{uncommented}\n"))
                .unwrap_or_else(|e| panic!("{dir:?}: {e}"));
            assert_eq!(
                parsed.lyrics.lyrics_dir,
                Some(PathBuf::from(dir)),
                "{dir:?}"
            );
        }
    }

    #[test]
    fn one_line_toml_string_examples() {
        assert_eq!(one_line_toml_string("plain"), "\"plain\"");
        assert_eq!(one_line_toml_string("a\nb"), "\"a\\nb\"");
        assert_eq!(one_line_toml_string("q\"\\\r\n"), "\"q\\\"\\\\\\r\\n\"");
        assert_eq!(one_line_toml_string("\u{1}\n"), "\"\\u0001\\n\"");
        for s in ["", "日本語", "x\ny", "tab\tend", "\u{7f}\n"] {
            let rendered = one_line_toml_string(s);
            assert!(!rendered.contains(['\n', '\r']), "{rendered:?}");
            let value: toml::Value = toml::from_str(&format!("v = {rendered}")).unwrap();
            assert_eq!(value.get("v").and_then(|v| v.as_str()), Some(s));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_ends_the_wait_for_shutdown() {
        let mut shutdown = Box::pin(shutdown_signal());
        // Poll first so the handlers are in place before the signal arrives
        // (without them SIGTERM would end this test process).
        let early = tokio::time::timeout(Duration::from_millis(50), &mut shutdown).await;
        assert!(early.is_err(), "nothing was sent yet");

        let pid = std::process::id().to_string();
        let sent = match std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
        {
            Ok(status) => status.success(),
            Err(e) => {
                eprintln!("skipped: cannot run kill: {e}");
                return;
            }
        };
        assert!(sent);
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("SIGTERM should end the wait");
    }

    #[test]
    fn one_line_replaces_control_characters() {
        assert_eq!(one_line("a\nb\r\tc\u{1b}"), "a b  c ");
        assert_eq!(one_line("日本語 ♪"), "日本語 ♪");
    }
}
