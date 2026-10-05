// Inline SVG icons, all drawn on a 24 px grid with a 1.75 px round stroke.
// Service icons are generic glyphs, never brand logos.

const PATHS = {
  nowPlaying: '<path d="M4 10v4M8 6.5v11M12 3.5v17M16 7.5v9M20 10v4"/>',
  lyrics:
    '<path d="M10 17.5V4.5h10M13.5 9h6.5M13.5 13.5h4"/><circle cx="7" cy="17.5" r="3" fill="currentColor" stroke="none"/>',
  connections:
    '<circle cx="6" cy="12" r="2.5"/><circle cx="18" cy="5.5" r="2.5"/><circle cx="18" cy="18.5" r="2.5"/><path d="M8.2 10.8l7.6-4.1M8.2 13.2l7.6 4.1"/>',
  settings:
    '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2.2"/><circle cx="9" cy="17" r="2.2"/>',
  warning:
    '<path d="M10.3 4.2 2.6 17.6A2 2 0 0 0 4.3 20.6h15.4a2 2 0 0 0 1.7-3L13.7 4.2a2 2 0 0 0-3.4 0z"/><path d="M12 9.5v4M12 17h.01"/>',
  broadcast:
    '<circle cx="12" cy="12" r="1.8" fill="currentColor"/><path d="M15.5 8.5a5 5 0 0 1 0 7M8.5 15.5a5 5 0 0 1 0-7M18.4 5.6a9 9 0 0 1 0 12.8M5.6 18.4a9 9 0 0 1 0-12.8"/>',
  folder: '<path d="M3.5 7.5a2 2 0 0 1 2-2h3.8l2 2h7.2a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2z"/>',
  external: '<path d="M14 4.5h5.5V10M19.5 4.5 11 13M17.5 14v3.5a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2v-9a2 2 0 0 1 2-2H10"/>',
  minus: '<path d="M6 12h12"/>',
  plus: '<path d="M12 6v12M6 12h12"/>',
  reset: '<path d="M4.5 12a7.5 7.5 0 1 0 2.3-5.4"/><path d="M4.5 4.5v4h4"/>',
  check: '<path d="m5.5 12.5 4 4 9-9.5"/>',
  chevronRight: '<path d="m9.5 6 6 6-6 6"/>',
  close: '<path d="M7 7l10 10M17 7 7 17"/>',
  trash:
    '<path d="M4.5 7h15M10 11v6M14 11v6M6.5 7l.8 11.2a2 2 0 0 0 2 1.8h5.4a2 2 0 0 0 2-1.8L17.5 7M9.5 7V4.5h5V7"/>',
  note: '<path d="M9 18V5.5l11-2V16"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="17.5" cy="16" r="2.5"/>',
  window: '<rect x="3.5" y="4.5" width="17" height="15" rx="2.5"/><path d="M3.5 9h17M7 6.8h.01M9.5 6.8h.01"/>',
  clock: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
  info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5M12 8h.01"/>',
  alert: '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v5.5M12 16.2h.01"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.4-4.4"/>',
  chat: '<path d="M20.5 11.5a8 8 0 0 1-11.7 7.1L4 19.8l1.2-4.4a8 8 0 1 1 15.3-3.9z"/><path d="M9 10.5h.01M12.5 10.5h.01M16 10.5h.01" stroke-width="2.4"/>',
  hash: '<path d="M5 9h15M4 15h15M10.5 4 8.5 20M15.5 4l-2 16"/>',
  send: '<path d="M20.5 3.5 10 14M20.5 3.5 14 20.5l-4-6.5-6.5-4z"/>',
  code: '<path d="m8 7.5-4.5 4.5L8 16.5M16 7.5l4.5 4.5-4.5 4.5M13.5 5l-3 14"/>',
  globe: '<circle cx="12" cy="12" r="8.5"/><path d="M3.5 12h17M12 3.5c2.4 2.3 3.6 5.1 3.6 8.5s-1.2 6.2-3.6 8.5c-2.4-2.3-3.6-5.1-3.6-8.5S9.6 5.8 12 3.5z"/>',
  users:
    '<circle cx="9" cy="8.5" r="3.2"/><path d="M3.5 19.5a5.5 5.5 0 0 1 11 0"/><circle cx="17" cy="9.5" r="2.4"/><path d="M16 14.2a4.6 4.6 0 0 1 5 4.6"/>',
  monitor: '<rect x="3" y="4.5" width="18" height="12.5" rx="2"/><path d="M8.5 20.5h7M12 17v3.5M7 13h6"/>',
  braces:
    '<path d="M8.5 4.5c-2 0-3 .9-3 2.7v2c0 1.4-.8 2.5-2 2.8 1.2.3 2 1.4 2 2.8v2c0 1.8 1 2.7 3 2.7M15.5 4.5c2 0 3 .9 3 2.7v2c0 1.4.8 2.5 2 2.8-1.2.3-2 1.4-2 2.8v2c0 1.8-1 2.7-3 2.7"/>',
  shield: '<path d="M12 3.5 19 6v5.5c0 4.4-3 7.7-7 9-4-1.3-7-4.6-7-9V6z"/><path d="m9 12 2.2 2.2L15.5 10"/>',
  text: '<path d="M5 7V5h14v2M12 5v14M9 19h6"/>',
  power: '<path d="M12 3.5v8M7.1 6.6a7.5 7.5 0 1 0 9.8 0"/>',
  pause: '<path d="M9 6v12M15 6v12"/>',
  play: '<path d="M8 5.5v13l10.5-6.5z"/>',
  file: '<path d="M13.5 3.5h-6a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2v-10z"/><path d="M13.5 3.5v5h5"/>',
  fileText:
    '<path d="M13.5 3.5h-6a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2v-10z"/><path d="M13.5 3.5v5h5M9 13h6M9 16.5h4"/>',
  database:
    '<ellipse cx="12" cy="6" rx="7.5" ry="2.8"/><path d="M4.5 6v12c0 1.5 3.4 2.8 7.5 2.8s7.5-1.3 7.5-2.8V6M4.5 12c0 1.5 3.4 2.8 7.5 2.8s7.5-1.3 7.5-2.8"/>',
  logs: '<path d="M9 6.5h11M9 12h11M9 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01" />',
  sparkle: '<path d="M12 3.5c.6 4.3 2.2 5.9 6.5 6.5-4.3.6-5.9 2.2-6.5 6.5-.6-4.3-2.2-5.9-6.5-6.5 4.3-.6 5.9-2.2 6.5-6.5z"/>',
  timer: '<circle cx="12" cy="13" r="7.5"/><path d="M12 9.5V13l2.5 1.5M9.5 3h5"/>',
  heart: '<path d="M12 19.5s-7.5-4.4-7.5-10a4.2 4.2 0 0 1 7.5-2.6 4.2 4.2 0 0 1 7.5 2.6c0 5.6-7.5 10-7.5 10z"/>',
  key: '<circle cx="8" cy="15" r="3.5"/><path d="m10.5 12.5 8-8M16 7l2.5 2.5M14 9l2 2"/>',
};

