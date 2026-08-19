// The Profiles section: the detail form for the selected environment, and the
// four operations that move profiles around (load, save, add, delete).
//
// The environment list itself lives in env-list.js; this module owns the form
// and the backend round-trips.

import { confirmDestructive, invoke } from './ipc.js';
import { $, clear, clearError, copyText, el, note, showError } from './dom.js';
import { STATUS_LABEL, ROLE_LABEL } from './labels.js';
import { fieldRow, staticRow, switchRow } from './rows.js';
import { renderEnvList } from './env-list.js';
import { renderLogFilterOptions } from './logs-view.js';
import {
  getProfiles,
  selectedIndex,
  selectedProfile,
  setProfiles,
  setSelectedId,
} from './store.js';

/// The Name field carries a fixed id so the new-profile flow can focus it
/// after a re-render without holding on to a node the render just replaced.
const NAME_FIELD_ID = 'field-name';

/// The name a brand-new profile gets. The backend slugifies it into a unique
/// id, so adding twice in a row is fine; the user renames it immediately
/// anyway, which is why the field is focused and selected on create.
const NEW_PROFILE_NAME = 'New Environment';

export async function loadProfiles() {
  try {
    // ProfileView is the Profile flattened together with status/detail/
    // fixCommand -- the view-only fields are stripped again in saveProfiles.
    const loaded = await invoke('list_profiles');
    setProfiles(loaded);
    clearError();

    // Keep the current selection if it still exists, otherwise fall back to
    // the first profile so the detail pane is never pointlessly empty.
    if (!selectedProfile()) {
      setSelectedId(loaded.length > 0 ? loaded[0].id : null);
    }

    renderEnvList();
    renderDetail();
    renderListControls();
    renderLogFilterOptions();
  } catch (error) {
    showError('Could not load profiles', error);
    // The list could not be read, so nothing on screen is selectable and
    // nothing should look deletable.
    setProfiles([]);
    setSelectedId(null);
    clear($('env-list'));
    clear($('detail'));
    renderListControls();
  }
}

/// Redraw everything the selection governs. env-list.js calls this when the
/// selection moves.
export function renderSelectionDependents() {
  renderDetail();
  renderListControls();
}

// --- detail ----------------------------------------------------------------

