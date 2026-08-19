// The Logs section: a profile filter and a snapshot of the proxy's output.
//
// A snapshot, not a stream. `read_logs` reads a ring buffer the Rust side keeps
// in memory, and the section reloads it on entry and on demand rather than
// subscribing -- there is no event to subscribe to, and a log pane that
// silently stops updating would be worse than one you refresh.

import { invoke } from './ipc.js';
import { $, clear, clearError, note, showError, withBusyNote } from './dom.js';
import { getProfiles } from './store.js';

/// Populate the filter dropdown from whatever profiles are configured,
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

export async function loadLogs() {
  // The dropdown may be empty on a cold start into the Logs section, in which
  // case the profile list has not loaded yet; an empty value means "all"
  // regardless.
  const selected = $('log-filter').value;
  try {
    // Reading a ring buffer in memory is far under the 200ms threshold, so the
    // note only ever appears if something is genuinely slow.
    const lines = await withBusyNote('log-note', 'Loading…', () =>
      invoke('read_logs', { id: selected || null }),
    );
    clearError();
    const output = $('logs');
    output.textContent = lines.length ? lines.join('\n') : 'No log output yet.';
    // Newest lines are last, so pin to the bottom.
    output.scrollTop = output.scrollHeight;
    note('log-note', `${lines.length} line${lines.length === 1 ? '' : 's'}`);
  } catch (error) {
    note('log-note', '');
    showError('Could not read logs', error);
  }
}
