// The demo backend: answers the commands in desktop/CONTRACT.md when the
// window runs in a plain browser (development and screenshots), so every
// state can be shown without the app. URL parameters:
//
//   ?scenario=playing | searching | notFound | instrumental | idle | paused |
//             discordWaiting | error   (also: estimated, untimed, musicPaused)
//   ?t=<seconds>   where the demo song starts (each scenario has a default)
//   ?os=windows | macos | linux   (default: this browser's system)
//   ?boot=<ms>     start like the app, with no engine running for that long
//
// ?page= and ?theme= are read by the window itself (main.js, theme-boot.js).
//
// Saving the settings restarts the demo engine like the app does (stopped,
// then running with nothing playing until its first look, then the lyrics
// looked up again). `window.lyrixDemo` lets checks (tools/flows.mjs) send an
// event such as `lyrix://closing`, edit the demo settings file, and read
// what `save_settings` received.

import { PLAYER_IDS, SONGS } from './mock-data.js';
import { filterProfanity, renderTemplate } from './template.js';

const VERSION = '0.1.0';
const BAN_WARNING = 'USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.';
const DEFAULT_CLIENT_ID = '1556752305653809272';
const LAST_RESORT_TEMPLATE = '{title} · {artist}';
/** Same as the engine: the view is sent at most every 100 ms. */
const TICK_MS = 100;
/**
 * A restart in the demo: stopping (the app clears Discord first), then from
 * the start to the new engine's first look, then the lyrics lookup.
 */
const RESTART = { stopMs: 450, firstLookMs: 400, lookupMs: 300 };

/** Demo songs played in each scenario, and where the first one starts (seconds). */
const SCENARIOS = {
  playing: { queue: [0, 1], start: 52.9 },
  searching: { queue: [0], start: 31 },
  notFound: { queue: [1], start: 40 },
  instrumental: { queue: [3], start: 41 },
  estimated: { queue: [2], start: 61 },
  untimed: { queue: [4], start: 61 },
  idle: { queue: [], start: 0 },
  paused: { queue: [0], start: 52.9 },
  musicPaused: { queue: [0], start: 97.5 },
  discordWaiting: { queue: [0], start: 52.9 },
  error: { queue: [], start: 0 },
};

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

function defaultConfig() {
  return {
    general: { poll_interval_ms: 500, offset_ms: 0 },
    status: {
      line_template: '🎵 {line}',
      no_lyrics_template: '{title} · {artist}',
      instrumental_text: '♪',
      show_when_paused: false,
      profanity_filter: false,
      profanity_words: [],
    },
    privacy: { blocked_apps: [], blocked_artists: [], title_only: false },
    lyrics: {
      lyrics_dir: null,
      cache: true,
      lrclib: true,
      lrclib_url: 'https://lrclib.net',
      netease: true,
      kugou: true,
    },
    sources: { preferred_apps: [], macos_adapter_dir: null },
    discord: {
      enabled: true,
      client_id: DEFAULT_CLIENT_ID,
      min_interval_ms: 4500,
      show_progress: true,
      large_image: 'gradient_musical_note_app_icon',
    },
    console: { enabled: true },
    advanced: { accept_ban_risk: false, discord_custom_status: false, spotify_cookie_lyrics: false },
  };
}

/** The checks of `Config::validate`, in the same order and words. */
function validate(config) {
  const issues = [];
  const error = (message) => issues.push({ severity: 'error', message });
  const warning = (message) => issues.push({ severity: 'warning', message });

  const poll = Number(config.general.poll_interval_ms);
  if (!(poll >= 100)) {
    error(`general.poll_interval_ms is ${poll} ms; it must be at least 100 ms.`);
  }
  if (config.discord.enabled && !String(config.discord.client_id).trim()) {
    warning(
      'discord.enabled is on but discord.client_id is empty, so Discord Rich Presence cannot connect. ' +
        'Set discord.client_id to a Discord application id.',
    );
  }
  for (const key of ['discord_custom_status', 'spotify_cookie_lyrics']) {
    if (!config.advanced[key]) {
      continue;
    }
    warning(
      config.advanced.accept_ban_risk
        ? `advanced.${key} is on. ${BAN_WARNING}`
        : `advanced.${key} is on but advanced.accept_ban_risk is false, so it stays off. ${BAN_WARNING}`,
    );
  }
  if (!config.discord.enabled && !config.console.enabled) {
    warning(
      'No target is enabled (discord.enabled and console.enabled are both off), so lyrics are not shown anywhere.',
    );
  }
  if (!config.status.line_template.trim()) {
    warning('status.line_template is empty, so lyric lines would show as an empty status.');
  }
  if (!config.status.no_lyrics_template.trim()) {
    warning('status.no_lyrics_template is empty, so songs without lyrics would show as an empty status.');
  }
  return issues;
}