/// The settings group for the selected environment: a header naming it, any
/// first-run hint or failure text, then the editable rows.
export function renderDetail() {
  const container = $('detail');
  clear(container);

  const profiles = getProfiles();
  const profile = selectedProfile();
  const index = selectedIndex();

  if (!profile) {
    $('detail-label').textContent = 'Settings';
    const group = el('div', 'group');
    group.appendChild(el('div', 'row row-empty', 'Select an environment.'));
    container.appendChild(group);
    return;
  }

  $('detail-label').textContent = `${profile.name || profile.id} Settings`;

  // --- next-step hint ----------------------------------------------------
  // Both the seeded environments and any newly added one start with empty
  // connection names, so they cannot start yet. Say so as a next step, not as
  // an error.
  if (profile.instances.some((i) => !i.connectionName)) {
    const hint = el('div', 'notice');
    hint.appendChild(
      el(
        'div',
        'notice-title',
        'This environment has no connection names yet, so it cannot start.',
      ),
    );
    // Finding the value is the user's job now, so the notice names the command
    // that prints it rather than only saying a name is missing. One line of
    // prose plus the command, as a native empty state is -- not a tutorial.
    hint.appendChild(
      el(
        'div',
        'notice-body',
        'Type each one into the fields below, in the form ' +
          'project:region:instance. To look them up:',
      ),
    );
    // The command gets the same copyable treatment as the ADC fix rather than
    // being run into the prose: it contains no spaces to wrap at, so as body
    // text it breaks mid-identifier at narrow widths and stops being copyable.
    hint.appendChild(lookupCommandRow(profile.project));
    container.appendChild(hint);
  }

  // --- failure detail + copyable fix command -----------------------------
  // The backend already phrases these as instructions, so they are shown
  // verbatim rather than reworded here.
  if (profile.detail) {
    const failure = el('div', 'notice notice-error');
    failure.appendChild(el('div', 'notice-title', 'Last start failed'));
    failure.appendChild(el('div', 'notice-body', profile.detail));
    const fix = profile.fixCommand || profile.fix_command;
    if (fix) {
      failure.appendChild(commandRow(fix));
    }
    container.appendChild(failure);
  }

  // --- the editable group ------------------------------------------------
  const group = el('div', 'group');

  // Name is display-only and freely editable; the id underneath it never
  // changes, so renaming cannot orphan a running proxy. The list row and the
  // group header both follow along as you type.
  group.appendChild(
    fieldRow(
      'Name',
      profile.name,
      (value) => {
        profiles[index].name = value;
        // The list row and the group header follow along as you type. Only
        // the list is re-rendered, never this pane -- re-rendering the detail
        // group would destroy the field being typed in.
        $('detail-label').textContent = `${value || profile.id} Settings`;
        renderEnvList();
        renderLogFilterOptions();
        markDirty();
      },
      '',
      'text',
      '',
      NAME_FIELD_ID,
    ),
  );

  group.appendChild(
    fieldRow('Project', profile.project, (value) => {
      profiles[index].project = value;
      markDirty();
    }),
  );

  profile.instances.forEach((instance, instanceIndex) => {
    group.appendChild(
      fieldRow(
        ROLE_LABEL[instance.role] || instance.role,
        instance.connectionName,
        (value) => {
          // camelCase: the Rust Instance renames this field, and sending
          // connection_name would silently drop the value.
          profiles[index].instances[instanceIndex].connectionName = value;
          markDirty();
        },
        'project:region:instance',
      ),
    );

    group.appendChild(
      fieldRow(
        'Port',
        String(instance.port),
        (value) => {
          // Kept as a number: the Rust `port` is a u16 and a string would
          // fail to deserialize. Non-numeric input becomes NaN, which
          // serializes as null and is rejected server-side with a message.
          profiles[index].instances[instanceIndex].port = Number(value);
          markDirty();
        },
        '',
        'number',
        'narrow',
      ),
    );
  });

  container.appendChild(group);

  // --- options -----------------------------------------------------------
  // Grouped apart from the connection details: these change how the app
  // treats the profile rather than what it connects to.
  const options = el('div', 'group');

  // Danger is the user's call. Profile names are free-form, so nothing can
  // infer "this is production" -- and starting production unintentionally is
  // the app's main hazard, so the marking has to be reachable.
  options.appendChild(
    switchRow(
      'Production',
      'Confirm before starting this environment.',
      profile.danger,
      (checked) => {
        profiles[index].danger = checked;
        renderEnvList();
        markDirty();
      },
    ),
  );

  options.appendChild(
    fieldRow(
      'VPN Probe Host',
      profile.vpnProbeHost ?? '',
      (value) => {
        // Empty means "no probe": the backend treats null as no signal
        // available rather than as a failure.
        profiles[index].vpnProbeHost = value.trim() === '' ? null : value;
        markDirty();
      },
      'Optional, e.g. pg.dev.private.example',
    ),
  );

  container.appendChild(options);

  // Read-only facts belong in their own group, as System Settings separates
  // editable controls from information.
  const info = el('div', 'group');
  info.appendChild(staticRow('Identifier', profile.id));
  info.appendChild(staticRow('Region', profile.region));
  info.appendChild(
    staticRow('Status', STATUS_LABEL[profile.status] || profile.status || '—'),
  );
  if (profile.impersonateServiceAccount) {
    info.appendChild(
      staticRow('Impersonate', profile.impersonateServiceAccount),
    );
  }
  container.appendChild(info);
}

/// A command shown as monospace text with a Copy button. Used for both the
/// backend's `fixCommand` and the connection-name lookup hint, so the two read
/// as the same kind of thing: a command you are meant to run yourself.
function commandRow(command) {
  const row = el('div', 'fix-row');
  row.appendChild(el('code', 'fix-command', command));
  const copy = el('button', 'small', 'Copy');
  copy.type = 'button';
  copy.addEventListener('click', () => copyText(command, copy));
  row.appendChild(copy);
  return row;
}

/// The `gcloud` invocation that prints a project's connection names.
///
/// `--format` is included because the bare `list` output does not show the
/// connection name at all, so a user who ran the short form would not find the
/// value they were sent to look for. An unset project leaves a placeholder
/// rather than emitting `--project=`, which would silently fail.
function lookupCommandRow(project) {
  return commandRow(
    `gcloud sql instances list --project=${project || '<project>'} ` +
      `--format='value(name,connectionName)'`,
  );
}

export function markDirty() {
  note('save-note', 'Unsaved changes');
}

// --- save ------------------------------------------------------------------

