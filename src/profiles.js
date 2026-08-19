// Profile editor and log viewer, in one window with a sidebar.
//
// The tray menu is the primary UI and handles start/stop; this window exists
// for the two things a native menu cannot do: editing a form and reading a log
// buffer. Those used to be two separate native windows, which meant two
// entries in Mission Control both titled "Cloud SQL Proxy" -- reading as two
// apps rather than one settings panel. They are now two sections of one window,
// selected from a source list on the left, which is how System Settings does
// it. The hash still names the section (`index.html#logs`), so the tray can
// deep-link straight to Logs and the selection survives a reload.
//
// The layout deliberately mirrors macOS System Settings rather than a web
// form: a sidebar of sections, then a grouped source list of environments, a
// grouped detail list for the selected one, and the window's actions in a
// footer. No web tab bar anywhere.
//
// No bundler and no framework: `withGlobalTauri` puts `invoke` on the window,
// the CSP is `default-src 'self'`, and everything here is plain ES module code
// served same-origin.

const invoke = window.__TAURI__.core.invoke;

/// The native window this page is drawn in, for ⌘W and ⌘M.
///
/// `withGlobalTauri` puts the whole API on the window, so this needs no
/// bundler. Guarded because the page is also driven headless in a plain
/// browser during development, where there is no window to control.
const tauriWindow = window.__TAURI__.window
  ? window.__TAURI__.window.getCurrentWindow()
  : null;

/// The native alert plugin, used for destructive confirmations.
///
/// `window.confirm()` inside a webview draws a *web* alert with the page's
/// origin in the title -- the single most web-app-looking thing this window
/// could put on screen. `tauri-plugin-dialog` draws a real NSAlert, which is
/// what the checklist asks for and what the tray already uses for the
/// production-start confirmation.
const nativeDialog = window.__TAURI__.dialog || null;

/// Ask a destructive yes/no question with a native alert. Falls back to the
/// webview's own confirm only when the plugin is absent (headless dev), so the
/// flow is never silently skipped.
async function confirmDestructive(message, title, okLabel) {
  if (nativeDialog && nativeDialog.confirm) {
    return nativeDialog.confirm(message, {
      title,
      kind: 'warning',
      okLabel,
      cancelLabel: 'Cancel',
    });
  }
  return window.confirm(`${title}\n\n${message}`);
}

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
// Sections and the sidebar
//
// One window, two sections, selected from the sidebar. A web-style tab bar was
// considered and rejected: a tab strip inside a settings window is the single
// most web-looking thing a native panel can have. A macOS settings window is
// one window with a source list, so that is what this is.
//
// The hash remains the source of truth for which section is showing. It is the
// one channel the tray has -- `WebviewUrl::App("index.html#logs")` on first
// open, and a JS eval of `location.hash` afterwards -- and it survives a
// reload, so the window reopens where it was left.
// ---------------------------------------------------------------------------

/// The sections, in sidebar order. Arrow keys walk this list.
const SECTIONS = ['profiles', 'logs'];

function currentView() {
  return window.location.hash === '#logs' ? 'logs' : 'profiles';
}

/// Swap the visible section to match the hash and move the sidebar selection
/// with it. Runs on load and on every hashchange, so clicking a sidebar row
/// (which only sets the hash) and the tray deep-linking both land here.
function applyView() {
  const view = currentView();
  const isLogs = view === 'logs';

  $('view-profiles').hidden = isLogs;
  $('view-logs').hidden = !isLogs;
  // The footer's actions all belong to the profile editor; the logs section has
  // its own Refresh in its own bar.
  $('footer').hidden = isLogs;
  document.body.classList.toggle('logs-view', isLogs);

  SECTIONS.forEach((section) => {
    const row = $(`nav-${section}`);
    const selected = section === view;
    row.classList.toggle('selected', selected);
    row.setAttribute('aria-selected', String(selected));
    // Only the selected row is in the tab order, as a native source list does:
    // Tab reaches the list, arrows move within it.
    row.tabIndex = selected ? 0 : -1;
  });

  // The window keeps one title -- it is one window now -- so the strip names
  // the app rather than the section the sidebar already names.
  document.title = 'Cloud SQL Proxy';

  // Logs are a snapshot, not a stream: load them whenever the section is
  // entered so re-showing the window always renders current output.
  if (isLogs) loadLogs();
}

/// Select a section. Writing the hash is the whole implementation: the
/// hashchange handler does the work, so every route in (sidebar click, arrow
/// key, tray deep link, reload) goes through exactly one code path.
function showSection(section) {
  const next = section === 'logs' ? '#logs' : '#profiles';
  if (window.location.hash === next) {
    // Same section: nothing to change, but a reload of the logs is still the
    // right response to clicking Logs while already on Logs.
    if (section === 'logs') loadLogs();
    return;
  }
  window.location.hash = next;
}

