// Entry point: wire the controls, connect the modules that need each other,
// and boot.
//
// This is the only module with side effects at import time, so every other
// module can be read as a unit rather than as something that also happens to
// attach listeners when loaded.
//
// The window is one native window with a sidebar and two sections -- Profiles
// and Logs -- which used to be two separate windows, each showing up in Mission
// Control under the same title. See shell.js for how the hash selects a section.
//
// No bundler and no framework: `withGlobalTauri` puts `invoke` on the window,
// the CSP is `default-src 'self'`, and these are plain ES modules with relative
// imports, served same-origin.

import { $, note } from './dom.js';
import {
  hideChanges,
  refreshConnectionNames,
  setAppliedListener,
} from './changes.js';
import { setSelectionListener } from './env-list.js';
import { loadLogs } from './logs-view.js';
import {
  addProfile,
  deleteProfile,
  loadProfiles,
  renderSelectionDependents,
  saveProfiles,
} from './profiles-view.js';
import {
  applyView,
  wireContextMenu,
  wireSidebar,
  wireWindowKeys,
} from './shell.js';

// --- cross-module wiring ---------------------------------------------------
//
// Two dependencies would be cycles if imported directly: the environment list
// has to redraw the detail form it drives, and the refresh diff has to re-read
// the profile list after Apply. Both are injected here instead, so every import
// edge in the graph points one way.

setSelectionListener(renderSelectionDependents);
setAppliedListener(loadProfiles);

// --- controls --------------------------------------------------------------

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

wireSidebar();
// Done is the window's default action, so Return presses it when nothing more
// local (the diff panel, a focused button) claims the keystroke.
wireWindowKeys(() => $('btn-save'));
wireContextMenu();

// --- boot ------------------------------------------------------------------

applyView();
// Both sections need the profile list: the logs filter dropdown is built from
// it, so load it regardless of which section is showing.
loadProfiles();
