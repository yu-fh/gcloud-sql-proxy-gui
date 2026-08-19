// The Logs section: the audit trail, filtered, plus where its file lives.
//
// A snapshot, not a stream. `read_logs` reads the ring buffer the Rust side
// keeps in memory, and the section reloads it on entry and on demand rather
// than subscribing -- there is no event to subscribe to, and a log pane that
// silently stops updating would be worse than one you refresh.
//
// What it renders is no longer just proxy output. Each record carries a
// timestamp, a severity, a category, and an optional profile, so a row is a
// small grid rather than a line of text: the severity has to be styleable on
// its own for an error to be findable without reading every line.

import { invoke } from './ipc.js';
import { $, clear, clearError, el, note, showError, withBusyNote } from './dom.js';
import { getProfiles } from './store.js';

/// How many records to render. The backend caps its buffer at 2000, and a DOM
/// with 2000 four-cell rows in it is both slow to build and slower to scroll,
/// so the view shows the newest slice and says so. Anyone who wants the whole
/// thing has the file, which is why the path is on screen.
const RENDER_LIMIT = 500;

/// Which category glyph-free label each record gets. Short, because it is a
/// fixed-width column next to the message and a long word would push every
/// message right.
const CATEGORY_LABEL = {
  system: 'sys',
  action: 'act',
  event: 'evt',
  proxy: 'proxy',
};

/// Populate the profile filter from whatever profiles are configured,
/// preserving the current selection.
export function renderLogFilterOptions() {
  const select = $('log-filter');
  const previous = select.value;
  clear(select);

  const all = document.createElement('option');
  all.value = '';
  all.textContent = 'All Profiles';
  select.appendChild(all);

  getProfiles().forEach((profile) => {
    const option = document.createElement('option');
    option.value = profile.id;
    option.textContent = profile.name || profile.id;
    select.appendChild(option);
  });

  select.value = previous;
}

/// The wall-clock time for one record.
///
/// Rust sends UTC (`atDisplay`) plus the epoch milliseconds (`at`). The
/// timestamp shown is formatted from `at` here, because the webview is the only
/// part of the app that knows the user's timezone and locale -- the Rust side
/// deliberately does not, since `time`'s local-offset support does not work in a
/// multi-threaded process on Unix. Seconds are included and the date is not: the
/// trail is read within a session far more often than across days, and the file
/// carries the full UTC stamp for when it is not.
function timeOf(record) {
  if (typeof record.at !== 'number' || !Number.isFinite(record.at)) {
    // Fall back to whatever the backend formatted rather than rendering
    // "Invalid Date".
    return record.atDisplay || '';
  }
  const date = new Date(record.at);
  return date.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });
}

/// One record as a row. Four cells: time, severity, source, message.
function logRow(record) {
  const severity = record.severity || 'info';
  const row = el('div', `log-row log-${severity}`);

  const time = el('span', 'log-time', timeOf(record));
  // The full UTC stamp -- what is in the file -- as the tooltip, so a row can
  // be matched to a file line without guessing at the timezone.
  if (record.atDisplay) time.title = record.atDisplay;
  row.appendChild(time);

  // The severity is text, not only a colour: colour alone fails for anyone who
  // cannot distinguish these two reds, and this pane has no other channel.
  row.appendChild(el('span', 'log-severity', severity));

  // Source is the category, or the profile id when there is one -- which is the
  // more useful of the two, because the category is largely implied by the
  // message and the profile is not.
  const source = record.profileId || CATEGORY_LABEL[record.category] || record.category || '';
  const sourceCell = el('span', 'log-source', source);
  if (record.profileId) {
    sourceCell.title = `${record.profileId} (${record.category})`;
  }
  row.appendChild(sourceCell);

  row.appendChild(el('span', 'log-message', record.message || ''));
  return row;
}

export async function loadLogs() {
  // The dropdowns may be empty on a cold start into the Logs section, in which
  // case the profile list has not loaded yet; an empty value means "no filter"
  // for both regardless.
  const profileId = $('log-filter').value;
  const severity = $('log-severity').value;

  try {
    // Reading a ring buffer in memory is far under the 200ms threshold, so the
    // note only ever appears if something is genuinely slow.
    const view = await withBusyNote('log-note', 'Loading…', () =>
      invoke('read_logs', {
        id: profileId || null,
        severity: severity || null,
      }),
    );
    clearError();
    renderLogs(view);
  } catch (error) {
    note('log-note', '');
    showError('Could not read logs', error);
  }
}

/// Draw one `LogsView`: the records, the count, and the file path.
function renderLogs(view) {
  const output = $('logs');
  clear(output);

  const records = Array.isArray(view && view.records) ? view.records : [];

  if (records.length === 0) {
    output.appendChild(el('div', 'log-empty', 'No log records yet.'));
  } else {
    // Newest last, so show the newest slice when there are more than the DOM
    // should hold, and say that is what happened.
    const shown = records.slice(-RENDER_LIMIT);
    if (shown.length < records.length) {
      output.appendChild(
        el(
          'div',
          'log-empty',
          `Showing the most recent ${shown.length} of ${records.length} ` +
            `records. The full trail is in the log file.`,
        ),
      );
    }
    // One fragment, one reflow: appending 500 rows individually to a live node
    // is the difference between an instant redraw and a visible stutter.
    const fragment = document.createDocumentFragment();
    shown.forEach((record) => fragment.appendChild(logRow(record)));
    output.appendChild(fragment);
    // Newest records are last, so pin to the bottom.
    output.scrollTop = output.scrollHeight;
  }

  const count = records.length;
  let summary = `${count} record${count === 1 ? '' : 's'}`;
  // A file that is not being written is worth saying out loud: the view is
  // complete but the trail on disk is not, and the user is the only one who can
  // fix a read-only log directory.
  if (view && view.writeFailures > 0) {
    summary += ` — ${view.writeFailures} write failure${
      view.writeFailures === 1 ? '' : 's'
    }; the file is not being updated`;
  }
  note('log-note', summary);

  const path = $('log-path');
  path.textContent = (view && view.filePath) || 'Not being written to disk';
  $('btn-reveal-log').disabled = !(view && view.filePath);
}

/// Open Finder on the log file. The path is on screen either way, so a failure
/// here is reported and nothing else is lost.
export async function revealLogFile() {
  try {
    await invoke('reveal_log_file');
    clearError();
  } catch (error) {
    showError('Could not show the log file', error);
  }
}
