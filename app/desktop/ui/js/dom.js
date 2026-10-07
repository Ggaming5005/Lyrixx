// Small DOM and formatting helpers shared by every page.

import { icon } from './icons.js';

/**
 * Creates an element. `props` keys:
 * - `class`, `text`, `style` (object), `dataset` (object)
 * - `on<Event>` functions, e.g. `onClick`
 * - `icon`: prepends an icon by name
 * - anything else becomes an attribute (`false`/`null` skip it, `true` sets it empty)
 * Children may be nodes, strings, arrays or null.
 */
export function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === null || value === undefined || value === false) {
      continue;
    }
    if (key === 'class') {
      el.className = value;
    } else if (key === 'text') {
      el.textContent = value;
    } else if (key === 'style') {
      for (const [name, v] of Object.entries(value)) {
        el.style.setProperty(name, v);
      }
    } else if (key === 'dataset') {
      Object.assign(el.dataset, value);
    } else if (key === 'icon') {
      el.append(icon(value));
    } else if (key.startsWith('on') && typeof value === 'function') {
      el.addEventListener(key.slice(2).toLowerCase(), value);
    } else {
      el.setAttribute(key, value === true ? '' : String(value));
    }
  }
  append(el, children);
  return el;
}

function append(el, children) {
  for (const child of children) {
    if (child === null || child === undefined || child === false) {
      continue;
    }
    if (Array.isArray(child)) {
      append(el, child);
    } else if (child instanceof Node) {
      el.append(child);
    } else {
      el.append(document.createTextNode(String(child)));
    }
  }
}

/** Replaces the children of `el`; like `h`, it skips null children and flattens arrays. */
export function fill(el, ...children) {
  el.replaceChildren();
  append(el, children);
  return el;
}

/** Sets `textContent` only when it changed (avoids needless layout work). */
export function setText(el, text) {
  const value = text ?? '';
  if (el.textContent !== value) {
    el.textContent = value;
  }
}

export function debounce(fn, ms) {
  let timer = 0;
  const debounced = (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), ms);
  };
  debounced.flush = (...args) => {
    clearTimeout(timer);
    fn(...args);
  };
  debounced.cancel = () => clearTimeout(timer);
  return debounced;
}

let uid = 0;
/** A unique element id with a readable prefix. */
export function nextId(prefix = 'id') {
  uid += 1;
  return `${prefix}-${uid}`;
}

/** `m:ss` (or `h:mm:ss`) for a duration in milliseconds. */
export function formatTime(ms) {
  const total = Math.max(0, Math.floor((ms || 0) / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, '0');
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, '0')}:${seconds}` : `${minutes}:${seconds}`;
}

/** `+0.25 s`, `−1.50 s` or `0.00 s` for an offset in milliseconds. */
export function formatOffset(ms) {
  const value = (Math.abs(ms) / 1000).toFixed(2);
  if (ms > 0) {
    return `+${value} s`;
  }
  if (ms < 0) {
    return `−${value} s`;
  }
  return `${value} s`;
}

const KNOWN_APPS = [
  ['spotify', 'Spotify'],
  ['applemusic', 'Apple Music'],
  ['apple.music', 'Apple Music'],
  ['itunes', 'iTunes'],
  ['music.ui', 'Apple Music'],
  ['msedge', 'Edge'],
  ['chrome', 'Chrome'],
  ['chromium', 'Chromium'],
  ['firefox', 'Firefox'],
  ['brave', 'Brave'],
  ['opera', 'Opera'],
  ['vivaldi', 'Vivaldi'],
  ['safari', 'Safari'],
  ['tidal', 'TIDAL'],
  ['deezer', 'Deezer'],
  ['youtube', 'YouTube Music'],
  ['vlc', 'VLC'],
  ['foobar', 'foobar2000'],
  ['musicbee', 'MusicBee'],
  ['aimp', 'AIMP'],
  ['zunemusic', 'Media Player'],
  ['mediaplayer', 'Media Player'],
  ['rhythmbox', 'Rhythmbox'],
  ['elisa', 'Elisa'],
  ['strawberry', 'Strawberry'],
  ['clementine', 'Clementine'],
  ['amberol', 'Amberol'],
  ['cider', 'Cider'],
  ['plexamp', 'Plexamp'],
  ['mpv', 'mpv'],
];

/**
 * A friendly name for the app id a source reports, e.g. `Spotify.exe`,
 * `org.mpris.MediaPlayer2.spotify` or `com.apple.Music` → `Spotify` / `Apple Music`.
 */
export function appName(id) {
  if (!id) {
    return '';
  }
  const lower = id.toLowerCase();
  for (const [needle, name] of KNOWN_APPS) {
    if (lower.includes(needle)) {
      return name;
    }
  }
  let name = id.replace(/^org\.mpris\.MediaPlayer2\./i, '').replace(/\.exe$/i, '');
  name = name.split(/[.!\\/]/).filter(Boolean).pop() || name;
  name = name.replace(/\.instance\d+$/i, '').replace(/[_-]+/g, ' ');
  return name.charAt(0).toUpperCase() + name.slice(1);
}

/** The lyrics provider name as people know it. */
export function sourceName(source) {
  switch (source) {
    case 'lrclib':
      return 'LRCLIB';
    case 'musixmatch':
      return 'Musixmatch';
    case 'netease':
      return 'NetEase';
    case 'kugou':
      return 'Kugou';
    case 'local':
      return 'Your files';
    case 'cache':
      return 'Saved lyrics';
    default:
      return source ? source.charAt(0).toUpperCase() + source.slice(1) : '';
  }
}

/** A stable 32-bit hash of a string (FNV-1a). */
export function hashString(text) {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i += 1) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return hash >>> 0;
}

/** Shows a short message at the bottom of the window. */
export function toast(message, { tone = 'ok', iconName } = {}) {
  const region = document.querySelector('.toasts');
  if (!region) {
    return;
  }
  const item = h(
    'div',
    { class: 'toast', dataset: { tone } },
    icon(iconName || (tone === 'error' ? 'alert' : 'check')),
    h('span', { text: message }),
  );
  region.append(item);
  setTimeout(() => {
    item.classList.add('is-leaving');
    setTimeout(() => item.remove(), 260);
  }, tone === 'error' ? 4200 : 1800);
}

/** Runs an API call; a rejection becomes an error toast (and `undefined`). */
export async function attempt(promise, failure = 'That did not work') {
  try {
    return await promise;
  } catch (error) {
    toast(`${failure}: ${error}`, { tone: 'error' });
    return undefined;
  }
}
