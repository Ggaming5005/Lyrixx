// Building blocks for the settings-style pages: grouped lists, rows,
// switches, fields bound to the settings model, tag inputs and the modal.

import { fill, h, nextId } from './dom.js';
import { icon } from './icons.js';

export function pageHeader(title, subtitle) {
  return h(
    'header',
    { class: 'page-header' },
    h('h1', { class: 'page-title', text: title }),
    subtitle ? h('p', { class: 'page-subtitle', text: subtitle }) : null,
  );
}

/** A group of rows, titled when `title` is given: `group({ title, icon, note }, ...rows)`. */
export function group({ title, icon: iconName, note }, ...rows) {
  const titleId = title ? nextId('group') : null;
  return h(
    'section',
    { class: 'group', 'aria-labelledby': titleId },
    title ? h('h2', { class: 'group-title', id: titleId }, iconName ? icon(iconName) : null, title) : null,
    h('div', { class: 'group-body' }, rows),
    note ? h('p', { class: 'group-note' }, note) : null,
  );
}

/**
 * One row: an optional leading icon or element, a title and description, and
 * a control on the right. `below` adds full-width content under the row.
 * `titleId` lets the control point at the title with `aria-labelledby`.
 */
export function row({ title, desc, lead, control, below, titleId = nextId('row'), className = '' }) {
  const leadEl =
    typeof lead === 'string' ? h('div', { class: 'row-lead', 'aria-hidden': 'true' }, icon(lead)) : lead || null;
  const head = [
    leadEl,
    h(
      'div',
      { class: 'row-text' },
      h('div', { class: 'row-title', id: titleId }, title),
      desc ? h('div', { class: 'row-desc' }, desc) : null,
    ),
    control ? h('div', { class: 'row-control' }, control) : null,
  ];
  if (!below) {
    return h('div', { class: `row ${className}`.trim() }, head);
  }
  const classes = ['row', 'row--stack', leadEl ? 'has-lead' : '', className].filter(Boolean).join(' ');
  return h('div', { class: classes }, h('div', { class: 'row-head' }, head), below);
}

/** A switch; `el.setChecked(value)` updates it without calling `onChange`. */
export function switchControl({ checked = false, labelledBy, label, onChange, disabled = false, danger = false }) {
  const el = h('button', {
    type: 'button',
    role: 'switch',
    class: danger ? 'switch switch--danger' : 'switch',
    'aria-checked': String(Boolean(checked)),
    'aria-labelledby': labelledBy,
    'aria-label': label,
    disabled,
  });
  el.addEventListener('click', () => {
    const next = el.getAttribute('aria-checked') !== 'true';
    el.setAttribute('aria-checked', String(next));
    onChange?.(next, el);
  });
  el.setChecked = (value) => el.setAttribute('aria-checked', String(Boolean(value)));
  return el;
}

/** A switch bound to a boolean setting. */
export function boundSwitch(model, path, { labelledBy, label, danger } = {}) {
  const el = switchControl({
    checked: model.get(path),
    labelledBy,
    label,
    danger,
    onChange: (value) => model.set(path, value, { now: true }),
  });
  model.subscribe(() => el.setChecked(model.get(path)));
  return el;
}

/** Shows the issues `Config::validate` reports for `path`, and marks `field` invalid. */
export function issueSlot(model, path, field) {
  const slot = h('div', { class: 'issue-slot', 'aria-live': 'polite' });
  slot.id = nextId('issue');
  const render = () => {
    const issues = model.issuesFor(path);
    slot.replaceChildren(
      ...issues.map((issue) =>
        h(
          'p',
          { class: 'issue', dataset: { severity: issue.severity } },
          icon(issue.severity === 'error' ? 'alert' : 'warning'),
          h('span', { text: issue.message }),
        ),
      ),
    );
    if (field) {
      field.setAttribute('aria-invalid', String(issues.some((issue) => issue.severity === 'error')));
      if (issues.length > 0) {
        field.setAttribute('aria-describedby', slot.id);
      } else {
        field.removeAttribute('aria-describedby');
      }
    }
  };
  model.subscribe(render);
  render();
  return slot;
}

/**
 * A text field bound to a string setting: saved after typing stops and when
 * the field loses focus. `toValue` converts the text before it is stored.
 */
