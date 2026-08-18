// Profile editor and log viewer for the two secondary windows.
//
// The tray menu is the primary UI and handles start/stop; this page exists for
// the two things a native menu cannot do: editing a form and reading a log
// buffer. One page serves both windows -- the tray opens `logs` at
// `index.html#logs` -- so the hash selects the view.
//
// The layout deliberately mirrors macOS System Settings rather than a web
// form: a grouped source list of environments at the top, a grouped detail
// list for the selected one below, and the window's actions in a footer. There
// is no in-page tab bar; each window shows exactly one view, chosen by hash.
//
// No bundler and no framework: `withGlobalTauri` puts `invoke` on the window,
// the CSP is `default-src 'self'`, and everything here is plain ES module code
// served same-origin.

const invoke = window.__TAURI__.core.invoke;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The profiles as edited in this window. Input handlers mutate this array in
/// place rather than re-reading the DOM at save time, so a full re-render is
/// never needed to keep edits -- and never destroys the field being typed in.
let profiles = [];

/// Which profile the detail pane is showing, by id. Kept as an id rather than
/// an index so it survives a reload that reorders or drops profiles.
let selectedId = null;

/// Proposed connection-name changes from the last `refresh_connection_names`.
/// Held here, unwritten, until the user clicks Apply.
let pendingChanges = [];

// ---------------------------------------------------------------------------
// Small DOM helpers
//
// Everything is built with createElement rather than innerHTML: connection
// names and backend error strings are interpolated all over this page, and
// textContent means none of it can ever be parsed as markup.
// ---------------------------------------------------------------------------

const $ = (id) => document.getElementById(id);

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

/// Show a backend or unexpected error as visible text. Every rejected invoke
/// routes here: a failure the user cannot see is worse than no feature.
function showError(context, error) {
  const banner = $('banner');
  const message = error && error.message ? error.message : String(error);
  banner.textContent = `${context}: ${message}`;
  banner.hidden = false;
  console.error(context, error);
}

function clearError() {
  const banner = $('banner');
  banner.hidden = true;
  banner.textContent = '';
}

/// Transient status text in the footer ("Saved.", "Refreshing…").
function note(id, text) {
  $(id).textContent = text || '';
}

// ---------------------------------------------------------------------------
// Views
//
// Two windows, two views, selected purely by `location.hash`. A web-style tab
// bar was removed deliberately: each window already has exactly one job, and a
// tab strip inside a settings window is the single most web-looking thing a
// native panel can have.
// ---------------------------------------------------------------------------

function currentView() {
  return window.location.hash === '#logs' ? 'logs' : 'profiles';
}

/// Swap views to match the hash. Both windows load the same document, so this
/// runs on load and on every hashchange.
function applyView() {
  const view = currentView();
  const isLogs = view === 'logs';

  $('view-profiles').hidden = isLogs;
  $('view-logs').hidden = !isLogs;
  // The footer's actions all belong to the profile editor; the logs view has
  // its own Refresh in its own bar.
  $('footer').hidden = isLogs;
  document.body.classList.toggle('logs-view', isLogs);

  const title = isLogs ? 'Logs' : 'Profiles';
  document.title = title;
  $('window-title').textContent = title;

  // Logs are a snapshot, not a stream: load them whenever the view is entered
  // so re-showing the window always renders current output.
  if (isLogs) loadLogs();
}

// ---------------------------------------------------------------------------
// Profiles view
// ---------------------------------------------------------------------------

const STATUS_LABEL = {
  stopped: 'Stopped',
  starting: 'Starting…',
  running: 'Running',
  failed: 'Failed',
};

const ROLE_LABEL = {
  primary: 'Primary',
  replica: 'Read Replica',
};

async function loadProfiles() {
  try {
    // ProfileView is the Profile flattened together with status/detail/
    // fixCommand -- the view-only fields are stripped again in saveProfiles.
    profiles = await invoke('list_profiles');
    clearError();

    // Keep the current selection if it still exists, otherwise fall back to
    // the first profile so the detail pane is never pointlessly empty.
    if (!profiles.some((p) => p.id === selectedId)) {
      selectedId = profiles.length > 0 ? profiles[0].id : null;
    }

    renderEnvList();
    renderDetail();
    renderLogFilterOptions();
  } catch (error) {
    showError('Could not load profiles', error);
    clear($('env-list'));
    clear($('detail'));
  }
}

