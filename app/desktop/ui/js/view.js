// The engine's View (CONTRACT.md) as a store, plus the rules for reading it.

/**
 * How long the window keeps the last song on screen once the engine runs
 * again after a restart, until the new engine reports it.
 */
export const RESTART_HOLD_MS = 1500;

/** True while the engine starts or restarts: not running, and no error. */
export function isRestarting(view) {
  return Boolean(view) && !view.running && !view.error;
}

/**
 * `'starting'` or `'restarting'` while the engine (re)starts and has not
 * looked yet (see createViewStore), otherwise null.
 */
export function pendingOf(view) {
  return view?.pending || (isRestarting(view) ? 'restarting' : null);
}

/** "Starting…" or "Restarting…" while `pendingOf(view)`, otherwise null. */
export function pendingLabel(view) {
  const pending = pendingOf(view);
  if (!pending) {
    return null;
  }
  return pending === 'starting' ? 'Starting…' : 'Restarting…';
}

/**
 * Keeps the View to draw and tells subscribers about each new one.
 *
 * Saving the settings restarts the engine (CONTRACT.md, Restarts): the view
 * goes `running: false` without an error, then `running: true` with nothing
 * playing (and no targets) until the new engine has looked, then back to the
 * song, whose lyrics are looked up again. So that a save never flashes
 * "Lyrix stopped", "Nothing playing" or a lyrics search, the store draws
 * those views differently, during the restart and for up to `holdMs` once
 * the engine runs again:
 *
 * - until the new engine has looked, the view carries `pending: 'restarting'`
 *   (`'starting'` when the app starts: no view ran before) and keeps the last
 *   song as `now`;
 * - the same song while its lyrics are looked up again keeps its lyrics.
 */
export function createViewStore(api, { holdMs = RESTART_HOLD_MS } = {}) {
  /** The view drawn now. */
  let view = null;
  /** The newest view from the engine, as it came. */
  let latest = null;
  let everRan = false;
  /**
   * From a restart until the song is back: `first` for the app's start,
   * `backAt` once the engine runs again.
   */
  let restart = null;
  let holdTimer = 0;
  const subscribers = new Set();

  const publish = (next) => {
    view = next;
    for (const callback of subscribers) {
      callback(view);
    }
  };

  /** `next` as drawn during a restart, or null to draw it as it is. */
  function bridge(next) {
    const last = view?.now || null;
    if (isRestarting(next) || (next.running && !next.now)) {
      return { ...next, now: last, pending: restart.first ? 'starting' : 'restarting' };
    }
    // The same song while its lyrics are looked up again: keep the lyrics.
    if (
      last &&
      next.running &&
      next.now.songKey === last.songKey &&
      next.now.lyrics.state === 'searching' &&
      last.lyrics.state !== 'searching'
    ) {
      return { ...next, now: { ...next.now, lyrics: last.lyrics } };
    }
    return null;
  }

  function receive(next) {
    latest = next;
    clearTimeout(holdTimer);
    if (isRestarting(next)) {
      restart = { first: !everRan, backAt: null };
    } else if (!next.running) {
      restart = null;
    } else if (restart && restart.backAt === null) {
      restart.backAt = Date.now();
    }
    everRan = everRan || next.running;

    let remaining = 0;
    if (restart) {
      remaining = restart.backAt === null ? Infinity : holdMs - (Date.now() - restart.backAt);
    }
    const drawn = remaining > 0 ? bridge(next) : null;
    if (!drawn) {
      restart = null;
      publish(next);
      return;
    }
    publish(drawn);
    if (Number.isFinite(remaining)) {
      // Nothing newer by then: draw the engine's view as it is.
      holdTimer = setTimeout(() => {
        restart = null;
        publish(latest);
      }, remaining);
    }
  }

  api.onView(receive);
  api.getView().then((first) => {
    // An event may have arrived first; it is newer than this answer.
    if (!latest) {
      receive(first);
    }
  });
  return {
    get: () => view,
    /** Calls `callback(view)` now (when there is one) and on every change. */
    subscribe(callback) {
      subscribers.add(callback);
      if (view) {
        callback(view);
      }
      return () => subscribers.delete(callback);
    },
  };
}

