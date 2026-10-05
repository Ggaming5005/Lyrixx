// Karaoke-style lyrics: the current line large and bright, its neighbours
// dimmer and smaller with distance, and the list moving so the current line
// stays at the same height.

import { h } from './dom.js';
import { lyricMoment } from './view.js';

/** Where the current line sits, as a share of the viewport height. */
const ANCHOR = 0.36;
/** How long a wheel or keyboard scroll keeps the list where the user left it. */
const BROWSE_MS = 2600;

/** Opacity, scale and blur by distance from the current line. */
const STEPS = [
  { o: 1, s: 1, b: 0 },
  { o: 0.52, s: 0.84, b: 0 },
  { o: 0.34, s: 0.8, b: 0.4 },
  { o: 0.22, s: 0.78, b: 0.9 },
  { o: 0.13, s: 0.76, b: 1.3 },
];

function breakItem() {
  return h(
    'div',
    { class: 'lyric lyric-break', role: 'listitem', 'aria-label': 'Instrumental' },
    h('span', { class: 'beat' }),
    h('span', { class: 'beat' }),
    h('span', { class: 'beat' }),
  );
}

/**
 * Creates the lyrics list inside `viewport`. `setLines(lines, key)` replaces
 * the lyrics (only when `key` changes); `update(shiftedMs)` moves to the line
 * at that position (already shifted by the offsets).
 */
export function createLyricsView(viewport) {
  const track = h('div', { class: 'lyrics-track', role: 'list' });
  viewport.append(track);

  let lines = [];
  let key = null;
  /** Rendered rows: `{ el }`; breaks that follow each other share one row. */
  let items = [];
  /** Line index → row index. The intro, when shown, is row 0. */
  let rowOf = new Map();
  let hasIntro = false;
  let current = undefined;
  let browse = 0;
  let browseTimer = 0;

  function setLines(next, nextKey) {
    if (nextKey === key) {
      return;
    }
    key = nextKey;
    lines = next;
    items = [];
    rowOf = new Map();
    current = undefined;
    browse = 0;

    const firstText = lines.findIndex((line) => line.text.trim() !== '');
    hasIntro = firstText >= 0 && lines[firstText].startMs > 0;
    if (hasIntro) {
      items.push({ el: breakItem(), isBreak: true });
    }
    lines.forEach((line, index) => {
      if (index < firstText) {
        return;
      }
      const text = line.text.trim();
      const last = items[items.length - 1];
      if (!text && last && last.isBreak) {
        rowOf.set(index, items.length - 1);
        return;
      }
      items.push(
        text
          ? { el: h('p', { class: 'lyric', role: 'listitem', text }), isBreak: false }
          : { el: breakItem(), isBreak: true },
      );
      rowOf.set(index, items.length - 1);
    });

    track.replaceChildren(...items.map((item) => item.el));
    jump();
  }

  function rowAt(shiftedMs) {
    const moment = lyricMoment(lines, shiftedMs);
    if (moment.index < 0) {
      // Before the first line: the intro row, or "just before row 0".
      return hasIntro ? 0 : -1;
    }
    return rowOf.get(moment.index) ?? -1;
  }

  function paint() {
    items.forEach((item, row) => {
      const distance = Math.abs(row - current);
      const step = STEPS[Math.min(distance, STEPS.length - 1)];
      const past = row < current;
      const style = item.el.style;
      style.setProperty('--o', String(past ? step.o * 0.8 : step.o));
      style.setProperty('--s', String(step.s));
      style.setProperty('--b', `${step.b}px`);
      const isCurrent = row === current;
      item.el.classList.toggle('is-current', isCurrent);
      if (isCurrent) {
        item.el.setAttribute('aria-current', 'true');
      } else {
        item.el.removeAttribute('aria-current');
      }
    });
    position();
  }

  function position() {
    const target = items[Math.max(0, current ?? 0)];
    if (!target) {
      return;
    }
    const anchor = viewport.clientHeight * ANCHOR;
    let y = anchor - (target.el.offsetTop + target.el.offsetHeight / 2);
    if (current === -1) {
      // Nothing sung yet and no intro row: keep the first line just below the anchor.
      y += target.el.offsetHeight;
    }
    track.style.transform = `translate3d(0, ${Math.round(y + browse)}px, 0)`;
  }

  /** Positions without animating (new lyrics, resized window). */
  function jump() {
    track.style.transition = 'none';
    position();
    void track.offsetHeight;
    track.style.transition = '';
  }

  function update(shiftedMs) {
    const row = rowAt(shiftedMs);
    if (row !== current) {
      current = row;
      paint();
    }
  }

  function endBrowsing() {
    browse = 0;
    viewport.classList.remove('is-browsing');
    position();
  }

  viewport.addEventListener(
    'wheel',
    (event) => {
      if (items.length === 0) {
        return;
      }
      event.preventDefault();
      const height = track.offsetHeight;
      browse = Math.max(-height, Math.min(height, browse - event.deltaY));
      viewport.classList.add('is-browsing');
      position();
      clearTimeout(browseTimer);
      browseTimer = setTimeout(endBrowsing, BROWSE_MS);
    },
    { passive: false },
  );

  new ResizeObserver(() => jump()).observe(viewport);

  return { setLines, update };
}
