// The two-phase connection-name refresh.
//
// `refresh_connection_names` writes nothing -- it returns proposals. They are
// rendered as an in-page diff (rather than a confirm() dialog, which cannot
// show a list legibly) and only `apply_changes` commits them. Splitting the
// propose step from the write is the core's design, and both the tray and this
// window honour it, so nothing can auto-apply whatever gcloud happened to say.

import { invoke } from './ipc.js';
import { $, clear, clearError, el, note, showError, withBusyNote } from './dom.js';
import { ROLE_LABEL } from './labels.js';
import { getPendingChanges, setPendingChanges } from './store.js';

/// Called after `apply_changes` commits, to re-read the profile list. Injected
/// so this module does not import the view that owns loading.
let onApplied = async () => {};

export function setAppliedListener(listener) {
  onApplied = listener;
}

export async function refreshConnectionNames() {
  note('save-note', '');
  try {
    const result = await withBusyNote('save-note', 'Asking gcloud…', () =>
      invoke('refresh_connection_names'),
    );
    clearError();
    setPendingChanges(result.changes || []);
    renderChanges();
  } catch (error) {
    hideChanges();
    showError('Could not refresh connection names', error);
  }
}

function renderChanges() {
  const panel = $('changes');
  const pendingChanges = getPendingChanges();
  clear(panel);
  panel.hidden = false;

  // Nothing to review is not a panel. A whole group plus a Dismiss button to
  // report "no changes" is the web habit of making a result into a page; the
  // footer status line already exists for exactly this and needs no dismissing.
  if (pendingChanges.length === 0) {
    panel.hidden = true;
    clear(panel);
    note('save-note', 'Connection names are already up to date.');
    return;
  }

  panel.appendChild(
    el(
      'h2',
      'group-label',
      `${pendingChanges.length} Proposed Change${pendingChanges.length === 1 ? '' : 's'}`,
    ),
  );

  const group = el('div', 'group');
  pendingChanges.forEach((change) => {
    const row = el('div', 'row row-change');
    row.appendChild(
      el(
        'span',
        'row-label',
        `${change.profileId ?? change.profile_id} · ${ROLE_LABEL[change.role] || change.role}`,
      ),
    );
    const diff = el('span', 'diff');
    // An empty `from` is the first-run case, where the seeded profile simply
    // had no name yet -- "Not set" reads better than a blank line.
    diff.appendChild(el('span', 'diff-from', change.from || 'Not set'));
    diff.appendChild(el('span', 'diff-arrow', '→'));
    diff.appendChild(el('span', 'diff-to', change.to));
    row.appendChild(diff);
    group.appendChild(row);
  });
  panel.appendChild(group);

  panel.appendChild(
    el('p', 'footnote', 'Nothing has been written yet. Review, then apply.'),
  );

  const actions = el('div', 'change-actions');
  const cancel = el('button', '', 'Cancel');
  cancel.type = 'button';
  cancel.addEventListener('click', hideChanges);
  const apply = el('button', 'default', 'Apply Changes');
  apply.type = 'button';
  apply.addEventListener('click', applyChanges);
  actions.appendChild(cancel);
  actions.appendChild(apply);
  panel.appendChild(actions);

  revealChanges();
}

/// Bring the diff into view.
///
/// Without this the panel renders below the fold of a short window with the
/// content region still scrolled to the top, so "Refresh Connection Names"
/// looks like a button that does nothing. Measured, not guessed: the panel's
/// top was 647px in a 560px viewport.
///
/// Deliberately instant. `behavior: 'smooth'` is the web idiom the checklist
/// names; AppKit scrolls a revealed control into view in one step.
function revealChanges() {
  const panel = $('changes');
  const content = document.querySelector('.content');
  if (!content) return;
  // Show the panel's top with a little of the group above it for context,
  // clamped to the scrollable range.
  const target = panel.offsetTop - 12;
  content.scrollTop = Math.max(0, Math.min(target, content.scrollHeight));
  // The panel is the subject now, so put the keyboard there too: Apply is the
  // default action and Escape cancels (see the document key handler).
  const apply =
    panel.querySelector('button.default') || panel.querySelector('button');
  if (apply) apply.focus();
}

export function hideChanges() {
  setPendingChanges([]);
  const panel = $('changes');
  panel.hidden = true;
  clear(panel);
}

export function changesAreOpen() {
  return !$('changes').hidden;
}

/// The button Return should activate while the diff is open: applying is the
/// action in front of the user, and saving underneath it would commit the wrong
/// thing.
export function changesDefaultButton() {
  return $('changes').querySelector('button.default');
}

async function applyChanges() {
  // Round-trip exactly the shape ChangeView deserializes: profileId (camelCase
  // per its serde rename), role, from, to.
  const changes = getPendingChanges().map((c) => ({
    profileId: c.profileId ?? c.profile_id,
    role: c.role,
    from: c.from,
    to: c.to,
  }));

  try {
    await invoke('apply_changes', { changes });
    clearError();
    hideChanges();
    note('save-note', 'Connection names updated.');
    await onApplied();
  } catch (error) {
    showError('Could not apply changes', error);
  }
}
