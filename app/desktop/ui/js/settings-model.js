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
  let saving = false;
  let again = false;
  let dirty = false;
  const subscribers = new Set();

  const emit = () => subscribers.forEach((callback) => callback());

  async function save() {
    if (saving) {
      again = true;
      return;
    }
    saving = true;
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
      saving = false;
      emit();
      if (again) {
        again = false;
        save();
      }
    }
  }

  const scheduleSave = debounce(save, SAVE_DELAY_MS);

  return {
    async load() {
      settings = await api.getSettings();
      config = clone(settings.config);
      issues = settings.issues || [];
      emit();
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