/// Strip the view-only fields before sending to Rust: `Profile` has no
/// status/detail/fixCommand and would reject them.
function toProfile(view) {
  return {
    id: view.id,
    name: view.name,
    project: view.project,
    region: view.region,
    instances: view.instances.map((i) => ({
      role: i.role,
      connectionName: i.connectionName,
      port: i.port,
    })),
    flags: view.flags,
    impersonateServiceAccount: view.impersonateServiceAccount ?? null,
    danger: view.danger,
    vpnProbeHost: view.vpnProbeHost ?? null,
  };
}

export async function saveProfiles() {
  note('save-note', 'Saving…');
  try {
    await invoke('save_profiles', { profiles: getProfiles().map(toProfile) });
    clearError();
    note('save-note', 'Saved.');
    // Re-read: validation may have normalised things, and statuses move on
    // their own regardless.
    await loadProfiles();
  } catch (error) {
    note('save-note', '');
    showError('Could not save profiles', error);
  }
}

// ---------------------------------------------------------------------------
// Add and delete
//
// Both go straight to the backend rather than editing the local array and
// waiting for Done: creating a profile needs an id only the backend can
// allocate uniquely, and deleting one has to stop its proxy first. Doing
// either optimistically here would let the tray and this window disagree about
// what exists.
// ---------------------------------------------------------------------------

/// Create a profile, select it, and put the cursor in its Name field so the
/// obvious next action -- naming it -- needs no further clicks.
///
/// Unsaved edits elsewhere are preserved: `add_profile` appends to the config
/// the backend already holds, and the reload below re-reads it, so anything
/// typed but not yet saved would be lost. That is why the button warns first
/// when there are unsaved changes.
export async function addProfile() {
  if (!(await confirmDiscardingEdits('Adding an environment'))) return;

  note('save-note', 'Adding…');
  try {
    const created = await invoke('add_profile', { name: NEW_PROFILE_NAME });
    clearError();
    setSelectedId(created.id);
    await loadProfiles();
    // The tray menu rebuilds itself when the profile set changes, so this
    // appears in the menu bar within a poll interval — no restart needed.
    note('save-note', 'Added.');

    const field = $(NAME_FIELD_ID);
    if (field) {
      field.focus();
      field.select();
    }
  } catch (error) {
    note('save-note', '');
    showError('Could not add an environment', error);
  }
}

/// Delete the selected profile after confirming, saying plainly what will
/// happen to it if it is currently running.
export async function deleteProfile() {
  const profile = selectedProfile();
  if (!profile) return;

  const label = profile.name || profile.id;
  const running = profile.status === 'running' || profile.status === 'starting';

  // A native alert, not a page modal: this is destructive and not undoable,
  // and the checklist is explicit that confirmations belong to NSAlert rather
  // than to a div with a backdrop.
  //
  // The consequences go in the body rather than the title, as an NSAlert's
  // informative text does; the title asks the question.
  const consequences = [];
  if (running) {
    consequences.push(
      'Its proxy is running and will be stopped first, which will ' +
        'disconnect anything using its ports.',
    );
  }
  if (profile.danger) {
    consequences.push('This environment is marked as production.');
  }
  consequences.push('This cannot be undone.');

  const ok = await confirmDestructive(
    consequences.join('\n\n'),
    `Delete the environment “${label}”?`,
    'Delete',
  );
  if (!ok) return;

  note('save-note', 'Deleting…');
  try {
    await invoke('delete_profile', { id: profile.id });
    clearError();
    // Let loadProfiles pick the fallback selection rather than guessing here.
    setSelectedId(null);
    await loadProfiles();
    // The tray rebuilds on profile-set changes, so the row disappears from
    // the menu bar within a poll interval.
    note('save-note', 'Deleted.');
  } catch (error) {
    note('save-note', '');
    showError('Could not delete the environment', error);
  }
}

/// Add and delete both reload from disk, so warn before dropping edits that
/// have been typed but not saved. Resolves true when it is safe to continue.
///
/// Async because the native alert is: `tauri-plugin-dialog` returns a promise
/// where `window.confirm` blocked the thread.
async function confirmDiscardingEdits(action) {
  if ($('save-note').textContent !== 'Unsaved changes') return true;
  return confirmDestructive(
    'Your unsaved changes will be lost.',
    `${action} will discard your unsaved changes. Continue?`,
    'Discard',
  );
}

/// Enable or disable the delete button. Nothing selected means nothing to
/// delete; a greyed-out button says so better than an error would.
export function renderListControls() {
  $('btn-delete').disabled = selectedProfile() === null;
}
