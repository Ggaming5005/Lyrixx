// The engine's View (CONTRACT.md) as a store, plus the rules for reading it.

/** Keeps the latest View and tells subscribers about each new one. */
export function createViewStore(api) {
  let view = null;
  const subscribers = new Set();
  const publish = (next) => {
    view = next;
    for (const callback of subscribers) {
      callback(view);
    }
  };
  api.onView(publish);
  api.getView().then((first) => {
    // An event may have arrived first; it is newer than this answer.
    if (!view) {
      publish(first);
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
