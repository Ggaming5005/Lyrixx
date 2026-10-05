// Checks how the window behaves over time, with the demo backend
// (ui/js/mock.js and its `window.lyrixDemo`): restarts after a save, the
// window closing with a change waiting, settings edited in the file, the
// status preview, lyrics with no timing and the modal without showModal().
//
// screenshots.mjs runs `checkFlows` after the screenshots; each check opens
// its own page and returns what went wrong, as readable lines.

/** Runs in the page: records what the window shows every 20 ms. */
function startSampler() {
  const q = (selector) => document.querySelector(selector);
  window.flowSamples = [];
  window.flowSampler = setInterval(() => {
    const empty = q('#page-now .now-empty');
    const state = q('#page-now .lyrics-state');
    window.flowSamples.push({
      title: q('#now-title')?.textContent ?? null,
      empty: empty && !empty.hidden ? q('#page-now .empty-title')?.textContent : null,
      sidebar: q('#sharing-detail')?.textContent ?? null,
      live: q('#page-now .live-text')?.textContent ?? null,
      art: Boolean(q('.backdrop-art')?.lastElementChild?.querySelector('img')),
      searching: Boolean(state && !state.hidden),
      lines: document.querySelectorAll('#page-now .lyric:not(.lyric-break)').length,
    });
  }, 20);
}

/**
 * What the window shows while `during` runs and `ms` after. With
 * `fromLoad`, sampling starts when the page `during` opens loads.
 */
async function sample(page, during, ms, { fromLoad = false } = {}) {
  if (fromLoad) {
    await page.addInitScript({ content: `document.addEventListener('DOMContentLoaded', ${startSampler});` });
  } else {
    await page.evaluate(startSampler);
  }
  await during();
  await page.waitForTimeout(ms);
  return page.evaluate(() => {
    clearInterval(window.flowSampler);
    return window.flowSamples;
  });
}

const saved = (page) => page.evaluate(() => window.lyrixDemo.saved());

