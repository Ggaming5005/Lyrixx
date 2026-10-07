#!/usr/bin/env node
// Takes the README screenshots of the Lyrix window with its demo backend.
//
//   node app/desktop/tools/screenshots.mjs
//
// Needs Node 18+ and Playwright with Chromium (`npm i -g playwright` and
// `npx playwright install chromium`, or NODE_PATH pointing at a
// node_modules folder that has it). Nothing else.
//
// It serves app/desktop/ui on a local port (with a strict Content Security
// Policy like the app's), opens it in Chromium with the demo backend
// (ui/js/mock.js), saves the screenshots to docs/images/ and then opens every
// scenario on every page, failing when the console shows an error. It also
// runs the checks in old-webkit.mjs (nothing newer than Safari 14, for
// macOS 11) and flows.mjs (restarts, closing, reloads, previews, modal).
//
// On Linux, the window's font stack ends in system-ui, which is usually
// DejaVu Sans. Set LYRIX_SCREENSHOT_FONTS to a folder with Inter (or install
// Inter) to get screenshots closer to Windows and macOS.

import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkFlows } from './flows.mjs';
import { checkOldWebKit } from './old-webkit.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const uiDir = resolve(here, '..', 'ui');
const outDir = resolve(here, '..', '..', '..', 'docs', 'images');

const WINDOW = { width: 1040, height: 680 };
const SCALE = 2;
const JPEG_QUALITY = 88;
const BUDGET_BYTES = 5 * 1024 * 1024;

/** The README screenshots: file name and window URL query. */
const SHOTS = [
  ['now-playing-dark.jpg', 'scenario=playing&theme=dark'],
  ['now-playing-light.jpg', 'scenario=playing&theme=light'],
  ['lyrics-not-found.jpg', 'scenario=notFound&theme=dark'],
  ['connections.jpg', 'scenario=playing&page=connections&theme=dark'],
  ['settings.jpg', 'scenario=playing&page=settings&theme=dark'],
  ['advanced.jpg', 'scenario=playing&page=advanced&theme=dark'],
];

const SCENARIOS = [
  'playing',
  'searching',
  'notFound',
  'instrumental',
  'idle',
  'paused',
  'discordWaiting',
  'error',
  'estimated',
  'untimed',
  'musicPaused',
];
const PAGES = ['now', 'lyrics', 'connections', 'settings', 'advanced'];

/** Like the app: scripts only from the page's own files, no inline code, no eval. */
const CSP = [
  "default-src 'self'",
  "script-src 'self'",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data: https: http:",
  "connect-src 'self'",
  "object-src 'none'",
  "base-uri 'none'",
].join('; ');

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

function loadPlaywright() {
  const require = createRequire(import.meta.url);
  for (const name of [process.env.PLAYWRIGHT_MODULE, 'playwright', 'playwright-core'].filter(Boolean)) {
    try {
      return require(name);
    } catch {
      // try the next one
    }
  }
  console.error('Playwright was not found. Install it (npm i -g playwright) or set NODE_PATH.');
  process.exit(2);
}

function serve(root) {
  const server = createServer((req, res) => {
    const path = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    const file = normalize(join(root, path === '/' ? 'index.html' : path));
    if (!file.startsWith(root + sep)) {
      res.writeHead(403).end();
      return;
    }
    try {
      if (!statSync(file).isFile()) {
        throw new Error('not a file');
      }
      res.writeHead(200, {
        'Content-Type': TYPES[extname(file)] || 'application/octet-stream',
        'Content-Security-Policy': CSP,
        'Cache-Control': 'no-store',
      });
      res.end(readFileSync(file));
    } catch {
      res.writeHead(404).end('not found');
    }
  });
  return new Promise((done) => server.listen(0, '127.0.0.1', () => done(server)));
}

/** On Linux: a fontconfig file that prefers Inter for sans-serif and system-ui. */
function fontEnv() {
  if (process.platform !== 'linux') {
    return { env: process.env, cleanup() {} };
  }
  const dir = mkdtempSync(join(tmpdir(), 'lyrix-fonts-'));
  const extra = process.env.LYRIX_SCREENSHOT_FONTS ? `<dir>${resolve(process.env.LYRIX_SCREENSHOT_FONTS)}</dir>` : '';
  const prefer = (family) =>
    `<alias binding="strong"><family>${family}</family><prefer><family>Inter</family></prefer></alias>`;
  writeFileSync(
    join(dir, 'fonts.conf'),
    `<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig>` +
      `<include ignore_missing="yes">/etc/fonts/fonts.conf</include>${extra}` +
      `${prefer('sans-serif')}${prefer('system-ui')}</fontconfig>`,
  );
  return {
    env: { ...process.env, FONTCONFIG_FILE: join(dir, 'fonts.conf') },
    cleanup: () => rmSync(dir, { recursive: true, force: true }),
  };
}

/** Opens `query`, waits for the window to settle and returns the console errors. */
async function open(page, base, query, settleMs) {
  const errors = [];
  const onConsole = (message) => {
    if (message.type() === 'error' || message.type() === 'warning') {
      errors.push(`${message.type()}: ${message.text()}`);
    }
  };
  const onError = (error) => errors.push(`page error: ${error.message}`);
  page.on('console', onConsole);
  page.on('pageerror', onError);
  await page.goto(`${base}/?${query}`, { waitUntil: 'load' });
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(settleMs);
  page.off('console', onConsole);
  page.off('pageerror', onError);
  return errors;
}

async function main() {
  const { chromium } = loadPlaywright();
  const server = await serve(uiDir);
  const base = `http://127.0.0.1:${server.address().port}`;
  const fonts = fontEnv();
  const browser = await chromium.launch({ env: fonts.env });
  const context = await browser.newContext({ viewport: WINDOW, deviceScaleFactor: SCALE });
  const page = await context.newPage();
  const problems = [];
  let total = 0;

  console.log('checking the scripts and styles for Safari 14…');
  problems.push(...checkOldWebKit(uiDir));

  try {
    for (const [name, query] of SHOTS) {
      const errors = await open(page, base, query, 1600);
      problems.push(...errors.map((error) => `${query}: ${error}`));
      const file = join(outDir, name);
      await page.screenshot({ path: file, type: 'jpeg', quality: JPEG_QUALITY });
      const size = statSync(file).size;
      total += size;
      console.log(`saved docs/images/${name} (${Math.round(size / 1024)} KB)`);
    }

    console.log('checking every scenario on every page…');
    for (const scenario of SCENARIOS) {
      for (const pageName of PAGES) {
        for (const theme of ['dark', 'light']) {
          const query = `scenario=${scenario}&page=${pageName}&theme=${theme}`;
          const errors = await open(page, base, query, 250);
          problems.push(...errors.map((error) => `${query}: ${error}`));
        }
      }
    }

    console.log('checking restarts, closing, reloads, previews and the modal…');
    problems.push(...(await checkFlows(context, base)));
  } finally {
    await browser.close();
    server.close();
    fonts.cleanup();
  }

  console.log(`screenshots: ${Math.round(total / 1024)} KB in total`);
  if (total > BUDGET_BYTES) {
    problems.push(`screenshots are ${Math.round(total / 1024)} KB, over the 5 MB budget`);
  }
  if (problems.length > 0) {
    console.error(`\n${problems.length} problem(s):\n${problems.join('\n')}`);
    process.exit(1);
  }
  console.log('no console errors');
}

await main();
