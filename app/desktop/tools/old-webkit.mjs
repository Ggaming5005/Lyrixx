#!/usr/bin/env node
// Checks that the window still works in the oldest WebKit Lyrix supports.
//
//   node app/desktop/tools/old-webkit.mjs
//
// On macOS the window is the system WebKit. tauri.conf.json allows macOS
// 11.0, which ships Safari 14, so the window's scripts and styles must not
// need anything newer (or must fall back on their own):
//
// - Scripts are parsed as ES2021 modules, the newest syntax Safari 14 has
//   (no top-level await, no class fields), with acorn when it can be found
//   (NODE_PATH, like Playwright for screenshots.mjs); otherwise this part
//   is skipped with a note.
// - Scripts, styles and index.html are searched for APIs and CSS that
//   Safari 14 lacks and that have no fallback here.
//
// screenshots.mjs runs this too. Known and accepted: Safari 14.0 (macOS
// 11.0 to 11.2; 11.3 brings 14.1) ignores `gap` in flex layouts, so spacing
// is tighter there.

import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));

/** [pattern, what it needs] for scripts and index.html. */
const SCRIPT_RULES = [
  [/\bObject\.hasOwn\(/, 'Object.hasOwn needs Safari 15.4; use Object.prototype.hasOwnProperty.call'],
  [/\.at\(/, 'Array/String.prototype.at needs Safari 15.4'],
  [/\bstructuredClone\(/, 'structuredClone needs Safari 15.4'],
  [/\.findLast(Index)?\(/, 'findLast needs Safari 15.4'],
  [/\.(toSorted|toReversed|toSpliced)\(/, 'change-array-by-copy methods need Safari 16'],
  [/\bObject\.groupBy\(|\bPromise\.withResolvers\(/, 'needs Safari 17.4'],
  [/\bcrypto\.randomUUID\(/, 'crypto.randomUUID needs Safari 15.4'],
  [/\brequestIdleCallback\(/, 'Safari has no requestIdleCallback'],
  [/\(\?<[=!]/, 'regular expression lookbehind needs Safari 16.4'],
  [/\binert\b/, 'inert needs Safari 15.5'],
  [/\bpopover\b|showPopover\(/, 'popover needs Safari 17'],
];

/** [pattern, what it needs] for styles. */
const STYLE_RULES = [
  [/:has\(/, ':has() needs Safari 15.4'],
  [/color-mix\(/, 'color-mix() needs Safari 16.2'],
  [/@layer\b/, '@layer needs Safari 15.4'],
  [/@container\b/, 'container queries need Safari 16'],
  [/@property\b/, '@property needs Safari 16.4'],
  [/^\s*&/, 'CSS nesting needs Safari 16.5'],
  [/\baspect-ratio\s*:/, 'aspect-ratio needs Safari 15; use a padding-top box'],
  [/^\s*inset\s*:/, 'inset needs Safari 14.1; use top, right, bottom and left'],
  [/\binset-(inline|block)\b/, 'inset-inline/-block need Safari 14.1'],
  [/^\s*(translate|scale|rotate)\s*:/, 'individual transform properties need Safari 14.1; use transform'],
  [/\d(d|s|l)v(h|w)\b/, 'dynamic viewport units need Safari 15.4'],
  [/\b(oklch|oklab|lab|lch|hwb|light-dark)\(/, 'this color function needs Safari 15 or later'],
  [/overflow(-[xy])?\s*:\s*clip\b/, 'overflow: clip needs Safari 16'],
];

function files(dir, extensions) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      found.push(...files(path, extensions));
    } else if (extensions.includes(extname(entry.name))) {
      found.push(path);
    }
  }
  return found.sort();
}

/** Lines of `text` without `//` and block comments (strings are kept). */
function codeLines(text, lineComments) {
  const blanked = text.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\n]/g, ' '));
  const lines = blanked.split('\n');
  return lineComments ? lines.map((line) => line.replace(/(^|\s)\/\/.*$/, '$1')) : lines;
}

function loadAcorn() {
  const require = createRequire(import.meta.url);
  try {
    return require('acorn');
  } catch {
    return null;
  }
}

/**
 * Checks `ui/` and returns the problems found, as readable lines. Prints a
 * note when the syntax part is skipped.
 */
export function checkOldWebKit(uiDir = resolve(here, '..', 'ui')) {
  const problems = [];
  const where = (file, line) => `${relative(uiDir, file)}:${line}`;
  const acorn = loadAcorn();
  if (!acorn) {
    console.warn('old-webkit: acorn was not found (set NODE_PATH), so the scripts were not parsed as ES2021.');
  }

  for (const file of files(uiDir, ['.js', '.html'])) {
    const text = readFileSync(file, 'utf8');
    if (acorn && extname(file) === '.js') {
      const module = !file.endsWith('theme-boot.js');
      try {
        acorn.parse(text, { ecmaVersion: 2021, sourceType: module ? 'module' : 'script' });
      } catch (error) {
        problems.push(`${where(file, error.loc?.line ?? 0)}: not ES2021 (Safari 14): ${error.message}`);
      }
    }
    codeLines(text, extname(file) === '.js').forEach((line, index) => {
      for (const [pattern, reason] of SCRIPT_RULES) {
        if (pattern.test(line)) {
          problems.push(`${where(file, index + 1)}: ${reason}`);
        }
      }
    });
  }

  for (const file of files(uiDir, ['.css'])) {
    const lines = codeLines(readFileSync(file, 'utf8'), false);
    lines.forEach((line, index) => {
      for (const [pattern, reason] of STYLE_RULES) {
        if (pattern.test(line)) {
          problems.push(`${where(file, index + 1)}: ${reason}`);
        }
      }
      if (/overflow-wrap\s*:\s*anywhere/.test(line) && !/word-break\s*:\s*break-word/.test(lines[index - 1] || '')) {
        problems.push(
          `${where(file, index + 1)}: overflow-wrap: anywhere needs Safari 15.4; put word-break: break-word before it`,
        );
      }
    });
  }
  return problems;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const problems = checkOldWebKit();
  if (problems.length > 0) {
    console.error(`${problems.length} problem(s) for Safari 14 (macOS 11):\n${problems.join('\n')}`);
    process.exit(1);
  }
  console.log('old-webkit: nothing newer than Safari 14 without a fallback');
}
