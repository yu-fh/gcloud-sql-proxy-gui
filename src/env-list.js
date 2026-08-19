// The environment source list: one row per profile, and the keyboard behaviour
// a native list has.
//
// Split from the detail pane it drives because the two have different jobs and
// different reasons to change: this one is a list widget (selection, arrow keys,
// type-ahead), the other is a form.

import { $, clear, el } from './dom.js';
import { STATUS_LABEL } from './labels.js';
import { getProfiles, getSelectedId, setSelectedId } from './store.js';

/// Called after the selection changes, so the detail pane and the +/− buttons
/// follow. Injected rather than imported to keep this module from depending on
/// the form it drives.
let onSelectionChange = () => {};

export function setSelectionListener(listener) {
  onSelectionChange = listener;
}

/// The grouped environment list: one row per profile, with a status dot on the
/// left and the status/production markers on the right. Selecting a row swaps
/// the detail group below it.
export function renderEnvList() {
  const list = $('env-list');
  const profiles = getProfiles();
  const selectedId = getSelectedId();
  clear(list);

  if (profiles.length === 0) {
    // Terse, as a native empty state is. The + button is directly below and
    // already says what it does; a sentence explaining it would be the web
    // habit of over-explaining an interface that is already self-evident.
    list.appendChild(el('div', 'row row-empty', 'No environments.'));
    return;
  }

  profiles.forEach((profile) => {
    const selected = profile.id === selectedId;
    const row = el('div', 'row row-select');
    row.setAttribute('role', 'option');
    row.setAttribute('aria-selected', String(selected));
    // Rows are reachable by keyboard: only the selected one is in the tab
    // order, arrow keys move within the list, as a native source list does.
    row.tabIndex = selected ? 0 : -1;
    if (selected) row.classList.add('selected');
    if (profile.danger) row.classList.add('danger');

    const status = profile.status || 'stopped';
    row.appendChild(el('span', `dot dot-${status}`));
    row.appendChild(el('span', 'row-label', profile.name || profile.id));

    const trailing = el('span', 'row-trailing');
    if (profile.danger) {
      // prd is the main hazard in this app; the marker rides in the list so it
      // is visible before the row is ever selected.
      const tag = el('span', 'tag tag-danger', 'Production');
      tag.title = 'Starting this profile connects you to production.';
      trailing.appendChild(tag);
    }
    if (status !== 'stopped') {
      trailing.appendChild(
        el('span', `status status-${status}`, STATUS_LABEL[status] || status),
      );
    }
    row.appendChild(trailing);
    // The chevron says "this row opens something", exactly as a System
    // Settings disclosure row does.
    row.appendChild(el('span', 'chevron', '›'));

    row.addEventListener('click', () => select(profile.id));
    row.addEventListener('keydown', (event) => onListKey(event, profile.id));

    list.appendChild(row);
  });
}

export function select(id) {
  if (id === getSelectedId()) return;
  setSelectedId(id);
  renderEnvList();
  onSelectionChange();
}

/// Type-ahead buffer. Native lists accumulate keystrokes for about a second,
/// so typing "st" lands on "Staging" rather than jumping to "S" then "T".
let typeAhead = '';
let typeAheadTimer = null;
const TYPE_AHEAD_MS = 900;

/// Find the next profile whose name starts with `prefix`, searching forward
/// from the current selection and wrapping, as AppKit's list search does.
function matchByPrefix(prefix, fromIndex) {
  const profiles = getProfiles();
  const lower = prefix.toLowerCase();
  for (let step = 0; step < profiles.length; step += 1) {
    // Start at the current row when the buffer is still growing (so "st"
    // keeps matching Staging), otherwise at the one after it.
    const i = (fromIndex + step) % profiles.length;
    const name = (profiles[i].name || profiles[i].id).toLowerCase();
    if (name.startsWith(lower)) return profiles[i];
  }
  return null;
}

/// Arrow keys move the selection, Home/End jump to the ends, Space/Enter
/// re-assert it, and letters do type-ahead. Mirrors how a native list behaves
/// when it has focus.
function onListKey(event, id) {
  const profiles = getProfiles();
  const index = profiles.findIndex((p) => p.id === id);
  let next = null;

  if (event.key === 'ArrowDown') next = profiles[index + 1];
  else if (event.key === 'ArrowUp') next = profiles[index - 1];
  else if (event.key === 'Home') next = profiles[0];
  else if (event.key === 'End') next = profiles[profiles.length - 1];
  else if (event.key === 'Enter' || event.key === ' ') next = profiles[index];
  else if (
    // A single printable character with no command/control modifier is a
    // type-ahead keystroke. Alt is allowed through because option-accented
    // letters are still letters.
    event.key.length === 1 &&
    !event.metaKey &&
    !event.ctrlKey &&
    event.key !== ' '
  ) {
    clearTimeout(typeAheadTimer);
    typeAheadTimer = setTimeout(() => {
      typeAhead = '';
    }, TYPE_AHEAD_MS);

    const extended = typeAhead + event.key;
    // A repeated single character cycles through the rows starting with it,
    // which is what a native list does when you press the same letter twice.
    const repeated = extended.length > 1 && /^(.)\1+$/.test(extended);

    if (repeated) {
      typeAhead = extended;
      next = matchByPrefix(event.key, index + 1);
    } else {
      // Growing the buffer refines the current match, so the search starts at
      // the selected row rather than after it.
      next = matchByPrefix(extended, index);
      if (next) {
        typeAhead = extended;
      } else {
        // The extended prefix matches nothing. A native list does not just
        // beep: it treats the keystroke as the start of a new search, so
        // typing "s" then "p" lands on Production rather than dying on "sp".
        typeAhead = event.key;
        next = matchByPrefix(event.key, index);
      }
    }
  } else return;

  event.preventDefault();
  if (!next) return;
  select(next.id);
  focusRow(next.id);
}

/// Put focus on a row by profile id. `renderEnvList` replaces the nodes on
/// every selection change, so focus has to be re-established on the new node
/// rather than kept on the one that was clicked.
function focusRow(id) {
  const rows = $('env-list').querySelectorAll('.row-select');
  const target = rows[getProfiles().findIndex((p) => p.id === id)];
  if (target) target.focus();
}