function selectedProfile() {
  return profiles.find((p) => p.id === selectedId) || null;
}

function selectedIndex() {
  return profiles.findIndex((p) => p.id === selectedId);
}

// --- source list -----------------------------------------------------------

/// The grouped environment list: one row per profile, with a status dot on the
/// left and the status/production markers on the right. Selecting a row swaps
/// the detail group below it.
function renderEnvList() {
  const list = $('env-list');
  clear(list);

  if (profiles.length === 0) {
    list.appendChild(el('div', 'row row-empty', 'No profiles configured.'));
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

function select(id) {
  if (id === selectedId) return;
  selectedId = id;
  renderEnvList();
  renderDetail();
}

/// Arrow keys move the selection, Space/Enter re-assert it. Mirrors how a
/// native list behaves when it has focus.
function onListKey(event, id) {
  const index = profiles.findIndex((p) => p.id === id);
  let next = null;

  if (event.key === 'ArrowDown') next = profiles[index + 1];
  else if (event.key === 'ArrowUp') next = profiles[index - 1];
  else if (event.key === 'Enter' || event.key === ' ') next = profiles[index];
  else return;

  event.preventDefault();
  if (!next) return;
  select(next.id);
  // renderEnvList replaced the nodes, so focus has to be re-established on the
  // newly selected row rather than the one that was clicked.
  const rows = $('env-list').querySelectorAll('.row-select');
  const target = rows[profiles.findIndex((p) => p.id === next.id)];
  if (target) target.focus();
}

// --- detail ----------------------------------------------------------------

/// The settings group for the selected environment: a header naming it, any
/// first-run hint or failure text, then the editable rows.
function renderDetail() {
  const container = $('detail');
  clear(container);

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

  // --- first-run hint ----------------------------------------------------
  // Seeded profiles ship with empty connection names, so a fresh install
  // cannot start anything. Say so as a next step, not as an error.
  if (profile.instances.some((i) => !i.connectionName)) {
    const hint = el('div', 'notice');
    hint.appendChild(
      el(
        'div',
        'notice-title',
        'This environment has no connection names yet, so it cannot start.',
      ),
    );
    hint.appendChild(
      el(
        'div',
        'notice-body',
        'Choose “Refresh Connection Names” below to look them up from gcloud, ' +
          'or type them in here.',
      ),
    );
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
      const row = el('div', 'fix-row');
      row.appendChild(el('code', 'fix-command', fix));
      const copy = el('button', 'small', 'Copy');
      copy.type = 'button';
      copy.addEventListener('click', () => copyText(fix, copy));
      row.appendChild(copy);
      failure.appendChild(row);
    }
    container.appendChild(failure);
  }

  // --- the editable group ------------------------------------------------
  const group = el('div', 'group');

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

  // Read-only facts belong in their own group, as System Settings separates
  // editable controls from information.
  const info = el('div', 'group');
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

/// One list row: label on the left, right-aligned text field on the right.
/// This is the shape of every editable row in System Settings.
let fieldSeq = 0;
function fieldRow(label, value, onInput, placeholder, type, sizeClass) {
  const row = el('div', 'row');
  const id = `f${fieldSeq++}`;

  const caption = el('label', 'row-label', label);
  caption.setAttribute('for', id);
  row.appendChild(caption);

  const input = document.createElement('input');
  input.id = id;
  input.className = `row-input ${sizeClass || ''}`.trim();
  input.type = type || 'text';
  input.value = value === null || value === undefined ? '' : value;
  if (placeholder) input.placeholder = placeholder;
  input.addEventListener('input', () => onInput(input.value));
  row.appendChild(input);

  return row;
}

/// A read-only row: label left, value right, in the secondary colour.
function staticRow(label, value) {
  const row = el('div', 'row');
  row.appendChild(el('span', 'row-label', label));
  row.appendChild(el('span', 'row-value', value));
  return row;
}

function markDirty() {
  note('save-note', 'Unsaved changes');
}

async function copyText(text, button) {
  const original = button.textContent;
  try {
    await navigator.clipboard.writeText(text);
    button.textContent = 'Copied';
  } catch (error) {
    // Clipboard access can be denied; the command is on screen either way,
    // so say what happened rather than failing silently.
    button.textContent = 'Copy failed';
    showError('Could not copy to the clipboard', error);
  }
  setTimeout(() => {
    button.textContent = original;
  }, 1500);
}

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
  };
}

