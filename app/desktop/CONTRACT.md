# Window ↔ backend contract

The Lyrix window (`ui/`) talks to the Rust side (`src/`) only through the
Tauri commands and the one event below. `ui/js/api.js` wraps them; when the
page is opened outside Tauri (a plain browser, screenshots), `ui/js/mock.js`
answers instead with demo data, so the UI can be built and checked without
the app.

All keys are camelCase except inside `config`, which is the settings file's
own structure (snake_case, exactly the TOML sections and keys).

## Event

### `lyrix://view`

Sent whenever the view changes, at most every 100 ms. The payload is a
`View` (below). The window also calls `get_view` once on load.

## Types

```ts
type View = {
  running: boolean;          // false while the engine restarts or failed to start
  error: string | null;      // why the engine is not running, for the user
  source: string;            // "windows-media" | "mpris" | "macos" | ""
  paused: boolean;           // sharing paused by the user
  now: Now | null;           // null: nothing is playing
  status: Status | null;     // what Discord etc. are asked to show; null = cleared
  targets: Target[];
};

type Now = {
  title: string; artist: string; album: string | null;
  durationMs: number | null;
  app: string;               // "Spotify.exe", "org.mpris.MediaPlayer2.spotify", ...
  playing: boolean;
  positionMs: number;        // position at positionAtUnixMs
  positionAtUnixMs: number;  // while playing: pos(t) = positionMs + (t - positionAtUnixMs) * rate
  rate: number;
  artwork: string | null;    // https:, http: or data: URL
  songKey: string;
  songOffsetMs: number;      // this song's offset, positive = lyrics later
  globalOffsetMs: number;    // general.offset_ms
  lyrics:
    | { state: "searching" }
    | { state: "notFound" }
    | { state: "found"; lines: { startMs: number; text: string }[];
        synced: boolean; instrumental: boolean; source: string };
};
// Current line at position p: the last line with
// startMs <= p - (songOffsetMs + globalOffsetMs). Empty text = a break.
// Unsynced lyrics already carry estimated startMs values (synced = false).

type Status = {
  text: string;              // exactly what the targets show (before each target's length limit)
  kind: "line" | "instrumental" | "noLyrics";
  line: string | null;
  estimated: boolean;
};

type Target = {
  id: "discord" | string;
  state: "starting" | "showing" | "cleared" | "waiting" | "rateLimited" | "retrying" | "off";
  detail: string | null;     // e.g. "Discord is not running"
};

type Issue = { severity: "error" | "warning"; message: string };

type Settings = {
  config: Config;            // the whole settings file as JSON (see Config in src/config.rs)
  defaults: Config;          // the built-in defaults, for "reset" buttons
  issues: Issue[];           // problems in the saved settings
  paths: { config: string; lyricsDir: string; cacheDir: string; logs: string };
};
```

`Config` mirrors `src/config.rs`: sections `general`, `status`, `privacy`,
`lyrics`, `sources`, `discord`, `console`, `advanced`. The window must send
the whole object back (keep fields it does not show unchanged).

## Commands

| Command | Arguments | Returns | Notes |
| --- | --- | --- | --- |
| `get_view` | – | `View` | |
| `get_settings` | – | `Settings` | |
| `save_settings` | `{ config: Config }` | `{ saved: boolean; issues: Issue[] }` | Not saved when any issue is an error. Saving restarts the engine with the new settings. |
| `preview_status` | `{ template: string; line?: string; next?: string; title: string; artist: string; album?: string }` | `string` | Renders a status template exactly like the engine. |
| `set_paused` | `{ paused: boolean }` | `boolean` | Pauses or resumes sharing (the same marker `lyrix pause` uses). |
| `adjust_offset` | `{ deltaMs: number }` | `number` | Changes the playing song's offset by `deltaMs`; returns the new offset. Error when nothing plays. |
| `reset_offset` | – | `number` | Sets the playing song's offset to 0. |
| `clear_cache` | – | `number` | Removes cached lyrics; returns how many. |
| `open_folder` | `{ which: "config" \| "lyrics" \| "cache" \| "logs" }` | – | Opens the folder (created if missing) in the file manager. |
| `open_url` | `{ url: string }` | – | Opens an `https://` URL in the browser; anything else is refused. |
| `get_autostart` | – | `boolean` | Whether Lyrix starts at login. |
| `set_autostart` | `{ enabled: boolean }` | `boolean` | |
| `app_info` | – | `{ version: string; os: "windows" \| "macos" \| "linux"; discordDefaultClientId: string }` | |
| `quit` | – | – | Clears statuses and exits. |

Errors are rejected promises carrying a human-readable string.