export function boundText(model, path, { labelledBy, label, placeholder, mono, narrow, toValue, fromValue } = {}) {
  const read = () => (fromValue ? fromValue(model.get(path)) : model.get(path) ?? '');
  const input = h('input', {
    type: 'text',
    class: ['field', mono ? 'field--mono' : '', narrow ? 'field--narrow' : ''].join(' ').trim(),
    placeholder,
    spellcheck: 'false',
    autocomplete: 'off',
    'aria-labelledby': labelledBy,
    'aria-label': label,
  });
  input.value = read();
  input.addEventListener('input', () => model.set(path, toValue ? toValue(input.value) : input.value));
  input.addEventListener('blur', () => model.flush());
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      model.flush();
    }
  });
  model.subscribe(() => {
    if (document.activeElement !== input) {
      input.value = read();
    }
  });
  return input;
}

/** A − value + control for a number setting. */
export function boundStepper(model, path, { step, min, max, format, labelledBy, labels }) {
  const value = h('span', { class: 'stepper-value', 'aria-live': 'polite' });
  const show = () => {
    value.textContent = format(Number(model.get(path)) || 0);
  };
  const change = (delta) => {
    const next = Math.min(max, Math.max(min, (Number(model.get(path)) || 0) + delta));
    model.set(path, next);
  };
  const el = h(
    'div',
    { class: 'stepper', role: 'group', 'aria-labelledby': labelledBy },
    h('button', { type: 'button', 'aria-label': labels[0], onClick: () => change(-step) }, icon('minus')),
    value,
    h('button', { type: 'button', 'aria-label': labels[1], onClick: () => change(step) }, icon('plus')),
  );
  model.subscribe(show);
  show();
  return el;
}

/** A list of words as removable tags, bound to a string-array setting. */
export function boundTags(model, path, { placeholder, labelledBy }) {
  const input = h('input', {
    type: 'text',
    placeholder,
    spellcheck: 'false',
    autocomplete: 'off',
    'aria-labelledby': labelledBy,
  });
  const list = h('span', { class: 'tag-list', role: 'list' });
  const box = h('div', { class: 'tags' }, list, input);
  box.addEventListener('click', (event) => {
    if (event.target === box) {
      input.focus();
    }
  });

  const values = () => model.get(path) || [];
  const render = () => {
    const tags = values().map((value, index) =>
      h(
        'span',
        { class: 'tag', role: 'listitem' },
        value,
        h(
          'button',
          {
            type: 'button',
            'aria-label': `Remove ${value}`,
            onClick: () => {
              const next = values().filter((_, i) => i !== index);
              model.set(path, next, { now: true });
              input.focus();
            },
          },
          icon('close'),
        ),
      ),
    );
    list.replaceChildren(...tags);
  };
  const add = () => {
    const words = input.value
      .split(',')
      .map((word) => word.trim())
      .filter(Boolean);
    const current = values();
    const fresh = words.filter((word) => !current.some((v) => v.toLowerCase() === word.toLowerCase()));
    input.value = '';
    if (fresh.length > 0) {
      model.set(path, [...current, ...fresh], { now: true });
    }
  };
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' || event.key === ',') {
      event.preventDefault();
      add();
    } else if (event.key === 'Backspace' && input.value === '' && values().length > 0) {
      model.set(path, values().slice(0, -1), { now: true });
    }
  });
  input.addEventListener('blur', add);
  model.subscribe(render);
  render();
  return box;
}

/**
 * Asks for confirmation in the window's modal. Resolves to true when the
 * confirm button is chosen; Escape or Cancel resolve to false.
 */
export function confirmDialog({ title, text, warning, confirmLabel = 'Continue', iconName = 'warning' }) {
  const dialog = document.getElementById('modal');
  const titleId = nextId('modal-title');
  const cancel = h('button', { type: 'button', class: 'btn btn--lg', text: 'Cancel' });
  const confirm = h('button', { type: 'button', class: 'btn btn--danger btn--lg', text: confirmLabel });
  dialog.setAttribute('aria-labelledby', titleId);
  fill(
    dialog,
    h(
      'div',
      { class: 'modal-body' },
      h('div', { class: 'modal-icon', 'aria-hidden': 'true' }, icon(iconName)),
      h('h2', { class: 'modal-title', id: titleId, text: title }),
      text ? h('p', { class: 'modal-text', text }) : null,
      warning ? h('p', { class: 'modal-warning', role: 'alert', text: warning }) : null,
    ),
    h('div', { class: 'modal-actions' }, cancel, confirm),
  );
  return new Promise((resolve) => {
    let answer = false;
    cancel.addEventListener('click', () => dialog.close());
    confirm.addEventListener('click', () => {
      answer = true;
      dialog.close();
    });
    dialog.addEventListener('close', () => resolve(answer), { once: true });
    dialog.showModal();
    cancel.focus();
  });
}