/** An SVG element for icon `name`, sized by CSS. */
export function icon(name, className = '') {
  const body = PATHS[name];
  if (!body) {
    throw new Error(`unknown icon: ${name}`);
  }
  const template = document.createElement('template');
  template.innerHTML =
    `<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" ` +
    `stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" ` +
    `focusable="false"${className ? ` class="${className}"` : ''}>${body}</svg>`;
  return template.content.firstElementChild;
}

/** Replaces every `[data-icon]` placeholder under `root` with its SVG. */
export function hydrateIcons(root = document) {
  for (const slot of root.querySelectorAll('[data-icon]')) {
    slot.replaceWith(icon(slot.dataset.icon, slot.getAttribute('class') || ''));
  }
}

/** The Lyrix glyph (a note whose flag is three lyric lines), white, for generated covers. */
export function lyrixGlyph() {
  const template = document.createElement('template');
  template.innerHTML =
    '<svg viewBox="230 228 568 568" aria-hidden="true" focusable="false">' +
    '<g fill="none" stroke="currentColor" stroke-width="76" stroke-linecap="round">' +
    '<path d="M440 296V650M440 296H740M562 416H722M562 536H652"/></g>' +
    '<ellipse cx="364" cy="668" rx="112" ry="90" transform="rotate(-20 364 668)" fill="currentColor"/></svg>';
  return template.content.firstElementChild;
}
