// Profile editor and log viewer for the two secondary windows.
//
// The tray menu is the primary UI and handles start/stop; this page exists for
// the two things a native menu cannot do: editing a form and reading a log
// buffer. One page serves both windows -- the tray opens `logs` at
// `index.html#logs` -- so the hash selects the view.
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

/// Transient status text next to a button ("Saved.", "Refreshing…").
function note(id, text) {
  $(id).textContent = text || '';
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

function currentView() {
  return window.location.hash === '#logs' ? 'logs' : 'profiles';
}

/// Swap views to match the hash. Both windows load the same document, so this
/// runs on load and on every hashchange.
function applyView() {
  const view = currentView();
  $('view-profiles').hidden = view !== 'profiles';
  $('view-logs').hidden = view === 'profiles';
  $('tab-profiles').classList.toggle('active', view === 'profiles');
  $('tab-logs').classList.toggle('active', view !== 'profiles');
  document.title = view === 'logs' ? 'Logs' : 'Profiles';

  // Logs are a snapshot, not a stream: load them whenever the view is entered
  // so switching tabs always shows current output.
  if (view === 'logs') loadLogs();
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
  replica: 'Read replica',
};

async function loadProfiles() {
  try {
    // ProfileView is the Profile flattened together with status/detail/
    // fixCommand -- the view-only fields are stripped again in saveProfiles.
    profiles = await invoke('list_profiles');
    clearError();
    renderProfiles();
    renderLogFilterOptions();
  } catch (error) {
    showError('Could not load profiles', error);
    $('profiles').textContent = '';
  }
}

function renderProfiles() {
  const container = $('profiles');
  clear(container);

  if (profiles.length === 0) {
    container.appendChild(el('p', 'empty', 'No profiles configured.'));
    return;
  }

  profiles.forEach((profile, index) => {
    container.appendChild(renderProfile(profile, index));
  });
}

function renderProfile(profile, index) {
  const card = el('section', 'profile');
  if (profile.danger) card.classList.add('danger');

  // --- header: name, danger marker, status badge -------------------------
  const header = el('header', 'profile-header');
  header.appendChild(el('h2', 'profile-name', profile.name || profile.id));
  if (profile.danger) {
    const badge = el('span', 'badge badge-danger', 'PRODUCTION');
    badge.title = 'Starting this profile connects you to production.';
    header.appendChild(badge);
  }
  const status = profile.status || 'stopped';
  header.appendChild(
    el('span', `badge badge-${status}`, STATUS_LABEL[status] || status),
  );
  card.appendChild(header);

  // --- failure detail + copyable fix command -----------------------------
  // The backend already phrases these as instructions, so they are shown
  // verbatim rather than reworded here.
  if (profile.detail) {
    const failure = el('div', 'failure');
    failure.appendChild(el('p', 'failure-message', profile.detail));
    const fix = profile.fixCommand || profile.fix_command;
    if (fix) {
      const row = el('div', 'fix-row');
      row.appendChild(el('code', 'fix-command', fix));
      const copy = el('button', '', 'Copy');
      copy.type = 'button';
      copy.addEventListener('click', () => copyText(fix, copy));
      row.appendChild(copy);
      failure.appendChild(row);
    }
    card.appendChild(failure);
  }

  // --- first-run hint ----------------------------------------------------
  // Seeded profiles ship with empty connection names, so a fresh install
  // cannot start anything. Say so as a next step, not as an error.
  if (profile.instances.some((i) => !i.connectionName)) {
    const hint = el('div', 'hint');
    hint.appendChild(
      el(
        'strong',
        '',
        'This profile has no connection names yet, so it cannot start.',
      ),
    );
    hint.appendChild(
      el(
        'p',
        '',
        'Click “Refresh connection names” above to look them up from gcloud, ' +
          'or paste them in below.',
      ),
    );
    card.appendChild(hint);
  }

  // --- editable project --------------------------------------------------
  const fields = el('div', 'fields');
  fields.appendChild(
    field('Project', profile.project, (value) => {
      profiles[index].project = value;
      markDirty();
    }),
  );
  card.appendChild(fields);

  // --- editable instances ------------------------------------------------
  profile.instances.forEach((instance, instanceIndex) => {
    const row = el('div', 'instance');
    row.appendChild(
      el('div', 'role', ROLE_LABEL[instance.role] || instance.role),
    );

    row.appendChild(
      field(
        'Connection name',
        instance.connectionName,
        (value) => {
          // camelCase: the Rust Instance renames this field, and sending
          // connection_name would silently drop the value.
          profiles[index].instances[instanceIndex].connectionName = value;
          markDirty();
        },
        'project:region:instance',
        'wide',
      ),
    );

    row.appendChild(
      field(
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
        'narrow',
        'number',
      ),
    );

    card.appendChild(row);
  });

  return card;
}

/// A labelled text input wired to `onInput`. Labels are associated by
/// generated id so clicking the label focuses the field.
let fieldSeq = 0;
function field(label, value, onInput, placeholder, sizeClass, type) {
  const wrap = el('label', `field ${sizeClass || ''}`.trim());
  const id = `f${fieldSeq++}`;
  const caption = el('span', 'field-label', label);
  caption.setAttribute('for', id);
  const input = document.createElement('input');
  input.id = id;
  input.type = type || 'text';
  input.value = value === null || value === undefined ? '' : value;
  if (placeholder) input.placeholder = placeholder;
  input.addEventListener('input', () => onInput(input.value));
  wrap.appendChild(caption);
  wrap.appendChild(input);
  return wrap;
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
  panel.appendChild(el('p', '', 'Asking gcloud for instances…'));

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
    panel.appendChild(
      el('p', '', 'Connection names are already up to date. Nothing to change.'),
    );
    const dismiss = el('button', '', 'Dismiss');
    dismiss.type = 'button';
    dismiss.addEventListener('click', hideChanges);
    panel.appendChild(dismiss);
    return;
  }

  panel.appendChild(
    el(
      'h2',
      '',
      `${pendingChanges.length} proposed change${pendingChanges.length === 1 ? '' : 's'}`,
    ),
  );
  panel.appendChild(
    el('p', 'muted', 'Nothing has been written yet. Review, then apply.'),
  );

  const list = el('ul', 'change-list');
  pendingChanges.forEach((change) => {
    const item = el('li', 'change');
    item.appendChild(
      el(
        'div',
        'change-where',
        `${change.profileId ?? change.profile_id} · ${ROLE_LABEL[change.role] || change.role}`,
      ),
    );
    // An empty `from` is the first-run case, where the seeded profile simply
    // had no name yet -- "(not set)" reads better than a blank line.
    item.appendChild(el('div', 'change-from', change.from || '(not set)'));
    item.appendChild(el('div', 'change-arrow', '→'));
    item.appendChild(el('div', 'change-to', change.to));
    list.appendChild(item);
  });
  panel.appendChild(list);

  const actions = el('div', 'toolbar');
  const apply = el('button', 'primary', 'Apply these changes');
  apply.type = 'button';
  apply.addEventListener('click', applyChanges);
  const cancel = el('button', '', 'Cancel');
  cancel.type = 'button';
  cancel.addEventListener('click', hideChanges);
  actions.appendChild(apply);
  actions.appendChild(cancel);
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
  all.textContent = 'All profiles';
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
