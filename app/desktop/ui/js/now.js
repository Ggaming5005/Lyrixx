// Now Playing: the cover, the song, a live progress bar, the lyrics timing
// nudge, karaoke lyrics and the live status strip, plus a designed look for
// every other moment (searching, no lyrics, instrumental, nothing playing,
// Lyrix stopped).

import { openFolder } from './actions.js';
import { seedGradient, seedHue } from './backdrop.js';
import { appName, attempt, fill, formatOffset, formatTime, h, setText, sourceName } from './dom.js';
import { icon, lyrixGlyph } from './icons.js';
import { createLiveStrip } from './live.js';
import { createLyricsView } from './lyrics-view.js';
import { positionAt, totalOffset } from './view.js';

const STEP_MS = 250;

const songSeed = (now) => `${now.title}\n${now.artist}`;

/**
 * A cover image that crossfades when it changes and falls back to a tile
 * generated from the song when there is no art (or it fails to load).
 */
export function createCover(className) {
  const el = h('div', { class: className });
  let key = null;

  function generated(seed) {
    const tile = h('div', { class: 'cover-generated' }, lyrixGlyph());
    tile.style.background = seedGradient(seedHue(seed));
    return tile;
  }

  function swapTo(next) {
    const previous = [...el.children];
    next.style.opacity = '0';
    el.append(next);
    requestAnimationFrame(() => {
      next.style.opacity = '1';
      setTimeout(() => previous.forEach((old) => old.remove()), 520);
    });
  }

  return {
    el,
    set(artwork, seed) {
      const nextKey = `${artwork || ''}|${seed}`;
      if (nextKey === key) {
        return;
      }
      key = nextKey;
      if (!artwork) {
        swapTo(generated(seed));
        return;
      }
      const img = h('img', { class: 'cover-img', alt: '', decoding: 'async', draggable: 'false' });
      img.addEventListener('load', () => key === nextKey && swapTo(img), { once: true });
      img.addEventListener('error', () => key === nextKey && swapTo(generated(seed)), { once: true });
      img.src = artwork;
    },
  };
}