/// Up/down move the sidebar selection, Home/End jump to the ends. The row is
/// a real button, so Space and Return already activate it.
function onSidebarKey(event) {
  const index = SECTIONS.indexOf(currentView());
  let next = null;

  if (event.key === 'ArrowDown') next = SECTIONS[index + 1];
  else if (event.key === 'ArrowUp') next = SECTIONS[index - 1];
  else if (event.key === 'Home') next = SECTIONS[0];
  else if (event.key === 'End') next = SECTIONS[SECTIONS.length - 1];
  else return;

  event.preventDefault();
  if (!next) return;
  showSection(next);
  // The selection moved, so focus follows it -- otherwise the next arrow key
  // would be measured from a row that is no longer selected.
  $(`nav-${next}`).focus();
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

/// The Name field carries a fixed id so the new-profile flow can focus it
/// after a re-render without holding on to a node the render just replaced.
const NAME_FIELD_ID = 'field-name';

/// The name a brand-new profile gets. The backend slugifies it into a unique
/// id, so adding twice in a row is fine; the user renames it immediately
/// anyway, which is why the field is focused and selected on create.
const NEW_PROFILE_NAME = 'New Environment';

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
    renderListControls();
    renderLogFilterOptions();
  } catch (error) {
    showError('Could not load profiles', error);
    // The list could not be read, so nothing on screen is selectable and
    // nothing should look deletable.
    profiles = [];
    selectedId = null;
    clear($('env-list'));
    clear($('detail'));
    renderListControls();
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

function select(id) {
  if (id === selectedId) return;
  selectedId = id;
  renderEnvList();
  renderDetail();
  renderListControls();
}

/// Type-ahead buffer. Native lists accumulate keystrokes for about a second,
/// so typing "st" lands on "Staging" rather than jumping to "S" then "T".
let typeAhead = '';
let typeAheadTimer = null;
const TYPE_AHEAD_MS = 900;

/// Find the next profile whose name starts with `prefix`, searching forward
/// from the current selection and wrapping, as AppKit's list search does.
function matchByPrefix(prefix, fromIndex) {
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
  const target = rows[profiles.findIndex((p) => p.id === id)];
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

  // Danger is now the user's call. Profile names are free-form, so nothing
  // can infer "this is production" -- and starting production unintentionally
  // is the app's main hazard, so the marking has to be reachable.
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

/// One list row: label on the left, right-aligned text field on the right.
/// This is the shape of every editable row in System Settings.
let fieldSeq = 0;
function fieldRow(label, value, onInput, placeholder, type, sizeClass, fixedId) {
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
function switchRow(label, description, checked, onChange) {
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
    vpnProbeHost: view.vpnProbeHost ?? null,
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
// Add and delete
//
// Both go straight to the backend rather than editing the local array and
// waiting for Done: creating a profile needs an id only the backend can
// allocate uniquely, and deleting one has to stop its proxy first. Doing
// either optimistically here would let the two views disagree about what
// exists.
// ---------------------------------------------------------------------------

/// Create a profile, select it, and put the cursor in its Name field so the
/// obvious next action -- naming it -- needs no further clicks.
///
/// Unsaved edits elsewhere are preserved: `add_profile` appends to the config
/// the backend already holds, and the reload below re-reads it, so anything
/// typed but not yet saved would be lost. That is why the button warns first
/// when there are unsaved changes.
async function addProfile() {
  if (!(await confirmDiscardingEdits('Adding an environment'))) return;

  note('save-note', 'Adding…');
  try {
    const created = await invoke('add_profile', { name: NEW_PROFILE_NAME });
    clearError();
    selectedId = created.id;
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
async function deleteProfile() {
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
    selectedId = null;
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
function renderListControls() {
  $('btn-delete').disabled = selectedProfile() === null;
}

// ---------------------------------------------------------------------------
// Refresh connection names
//
// `refresh_connection_names` writes nothing -- it returns proposals. They are
// rendered as an in-page diff (rather than a confirm() dialog, which cannot
// show a list legibly) and only `apply_changes` commits them.
// ---------------------------------------------------------------------------

/// How long an operation may take before it is worth telling the user it is
/// running. Under this, a native app shows nothing and simply commits the
/// result when it lands; a placeholder makes a fast operation feel slow.
const BUSY_THRESHOLD_MS = 200;

/// Run `work`, showing `message` only if it is still running after 200ms.
/// Returns whatever `work` resolves to.
///
/// This replaces the old unconditional placeholder row, which was a loading
/// skeleton by another name: it appeared for a gcloud call that usually
/// answers in well under a frame's worth of perceptible delay.
async function withBusyNote(id, message, work) {
  let shown = false;
  const timer = setTimeout(() => {
    shown = true;
    note(id, message);
  }, BUSY_THRESHOLD_MS);
  try {
    return await work();
  } finally {
    clearTimeout(timer);
    if (shown) note(id, '');
  }
}

async function refreshConnectionNames() {
  note('save-note', '');
  try {
    const result = await withBusyNote('save-note', 'Asking gcloud…', () =>
      invoke('refresh_connection_names'),
    );
    clearError();
    pendingChanges = result.changes || [];
    renderChanges();
  } catch (error) {
    hideChanges();
    showError('Could not refresh connection names', error);
  }
}

function renderChanges() {
  const panel = $('changes');
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
/// Without this the panel renders ~90px below the fold of a 560px window with
/// the content region still scrolled to the top, so "Refresh Connection Names"
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
  const apply = panel.querySelector('button.default') || panel.querySelector('button');
  if (apply) apply.focus();
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
  // The dropdown may be empty on a logs-window cold start, in which case the
  // profile list has not loaded yet; an empty value means "all" regardless.
  const selected = $('log-filter').value;
  try {
    // Reading a ring buffer in memory is far under the 200ms threshold, so the
    // note only ever appears if something is genuinely slow.
    const lines = await withBusyNote('log-note', 'Loading…', () =>
      invoke('read_logs', { id: selected || null }),
    );
    clearError();
    const output = $('logs');
    output.textContent = lines.length
      ? lines.join('\n')
      : 'No log output yet.';
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
$('btn-add').addEventListener('click', addProfile);
$('btn-delete').addEventListener('click', deleteProfile);
$('btn-reload').addEventListener('click', () => {
  note('save-note', '');
  hideChanges();
  loadProfiles();
});
$('btn-refresh').addEventListener('click', refreshConnectionNames);
$('btn-logs-refresh').addEventListener('click', loadLogs);
$('log-filter').addEventListener('change', loadLogs);
window.addEventListener('hashchange', applyView);

SECTIONS.forEach((section) => {
  const row = $(`nav-${section}`);
  row.addEventListener('click', () => showSection(section));
  row.addEventListener('keydown', onSidebarKey);
});

// --- native window keyboard ------------------------------------------------
//
// The app has no menu bar of its own (ActivationPolicy::Accessory, so no
// application menu), which means nothing supplies the ⌘W and ⌘M that every
// macOS window is expected to answer. Without these the window can only be
// dismissed by aiming at the traffic light -- the clearest "this is a web page
// in a frame" tell the window has, because the shortcut is muscle memory.

document.addEventListener('keydown', (event) => {
  // ⌘W puts the window away, ⌘M minimises. Both are window commands, not page
  // commands, so they go to the shell rather than being handled here.
  //
  // ⌘W hides rather than closes: the app lives in the menu bar, so a window is
  // something you put away, not something whose closing means anything. Hiding
  // also keeps the webview alive, so reopening from the tray is instant and
  // does not lose scroll position or a half-typed field. (The Rust side turns
  // the red traffic light into the same thing.)
  if (event.metaKey && !event.altKey && !event.ctrlKey) {
    const key = event.key.toLowerCase();
    if (key === 'w') {
      event.preventDefault();
      if (tauriWindow) tauriWindow.hide();
      return;
    }
    if (key === 'm') {
      event.preventDefault();
      if (tauriWindow) tauriWindow.minimize();
      return;
    }
  }

  if (event.key === 'Escape') {
    event.preventDefault();
    // Escape must always do something. It peels one layer at a time -- the
    // most local dismissable thing first -- and puts the window away only when
    // there is nothing left to dismiss, which is what a native settings sheet
    // does.
    if (!$('changes').hidden) {
      hideChanges();
      $('btn-refresh').focus();
      return;
    }
    if (!$('banner').hidden) {
      clearError();
      return;
    }
    // A field being edited gives up focus rather than closing the window, so
    // Escape never discards a window's worth of typing in one keystroke.
    const active = document.activeElement;
    if (active && active.tagName === 'INPUT') {
      active.blur();
      return;
    }
    if (tauriWindow) tauriWindow.hide();
  }
});

// Return activates the default button, as it does in a sheet -- but not while
// a button already has focus (that would double-fire) and not in the logs
// window, which has no default action.
document.addEventListener('keydown', (event) => {
  if (event.key !== 'Enter' || event.metaKey) return;
  if (currentView() === 'logs') return;
  const active = document.activeElement;
  if (active && (active.tagName === 'BUTTON' || active.tagName === 'SELECT')) {
    return;
  }
  // The diff panel owns Return while it is open: applying is the action in
  // front of the user, and saving underneath it would commit the wrong thing.
  const panel = $('changes');
  const target = panel.hidden ? $('btn-save') : panel.querySelector('button.default');
  if (!target) return;
  event.preventDefault();
  target.click();
});

// WebKit's own context menu offers Reload and Inspect Element, neither of
// which exists in a native app. There is no NSMenu to put here from inside the
// webview, so the honest choice is no menu at all -- except over real text,
// where the system's Copy/Look Up menu is the correct native behaviour.
document.addEventListener('contextmenu', (event) => {
  const target = event.target;
  const editable =
    target &&
    (target.tagName === 'INPUT' ||
      target.tagName === 'TEXTAREA' ||
      target.closest('.logs, .fix-command, .banner, .notice-body'));
  if (!editable) event.preventDefault();
});

applyView();
// Both sections need the profile list: the logs filter dropdown is built from
// it, so load it regardless of which section is showing.
loadProfiles();
