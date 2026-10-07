// Starts the window: theme, platform, the view store, the settings model,
// the sidebar and the pages (each built the first time it is shown).
//
// No top-level await here or in the modules it imports: macOS 11 can run
// Safari 14's WebKit, which cannot load such modules.

import { api } from './api.js';
import { createBackdrop } from './backdrop.js';
import { attempt } from './dom.js';
import { hydrateIcons } from './icons.js';
import { createNowPage } from './now.js';
import { createAdvancedPage } from './pages/advanced.js';
import { createConnectionsPage } from './pages/connections.js';
import { createLyricsPage } from './pages/lyrics.js';
import { createSettingsPage } from './pages/settings.js';
import { createSettingsModel } from './settings-model.js';
import { createViewStore, pendingOf, sharingSummary } from './view.js';

const PAGES = {
  now: { create: createNowPage, needsSettings: false },
  lyrics: { create: createLyricsPage, needsSettings: true },
  connections: { create: createConnectionsPage, needsSettings: true },
  settings: { create: createSettingsPage, needsSettings: true },
  advanced: { create: createAdvancedPage, needsSettings: true },
};

const params = new URLSearchParams(window.location.search);
const root = document.documentElement;

async function main() {
  hydrateIcons();

  // Theme: ?theme= wins; otherwise follow the system as it changes.
  if (!['dark', 'light'].includes(params.get('theme'))) {
    const query = window.matchMedia('(prefers-color-scheme: light)');
    query.addEventListener('change', () => root.setAttribute('data-theme', query.matches ? 'light' : 'dark'));
  }

  const info = await api.appInfo().catch(() => ({ version: '', os: 'windows', discordDefaultClientId: '' }));
  root.dataset.os = info.os;
  if (info.os === 'macos') {
    document.querySelector('.drag-region').hidden = false;
  }

  const store = createViewStore(api);
  const model = createSettingsModel(api);
  const backdrop = createBackdrop(document.querySelector('.backdrop-art'));
  const settingsReady = model.load().catch((error) => {
    console.error('Could not load the settings', error);
  });

  // The window is about to close: take what the field being edited holds
  // (fields save on blur), then send a change that still waits for its save.
  api.onClosing(() => {
    if (document.activeElement instanceof HTMLElement) {
      document.activeElement.blur();
    }
    model.flushNow();
  });

  // Back in the window: pick up settings edited in the file meanwhile.
  const reloadSettings = () => {
    settingsReady.then(() => model.reload()).catch((error) => console.error('Could not reload the settings', error));
  };
  window.addEventListener('focus', reloadSettings);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) {
      reloadSettings();
    }
  });

  const controllers = {};
  let current = null;

  function navigate(name) {
    if (window.location.hash !== `#${name}`) {
      window.location.hash = name;
    } else {
      show(name);
    }
  }

  async function controllerFor(name) {
    if (!controllers[name]) {
      if (PAGES[name].needsSettings) {
        await settingsReady;
      }
      if (!controllers[name]) {
        const pageRoot = document.getElementById(`page-${name}`);
        controllers[name] = PAGES[name].create({ root: pageRoot, api, store, model, info, backdrop, navigate });
      }
    }
    return controllers[name];
  }

  async function show(name) {
    const page = Object.prototype.hasOwnProperty.call(PAGES, name) ? name : 'now';
    if (page === current) {
      return;
    }
    current = page;
    for (const link of document.querySelectorAll('.nav-item[data-page]')) {
      if (link.dataset.page === page) {
        link.setAttribute('aria-current', 'page');
      } else {
        link.removeAttribute('aria-current');
      }
    }
    const controller = await controllerFor(page);
    if (current !== page) {
      return;
    }
    for (const [other, otherController] of Object.entries(controllers)) {
      if (other !== page) {
        document.getElementById(`page-${other}`).hidden = true;
        otherController.hide();
      }
    }
    document.getElementById(`page-${page}`).hidden = false;
    controller.show();
  }

  window.addEventListener('hashchange', () => show(window.location.hash.slice(1)));

  // The Now Playing page drives the backdrop, so it always exists.
  await controllerFor('now');
  show(params.get('page') || window.location.hash.slice(1) || 'now');

  // Sidebar: sharing on / paused ---------------------------------------------------------
  const sharingDot = document.getElementById('sharing-dot');
  const sharingSwitch = document.getElementById('sharing-switch');
  const sharingDetail = document.getElementById('sharing-detail');
  const sharingRail = document.getElementById('sharing-rail');
  let paused = false;

  async function toggleSharing() {
    const next = !paused;
    sharingSwitch.setAttribute('aria-checked', String(!next));
    const result = await attempt(api.setPaused(next), next ? 'Could not pause sharing' : 'Could not resume sharing');
    if (typeof result !== 'boolean') {
      sharingSwitch.setAttribute('aria-checked', String(!paused));
    }
  }

  sharingSwitch.addEventListener('click', toggleSharing);
  sharingRail.addEventListener('click', toggleSharing);

  store.subscribe((view) => {
    paused = view.paused;
    const summary = sharingSummary(view);
    // Off only when Lyrix stopped; pausing works while it restarts.
    sharingSwitch.disabled = !view.running && !pendingOf(view);
    sharingSwitch.setAttribute('aria-checked', String(!view.paused));
    sharingRail.setAttribute('aria-pressed', String(!view.paused));
    sharingRail.title = view.paused ? 'Sharing paused' : `Sharing: ${summary.short}`;
    sharingDetail.textContent = summary.short;
    sharingDot.dataset.tone = summary.tone;
  });

  // Sidebar: a dot on Advanced while its risk switch is on.
  const advancedBadge = document.getElementById('advanced-badge');
  model.subscribe(() => {
    advancedBadge.hidden = !model.get('advanced.accept_ban_risk');
  });
}

main().catch((error) => console.error('The window could not start', error));
