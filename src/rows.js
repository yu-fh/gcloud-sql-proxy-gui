// The row primitives every grouped list in this window is built from: an
// editable field row, a switch row, and a read-only row.
//
// These are the shape of a System Settings row -- label on the left, control or
// value right-aligned on the right -- and they are the reason the detail pane,
// the options group, and the info group all line up on the same right edge.

import { el } from './dom.js';

/// Generated ids, so every control has a label whose `for` actually points at
/// it even though nothing here is authored in HTML.
let fieldSeq = 0;

/// One list row: label on the left, right-aligned text field on the right.
/// This is the shape of every editable row in System Settings.
export function fieldRow(
  label,
  value,
  onInput,
  placeholder,
  type,
  sizeClass,
  fixedId,
) {
  const row = el('div', 'row');
  // A caller that needs to find this field again after a re-render passes its
  // own id; everything else gets a generated one. Either way the label's
  // `for` is set from the same value, so the association never breaks.
  const id = fixedId || `f${fieldSeq++}`;

  const caption = el('label', 'row-label', label);
  caption.setAttribute('for', id);
  row.appendChild(caption);

  const input = document.createElement('input');
  input.id = id;
  input.className = `row-input ${sizeClass || ''}`.trim();
  input.type = type || 'text';
  input.value = value === null || value === undefined ? '' : value;
  if (placeholder) input.placeholder = placeholder;
  // Every field here holds an identifier -- a connection name, a GCP project,
  // a port, a hostname. None of it is prose, so the browser's writing aids are
  // all wrong: spellcheck redlines "fh-dev-1234", autocapitalize would upcase
  // a project id, and autocorrect would rewrite a hostname. AppKit text fields
  // have these off unless asked; a webview has them on.
  input.spellcheck = false;
  input.setAttribute('autocomplete', 'off');
  input.setAttribute('autocorrect', 'off');
  input.setAttribute('autocapitalize', 'off');
  input.addEventListener('input', () => onInput(input.value));
  row.appendChild(input);

  return row;
}

/// A row with a leading label (plus optional secondary line) and a trailing
/// switch. macOS puts the control on the right of the row it governs.
export function switchRow(label, description, checked, onChange) {
  const row = el('div', 'row row-switch');
  const id = `f${fieldSeq++}`;

  const text = el('div', 'row-text');
  const caption = el('label', 'row-label', label);
  caption.setAttribute('for', id);
  text.appendChild(caption);
  if (description) text.appendChild(el('div', 'row-description', description));
  row.appendChild(text);

  // A real checkbox under an accent-filled pill: keyboard, VoiceOver, and
  // the space bar all keep working without reimplementing any of it.
  const wrap = el('label', 'switch');
  const input = document.createElement('input');
  input.id = id;
  input.type = 'checkbox';
  input.checked = Boolean(checked);
  input.addEventListener('change', () => onChange(input.checked));
  wrap.appendChild(input);
  wrap.appendChild(el('span', 'switch-track'));
  row.appendChild(wrap);

  return row;
}

/// A read-only row: label left, value right, in the secondary colour.
export function staticRow(label, value) {
  const row = el('div', 'row');
  row.appendChild(el('span', 'row-label', label));
  row.appendChild(el('span', 'row-value', value));
  return row;
}