const FLOWS = {
  /** A save restarts the engine; the song, its lyrics and the backdrop stay. */
  async 'a restart keeps the song on screen'(page, check) {
    const before = await page.evaluate(() => document.querySelector('#now-title').textContent);
    const samples = await sample(
      page,
      () => page.getByRole('switch', { name: 'Keep the status while paused' }).click(),
      2200,
    );
    check((await saved(page)).length === 1, 'the switch did not save once');
    check(samples.some((s) => s.sidebar === 'Restarting…'), 'the sidebar never said "Restarting…"');
    check(samples.some((s) => s.live === 'Restarting…'), 'the live strip never said "Restarting…"');
    for (const [label, bad] of [
      ['"Lyrix stopped"', (s) => s.sidebar === 'Lyrix stopped'],
      ['"Discord is off"', (s) => s.sidebar === 'Discord is off'],
      ['the song changed or went away', (s) => s.title !== before || s.empty !== null],
      ['the backdrop lost the cover', (s) => !s.art],
      ['the lyrics were searched again on screen', (s) => s.searching || s.lines === 0],
    ]) {
      const at = samples.findIndex(bad);
      check(at < 0, `${label} at ${at * 20} ms: ${JSON.stringify(samples[at])}`);
    }
    check(samples[samples.length - 1].sidebar === 'Live on Discord', 'not live on Discord after the restart');
  },

  /** With nothing playing, a restart is still no error. */
  async 'a restart with nothing playing'(page, check) {
    const samples = await sample(
      page,
      () => page.getByRole('switch', { name: 'Keep the status while paused' }).click(),
      1600,
    );
    check(samples.some((s) => s.sidebar === 'Restarting…'), 'the sidebar never said "Restarting…"');
    check(!samples.some((s) => s.sidebar === 'Lyrix stopped' || s.sidebar === 'Discord is off'), 'an error showed');
    check(samples.every((s) => s.empty === 'Nothing playing'), 'the page did not keep saying "Nothing playing"');
  },

  /** The app starting: "Starting…", never an error or "Nothing playing" first. */
  async 'starting'(page, check, { reload }) {
    const samples = await sample(page, () => reload(), 1800, { fromLoad: true });
    const first = samples.findIndex((s) => s.empty === 'Starting…');
    check(first >= 0, 'the page never said "Starting…"');
    check(samples.some((s) => s.sidebar === 'Starting…'), 'the sidebar never said "Starting…"');
    check(
      !samples.some((s) => ['Lyrix stopped', 'Restarting…', 'Discord is off'].includes(s.sidebar)),
      'the sidebar showed something else while starting',
    );
    check(!samples.some((s) => s.empty === 'Nothing playing'), '"Nothing playing" showed while starting');
    check(samples[samples.length - 1].title === 'Paper Satellites', 'the song did not show after starting');
  },

  /** Closing right after a change: it is sent at once, not after the pause. */
  async 'closing sends a change waiting for its pause'(page, check) {
    await page.getByRole('button', { name: 'Show lyrics 0.1 seconds later' }).click();
    await page.evaluate(() => window.lyrixDemo.emit('lyrix://closing'));
    await page.waitForTimeout(150);
    const sent = await saved(page);
    check(
      sent.length === 1 && sent[0].general.offset_ms === 100,
      `not sent at once: ${JSON.stringify(sent.map((c) => c.general))}`,
    );
    await page.waitForTimeout(1000);
    check((await saved(page)).length === 1, 'the change was sent twice');

    // A word typed in a list field, without Enter, is kept too.
    await page.getByRole('textbox', { name: 'Ignore these players' }).fill('vlc');
    await page.evaluate(() => window.lyrixDemo.emit('lyrix://closing'));
    await page.waitForTimeout(150);
    const after = await saved(page);
    check(
      after.length === 2 && after[1].privacy.blocked_apps.join() === 'vlc',
      `the word being typed was not sent: ${JSON.stringify(after.map((c) => c.privacy.blocked_apps))}`,
    );
  },

  /** Closing while a save is in flight and another waits behind it. */
  async 'closing sends a change queued behind a save'(page, check) {
    await page.getByRole('switch', { name: 'Keep the status while paused' }).click();
    await page.waitForTimeout(60);
    await page.getByRole('switch', { name: 'Profanity filter' }).click();
    await page.evaluate(() => window.lyrixDemo.emit('lyrix://closing'));
    await page.waitForTimeout(150);
    const sent = await saved(page);
    const last = sent[sent.length - 1];
    check(
      sent.length === 2 && last.status.show_when_paused && last.status.profanity_filter,
      `the queued change was not sent at once: ${JSON.stringify(sent.map((c) => c.status))}`,
    );
    await page.waitForTimeout(1500);
    check((await saved(page)).length === 2, 'a change was sent again after the close');
  },

  /** Settings edited in the file show up when the window gets the focus back. */
  async 'settings reload on focus'(page, check) {
    await page.evaluate(() => {
      window.lyrixDemo.editSettings('status.line_template', '♫ {line}');
      window.lyrixDemo.editSettings('sources.preferred_apps', ['spotify']);
      window.dispatchEvent(new Event('focus'));
    });
    await page.waitForTimeout(200);
    const field = page.getByRole('textbox', { name: 'While a line is sung' });
    check((await field.inputValue()) === '♫ {line}', 'the edited template did not show up');
    await page.getByRole('switch', { name: 'Keep the status while paused' }).click();
    await page.waitForTimeout(100);
    const sent = await saved(page);
    check(
      sent.length === 1 &&
        sent[0].sources.preferred_apps.join() === 'spotify' &&
        sent[0].status.line_template === '♫ {line}',
      `the next save undid the file edit: ${JSON.stringify(sent.map((c) => [c.sources, c.status.line_template]))}`,
    );

    // A change waiting for its save is never replaced by a reload.
    const other = page.getByRole('textbox', { name: 'When there are no lyrics' });
    await other.fill('{title} by {artist}');
    await page.evaluate(() => {
      window.lyrixDemo.editSettings('status.no_lyrics_template', 'from the file');
      window.dispatchEvent(new Event('focus'));
    });
    await page.waitForTimeout(1600);
    const after = await saved(page);
    check(
      after[after.length - 1].status.no_lyrics_template === '{title} by {artist}',
      'a reload replaced a change waiting for its save',
    );
  },

  /** Each preview gets what the engine gives its template. */
  async 'previews like the engine'(page, check) {
    await page.getByRole('textbox', { name: 'While a line is sung' }).fill('{line}');
    await page.getByRole('textbox', { name: 'When there are no lyrics' }).fill('{title} · {line}{next}');
    await page.getByRole('textbox', { name: 'Intros and breaks' }).fill('{line}');
    await page.waitForTimeout(400);
    const [singing, none, intro] = await page.locator('.preview-text').allTextContents();
    check(singing && !singing.startsWith('Empty'), `"While singing" shows no line: ${singing}`);
    check(none === 'Paper Satellites', `"No lyrics" got a line: ${none}`);
    check(intro.startsWith('Empty'), `"Intro or break" got a line: ${intro}`);
  },

  /** Plain lyrics with no timing: listed, none current, no estimate. */
  async 'lyrics with no timing'(page, check) {
    const state = await page.evaluate(() => ({
      lines: document.querySelectorAll('#page-now .lyric:not(.lyric-break)').length,
      current: document.querySelectorAll('#page-now .is-current').length,
      head: document.querySelector('#page-now .lyrics-head').textContent,
      untimed: document.querySelector('#page-now .lyrics-viewport').classList.contains('is-untimed'),
      nudge: getComputedStyle(document.querySelector('#page-now .timing')).visibility,
      live: document.querySelector('#page-now .live-text').textContent,
    }));
    check(state.lines === 12 && state.untimed, `the lines are not listed: ${JSON.stringify(state)}`);
    check(state.current === 0, 'a line is marked as current');
    check(!state.head.includes('Estimated'), 'marked as estimated timing');
    check(state.nudge === 'hidden', 'the timing nudge shows');
    check(state.live === 'Northbound Lullaby (Live) · Ada Winterline', `the status is not the song: ${state.live}`);
    await page.evaluate(() => {
      window.location.hash = 'settings';
    });
    await page.waitForTimeout(400);
    const [singing] = await page.locator('.preview-text').allTextContents();
    check(singing === '🎵 The line you are hearing', `the preview took a line from them: ${singing}`);
  },

  /** The Advanced modal, as a real <dialog> or without showModal() (old Safari). */
  async 'the modal'(page, check, { label }) {
    const modal = page.locator('#modal');
    check((await modal.evaluate((el) => getComputedStyle(el).display)) === 'none', `${label}: the closed modal shows`);
    const accept = page.getByRole('switch', { name: 'I accept the risk' });
    await accept.click();
    await page.waitForTimeout(100);
    const box = await modal.boundingBox();
    check(box && box.width > 300 && box.height > 150, `${label}: the modal did not open`);
    if (box) {
      const centre = box.x + box.width / 2;
      check(Math.abs(centre - 520) < 4, `${label}: the modal is not centred (${centre})`);
    }
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    check(await modal.evaluate((el) => el.contains(document.activeElement)), `${label}: Tab left the modal`);
    await page.keyboard.press('Escape');
    await page.waitForTimeout(100);
    check(!(await modal.evaluate((el) => el.hasAttribute('open'))), `${label}: Escape did not close the modal`);
    check((await page.locator('.modal-backdrop').count()) === 0, `${label}: the backdrop stayed`);
    check((await accept.getAttribute('aria-checked')) === 'false', `${label}: Escape turned the risk on`);
    await accept.click();
    await page.getByRole('button', { name: 'I understand, turn on' }).click();
    await page.waitForTimeout(150);
    check((await accept.getAttribute('aria-checked')) === 'true', `${label}: confirming did not turn the risk on`);
    const sent = await saved(page);
    check(sent.length === 1 && sent[0].advanced.accept_ban_risk === true, `${label}: the risk was not saved once`);
  },
};