async function saveProfiles() {
  note('save-note', 'Saving…');
  try {
    await invoke('save_profiles', { profiles: profiles.map(toProfile) });
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
// Refresh connection names
//
// `refresh_connection_names` writes nothing -- it returns proposals. They are
// rendered as an in-page diff (rather than a confirm() dialog, which cannot
// show a list legibly) and only `apply_changes` commits them.
// ---------------------------------------------------------------------------

async function refreshConnectionNames() {
  note('save-note', '');
  const panel = $('changes');
  clear(panel);
  panel.hidden = false;
  panel.appendChild(el('h2', 'group-label', 'Refresh'));
  const group = el('div', 'group');
  group.appendChild(el('div', 'row row-empty', 'Asking gcloud for instances…'));
  panel.appendChild(group);

  try {
    const result = await invoke('refresh_connection_names');
    clearError();
    pendingChanges = result.changes || [];
    renderChanges();
  } catch (error) {
    panel.hidden = true;
    clear(panel);
    pendingChanges = [];
    showError('Could not refresh connection names', error);
  }
}

function renderChanges() {
  const panel = $('changes');
  clear(panel);
  panel.hidden = false;

  if (pendingChanges.length === 0) {
    panel.appendChild(el('h2', 'group-label', 'Refresh'));
    const group = el('div', 'group');
    group.appendChild(
      el(
        'div',
        'row row-empty',
        'Connection names are already up to date. Nothing to change.',
      ),
    );
    panel.appendChild(group);

    const actions = el('div', 'change-actions');
    const dismiss = el('button', '', 'Dismiss');
    dismiss.type = 'button';
    dismiss.addEventListener('click', hideChanges);
    actions.appendChild(dismiss);
    panel.appendChild(actions);
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
}

function hideChanges() {
  pendingChanges = [];
  const panel = $('changes');
  panel.hidden = true;
  clear(panel);
}

async function applyChanges() {
  // Round-trip exactly the shape ChangeView deserializes: profileId (camelCase
  // per its serde rename), role, from, to.
  const changes = pendingChanges.map((c) => ({
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
    await loadProfiles();
  } catch (error) {
    showError('Could not apply changes', error);
  }
}

// ---------------------------------------------------------------------------
// Logs view
// ---------------------------------------------------------------------------

/// Populate the filter dropdown from whatever profiles are configured,
/// preserving the current selection.
function renderLogFilterOptions() {
  const select = $('log-filter');
  const previous = select.value;
  clear(select);

  const all = document.createElement('option');
  all.value = '';
  all.textContent = 'All Profiles';
  select.appendChild(all);

  profiles.forEach((profile) => {
    const option = document.createElement('option');
    option.value = profile.id;
    option.textContent = profile.name || profile.id;
    select.appendChild(option);
  });

  select.value = previous;
}

async function loadLogs() {
  note('log-note', 'Loading…');
  // The dropdown may be empty on a logs-window cold start, in which case the
  // profile list has not loaded yet; an empty value means "all" regardless.
  const selected = $('log-filter').value;
  try {
    const lines = await invoke('read_logs', { id: selected || null });
    clearError();
    const output = $('logs');
    output.textContent = lines.length
      ? lines.join('\n')
      : 'No log output yet. Start a profile from the menu bar icon.';
    // Newest lines are last, so pin to the bottom.
    output.scrollTop = output.scrollHeight;
    note('log-note', `${lines.length} line${lines.length === 1 ? '' : 's'}`);
  } catch (error) {
    note('log-note', '');
    showError('Could not read logs', error);
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

$('btn-save').addEventListener('click', saveProfiles);
$('btn-reload').addEventListener('click', () => {
  note('save-note', '');
  hideChanges();
  loadProfiles();
});
$('btn-refresh').addEventListener('click', refreshConnectionNames);
$('btn-logs-refresh').addEventListener('click', loadLogs);
$('log-filter').addEventListener('change', loadLogs);
window.addEventListener('hashchange', applyView);

applyView();
// Both views need the profile list: the logs window uses it for the filter
// dropdown, so load it regardless of which view is showing.
loadProfiles();
