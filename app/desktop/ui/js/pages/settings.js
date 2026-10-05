// Settings: status text (with a live preview), behaviour, privacy, the app
// and about. Every change saves itself.

import { openFolder, openUrl } from '../actions.js';
import {
  boundStepper,
  boundSwitch,
  boundTags,
  boundText,
  group,
  issueSlot,
  pageHeader,
  row,
  switchControl,
} from '../controls.js';
import { attempt, debounce, formatOffset, h, nextId, setText, toast } from '../dom.js';
import { icon } from '../icons.js';
import { lyricMoment, positionAt, totalOffset } from '../view.js';

const REPO_URL = 'https://github.com/Ggaming5005/Lyrixx';
const SAMPLE = {
  title: 'Song title',
  artist: 'Artist',
  album: 'Album',
  line: 'The line you are hearing',
  next: 'The line after it',
};

/** Settings whose issues show next to their own field (here or on another page). */
const SHOWN_BY_FIELDS = /^(status\.line_template|status\.no_lyrics_template|discord\.client_id|advanced\.)/;

/** The values a preview uses: the song playing now, or a sample. */
function previewContext(view) {
  const now = view?.now;
  if (!now) {
    return SAMPLE;
  }
  const ctx = { title: now.title, artist: now.artist, album: now.album || undefined, line: SAMPLE.line, next: SAMPLE.next };
  if (now.lyrics.state === 'found' && now.lyrics.lines.length > 0) {
    const { lines } = now.lyrics;
    const shifted = positionAt(now) - totalOffset(now);
    const moment = lyricMoment(lines, shifted);
    const texts = lines.map((line, index) => ({ index, text: line.text.trim() })).filter((line) => line.text);
    const current = moment.kind === 'line' ? texts.find((line) => line.index === moment.index) : null;
    const fallback = texts[0];
    const line = current || fallback;
    if (line) {
      ctx.line = line.text;
      ctx.next = texts.find((other) => other.index > line.index)?.text;
    }
  }
  return ctx;
}

const PLACEHOLDERS = [
  ['{line}', 'The line you are hearing'],
  ['{next}', 'The line after it'],
  ['{title}', 'The song title'],
  ['{artist}', 'The artist'],
  ['{album}', 'The album'],
];

/** A template setting: its label on the left, the field and its issues on the right. */
function templateRow(model, path, { title, desc, onFocus }) {
  const labelId = nextId('template');
  const input = boundText(model, path, { labelledBy: labelId });
  input.addEventListener('focus', () => onFocus(input));
  const el = h(
    'div',
    { class: 'row template-row' },
    h(
      'div',
      { class: 'row-text' },
      h('div', { class: 'row-title', id: labelId, text: title }),
      h('div', { class: 'row-desc', text: desc }),
    ),
    h('div', { class: 'template-input' }, input, issueSlot(model, path, input)),
  );
  return { el, input };
}