/** [name, page query, extra options] for each run. */
const RUNS = [
  ['a restart keeps the song on screen', 'scenario=playing&page=settings'],
  ['a restart with nothing playing', 'scenario=idle&page=settings'],
  ['starting', 'scenario=playing&boot=600'],
  ['closing sends a change waiting for its pause', 'scenario=playing&page=settings'],
  ['closing sends a change queued behind a save', 'scenario=playing&page=settings'],
  ['settings reload on focus', 'scenario=playing&page=settings'],
  ['previews like the engine', 'scenario=playing&page=settings'],
  ['lyrics with no timing', 'scenario=untimed'],
  ['the modal', 'scenario=playing&page=advanced', { label: 'dialog' }],
  [
    'the modal',
    'scenario=playing&page=advanced',
    {
      label: 'without showModal',
      init: () => {
        delete HTMLDialogElement.prototype.showModal;
        delete HTMLDialogElement.prototype.close;
      },
    },
  ],
];

/** Runs every flow in `context` against the window at `base`; returns the problems. */
export async function checkFlows(context, base) {
  const problems = [];
  for (const [name, query, options = {}] of RUNS) {
    const page = await context.newPage();
    const fail = (message) => problems.push(`${name}: ${message}`);
    page.on('console', (message) => {
      if (message.type() === 'error' || message.type() === 'warning') {
        fail(`${message.type()}: ${message.text()}`);
      }
    });
    page.on('pageerror', (error) => fail(`page error: ${error.message}`));
    if (options.init) {
      await page.addInitScript(options.init);
    }
    const url = `${base}/?${query}&theme=dark`;
    const reload = async () => {
      await page.goto(url, { waitUntil: 'load' });
    };
    try {
      if (name !== 'starting') {
        await reload();
        await page.waitForTimeout(700);
      }
      await FLOWS[name](page, (ok, message) => ok || fail(message), { ...options, reload });
    } catch (error) {
      fail(`threw: ${error.message}`);
    } finally {
      await page.close();
    }
  }
  return problems;
}
