// Lyrics: where lyrics come from (in order), saved lyrics, and how to name
// your own files.

import { openFolder, openUrl } from '../actions.js';
import { boundSwitch, group, pageHeader, row } from '../controls.js';
import { attempt, h, nextId, toast } from '../dom.js';
import { icon } from '../icons.js';

function stepNumber(n) {
  return h('span', { class: 'step-number', 'aria-hidden': 'true', text: String(n) });
}

/** `Artist - Title.lrc` with the parts you replace highlighted. */
function fileName(parts) {
  return h(
    'div',
    { class: 'file-name' },
    parts.map((part) => (part.startsWith('{') ? h('em', { text: part.slice(1, -1) }) : part)),
  );
}

function fileRow(iconName, parts, desc) {
  return h(
    'div',
    { class: 'naming-file' },
    h('div', { class: 'file-icon', 'aria-hidden': 'true' }, icon(iconName)),
    h('div', {}, fileName(parts), h('div', { class: 'file-desc', text: desc })),
  );
}

const SAMPLE = [
  ['[00:12.40]', 'We folded maps into paper planes'],
  ['[00:16.90]', 'And threw them out of the seventh floor'],
  ['[00:21.30]', 'The city hummed in a borrowed key'],
];

/** A row for one online lyrics database, with its switch. */
function sourceRow({ api, model, step, key, title, desc, url, label }) {
  const titleId = nextId(key);
  return row({
    lead: stepNumber(step),
    titleId,
    title,
    desc: [
      `${desc} `,
      h(
        'button',
        { type: 'button', class: 'link-btn', onClick: () => openUrl(api, url) },
        label,
        icon('external'),
      ),
    ],
    control: boundSwitch(model, `lyrics.${key}`, { labelledBy: titleId }),
  });
}

export function createLyricsPage({ root, api, model }) {
  const cacheId = nextId('cache');

  const folderPath = h('span', { class: 'path', title: model.paths.lyricsDir, text: model.paths.lyricsDir });
  const yourFiles = row({
    lead: stepNumber(1),
    title: 'Your files',
    desc: 'Your own .lrc files come first, so you can add or fix any song. A .txt is used when no synced lyrics are found.',
    below: h(
      'div',
      { class: 'path-box' },
      h('span', { class: 'visually-hidden', text: 'Lyrics folder: ' }),
      folderPath,
      h(
        'button',
        { type: 'button', class: 'btn', onClick: () => openFolder(api, 'lyrics') },
        icon('folder'),
        'Open folder',
      ),
    ),
  });

  const lrclib = sourceRow({
    api,
    model,
    step: 2,
    key: 'lrclib',
    title: 'LRCLIB',
    desc: 'A free, open lyrics database, no account needed.',
    url: 'https://lrclib.net',
    label: 'lrclib.net',
  });

  const netease = sourceRow({
    api,
    model,
    step: 3,
    key: 'netease',
    title: 'NetEase Cloud Music',
    desc: 'A huge catalog of timed lyrics in many languages, no account needed. It isn’t an official service, so it may stop answering.',
    url: 'https://music.163.com',
    label: 'music.163.com',
  });

  const kugou = sourceRow({
    api,
    model,
    step: 4,
    key: 'kugou',
    title: 'Kugou',
    desc: 'Strongest for Chinese and other Asian songs, no account needed. It isn’t an official service either.',
    url: 'https://www.kugou.com',
    label: 'kugou.com',
  });

  const flowEnd = h(
    'div',
    { class: 'flow-end' },
    icon('note'),
    h('span', { text: 'Nothing found? Your status shows the song name instead.' }),
  );

  const clearButton = h(
    'button',
    {
      type: 'button',
      class: 'btn',
      onClick: async () => {
        const removed = await attempt(api.clearCache(), 'Could not clear saved lyrics');
        if (typeof removed === 'number') {
          toast(removed === 0 ? 'Nothing to clear' : `Cleared ${removed} saved ${removed === 1 ? 'song' : 'songs'}`);
        }
      },
    },
    icon('trash'),
    'Clear',
  );

  const sample = h(
    'pre',
    { class: 'lrc-sample', 'aria-label': 'Example of an .lrc file' },
    SAMPLE.map(([time, text], i) => [h('b', { text: time }), ` ${text}${i < SAMPLE.length - 1 ? '\n' : ''}`]),
  );

  root.append(
    h(
      'div',
      { class: 'page-inner' },
      pageHeader('Lyrics', 'Lyrix finds lyrics on its own. This is where it looks, in order.'),
      group({ title: 'Where lyrics come from', icon: 'search' }, yourFiles, lrclib, netease, kugou, flowEnd),
      group(
        {
          title: 'Saved lyrics',
          icon: 'database',
          note: 'Your own .lrc files always come first, even for songs Lyrix already saved.',
        },
        row({
          lead: 'database',
          titleId: cacheId,
          title: 'Keep lyrics on this computer',
          desc: 'Songs you played before load instantly, even offline.',
          control: boundSwitch(model, 'lyrics.cache', { labelledBy: cacheId }),
        }),
        row({
          lead: 'trash',
          title: 'Clear saved lyrics',
          desc: 'Lyrix looks each song up again the next time it plays.',
          control: clearButton,
        }),
      ),
      group(
        {
          title: 'Naming your files',
          icon: 'fileText',
          note: 'Names are matched loosely: letter case, accents, punctuation and extras like “(Remastered)” don’t matter. When both exist, .lrc wins over .txt.',
        },
        h(
          'div',
          { class: 'naming' },
          fileRow('fileText', ['{Artist}', ' - ', '{Title}', '.lrc'], 'Synced lyrics: every line starts at its own time.'),
          fileRow(
            'file',
            ['{Artist}', ' - ', '{Title}', '.txt'],
            'Plain lyrics, used when no synced lyrics are found. Lyrix spreads them over the song and marks the timing as estimated.',
          ),
          fileRow('file', ['{Title}', '.lrc'], 'Works too, when the title is enough to tell songs apart.'),
          sample,
        ),
      ),
    ),
  );

  model.subscribe(() => {
    folderPath.textContent = model.paths.lyricsDir || '';
    folderPath.title = model.paths.lyricsDir || '';
  });

  return { show() {}, hide() {} };
}
