// Connections: Discord as the hero, then the places that are coming later.

import { openUrl } from '../actions.js';
import { boundSwitch, boundText, group, issueSlot, pageHeader, row } from '../controls.js';
import { formatTime, h, nextId, setText } from '../dom.js';
import { icon } from '../icons.js';
import { createCover } from '../now.js';
import { discordOf, pendingLabel, pendingOf, positionAt, sharingSummary } from '../view.js';

const PORTAL_URL = 'https://discord.com/developers/applications';

const COMING = [
  ['hash', 'Slack', 'Your Slack status, line by line.'],
  ['send', 'Telegram', 'The line you hear in your Telegram bio.'],
  ['code', 'GitHub', 'Your GitHub profile status.'],
  ['globe', 'Matrix', 'Your status on any Matrix server.'],
  ['users', 'Microsoft Teams', 'Your Teams status message.'],
  ['monitor', 'OBS overlay', 'Live lyrics on your stream.'],
  ['braces', 'Webhooks', 'Every line sent to a URL you choose.'],
];

export function createConnectionsPage({ root, api, store, model, info }) {
  const enabledId = nextId('discord-on');
  const progressId = nextId('discord-progress');
  const clientLabelId = nextId('client-id');

  // Header with the live state ---------------------------------------------------
  const stateDot = h('span', { class: 'dot' });
  const stateText = h('span');
  const head = h(
    'div',
    { class: 'hero-card-head' },
    h('div', { class: 'service-icon', 'aria-hidden': 'true' }, icon('chat')),
    h(
      'div',
      { class: 'row-text' },
      h('h2', { class: 'hero-card-title', text: 'Discord' }),
      h('p', { class: 'hero-card-sub', text: 'Rich Presence. No login: keep the Discord app open on this computer.' }),
    ),
    h('div', { class: 'state-pill', role: 'status' }, stateDot, stateText),
  );

  // What your profile shows -------------------------------------------------------
  const art = createCover('activity-art');
  const appName = h('span', { class: 'activity-name' });
  const details = h('div', { class: 'activity-details' });
  const state = h('div', { class: 'activity-state' });
  const bar = h('span');
  const barStart = h('span');
  const barEnd = h('span');
  const progress = h(
    'div',
    { class: 'activity-progress', 'aria-hidden': 'true' },
    barStart,
    h('div', { class: 'activity-bar' }, bar),
    barEnd,
  );
  const activity = h(
    'div',
    { class: 'activity', role: 'group', 'aria-label': 'What your profile shows' },
    art.el,
    h(
      'div',
      { class: 'activity-body' },
      h('div', { class: 'activity-kicker' }, 'Listening to ', appName),
      details,
      state,
      progress,
    ),
  );

  // Your own application ----------------------------------------------------------
  const clientField = boundText(model, 'discord.client_id', {
    labelledBy: clientLabelId,
    placeholder: info.discordDefaultClientId,
    mono: true,
  });
  clientField.setAttribute('inputmode', 'numeric');
  const useDefault = h(
    'button',
    {
      type: 'button',
      class: 'btn btn--ghost',
      onClick: () => {
        model.set('discord.client_id', info.discordDefaultClientId, { now: true });
        clientField.value = info.discordDefaultClientId;
      },
    },
    icon('reset'),
    'Use Lyrix',
  );
  const ownApp = h(
    'details',
    { class: 'disclosure' },
    h(
      'summary',
      {},
      h('div', { class: 'row-lead', 'aria-hidden': 'true' }, icon('key')),
      h(
        'div',
        { class: 'row-text' },
        h('div', { class: 'row-title', text: 'Use your own Discord application' }),
        h('div', { class: 'row-desc', text: 'Show a name other than “Lyrix” after “Listening to”.' }),
      ),
      h('span', { class: 'disclosure-chevron', 'aria-hidden': 'true' }, icon('chevronRight')),
    ),
    h(
      'div',
      { class: 'disclosure-body' },
      h(
        'p',
        { class: 'prose' },
        'Create an application in the Discord Developer Portal; its name is what your profile shows. Copy its ',
        h('b', { text: 'Application ID' }),
        ' and paste it here.',
      ),
      h(
        'div',
        { class: 'client-id-row' },
        h('label', { class: 'visually-hidden', id: clientLabelId, text: 'Discord application ID' }),
        clientField,
        useDefault,
        h(
          'button',
          { type: 'button', class: 'btn', onClick: () => openUrl(api, PORTAL_URL) },
          'Developer Portal',
          icon('external'),
        ),
      ),
      issueSlot(model, 'discord.client_id', clientField),
    ),
  );

  const hero = h(
    'section',
    { class: 'hero-card', 'aria-label': 'Discord' },
    head,
    h(
      'div',
      { class: 'hero-card-preview' },
      h('div', { class: 'group-title', text: 'What your profile shows' }),
      activity,
    ),
    row({
      lead: 'broadcast',
      titleId: enabledId,
      title: 'Show lyrics on Discord',
      desc: 'Your profile and the member list show the line you are hearing.',
      control: boundSwitch(model, 'discord.enabled', { labelledBy: enabledId }),
    }),
    row({
      lead: 'timer',
      titleId: progressId,
      title: 'Show progress bar',
      desc: 'How far into the song you are, under your status.',
      control: boundSwitch(model, 'discord.show_progress', { labelledBy: progressId }),
    }),
    ownApp,
  );

  const coming = group(
    { title: 'Coming later', icon: 'sparkle' },
    COMING.map(([iconName, name, desc]) =>
      row({
        className: 'soon',
        lead: h('div', { class: 'row-lead soon-icon', 'aria-hidden': 'true' }, icon(iconName)),
        title: name,
        desc,
        control: h('span', { class: 'badge', text: 'Coming soon' }),
      }),
    ),
  );

  root.append(
    h(
      'div',
      { class: 'page-inner' },
      pageHeader('Connections', 'Where your lyrics show up. Discord works today with no setup; more places are on the way.'),
      hero,
      coming,
    ),
  );

  // Live updates ---------------------------------------------------------------------
  let visible = false;
  let timer = 0;

  function renderProgress() {
    const now = store.get()?.now;
    const show = Boolean(model.get('discord.show_progress') && now?.durationMs && store.get()?.status);
    progress.hidden = !show;
    if (show) {
      const position = positionAt(now);
      bar.style.transform = `scaleX(${(position / now.durationMs).toFixed(4)})`;
      setText(barStart, formatTime(position));
      setText(barEnd, formatTime(now.durationMs));
    }
  }

  function render() {
    const view = store.get();
    if (!view) {
      return;
    }
    const discord = discordOf(view);
    const summary = sharingSummary(view);
    stateDot.dataset.tone = summary.tone;
    setText(stateText, discord || !view.running || pendingOf(view) ? summary.short : 'Off');
    stateText.title = discord?.detail || '';

    const ownId = model.get('discord.client_id');
    setText(appName, !ownId || ownId === info.discordDefaultClientId ? 'Lyrix' : 'your app');
    const now = view.now;
    if (now) {
      art.set(now.artwork, `${now.title}\n${now.artist}`);
    } else {
      art.set(null, 'Lyrix');
    }
    setText(details, view.status?.text || pendingLabel(view) || 'Nothing to show right now');
    details.classList.toggle('is-muted', !view.status);
    const estimated = view.status?.estimated ? ' (estimated timing)' : '';
    setText(state, now && view.status ? `${now.title} · ${now.artist}${estimated}` : 'Your status is clear');
    activity.style.opacity = view.status ? '1' : '0.6';
    renderProgress();
  }

  store.subscribe(render);
  model.subscribe(render);

  return {
    show() {
      visible = true;
      render();
      clearInterval(timer);
      timer = setInterval(() => visible && renderProgress(), 1000);
    },
    hide() {
      visible = false;
      clearInterval(timer);
    },
  };
}