function demoPaths(os) {
  switch (os) {
    case 'windows':
      return {
        config: 'C:\\Users\\you\\AppData\\Roaming\\Lyrix\\config\\config.toml',
        lyricsDir: 'C:\\Users\\you\\AppData\\Roaming\\Lyrix\\data\\lyrics',
        cacheDir: 'C:\\Users\\you\\AppData\\Local\\Lyrix\\cache\\lyrics',
        logs: 'C:\\Users\\you\\AppData\\Roaming\\Lyrix\\data\\logs',
      };
    case 'macos':
      return {
        config: '/Users/you/Library/Application Support/Lyrix/config.toml',
        lyricsDir: '/Users/you/Library/Application Support/Lyrix/lyrics',
        cacheDir: '/Users/you/Library/Caches/Lyrix/lyrics',
        logs: '/Users/you/Library/Application Support/Lyrix/logs',
      };
    default:
      return {
        config: '/home/you/.config/lyrix/config.toml',
        lyricsDir: '/home/you/.local/share/lyrix/lyrics',
        cacheDir: '/home/you/.cache/lyrix/lyrics',
        logs: '/home/you/.local/share/lyrix/logs',
      };
  }
}

function detectOs() {
  const platform = `${navigator.userAgentData?.platform || ''} ${navigator.platform || ''} ${navigator.userAgent}`;
  if (/mac/i.test(platform)) {
    return 'macos';
  }
  if (/win/i.test(platform)) {
    return 'windows';
  }
  return 'linux';
}

const fold = (text) =>
  text
    .normalize('NFKD')
    .replace(/\p{M}/gu, '')
    .toLowerCase()
    .replace(/&/g, ' and ')
    .replace(/[^\p{L}\p{N}\s]/gu, '')
    .replace(/\s+/g, ' ')
    .trim();

/** Where `position` falls: the intro, a break or a line, and the next line's text. */
function lookupLine(lines, position) {
  const firstText = lines.findIndex((line) => line.text.trim());
  const next = (from) => lines.slice(from).find((line) => line.text.trim())?.text.trim() ?? null;
  let index = -1;
  for (let i = 0; i < lines.length && lines[i].startMs <= position; i += 1) {
    index = i;
  }
  if (position < 0 || firstText < 0 || index < firstText) {
    return { kind: 'intro', next: next(0) };
  }
  const text = lines[index].text.trim();
  return { kind: text ? 'line' : 'break', text, next: next(index + 1) };
}

const clone = (value) => (value === undefined ? undefined : JSON.parse(JSON.stringify(value)));

