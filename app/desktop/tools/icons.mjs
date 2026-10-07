#!/usr/bin/env node
// Renders the app icon from icons/logo.svg.
//
//   node app/desktop/tools/icons.mjs
//
// Writes, with Playwright's Chromium (see screenshots.mjs for setup):
//   - a 1024x1024 PNG of icons/logo.svg, the source for `tauri icon`
//     (in a temporary folder; its path is printed)
//   - icons/tray.png: 64x64, the colour icon cropped to the rounded square
//   - icons/tray-mono.png: 64x64, the white glyph alone on transparent, for
//     menu bars and dark taskbars that want a template icon
//
// Then it prints the command that makes every platform icon from the 1024 PNG:
//   cd app/desktop && npx --yes @tauri-apps/cli@2 icon <png> -o icons
// (delete the android/ and ios/ folders that command adds).

import { mkdtempSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const iconsDir = resolve(here, '..', 'icons');
const logo = readFileSync(join(iconsDir, 'logo.svg'), 'utf8');

/** The glyph of logo.svg (same coordinates), white, with nothing behind it. */
const MONO = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="214 212 600 600">
  <g fill="none" stroke="#fff" stroke-width="76" stroke-linecap="round">
    <path d="M440 296 V650"/>
    <path d="M440 296 H740 M562 416 H722 M562 536 H652"/>
  </g>
  <ellipse cx="364" cy="668" rx="112" ry="90" transform="rotate(-20 364 668)" fill="#fff"/>
</svg>`;

/** logo.svg cropped to its rounded square, so the tray icon uses every pixel. */
const TRAY = logo.replace('viewBox="0 0 1024 1024" width="1024" height="1024"', 'viewBox="60 60 904 904"');

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

async function render(browser, svg, size, path) {
  const page = await browser.newPage({ viewport: { width: size, height: size }, deviceScaleFactor: 1 });
  const sized = svg.replace('<svg ', `<svg width="${size}" height="${size}" `).replace(/ width="1024" height="1024"/, '');
  await page.setContent(
    `<!doctype html><html><head><style>html,body{margin:0;background:transparent}svg{display:block}</style></head>` +
      `<body>${sized}</body></html>`,
  );
  await page.screenshot({ path, omitBackground: true, clip: { x: 0, y: 0, width: size, height: size } });
  await page.close();
  console.log(`saved ${path}`);
}

if (!TRAY.includes('viewBox="60 60 904 904"')) {
  console.error('icons/logo.svg no longer has the expected viewBox; update tools/icons.mjs.');
  process.exit(1);
}

const { chromium } = loadPlaywright();
const browser = await chromium.launch();
const source = join(mkdtempSync(join(tmpdir(), 'lyrix-icon-')), 'lyrix-1024.png');
try {
  await render(browser, logo, 1024, source);
  await render(browser, TRAY, 64, join(iconsDir, 'tray.png'));
  await render(browser, MONO, 64, join(iconsDir, 'tray-mono.png'));
} finally {
  await browser.close();
}
console.log(`\nNext: cd app/desktop && npx --yes @tauri-apps/cli@2 icon ${source} -o icons`);
console.log('then delete icons/android and icons/ios.');
