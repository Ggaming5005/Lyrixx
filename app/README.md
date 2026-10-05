# Lyrix

Lyrix shows the lyric line you're hearing right now as your status. It runs on your own computer, reads what's playing from the operating system, and finds synced lyrics on its own. No server, no shared login, no audio recording.

```
▶ Never Gonna Give You Up — Rick Astley
♫ 🎵 We're no strangers to love
♫ 🎵 You know the rules and so do I
```

In Discord, your profile and the member list show **Listening to 🎵 We're no strangers to love**, with a progress bar for the song.

## How it works

1. **What's playing.** Every music app tells the operating system what it's playing, so it can show in the media controls. Lyrix reads that: the title, artist, album, length and position.
   - Windows: the system media sessions (Spotify, Apple Music, Tidal, Deezer, browsers and more).
   - Linux: MPRIS, which nearly every player and browser supports.
   - macOS: Spotify and Apple Music directly. Install [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter) and set `sources.macos_adapter_dir` to cover every app in the Now Playing widget.
2. **Lyrics.** Lyrix looks in this order: lyrics it found before (cache), your own `.lrc` and `.txt` files, then [LRCLIB](https://lrclib.net), a free open lyrics database. Lyrics without timing are spread across the song and marked as estimated. When nothing is found, the status names the song instead.
3. **Your status.** Lyrix keeps its own clock between readings, picks the line for the current moment, and sends it to each target no faster than that target allows.

## Getting started

Download the `lyrix` binary for your system from the latest CI run (the **Artifacts** section), or build it:

```sh
cd app
cargo build --release      # the binary is target/release/lyrix
```

Then:

```sh
lyrix config init          # writes a config file with every setting
lyrix now                  # checks Lyrix can see your music
lyrix lyrics --artist "Rick Astley" --title "Never Gonna Give You Up"
lyrix                      # runs until Ctrl+C
```

### Discord

Rich Presence works with no setup: keep the Discord desktop app running on the same computer and your profile shows **Listening to Lyrix** with the current lyric line. No login or token is needed.

To show a different name, create your own application in the [Discord Developer Portal](https://discord.com/developers/applications), copy its **Application ID**, run `lyrix config path`, and set it in that file:

```toml
[discord]
client_id = "your application id"
```

## Commands

| Command | What it does |
| --- | --- |
| `lyrix` or `lyrix run` | Shows lyrics as your status until Ctrl+C. `--no-discord`, `--quiet` |
| `lyrix now` | Prints what's playing |
| `lyrix lyrics` | Prints the lyrics for the song playing now, or for `--artist` + `--title` |
| `lyrix pause` / `lyrix resume` | Clears your status and stops updating, or starts again |
| `lyrix offset +300` | Lyrics too early for this song? Shows them 300 ms later from now on. `offset reset` removes it |
| `lyrix config init` / `path` / `show` / `check` | Creates, finds, prints or checks the settings |
| `lyrix cache clear` | Forgets cached lyrics |

Add `-v` for more detail in the logs.

## Settings

The config file is TOML. The main settings:

| Setting | Default | Meaning |
| --- | --- | --- |
| `status.line_template` | `🎵 {line}` | Text while a line is sung. Placeholders: `{line}` `{next}` `{title}` `{artist}` `{album}` |
| `status.no_lyrics_template` | `{title} · {artist}` | Text when there are no lyrics |
| `status.instrumental_text` | `♪` | Text during intros and instrumental breaks |
| `status.show_when_paused` | `false` | Keep the status while paused |
| `status.profanity_filter` | `false` | Mask swear words, e.g. for a work account |
| `privacy.blocked_apps` | `[]` | Players to ignore, e.g. `["chrome"]` |
| `privacy.blocked_artists` | `[]` | Artists never shown |
| `privacy.title_only` | `false` | Show the song, never lyric lines |
| `general.offset_ms` | `0` | Shift every song's lyrics (positive is later) |
| `lyrics.lyrics_dir` | data folder | Your own `.lrc` / `.txt` files, named `Artist - Title.lrc` |
| `discord.min_interval_ms` | `2000` | Fewest milliseconds between Discord updates |

### Advanced mode

> **USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.**

The `[advanced]` section has switches for options that use your own account in ways the services don't allow (Discord custom status with your account token, Spotify lyrics with your browser login). Nothing in it runs unless `accept_ban_risk = true`. Their connectors are not included in this build yet.

## Development

```sh
cd app
cargo test                 # unit tests
cargo clippy --all-targets -- -D warnings
cargo test --lib sources:: -- --ignored   # Linux: MPRIS tests against a private D-Bus
```

CI runs format, lint, tests and a release build on Windows, macOS and Linux, and uploads each binary.
