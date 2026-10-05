// Advanced: the ban warning, the risk switch (confirmed in a modal) and the
// two options, which only save the setting; their connectors are not in
// this build.

import { boundSwitch, confirmDialog, group, issueSlot, pageHeader, row, switchControl } from '../controls.js';
import { h, nextId } from '../dom.js';
import { icon } from '../icons.js';

/** The exact words of `BAN_WARNING` in src/config.rs. */
export const BAN_WARNING = 'USING THIS MIGHT GET YOU BANNED. YOU HAVE BEEN WARNED.';

export function createAdvancedPage({ root, model }) {
  const acceptId = nextId('accept');
  const accept = switchControl({
    checked: model.get('advanced.accept_ban_risk'),
    labelledBy: acceptId,
    danger: true,
    onChange: async (on, el) => {
      if (!on) {
        model.set('advanced.accept_ban_risk', false, { now: true });
        return;
      }
      el.setChecked(false);
      const confirmed = await confirmDialog({
        title: 'Turn on Advanced mode?',
        text: 'These options would use your own accounts in ways Discord and Spotify do not allow. Your accounts could be limited or banned.',
        warning: BAN_WARNING,
        confirmLabel: 'I understand, turn on',
      });
      if (confirmed) {
        model.set('advanced.accept_ban_risk', true, { now: true });
      }
      el.setChecked(model.get('advanced.accept_ban_risk'));
      el.focus();
    },
  });

  const option = (path, lead, title, desc) => {
    const titleId = nextId('advanced');
    const toggle = boundSwitch(model, path, { labelledBy: titleId });
    const el = row({
      lead,
      titleId,
      title,
      desc: [desc, issueSlot(model, path, toggle)],
      control: [h('span', { class: 'badge badge--warn', text: 'Not in this build yet' }), toggle],
    });
    return { el, toggle };
  };

  const options = [
    option(
      'advanced.discord_custom_status',
      'chat',
      'Discord custom status with your account',
      'Puts the line in your custom status instead of Rich Presence, using your account token.',
    ),
    option(
      'advanced.spotify_cookie_lyrics',
      'note',
      'Spotify lyrics with your cookie',
      'Gets lyrics from Spotify with your own browser login.',
    ),
  ];

  const optionsGroup = group(
    {
      title: 'Options',
      icon: 'sparkle',
      note: 'These switches only save your choice. Lyrix never asks for your password, token or cookie in this build.',
    },
    options.map(({ el }) => el),
  );

  const sync = () => {
    const accepted = Boolean(model.get('advanced.accept_ban_risk'));
    accept.setChecked(accepted);
    for (const { el, toggle } of options) {
      toggle.disabled = !accepted;
      el.classList.toggle('is-locked', !accepted);
    }
  };
  model.subscribe(sync);
  sync();

  root.append(
    h(
      'div',
      { class: 'page-inner' },
      pageHeader('Advanced', 'Options that use your own accounts in ways the services do not allow.'),
      h(
        'div',
        { class: 'danger-banner', role: 'note' },
        h('div', { class: 'danger-banner-icon', 'aria-hidden': 'true' }, icon('warning')),
        h(
          'div',
          {},
          h('p', { class: 'danger-banner-title', text: BAN_WARNING }),
          h('p', {
            class: 'danger-banner-text',
            text: 'Nothing on this page runs unless you accept the risk. Rich Presence on the Connections page is the safe way to share your lyrics.',
          }),
        ),
      ),
      group(
        {},
        row({
          lead: h('div', { class: 'row-lead row-lead--danger', 'aria-hidden': 'true' }, icon('shield')),
          titleId: acceptId,
          title: 'I accept the risk',
          desc: 'Needed before any option below can be turned on.',
          control: accept,
        }),
      ),
      optionsGroup,
    ),
  );

  return { show() {}, hide() {} };
}