/**
 * The playback position at `unixMs`: `positionMs + (t - positionAtUnixMs) * rate`
 * while playing, clamped to the song.
 */
export function positionAt(now, unixMs = Date.now()) {
  if (!now) {
    return 0;
  }
  let position = now.positionMs;
  if (now.playing) {
    position += (unixMs - now.positionAtUnixMs) * (now.rate || 1);
  }
  if (now.durationMs) {
    position = Math.min(position, now.durationMs);
  }
  return Math.max(0, position);
}

/** The offset applied to lyric timings: this song's plus the global one. */
export function totalOffset(now) {
  return (now?.songOffsetMs || 0) + (now?.globalOffsetMs || 0);
}

/**
 * Whether found lyrics can place a line in time: synced ones, or unsynced
 * ones spread over the song. Unsynced lyrics of a song with no known length
 * all start at 0; no line of those is ever the current one (CONTRACT.md).
 */
export function hasTiming(lyrics) {
  return Boolean(lyrics.synced) || lyrics.lines.some((line) => line.startMs > 0);
}

/** Index of the last line with `startMs <= shiftedMs`, or -1 before the first. */
export function lineIndexAt(lines, shiftedMs) {
  let low = 0;
  let high = lines.length;
  while (low < high) {
    const mid = (low + high) >>> 1;
    if (lines[mid].startMs <= shiftedMs) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return low - 1;
}

/**
 * Where the song is in its lyrics, following CONTRACT.md: the current line is
 * the last one with `startMs <= position - offset`; an empty line is a break;
 * before the first line with text is the intro (index -1).
 */
export function lyricMoment(lines, shiftedMs) {
  const firstText = lines.findIndex((line) => line.text.trim() !== '');
  const index = lineIndexAt(lines, shiftedMs);
  if (firstText < 0 || index < firstText) {
    return { index: -1, kind: 'intro' };
  }
  return { index, kind: lines[index].text.trim() ? 'line' : 'break' };
}

/** The Discord target, or null when Discord is not among the targets. */
export function discordOf(view) {
  return view?.targets?.find((target) => target.id === 'discord') || null;
}

/**
 * How the status is doing, for the sidebar and the live strip:
 * `{ tone: 'ok' | 'warn' | 'danger' | 'idle', short, long }`.
 */
export function sharingSummary(view) {
  if (!view) {
    return { tone: 'idle', short: 'Starting…', long: 'Starting…' };
  }
  const pending = pendingOf(view);
  if (pending) {
    return pending === 'starting'
      ? { tone: 'idle', short: 'Starting…', long: 'Lyrix is starting…' }
      : { tone: 'idle', short: 'Restarting…', long: 'Lyrix is restarting…' };
  }
  if (!view.running) {
    return { tone: 'danger', short: 'Lyrix stopped', long: 'Lyrix is not running' };
  }
  if (view.paused) {
    return { tone: 'idle', short: 'Paused', long: 'Your status is cleared' };
  }
  const discord = discordOf(view);
  if (!discord) {
    return { tone: 'idle', short: 'Discord is off', long: 'Discord is off in Connections' };
  }
  switch (discord.state) {
    case 'showing':
      return { tone: 'ok', short: 'Live on Discord', long: 'Showing on Discord' };
    case 'starting':
      return { tone: 'warn', short: 'Connecting…', long: 'Connecting to Discord…' };
    case 'cleared':
      return view.now
        ? { tone: 'idle', short: 'On, nothing to show', long: 'Connected, nothing to show right now' }
        : { tone: 'idle', short: 'On, nothing playing', long: 'Connected, waiting for music' };
    case 'waiting':
      return { tone: 'warn', short: 'Waiting for Discord', long: 'Open Discord to show your status' };
    case 'rateLimited':
      return { tone: 'warn', short: 'Slowed down', long: 'Discord asked to slow down; trying again soon' };
    case 'retrying':
      return { tone: 'warn', short: 'Retrying…', long: discord.detail || 'Retrying…' };
    case 'off':
      return { tone: 'danger', short: 'Discord stopped', long: discord.detail ? `Off: ${discord.detail}` : 'Off' };
    default:
      return { tone: 'idle', short: discord.state, long: discord.state };
  }
}