export function createSettingsPage({ root, api, store, model, info }) {
  // Status text -------------------------------------------------------------------
  const previews = [
    ['While singing', 'status.line_template'],
    ['No lyrics', 'status.no_lyrics_template'],
    ['Intro or break', 'status.instrumental_text'],
  ].map(([when, path]) => ({ when, path, text: h('div', { class: 'preview-text selectable' }) }));

  const previewCard = h(
    'div',
    { class: 'preview-card', 'aria-live': 'polite' },
    h('div', { class: 'preview-kicker' }, icon('broadcast'), 'Preview'),
    previews.map(({ when, text }) =>
      h('div', { class: 'preview-row' }, h('div', { class: 'preview-when', text: when }), text),
    ),
  );

  let previewToken = 0;
  const refreshPreview = debounce(async () => {
    const token = ++previewToken;
    const ctx = previewContext(store.get());
    const results = await Promise.all(
      previews.map(({ path }) =>
        api.previewStatus({ template: model.get(path) || '', ...ctx }).catch(() => null),
      ),
    );
    if (token !== previewToken) {
      return;
    }
    results.forEach((result, i) => {
      const empty = !result;
      setText(previews[i].text, empty ? 'Empty: falls back to the song name' : result);
      previews[i].text.classList.toggle('is-empty', empty);
    });
  }, 120);

  let target = null;
  const focusTarget = (input) => {
    target = input;
  };
  const templates = [
    templateRow(model, 'status.line_template', {
      title: 'While a line is sung',
      desc: 'Shown for each lyric line.',
      onFocus: focusTarget,
    }),
    templateRow(model, 'status.no_lyrics_template', {
      title: 'When there are no lyrics',
      desc: 'Also while lyrics load, and when you share the song only.',
      onFocus: focusTarget,
    }),
    templateRow(model, 'status.instrumental_text', {
      title: 'Intros and breaks',
      desc: 'Between sung lines.',
      onFocus: focusTarget,
    }),
  ];
  target = templates[0].input;

  const insert = (token) => {
    const input = target;
    const start = input.selectionStart ?? input.value.length;
    const end = input.selectionEnd ?? input.value.length;
    input.setRangeText(token, start, end, 'end');
    input.dispatchEvent(new Event('input'));
    input.focus();
  };
  const chips = h(
    'div',
    { class: 'row template-chips-row' },
    h('span', { class: 'template-chips-label', text: 'Insert' }),
    h(
      'div',
      { class: 'template-chips', role: 'group', 'aria-label': 'Insert a placeholder into the selected field' },
      PLACEHOLDERS.map(([token, meaning]) =>
        h(
          'button',
          {
            type: 'button',
            class: 'chip chip--button',
            title: `${meaning}`,
            'aria-label': `Insert ${token}: ${meaning.toLowerCase()}`,
            // Keep the cursor in the field being edited.
            onMouseDown: (event) => event.preventDefault(),
            onClick: () => insert(token),
          },
          token,
        ),
      ),
    ),
  );

  const statusGroup = group(
    {
      title: 'Status text',
      icon: 'text',
      note: 'Pick a field, then a placeholder to insert it where the cursor is. Empty values and the separators next to them are left out.',
    },
    h('div', { class: 'row row--stack' }, previewCard),
    templates.map((template) => template.el),
    chips,
  );

  // Behaviour ---------------------------------------------------------------------
  const pausedId = nextId('paused');
  const offsetId = nextId('offset');
  const behaviourGroup = group(
    { title: 'Behaviour', icon: 'clock' },
    row({
      lead: 'pause',
      titleId: pausedId,
      title: 'Keep the status while paused',
      desc: 'Otherwise your status clears when the music pauses.',
      control: boundSwitch(model, 'status.show_when_paused', { labelledBy: pausedId }),
    }),
    row({
      lead: 'timer',
      titleId: offsetId,
      title: 'Lyrics timing for every song',
      desc: 'Positive shows lines later. Fine-tune single songs on Now Playing.',
      control: boundStepper(model, 'general.offset_ms', {
        step: 100,
        min: -10_000,
        max: 10_000,
        format: formatOffset,
        labelledBy: offsetId,
        labels: ['Show lyrics 0.1 seconds earlier', 'Show lyrics 0.1 seconds later'],
      }),
    }),
  );

  // Privacy -----------------------------------------------------------------------
  const titleOnlyId = nextId('title-only');
  const appsId = nextId('apps');
  const artistsId = nextId('artists');
  const profanityId = nextId('profanity');
  const wordsId = nextId('words');
  const wordsRow = h(
    'div',
    { class: 'row-extra' },
    h('div', { class: 'row-desc row-extra-label', id: wordsId, text: 'Words to mask. Leave empty to use the built-in list.' }),
    boundTags(model, 'status.profanity_words', { labelledBy: wordsId, placeholder: 'Add a word…' }),
  );
  const privacyGroup = group(
    { title: 'Privacy', icon: 'shield' },
    row({
      lead: 'note',
      titleId: titleOnlyId,
      title: 'Share the song only',
      desc: 'Never show lyric lines: your status names the song instead.',
      control: boundSwitch(model, 'privacy.title_only', { labelledBy: titleOnlyId }),
    }),
    row({
      lead: 'window',
      titleId: appsId,
      title: 'Ignore these players',
      desc: 'Part of the app name is enough, for example chrome or vlc.',
      below: boundTags(model, 'privacy.blocked_apps', { labelledBy: appsId, placeholder: 'Add a player…' }),
    }),
    row({
      lead: 'users',
      titleId: artistsId,
      title: 'Never show these artists',
      desc: 'Your status clears while they play.',
      below: boundTags(model, 'privacy.blocked_artists', { labelledBy: artistsId, placeholder: 'Add an artist…' }),
    }),
    row({
      lead: 'shield',
      titleId: profanityId,
      title: 'Profanity filter',
      desc: 'Masks swear words, like f***, before anything is shared.',
      control: boundSwitch(model, 'status.profanity_filter', { labelledBy: profanityId }),
    }),
    wordsRow,
  );
  const syncWords = () => {
    wordsRow.hidden = !model.get('status.profanity_filter');
  };
  model.subscribe(syncWords);
  syncWords();

  // App ---------------------------------------------------------------------------
  const autostartId = nextId('autostart');
  const autostart = switchControl({
    labelledBy: autostartId,
    disabled: true,
    onChange: async (enabled, el) => {
      el.disabled = true;
      const result = await attempt(api.setAutostart(enabled), 'Could not change start at login');
      el.disabled = false;
      if (typeof result === 'boolean') {
        el.setChecked(result);
        toast('Saved');
      } else {
        el.setChecked(!enabled);
      }
    },
  });
  api
    .getAutostart()
    .then((enabled) => {
      autostart.setChecked(enabled);
      autostart.disabled = false;
    })
    .catch(() => {
      autostart.title = 'Not available';
    });

  const configPath = h('span', { class: 'path', text: model.paths.config, title: model.paths.config });
  const appGroup = group(
    { title: 'App', icon: 'window' },
    row({
      lead: 'power',
      titleId: autostartId,
      title: 'Start Lyrix when you log in',
      desc: 'It starts quietly in the tray.',
      control: autostart,
    }),
    row({
      lead: 'info',
      title: 'Closing the window keeps Lyrix running in the tray',
      desc: 'Quit from the tray icon, or here. Quitting clears your status.',
      control: h(
        'button',
        { type: 'button', class: 'btn', onClick: () => attempt(api.quit(), 'Could not quit') },
        'Quit Lyrix',
      ),
    }),
    row({
      lead: 'settings',
      title: 'Settings file',
      desc: configPath,
      control: h(
        'button',
        { type: 'button', class: 'btn', onClick: () => openFolder(api, 'config') },
        icon('folder'),
        'Open folder',
      ),
    }),
    row({
      lead: 'logs',
      title: 'Logs',
      desc: 'What Lyrix did, for when something looks wrong.',
      control: h(
        'button',
        { type: 'button', class: 'btn', onClick: () => openFolder(api, 'logs') },
        icon('folder'),
        'Open logs',
      ),
    }),
  );

  // About -------------------------------------------------------------------------
  const aboutGroup = group(
    { title: 'About', icon: 'heart' },
    h(
      'div',
      { class: 'about' },
      h('img', { class: 'about-logo', src: 'assets/logo.svg', alt: '' }),
      h(
        'div',
        { class: 'row-text' },
        h('div', { class: 'about-name', text: 'Lyrix' }),
        h('div', { class: 'about-version', text: `Version ${info.version}` }),
      ),
      h(
        'button',
        { type: 'button', class: 'btn', onClick: () => openUrl(api, REPO_URL) },
        icon('code'),
        'GitHub',
        icon('external'),
      ),
    ),
    row({
      lead: 'globe',
      title: 'Lyrics from LRCLIB and your own files',
      desc: 'Lyrix runs on your computer: no account, no server, no audio recording.',
    }),
  );

  // Banner for issues that have no field of their own -----------------------------
  const banner = h('div', { class: 'settings-banner', role: 'status', hidden: true });
  const renderBanner = () => {
    const loose = model.looseIssues(SHOWN_BY_FIELDS);
    banner.hidden = loose.length === 0;
    banner.dataset.severity = loose.some((issue) => issue.severity === 'error') ? 'error' : 'warning';
    banner.replaceChildren(
      icon('warning'),
      h(
        'div',
        {},
        loose.map((issue) => h('p', { text: issue.message })),
      ),
    );
  };
  model.subscribe(renderBanner);
  renderBanner();

  root.append(
    h(
      'div',
      { class: 'page-inner' },
      pageHeader('Settings', 'Changes save on their own.'),
      banner,
      statusGroup,
      behaviourGroup,
      privacyGroup,
      appGroup,
      aboutGroup,
    ),
  );

  model.subscribe(refreshPreview);
  let lastLine = null;
  store.subscribe((view) => {
    const key = `${view.now?.songKey}|${view.status?.line}`;
    if (key !== lastLine) {
      lastLine = key;
      refreshPreview();
    }
  });
  refreshPreview();

  return { show: refreshPreview, hide() {} };
}