/** Creates the page inside `root`; returns `{ show, hide }` for the router. */
export function createNowPage({ root, api, store, backdrop, navigate }) {
  // Left column ---------------------------------------------------------------
  const cover = createCover('cover');
  const title = h('h1', { class: 'track-title', id: 'now-title' });
  const artist = h('p', { class: 'track-artist' });
  const album = h('p', { class: 'track-album' });
  const badges = h('div', { class: 'track-badges' });

  const progressFill = h('div', { class: 'progress-fill' });
  const elapsed = h('span');
  const total = h('span');
  const progress = h(
    'div',
    { class: 'progress', role: 'progressbar', 'aria-label': 'Song position', 'aria-valuemin': '0' },
    h('div', { class: 'progress-track' }, progressFill),
    h('div', { class: 'progress-times', 'aria-hidden': 'true' }, elapsed, total),
  );

  const offsetValue = h('span', { class: 'stepper-value', 'aria-live': 'polite' });
  const timingHint = h('div', { class: 'timing-hint' });
  const resetButton = h(
    'button',
    {
      type: 'button',
      class: 'icon-btn timing-reset',
      'aria-label': 'Reset lyrics timing for this song',
      title: 'Reset timing',
      onClick: () => changeOffset(api.resetOffset()),
    },
    icon('reset'),
  );
  const timing = h(
    'div',
    { class: 'timing' },
    h(
      'div',
      { class: 'timing-text' },
      h('div', { class: 'timing-label', id: 'timing-label', text: 'Lyrics timing' }),
      timingHint,
    ),
    h(
      'div',
      { class: 'timing-controls', role: 'group', 'aria-labelledby': 'timing-label' },
      resetButton,
      h(
        'div',
        { class: 'stepper' },
        h(
          'button',
          {
            type: 'button',
            'aria-label': 'Show lyrics 0.25 seconds earlier',
            title: 'Earlier',
            onClick: () => changeOffset(api.adjustOffset(-STEP_MS)),
          },
          icon('minus'),
        ),
        offsetValue,
        h(
          'button',
          {
            type: 'button',
            'aria-label': 'Show lyrics 0.25 seconds later',
            title: 'Later',
            onClick: () => changeOffset(api.adjustOffset(STEP_MS)),
          },
          icon('plus'),
        ),
      ),
    ),
  );

  const side = h(
    'div',
    { class: 'now-side' },
    cover.el,
    h('div', { class: 'track' }, title, artist, album, badges),
    progress,
    timing,
  );

  // Right column --------------------------------------------------------------
  const lyricsHead = h('div', { class: 'lyrics-head' });
  const viewport = h('div', { class: 'lyrics-viewport', 'aria-label': 'Lyrics' });
  const lyrics = createLyricsView(viewport);
  const lyricsState = h('div', { class: 'lyrics-state' });
  const lyricsColumn = h('div', { class: 'now-lyrics' }, lyricsHead, viewport, lyricsState);

  const hero = h('div', { class: 'now-hero' }, side, lyricsColumn);
  const empty = h('div', { class: 'now-empty' });
  const live = createLiveStrip({ api, navigate });
  root.append(hero, empty, live.el);

  let visible = false;
  let frame = 0;
  let lastSecond = -1;
  let lastRatio = -1;
  let headKey = null;
  let stateKey = null;
  let emptyKey = null;
  let badgeKey = null;

  async function changeOffset(request) {
    const value = await attempt(request, 'Could not change the timing');
    if (typeof value === 'number') {
      showOffset(value, store.get()?.now?.globalOffsetMs || 0);
    }
  }

  function showOffset(songMs, globalMs) {
    setText(offsetValue, formatOffset(songMs));
    resetButton.hidden = songMs === 0;
    let hint = songMs === 0 ? 'Lines early? Press +' : 'Saved for this song';
    if (globalMs) {
      hint = `Plus ${formatOffset(globalMs)} on all songs`;
    }
    setText(timingHint, hint);
  }

  // Lyrics column states --------------------------------------------------------

  function showLyricsState(key, build) {
    viewport.hidden = true;
    lyricsState.hidden = false;
    lyricsState.classList.toggle('lyrics-state--searching', key === 'searching');
    if (key !== stateKey) {
      stateKey = key;
      fill(lyricsState, build());
    }
  }

  function showLyricsList(now) {
    viewport.hidden = false;
    lyricsState.hidden = true;
    stateKey = null;
    const { lines, synced, source } = now.lyrics;
    lyrics.setLines(lines, `${now.songKey}|${source}|${synced}|${lines.length}|${lines[lines.length - 1]?.startMs}`);
  }

  const statusQuote = (view) => (view.status?.text ? h('q', { class: 'selectable', text: view.status.text }) : null);

  function renderLyrics(view) {
    const { now } = view;
    const state = now.lyrics.state;
    const found = state === 'found';

    const nextHeadKey = found
      ? `${now.lyrics.source}|${now.lyrics.synced}|${now.lyrics.instrumental}`
      : state;
    if (nextHeadKey !== headKey) {
      headKey = nextHeadKey;
      const parts = [];
      if (state === 'searching') {
        parts.push(h('span', { class: 'lyrics-searching' }, icon('search'), 'Looking for lyrics…'));
      }
      if (found && !now.lyrics.instrumental && now.lyrics.lines.length > 0) {
        parts.push(
          h(
            'span',
            { class: 'lyrics-source' },
            icon(now.lyrics.source === 'local' ? 'folder' : 'globe'),
            `Lyrics from ${sourceName(now.lyrics.source)}`,
          ),
        );
        if (!now.lyrics.synced) {
          parts.push(
            h(
              'span',
              {
                class: 'chip chip--warn',
                title: 'These lyrics have no timing, so Lyrix spreads them over the song.',
              },
              icon('clock'),
              'Estimated timing',
            ),
          );
        }
      }
      lyricsHead.replaceChildren(...parts);
    }

    if (state === 'searching') {
      showLyricsState('searching', () => [
        h(
          'div',
          { class: 'shimmer', 'aria-hidden': 'true' },
          [1, 2, 3, 4, 5].map(() => h('div', { class: 'shimmer-line' })),
        ),
      ]);
      lyricsState.setAttribute('aria-busy', 'true');
      return;
    }
    lyricsState.removeAttribute('aria-busy');

    if (found && now.lyrics.instrumental) {
      showLyricsState(`instrumental|${view.status?.text}`, () => [
        h('div', { class: 'eq', 'aria-hidden': 'true' }, [1, 2, 3, 4, 5].map(() => h('span'))),
        h('h2', { class: 'lyrics-state-title', text: 'Instrumental' }),
        h(
          'p',
          { class: 'lyrics-state-text' },
          view.status?.text ? ['No vocals in this one. Your status shows ', statusQuote(view), '.'] : 'No vocals in this one.',
        ),
      ]);
      return;
    }

    if (state === 'notFound' || (found && now.lyrics.lines.length === 0)) {
      showLyricsState(`notFound|${view.status?.text}`, () => [
        h('div', { class: 'lyrics-state-icon' }, icon('note')),
        h('h2', { class: 'lyrics-state-title', text: 'No lyrics for this song' }),
        h(
          'p',
          { class: 'lyrics-state-text' },
          view.status?.text
            ? ['That’s fine: your status shows ', statusQuote(view), ' instead.']
            : 'That’s fine: your status shows the song name instead.',
        ),
        h(
          'div',
          { class: 'lyrics-state-actions' },
          h(
            'button',
            { type: 'button', class: 'btn btn--primary btn--lg', onClick: () => openFolder(api, 'lyrics') },
            icon('plus'),
            'Add your own lyrics',
          ),
          h('button', { type: 'button', class: 'btn btn--ghost btn--lg', onClick: () => navigate('lyrics') }, 'How it works'),
        ),
        h(
          'p',
          { class: 'lyrics-state-hint' },
          'Save a file named ',
          h('code', { class: 'selectable', text: `${now.artist} - ${now.title}.lrc` }),
          ' in your lyrics folder.',
        ),
      ]);
      return;
    }

    showLyricsList(now);
  }

  // Whole-hero states -------------------------------------------------------------

  function showEmpty(key, build) {
    hero.hidden = true;
    empty.hidden = false;
    if (key !== emptyKey) {
      emptyKey = key;
      fill(empty, build());
    }
  }

  function renderIdle() {
    showEmpty('idle', () => [
      h(
        'div',
        { class: 'empty-art', 'aria-hidden': 'true' },
        h('span', { class: 'ring' }),
        h('span', { class: 'ring' }),
        h('span', { class: 'ring' }),
        h('img', { class: 'empty-logo', src: 'assets/logo.svg', alt: '' }),
      ),
      h('h1', { class: 'empty-title', text: 'Nothing playing' }),
      h('p', {
        class: 'empty-text',
        text: 'Play something in Spotify, YouTube, Apple Music or any player. Lyrix picks it up within a second.',
      }),
    ]);
  }

  function renderStopped(view) {
    showEmpty(`error|${view.error}`, () => [
      h('div', { class: 'empty-icon', 'aria-hidden': 'true' }, icon('alert')),
      h('h1', { class: 'empty-title', text: 'Lyrix stopped' }),
      h('p', {
        class: 'empty-text',
        text: 'Lyrix could not follow your music, so nothing is shared. It starts again on its own when you save your settings.',
      }),
      view.error ? h('p', { class: 'empty-error', text: view.error }) : null,
      h(
        'div',
        { class: 'empty-actions' },
        h('button', { type: 'button', class: 'btn btn--lg', onClick: () => openFolder(api, 'logs') }, icon('logs'), 'Open logs'),
        h(
          'button',
          { type: 'button', class: 'btn btn--lg', onClick: () => navigate('settings') },
          icon('settings'),
          'Settings',
        ),
      ),
    ]);
  }

  // The song ------------------------------------------------------------------------

  function renderSong(view) {
    const { now } = view;
    hero.hidden = false;
    empty.hidden = true;
    emptyKey = null;

    cover.set(now.artwork, songSeed(now));
    setText(title, now.title);
    title.title = now.album ? `${now.title} — ${now.album}` : now.title;
    setText(artist, now.artist);
    setText(album, now.album || '');
    album.hidden = !now.album;

    const app = appName(now.app);
    const nextBadgeKey = `${app}|${now.playing}`;
    if (nextBadgeKey !== badgeKey) {
      badgeKey = nextBadgeKey;
      fill(
        badges,
        app ? h('span', { class: 'chip', title: now.app }, icon('window'), app) : null,
        now.playing ? null : h('span', { class: 'chip' }, icon('pause'), 'Paused'),
      );
    }

    progress.hidden = !now.durationMs;
    setText(total, formatTime(now.durationMs));
    progress.setAttribute('aria-valuemax', String(Math.round((now.durationMs || 0) / 1000)));

    showOffset(now.songOffsetMs, now.globalOffsetMs);
    // The timing nudge only means something when there are lines to move. It
    // keeps its space so the cover does not jump when lyrics arrive.
    const hasLines = now.lyrics.state === 'found' && !now.lyrics.instrumental && now.lyrics.lines.length > 0;
    timing.style.visibility = hasLines ? '' : 'hidden';

    renderLyrics(view);
  }

  function render(view) {
    root.dataset.playing = String(Boolean(view.now?.playing));
    if (!view.running) {
      backdrop.showBrand();
      renderStopped(view);
    } else if (!view.now) {
      backdrop.showBrand();
      renderIdle();
    } else {
      backdrop.show({ artwork: view.now.artwork, seed: songSeed(view.now) });
      renderSong(view);
    }
    live.render(view);
    lastRatio = -1;
    lastSecond = -1;
    tick();
  }

  // Live position -------------------------------------------------------------------

  function step() {
    frame = 0;
    const now = store.get()?.now;
    if (!visible || !now || hero.hidden) {
      return;
    }
    const position = positionAt(now);
    if (now.durationMs) {
      const ratio = position / now.durationMs;
      if (Math.abs(ratio - lastRatio) > 0.0004) {
        lastRatio = ratio;
        progressFill.style.transform = `scaleX(${ratio.toFixed(4)})`;
      }
    }
    const second = Math.floor(position / 1000);
    if (second !== lastSecond) {
      lastSecond = second;
      setText(elapsed, formatTime(position));
      progress.setAttribute('aria-valuenow', String(second));
      progress.setAttribute('aria-valuetext', `${formatTime(position)} of ${formatTime(now.durationMs)}`);
    }
    if (now.lyrics.state === 'found' && now.lyrics.lines.length > 0) {
      lyrics.update(position - totalOffset(now));
    }
    if (now.playing && !document.hidden) {
      frame = requestAnimationFrame(step);
    }
  }

  function tick() {
    if (!frame) {
      frame = requestAnimationFrame(step);
    }
  }

  document.addEventListener('visibilitychange', tick);
  store.subscribe(render);

  return {
    show() {
      visible = true;
      tick();
    },
    hide() {
      visible = false;
    },
  };
}
