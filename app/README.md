# Lyrix

Lyrix shows the lyric line you're hearing right now as your status. It runs on your own computer, reads what's playing from the operating system, and finds synced lyrics on its own. No server, no shared login, no audio recording.

```
▶ Paper Satellites — Juniper & The Lowlights
♫ 🎵 We folded maps into paper planes
♫ 🎵 And threw them out of the seventh floor
```

In Discord, your profile and the member list show **Listening to Lyrix** with **🎵 We folded maps into paper planes** under it, and a progress bar for the song.

This page covers the `lyrix` command, every setting and development. For the app, downloads and troubleshooting, see the [main README](../README.md).

## How it works

1. **What's playing.** Every music app tells the operating system what it's playing, so it can show in the media controls. Lyrix reads that: the title, artist, album, length and position.
   - Windows: the system media sessions (Spotify, Apple Music, Tidal, Deezer, browsers and more).
   - Linux: MPRIS, which nearly every player and browser supports.
   - macOS: Spotify and Apple Music directly. Install [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter) and set `sources.macos_adapter_dir` to cover every app in the Now Playing widget.
2. **Lyrics.** Lyrix looks in this order: your own `.lrc` and `.txt` files, lyrics it found before (cache), [LRCLIB](https://lrclib.net), a free open lyrics database, then [NetEase Cloud Music](https://music.163.com) and [Kugou](https://www.kugou.com). NetEase and Kugou need no account but aren't official APIs (they are the endpoints their own players use), so they may stop answering; credit lines such as `作词 : …` or `Lyrics by: …` around their lyrics are left out, and versions without singing (instrumental, karaoke, `伴奏`) are skipped unless that is what's playing. Your files are never cached, so an `.lrc` you add wins even for a song looked up before; a plain `.txt` is used only when no synced lyrics are found. Turning a source on looks songs up again that the cache only has plain lyrics or "not found" for. Lyrics without timing are spread across the song and marked as estimated. When nothing is found, the status names the song instead.
3. **Your status.** Lyrix keeps its own clock between readings, picks the line for the current moment, and sends it to each target no faster than that target allows.

## Getting started

### The app

Download the installer for your system from the latest [release](https://github.com/Ggaming5005/Lyrixx/releases), or from the latest CI run (the **Artifacts** section): **Lyrix-Windows** (setup `.exe`), **Lyrix-macOS** (`.dmg`) or **Lyrix-Linux** (`.AppImage` and `.deb`).

Lyrix opens its window and lives in the tray (the menu bar on macOS). Closing the window keeps it running; **Quit Lyrix** in the tray menu clears your status and stops it. It can start at login, in the tray only. Without a tray to show its icon (GNOME without the AppIndicator extension), the window always opens and closing it quits Lyrix. The app and the `lyrix` command share their settings, so either one can change them; run one of them at a time, since both would set the same status.

### The command

Download the `lyrix` binary for your system from the latest CI run (**lyrix-cli-…** under **Artifacts**), or build it:

```sh
cd app
cargo build --release -p lyrix      # the binary is target/release/lyrix
```

Then:

```sh
lyrix config init          # writes a config file with every setting
lyrix now                  # checks Lyrix can see your music
lyrix lyrics --artist "Artist" --title "Song title"
lyrix                      # runs until Ctrl+C
```

### Discord

Rich Presence works with no setup: keep the Discord desktop app running on the same computer and your profile shows **Listening to Lyrix** with the current lyric line. No login or token is needed.

To show a different name, create your own application in the [Discord Developer Portal](https://discord.com/developers/applications), copy its **Application ID**, run `lyrix config path`, and set it in that file:

```toml
[discord]
client_id = "your application id"
large_image = "your image name"
```

`large_image` is the picture next to the lyrics: the name of an image you uploaded to that application under **Rich Presence → Art Assets** (square, at least 512×512), or an `https://` link to a picture. Leave it empty (`""`) for no picture. New images can take a few minutes to show up in Discord.

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
| `lyrics.netease` / `lyrics.kugou` | `true` | Ask NetEase Cloud Music / Kugou when LRCLIB has no synced lyrics |
| `discord.large_image` | Lyrix's art | The picture next to the lyrics: an image name from your application's Art Assets, or an `https://` link. `""` shows none |
| `discord.min_interval_ms` | `4500` | Fewest milliseconds between Discord updates. Discord takes at most 5 updates per 20 s and silently drops the rest, so lower values act as 4500 |

### Advanced mode

> **USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.**

The `[advanced]` section has switches for options that use your own account in ways the services don't allow (Discord custom status with your account token, Spotify lyrics with your browser login). Nothing in it runs unless `accept_ban_risk = true`. Their connectors are not included in this build yet.

## Development

`app` is a Cargo workspace: the `lyrix` library and command at its root, and the desktop app (Tauri 2) in `desktop`, whose window (`desktop/ui`, plain HTML, CSS and JavaScript) talks to Rust only as `desktop/CONTRACT.md` describes. On Linux the app needs the WebKitGTK 4.1, Ayatana AppIndicator, librsvg and xdo development packages.

```sh
cd app
cargo test --workspace     # unit tests
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p lyrix --lib sources:: -- --ignored   # Linux: MPRIS tests against a private D-Bus
cargo test -p lyrix --test e2e_linux -- --nocapture   # Linux: the real binary end to end (needs dbus-daemon, else skipped)
cargo run -p lyrix-desktop # the app, with its log on stderr too
cd desktop && npx --yes @tauri-apps/cli@2 build   # installers, in target/release/bundle
```

The end-to-end test (also part of `cargo test` on Linux) runs `lyrix now`, `lyrix run` and `lyrix lyrics` against a private D-Bus with a fake MPRIS player, a mock LRCLIB server and a fake Discord IPC socket, all local.

CI runs format, lint and tests on Windows, macOS and Linux, then builds and uploads the installers and the `lyrix` command. Pushing a `v*` tag builds the installers into a draft GitHub release.
