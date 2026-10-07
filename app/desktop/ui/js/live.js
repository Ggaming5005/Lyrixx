// The "Live status" strip: exactly what Discord is asked to show right now,
// with a coloured dot for how Discord is doing.

import { attempt, h, setText } from './dom.js';
import { icon } from './icons.js';
import { discordOf, pendingLabel, pendingOf, sharingSummary } from './view.js';

function placeholder(view) {
  const pending = pendingLabel(view);
  if (pending) {
    return pending;
  }
  if (!view.running) {
    return 'Nothing is shared while Lyrix is stopped';
  }
  if (view.paused) {
    return 'Sharing is paused';
  }
  if (!view.now) {
    return 'Nothing is shared right now';
  }
  if (!view.now.playing) {
    return 'Cleared while the music is paused';
  }
  return 'Nothing is shared for this song';
}

/** Creates the strip; `render(view)` updates it. */
export function createLiveStrip({ api, navigate }) {
  const dot = h('span', { class: 'dot' });
  const label = h('div', { class: 'live-label' });
  const text = h('div', { class: 'live-text selectable' });
  const state = h('span');
  const stateDot = h('span', { class: 'dot' });
  const side = h('div', { class: 'live-side' }, h('div', { class: 'live-state' }, stateDot, state));
  let actionKey = '';
  let action = null;

  const el = h(
    'section',
    { class: 'live', 'aria-label': 'Live status' },
    h('div', { class: 'live-icon' }, icon('chat'), dot),
    h('div', { class: 'live-body' }, label, text),
    side,
  );

  function setAction(nextKey, make) {
    if (nextKey === actionKey) {
      return;
    }
    actionKey = nextKey;
    action?.remove();
    action = make ? make() : null;
    if (action) {
      side.append(action);
    }
  }

  function render(view) {
    const summary = sharingSummary(view);
    const discord = discordOf(view);
    dot.dataset.tone = summary.tone;
    stateDot.dataset.tone = summary.tone;
    setText(label, discord ? 'Live status · Discord' : 'Live status');
    const shown = view.status?.text;
    setText(text, shown || placeholder(view));
    text.classList.toggle('is-muted', !shown);
    text.title = shown || '';
    setText(state, summary.long);
    state.title = discord?.detail || '';

    if (view.running && view.paused) {
      setAction('resume', () =>
        h(
          'button',
          {
            type: 'button',
            class: 'btn btn--primary',
            onClick: () => attempt(api.setPaused(false), 'Could not resume sharing'),
          },
          icon('play'),
          'Resume',
        ),
      );
    } else if (view.running && !discord && !pendingOf(view)) {
      setAction('connections', () =>
        h('button', { type: 'button', class: 'btn', onClick: () => navigate('connections') }, 'Connections'),
      );
    } else {
      setAction('', null);
    }
  }

  return { el, render };
}
