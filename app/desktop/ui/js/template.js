// A JavaScript copy of the status template renderer in src/template.rs, used by
// the demo backend (mock.js) so its previews read like the real thing.
// The real app renders templates in Rust (`preview_status`).

const SEPARATORS = ['·', '•', '-', '–', '—', '|', '/', ':', ','];

/**
 * Renders `{line}` `{next}` `{title}` `{artist}` `{album}` the way the engine
 * does: `{{`/`}}` are literal braces, unknown placeholders stay as written, and
 * separators or brackets left dangling by an empty value are dropped.
 */
export function renderTemplate(template, ctx) {
  let pieces = tokenize(template, ctx);
  dropEmptyBrackets(pieces);
  pieces = mergeEmptyRuns(pieces);
  dropDanglingSeparators(pieces);
  return join(pieces);
}

/** Masks each listed word (whole words, any case) as `f***`. */
export function filterProfanity(text, words) {
  const banned = new Set(words.map((w) => w.trim().toLowerCase()).filter(Boolean));
  if (banned.size === 0) {
    return text;
  }
  return text.replace(/[\p{L}\p{N}]+/gu, (word) => {
    if (!banned.has(word.toLowerCase())) {
      return word;
    }
    const [first, ...rest] = [...word];
    return first + '*'.repeat(rest.length);
  });
}

const isLit = (piece) => piece && piece.kind === 'lit';
const isVal = (piece) => piece && piece.kind === 'val';
const isEmpty = (piece) => piece && piece.kind === 'empty';
const isBlank = (text) => text.trim() === '';

function lookup(name, ctx) {
  switch (name) {
    case 'line':
      return ctx.line ?? '';
    case 'next':
      return ctx.next ?? '';
    case 'title':
      return ctx.title ?? '';
    case 'artist':
      return ctx.artist ?? '';
    case 'album':
      return ctx.album ?? '';
    default:
      return null;
  }
}

function tokenize(template, ctx) {
  const pieces = [];
  let lit = '';
  let rest = template;
  for (;;) {
    const brace = rest.search(/[{}]/);
    if (brace < 0) {
      break;
    }
    lit += rest.slice(0, brace);
    const tail = rest.slice(brace);
    if (tail.startsWith('{{')) {
      lit += '{';
      rest = tail.slice(2);
    } else if (tail.startsWith('}}')) {
      lit += '}';
      rest = tail.slice(2);
    } else if (tail.startsWith('{')) {
      const after = tail.slice(1);
      const end = after.search(/[{}]/);
      if (end >= 0 && after[end] === '}') {
        const name = after.slice(0, end);
        const value = lookup(name, ctx);
        if (value === null) {
          lit += `{${name}}`;
        } else {
          if (lit) {
            pieces.push({ kind: 'lit', text: lit });
            lit = '';
          }
          const trimmed = value.trim();
          pieces.push(trimmed ? { kind: 'val', text: trimmed } : { kind: 'empty', space: false });
        }
        rest = after.slice(end + 1);
      } else {
        // An opening brace with no closing one is literal text.
        lit += '{';
        rest = after;
      }
    } else {
      // A lone closing brace is literal text.
      lit += '}';
      rest = tail.slice(1);
    }
  }
  lit += rest;
  if (lit) {
    pieces.push({ kind: 'lit', text: lit });
  }
  return pieces;
}

/** `({album})` with no album: drops the brackets around the empty value. */
function dropEmptyBrackets(pieces) {
  for (let i = 1; i < pieces.length - 1; i += 1) {
    const [left, middle, right] = [pieces[i - 1], pieces[i], pieces[i + 1]];
    if (!isEmpty(middle) || !isLit(left) || !isLit(right)) {
      continue;
    }
    const last = left.text.slice(-1);
    const close = last === '(' ? ')' : last === '[' ? ']' : null;
    if (close && right.text.startsWith(close)) {
      right.text = right.text.slice(1);
      left.text = left.text.slice(0, -1);
    }
  }
}

/** Drops empty literals and merges empty values separated only by whitespace. */
function mergeEmptyRuns(pieces) {
  const out = [];
  for (const piece of pieces) {
    if (isLit(piece) && piece.text === '') {
      continue;
    }
    if (!isEmpty(piece)) {
      out.push(piece);
      continue;
    }
    const last = out[out.length - 1];
    const whitespaceBetween = isLit(last) && isBlank(last.text) && isEmpty(out[out.length - 2]);
    if (whitespaceBetween) {
      out.pop();
    }
    const top = out[out.length - 1];
    if (isEmpty(top)) {
      top.space = top.space || piece.space || whitespaceBetween;
    } else {
      out.push({ kind: 'empty', space: piece.space });
    }
  }
  return out;
}

function stripLeadingSeparator(text) {
  const trimmed = text.trimStart();
  const first = [...trimmed][0];
  if (first === undefined) {
    return null;
  }
  const after = trimmed.slice(first.length);
  const standalone = after === '' || /^\s/u.test(after);
  return SEPARATORS.includes(first) && standalone ? after : null;
}

function stripTrailingSeparator(text) {
  const chars = [...text.trimEnd()];
  const last = chars.pop();
  if (last === undefined) {
    return null;
  }
  const before = chars.join('');
  const standalone = before === '' || /\s$/u.test(before);
  return SEPARATORS.includes(last) && standalone ? before : null;
}

/** Removes the separators that empty values leave dangling (see template.rs). */
function dropDanglingSeparators(pieces) {
  let left = null;
  for (let i = 0; i < pieces.length; i += 1) {
    const piece = pieces[i];
    if (isVal(piece) || (isLit(piece) && !isBlank(piece.text))) {
      left = i;
      continue;
    }
    if (!isEmpty(piece)) {
      continue;
    }
    let right = null;
    for (let j = i + 1; j < pieces.length; j += 1) {
      if (isVal(pieces[j]) || (isLit(pieces[j]) && !isBlank(pieces[j].text))) {
        right = j;
        break;
      }
    }
    const leftRest = left !== null && isLit(pieces[left]) ? stripTrailingSeparator(pieces[left].text) : null;
    const rightRest = right !== null && isLit(pieces[right]) ? stripLeadingSeparator(pieces[right].text) : null;
    const leftIsValue = left !== null && isVal(pieces[left]);
    const rightIsValue = right !== null && isVal(pieces[right]);
    let replace = null;
    if (leftRest !== null && rightRest !== null) {
      replace = [right, rightRest];
    } else if (leftRest !== null && !rightIsValue) {
      replace = [left, leftRest];
    } else if (rightRest !== null && !leftIsValue) {
      replace = [right, rightRest];
    }
    if (replace) {
      pieces[replace[0]] = { kind: 'lit', text: replace[1] };
    }
  }
}

/** Concatenates; whitespace touching an empty value collapses to one space. */
function join(pieces) {
  let out = '';
  let afterEmpty = false;
  let pendingSpace = false;
  for (const piece of pieces) {
    if (isEmpty(piece)) {
      const kept = out.trimEnd().length;
      if (kept < out.length || piece.space) {
        pendingSpace = true;
      }
      out = out.slice(0, kept);
      afterEmpty = true;
      continue;
    }
    let text = piece.text;
    if (afterEmpty) {
      const trimmed = text.trimStart();
      if (trimmed.length < text.length) {
        pendingSpace = true;
      }
      text = trimmed;
    }
    if (!text) {
      continue;
    }
    if (pendingSpace && out) {
      out += ' ';
    }
    pendingSpace = false;
    afterEmpty = false;
    out += text;
  }
  return out.trim();
}