/** Creates the demo backend: `{ invoke(command, args), listen(event, callback) }`. */
export function createMockBackend(params) {
  const scenario = Object.prototype.hasOwnProperty.call(SCENARIOS, params.get('scenario'))
    ? params.get('scenario')
    : 'playing';
  const os = ['windows', 'macos', 'linux'].includes(params.get('os')) ? params.get('os') : detectOs();
  const plan = SCENARIOS[scenario];
  const startSeconds = Number.parseFloat(params.get('t'));

  const defaults = defaultConfig();
  let config = defaultConfig();
  let autostart = true;
  let cachedLyrics = 14;
  let sharingPaused = scenario === 'paused';
  const songOffsets = new Map();
  /** Event name → callbacks. */
  const listeners = new Map();
  let lastSent = '';
  /** 'running', or during a restart 'stopped' and then 'started' (not looked yet). */
  let engine = 'running';
  let restarts = Promise.resolve();
  let firstLook = 0;
  /** Every config `save_settings` received, oldest first. */
  const saved = [];

  let queueIndex = 0;
  let track = null;
  let lyricsReadyAt = 0;

  function startSong(index, positionMs) {
    const song = SONGS[plan.queue[index]];
    const now = Date.now();
    track = {
      song,
      app: PLAYER_IDS[song.player][os],
      playing: scenario !== 'musicPaused',
      positionMs,
      positionAtUnixMs: now,
      songKey: `${fold(song.artist)} - ${fold(song.title)}`,
    };
    // A new song looks its lyrics up first; the first song is already loaded.
    lyricsReadyAt = positionMs === 0 ? now + 1200 : now;
  }

  if (plan.queue.length > 0) {
    startSong(0, (Number.isFinite(startSeconds) ? startSeconds : plan.start) * 1000);
  }

  function position() {
    if (!track) {
      return 0;
    }
    const elapsed = track.playing ? Date.now() - track.positionAtUnixMs : 0;
    return Math.min(track.song.durationMs ?? Infinity, Math.max(0, track.positionMs + elapsed));
  }

  function lyricsNow() {
    if (scenario === 'searching' || Date.now() < lyricsReadyAt) {
      return { state: 'searching' };
    }
    if (scenario === 'notFound') {
      return { state: 'notFound' };
    }
    return track.song.lyrics;
  }

  function isBlockedApp(app) {
    return config.privacy.blocked_apps.some((b) => b.trim() && app.toLowerCase().includes(b.trim().toLowerCase()));
  }

  function isBlockedArtist(artist) {
    return config.privacy.blocked_artists.some((b) => fold(b) && fold(b) === fold(artist));
  }

  /** Mirrors `compose_status` in src/status.rs. */
  function composeStatus(song, lyrics, positionMs, playing, offsetMs) {
    const settings = config.status;
    if (!playing && !settings.show_when_paused) {
      return null;
    }
    if (isBlockedArtist(song.artist)) {
      return null;
    }
    // Like `has_timing`: unsynced lines that all start at 0 cannot be placed.
    const usable =
      !config.privacy.title_only &&
      lyrics.state === 'found' &&
      !lyrics.instrumental &&
      lyrics.lines.some((line) => line.text.trim()) &&
      (lyrics.synced || lyrics.lines.some((line) => line.startMs > 0))
        ? lyrics
        : null;

    let moment;
    if (usable) {
      const at = lookupLine(usable.lines, positionMs - offsetMs);
      moment =
        at.kind === 'line'
          ? { kind: 'line', template: settings.line_template, line: at.text, next: at.next }
          : { kind: 'instrumental', template: settings.instrumental_text, line: null, next: at.next };
      moment.estimated = !usable.synced;
    } else {
      moment = {
        kind: lyrics.state === 'found' && lyrics.instrumental ? 'instrumental' : 'noLyrics',
        template: settings.no_lyrics_template,
        line: null,
        next: null,
        estimated: false,
      };
    }

    const ctx = { line: moment.line, next: moment.next, title: song.title, artist: song.artist, album: song.album };
    let text = renderTemplate(moment.template, ctx);
    if (!text) {
      text = renderTemplate(settings.no_lyrics_template, ctx);
    }
    if (!text) {
      text = renderTemplate(LAST_RESORT_TEMPLATE, ctx);
    }
    let line = moment.line;
    if (settings.profanity_filter) {
      // The real app falls back to a built-in word list; the demo only masks listed words.
      text = filterProfanity(text, settings.profanity_words);
      line = line && filterProfanity(line, settings.profanity_words);
    }
    return { text, kind: moment.kind, line, estimated: moment.estimated };
  }

  function discordTarget(status) {
    if (scenario === 'discordWaiting') {
      return { id: 'discord', state: 'waiting', detail: 'Discord is not running; trying again every 15 s' };
    }
    if (!/^\d{17,20}$/.test(config.discord.client_id.trim())) {
      return {
        id: 'discord',
        state: 'off',
        detail: `Discord did not accept the application id "${config.discord.client_id.trim()}"`,
      };
    }
    return { id: 'discord', state: status ? 'showing' : 'cleared', detail: null };
  }

  function buildView() {
    const source = { windows: 'windows-media', macos: 'macos', linux: 'mpris' }[os];
    if (engine !== 'running') {
      // View::stopped(None), then `running` once the new engine started.
      return {
        running: engine === 'started',
        error: null,
        source: '',
        paused: sharingPaused,
        now: null,
        status: null,
        targets: [],
      };
    }
    if (scenario === 'error') {
      return {
        running: false,
        error: 'Could not read what is playing: the system media service did not answer within 5 seconds.',
        source,
        paused: sharingPaused,
        now: null,
        status: null,
        targets: [],
      };
    }

    const nowPlaying = track && !isBlockedApp(track.app) ? track : null;
    let now = null;
    let status = null;
    if (nowPlaying) {
      const { song } = nowPlaying;
      const lyrics = lyricsNow();
      const songOffsetMs = songOffsets.get(nowPlaying.songKey) || 0;
      const globalOffsetMs = Number(config.general.offset_ms) || 0;
      now = {
        title: song.title,
        artist: song.artist,
        album: song.album,
        durationMs: song.durationMs,
        app: nowPlaying.app,
        playing: nowPlaying.playing,
        positionMs: Math.round(nowPlaying.positionMs),
        positionAtUnixMs: nowPlaying.positionAtUnixMs,
        rate: 1,
        artwork: song.artwork,
        songKey: nowPlaying.songKey,
        songOffsetMs,
        globalOffsetMs,
        lyrics,
      };
      if (!sharingPaused) {
        status = composeStatus(song, lyrics, position(), nowPlaying.playing, songOffsetMs + globalOffsetMs);
      }
    }
    return {
      running: true,
      error: null,
      source,
      paused: sharingPaused,
      now,
      status,
      targets: config.discord.enabled ? [discordTarget(status)] : [],
    };
  }

  function emit(event, payload) {
    for (const callback of listeners.get(event) || []) {
      callback(clone(payload));
    }
  }

  function tick() {
    if (track && track.playing && track.song.durationMs && position() >= track.song.durationMs) {
      queueIndex = (queueIndex + 1) % plan.queue.length;
      startSong(queueIndex, 0);
    }
    const view = buildView();
    const json = JSON.stringify(view);
    if (json !== lastSent) {
      lastSent = json;
      emit('lyrix://view', view);
    }
  }

  setInterval(tick, TICK_MS);

  /** A new engine runs; it looks (and finds the song) a little later. */
  function launch() {
    engine = 'started';
    tick();
    firstLook = setTimeout(() => {
      engine = 'running';
      if (track) {
        lyricsReadyAt = Date.now() + RESTART.lookupMs;
      }
      tick();
    }, RESTART.firstLookMs);
  }

  /** Like `Supervisor::restart`: one at a time, done once the new engine runs. */
  function restart() {
    restarts = restarts.then(async () => {
      clearTimeout(firstLook);
      engine = 'stopped';
      tick();
      await sleep(RESTART.stopMs);
      launch();
    });
    return restarts;
  }

  // ?boot=<ms>: start like the app, with no engine for that long.
  const boot = Number.parseInt(params.get('boot'), 10);
  if (boot > 0) {
    engine = 'stopped';
    restarts = sleep(boot).then(launch);
  }

  // For checks in a browser (tools/flows.mjs); the app has no such thing.
  window.lyrixDemo = {
    /** Sends an event to the window, e.g. `lyrix://closing`. */
    emit,
    /** Changes the demo settings file, like an edit made outside the window. */
    editSettings(path, value) {
      const keys = path.split('.');
      const last = keys.pop();
      keys.reduce((node, key) => node[key], config)[last] = clone(value);
    },
    /** The configs `save_settings` received, oldest first. */
    saved: () => clone(saved),
  };

  const commands = {
    get_view: () => buildView(),
    get_settings: () => ({
      config: clone(config),
      defaults: clone(defaults),
      issues: validate(config),
      paths: demoPaths(os),
    }),
    save_settings: async ({ config: next }) => {
      saved.push(clone(next));
      const issues = validate(next);
      if (issues.some((issue) => issue.severity === 'error')) {
        return { saved: false, issues };
      }
      config = clone(next);
      await restart();
      return { saved: true, issues };
    },
    preview_status: (args) => renderTemplate(args.template, args),
    set_paused: ({ paused }) => {
      sharingPaused = Boolean(paused);
      tick();
      return sharingPaused;
    },
    adjust_offset: ({ deltaMs }) => {
      if (!track) {
        throw 'Nothing is playing';
      }
      const value = (songOffsets.get(track.songKey) || 0) + Number(deltaMs);
      songOffsets.set(track.songKey, value);
      tick();
      return value;
    },
    reset_offset: () => {
      if (!track) {
        throw 'Nothing is playing';
      }
      songOffsets.set(track.songKey, 0);
      tick();
      return 0;
    },
    clear_cache: () => {
      const removed = cachedLyrics;
      cachedLyrics = 0;
      return removed;
    },
    open_folder: ({ which }) => {
      if (!['config', 'lyrics', 'cache', 'logs'].includes(which)) {
        throw `Unknown folder: ${which}`;
      }
      console.info(`[demo] open folder: ${which}`);
      return null;
    },
    open_url: ({ url }) => {
      if (!/^https:\/\//.test(url)) {
        throw 'Only https:// links can be opened';
      }
      window.open(url, '_blank', 'noopener,noreferrer');
      return null;
    },
    get_autostart: () => autostart,
    set_autostart: ({ enabled }) => {
      autostart = Boolean(enabled);
      return autostart;
    },
    app_info: () => ({ version: VERSION, os, discordDefaultClientId: DEFAULT_CLIENT_ID }),
    quit: () => {
      console.info('[demo] quit');
      return null;
    },
  };

  return {
    invoke(command, args = {}) {
      return new Promise((resolve, reject) => {
        // A short delay, like a round trip to the app.
        setTimeout(() => {
          const handler = commands[command];
          if (!handler) {
            reject(`Unknown command: ${command}`);
            return;
          }
          new Promise((done) => done(handler(clone(args)))).then(
            (result) => resolve(clone(result)),
            (error) => reject(String(error)),
          );
        }, 16);
      });
    },
    listen(event, callback) {
      if (!listeners.has(event)) {
        listeners.set(event, new Set());
      }
      listeners.get(event).add(callback);
      return Promise.resolve(() => listeners.get(event).delete(callback));
    },
  };
}
