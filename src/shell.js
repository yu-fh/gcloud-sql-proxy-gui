// The window shell: which section is showing, the sidebar that chooses it, and
// the keyboard and context-menu behaviour a native window is expected to have.
//
// One window, two sections, selected from the sidebar. A web-style tab bar was
// considered and rejected: a tab strip inside a settings window is the single
// most web-looking thing a native panel can have. A macOS settings window is
// one window with a source list, so that is what this is.
//
// The hash remains the source of truth for which section is showing. It is the
// one channel the tray has -- `WebviewUrl::App("index.html#logs")` on first
// open, and a JS write to `location.hash` afterwards -- and it survives a
// reload, so the window reopens where it was left.

import { tauriWindow } from './ipc.js';
import { $, clearError } from './dom.js';
import { loadLogs } from './logs-view.js';
import { changesAreOpen, changesDefaultButton, hideChanges } from './changes.js';

/// The sections, in sidebar order. Arrow keys walk this list.
const SECTIONS = ['profiles', 'logs'];

export function currentView() {
  return window.location.hash === '#logs' ? 'logs' : 'profiles';
}

/// Swap the visible section to match the hash and move the sidebar selection
/// with it. Runs on load and on every hashchange, so clicking a sidebar row
/// (which only sets the hash) and the tray deep-linking both land here.
export function applyView() {
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

/// Wire the sidebar rows. Called once at boot.
export function wireSidebar() {
  SECTIONS.forEach((section) => {
    const row = $(`nav-${section}`);
    row.addEventListener('click', () => showSection(section));
    row.addEventListener('keydown', onSidebarKey);
  });
  window.addEventListener('hashchange', applyView);
}

// --- native window keyboard ------------------------------------------------
//
// The app has no menu bar of its own (ActivationPolicy::Accessory, so no
// application menu), which means nothing supplies the ⌘W and ⌘M that every
// macOS window is expected to answer. Without these the window can only be
// dismissed by aiming at the traffic light -- the clearest "this is a web page
// in a frame" tell the window has, because the shortcut is muscle memory.

/// Install the window-level key handlers. `defaultAction` is the button Return
/// should press when nothing more local claims it, passed in so this module does
/// not need to know that Done saves.
export function wireWindowKeys(defaultAction) {
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
      if (changesAreOpen()) {
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
  // section, which has no default action.
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Enter' || event.metaKey) return;
    if (currentView() === 'logs') return;
    const active = document.activeElement;
    if (active && (active.tagName === 'BUTTON' || active.tagName === 'SELECT')) {
      return;
    }
    // The diff panel owns Return while it is open: applying is the action in
    // front of the user, and saving underneath it would commit the wrong thing.
    const target = changesAreOpen() ? changesDefaultButton() : defaultAction();
    if (!target) return;
    event.preventDefault();
    target.click();
  });
}

// WebKit's own context menu offers Reload and Inspect Element, neither of
// which exists in a native app. There is no NSMenu to put here from inside the
// webview, so the honest choice is no menu at all -- except over real text,
// where the system's Copy/Look Up menu is the correct native behaviour.
export function wireContextMenu() {
  document.addEventListener('contextmenu', (event) => {
    const target = event.target;
    const editable =
      target &&
      (target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.closest('.logs, .fix-command, .banner, .notice-body'));
    if (!editable) event.preventDefault();
  });
}
