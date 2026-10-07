// The settings file as the window edits it: a working copy of `config` that
// saves itself (debounced) through `save_settings`, and the issues that came
// back, so each field can show its own message.

import { debounce, toast } from './dom.js';

const SAVE_DELAY_MS = 650;

const clone = (value) => JSON.parse(JSON.stringify(value));

function getPath(object, path) {
  return path.split('.').reduce((value, key) => (value == null ? undefined : value[key]), object);
}

function setPath(object, path, value) {
  const keys = path.split('.');
  const last = keys.pop();
  const parent = keys.reduce((node, key) => node[key], object);
  parent[last] = value;
}

/** Friendlier words for the messages `Config::validate` writes, keyed by setting. */
const FRIENDLY = [
  [/^advanced\.\w+ is on but advanced\.accept_ban_risk is false, so it stays off\./, (m) => m.replace(/^.*?stays off\./, 'Stays off until you accept the risk.')],
  [/^advanced\.\w+ is on\./, (m) => m.replace(/^advanced\.\w+ is on\./, 'On.')],
  [/^status\.line_template is empty/, 'Empty: lyric lines would show as an empty status.'],
  [/^status\.no_lyrics_template is empty/, 'Empty: songs without lyrics would show as an empty status.'],
  [/discord\.client_id is empty/, 'Add an application ID, or Discord cannot connect.'],
  [/^No target is enabled/, 'Discord is off, so your lyrics are not shown anywhere.'],
  [
    /^lyrics\.musixmatch_key does not look like/,
    'This doesn’t look like a Musixmatch API key (32 letters and digits), so Musixmatch will probably refuse it.',
  ],
];

function friendly(issue) {
  const match = FRIENDLY.find(([pattern]) => pattern.test(issue.message));
  let message = issue.message;
  if (match) {
    message = typeof match[1] === 'function' ? match[1](message) : match[1];
  }
  return { severity: issue.severity, message };
}

/** The setting keys a message is about (`section.key` words in it). */
function keysIn(message) {
  return [...message.matchAll(/\b([a-z]+\.[a-z_]+)\b/g)].map((match) => match[1]);
}

/**
 * Creates the model. Call `load()` once; `get(path)` and `set(path, value)`
 * use dotted paths such as `status.line_template`.
 */
export function createSettingsModel(api) {
  let settings = null;
  let config = null;
  let issues = [];
  /** `save_settings` calls not answered yet. */
  let inFlight = 0;
  /** A save was asked for while another was in flight. */
  let again = false;
  /** Changes not sent yet. */
  let dirty = false;
  const subscribers = new Set();

  const emit = () => subscribers.forEach((callback) => callback());
  const pending = () => dirty || again || inFlight > 0;

  /** Sends the whole settings as they are now. */
  async function send() {
    inFlight += 1;
    dirty = false;
    try {
      const result = await api.saveSettings(clone(config));
      issues = result.issues || [];
      if (result.saved) {
        toast('Saved');
      } else {
        toast('Not saved: fix the highlighted setting', { tone: 'error' });
      }
    } catch (error) {
      toast(`Could not save: ${error}`, { tone: 'error' });
    } finally {
      inFlight -= 1;
      emit();
      if (again && inFlight === 0) {
        again = false;
        send();
      }
    }
  }

  /** Saves one at a time: a save asked for meanwhile runs after it, once. */
  function save() {
    if (inFlight > 0) {
      again = true;
      return;
    }
    send();
  }

  const scheduleSave = debounce(save, SAVE_DELAY_MS);

  return {
    async load() {
      settings = await api.getSettings();
      config = clone(settings.config);
      issues = settings.issues || [];
      emit();
    },
    /**
     * Loads the settings again (they may have been edited in the file), unless
     * a change is waiting or being saved, which would be lost.
     */
    async reload() {
      if (!config || pending()) {
        return;
      }
      const fresh = await api.getSettings();
      if (pending()) {
        return;
      }
      const same =
        JSON.stringify(fresh.config) === JSON.stringify(config) &&
        JSON.stringify(fresh.issues || []) === JSON.stringify(issues) &&
        JSON.stringify(fresh.paths) === JSON.stringify(settings.paths);
      settings = fresh;
      if (!same) {
        config = clone(fresh.config);
        issues = fresh.issues || [];
        emit();
      }
    },
    /**
     * Sends a waiting change at once, without waiting for the pause or for a
     * save in flight (that one already sent its settings): the window is
     * closing.
     */
    flushNow() {
      scheduleSave.cancel();
      if (!dirty && !again) {
        return;
      }
      again = false;
      send();
    },
    get loaded() {
      return config !== null;
    },
    get paths() {
      return settings?.paths || {};
    },
    get(path) {
      return getPath(config, path);
    },
    getDefault(path) {
      return getPath(settings?.defaults, path);
    },
    /** Changes one setting; saved after a short pause (or now with `{ now: true }`). */
    set(path, value, { now = false } = {}) {
      if (JSON.stringify(getPath(config, path)) === JSON.stringify(value)) {
        return;
      }
      setPath(config, path, value);
      dirty = true;
      emit();
      if (now) {
        scheduleSave.flush();
      } else {
        scheduleSave();
      }
    },
    /** Saves a pending change right away (e.g. when a field loses focus). */
    flush() {
      if (dirty) {
        scheduleSave.flush();
      }
    },
    /** Issues that name `path`, as `{ severity, message }` with friendlier wording. */
    issuesFor(path) {
      return issues.filter((issue) => keysIn(issue.message).includes(path)).map(friendly);
    },
    /** Issues about none of the settings matching `shown` (shown next to their own field). */
    looseIssues(shown) {
      return issues.filter((issue) => !keysIn(issue.message).some((key) => shown.test(key))).map(friendly);
    },
    subscribe(callback) {
      subscribers.add(callback);
      return () => subscribers.delete(callback);
    },
  };
}
