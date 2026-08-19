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
import { setSelectionListener } from './env-list.js';
import { loadLogs, revealLogFile } from './logs-view.js';
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
// The environment list has to redraw the detail form it drives, which would be
// an import cycle if it reached for the view directly. It is injected here
// instead, so every import edge in the graph points one way.

setSelectionListener(renderSelectionDependents);

// --- controls --------------------------------------------------------------

$('btn-save').addEventListener('click', saveProfiles);
$('btn-add').addEventListener('click', addProfile);
$('btn-delete').addEventListener('click', deleteProfile);
$('btn-reload').addEventListener('click', () => {
  note('save-note', '');
  loadProfiles();
});
$('btn-logs-refresh').addEventListener('click', loadLogs);
// Both filters re-query the backend rather than filtering the rendered rows:
// the buffer is capped, so a client-side filter over an already-truncated
// snapshot would silently hide records that the backend could have returned.
$('log-filter').addEventListener('change', loadLogs);
$('log-severity').addEventListener('change', loadLogs);
$('btn-reveal-log').addEventListener('click', revealLogFile);

wireSidebar();
// Done is the window's default action, so Return presses it when nothing more
// local (a focused button) claims the keystroke.
wireWindowKeys(() => $('btn-save'));
wireContextMenu();

// --- boot ------------------------------------------------------------------

applyView();
// Both sections need the profile list: the logs filter dropdown is built from
// it, so load it regardless of which section is showing.
loadProfiles();
