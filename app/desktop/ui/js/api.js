// The only way the window talks to Lyrix: the commands and the event in
// desktop/CONTRACT.md. Inside the app they go through Tauri; in a plain
// browser the demo backend in mock.js answers instead.
//
// Every call returns a promise; a failed command rejects with a
// human-readable string.

const tauri = window.__TAURI__;
const inApp = Boolean(tauri && tauri.core && tauri.event);

let invoke;
let listen;

if (inApp) {
  invoke = (command, args) => tauri.core.invoke(command, args);
  listen = (event, callback) => tauri.event.listen(event, (message) => callback(message.payload));
} else {
  // Loaded on first use: a top-level await would keep Safari 14 (macOS 11)
  // from loading the window's modules.
  const mock = import('./mock.js').then(({ createMockBackend }) =>
    createMockBackend(new URLSearchParams(window.location.search)),
  );
  invoke = (command, args) => mock.then((backend) => backend.invoke(command, args));
  listen = (event, callback) => mock.then((backend) => backend.listen(event, callback));
}

/** True when the window shows demo data instead of the app. */
export const isDemo = !inApp;

export const api = {
  /** @returns {Promise<View>} */
  getView: () => invoke('get_view'),
  /** @returns {Promise<Settings>} */
  getSettings: () => invoke('get_settings'),
  /** @returns {Promise<{ saved: boolean, issues: Issue[] }>} */
  saveSettings: (config) => invoke('save_settings', { config }),
  /** @returns {Promise<string>} */
  previewStatus: ({ template, line, next, title, artist, album }) =>
    invoke('preview_status', { template, line, next, title, artist, album }),
  /** @returns {Promise<boolean>} */
  setPaused: (paused) => invoke('set_paused', { paused }),
  /** @returns {Promise<number>} the song's new offset */
  adjustOffset: (deltaMs) => invoke('adjust_offset', { deltaMs }),
  /** @returns {Promise<number>} */
  resetOffset: () => invoke('reset_offset'),
  /** @returns {Promise<number>} how many cached lyrics were removed */
  clearCache: () => invoke('clear_cache'),
  /** @param {'config' | 'lyrics' | 'cache' | 'logs'} which */
  openFolder: (which) => invoke('open_folder', { which }),
  /** Opens an https:// link in the browser. */
  openUrl: (url) => invoke('open_url', { url }),
  /** @returns {Promise<boolean>} */
  getAutostart: () => invoke('get_autostart'),
  /** @returns {Promise<boolean>} */
  setAutostart: (enabled) => invoke('set_autostart', { enabled }),
  /** @returns {Promise<{ version: string, os: 'windows' | 'macos' | 'linux', discordDefaultClientId: string }>} */
  appInfo: () => invoke('app_info'),
  quit: () => invoke('quit'),
  /**
   * Calls `callback(view)` on every `lyrix://view` event.
   * @returns {Promise<() => void>} stops listening
   */
  onView: (callback) => listen('lyrix://view', callback),
  /**
   * Calls `callback()` on `lyrix://closing`: the window is about to be closed.
   * @returns {Promise<() => void>} stops listening
   */
  onClosing: (callback) => listen('lyrix://closing', () => callback()),
};
