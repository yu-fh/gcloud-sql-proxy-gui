// The window's editing state, in one place.
//
// Split out of the profiles view because the logs section reads the profile
// list too (its filter dropdown is built from it), and a module-level `let` in
// the view would have made that an import cycle.
//
// The profiles array is mutated in place by the field handlers rather than
// re-read from the DOM at save time, so a full re-render is never needed to keep
// edits -- and re-rendering the detail pane never destroys the field being typed
// in. Accessors rather than a bare exported `let`: an ES module binding is
// read-only to importers, so assignment has to live here anyway, and making that
// explicit is clearer than a setter that looks like a variable.

/// The profiles as edited in this window.
let profiles = [];

/// Which profile the detail pane is showing, by id. Kept as an id rather than
/// an index so it survives a reload that reorders or drops profiles.
let selectedId = null;

/// Proposed connection-name changes from the last `refresh_connection_names`.
/// Held here, unwritten, until the user clicks Apply.
let pendingChanges = [];

export function getProfiles() {
  return profiles;
}

export function setProfiles(next) {
  profiles = next;
}

export function getSelectedId() {
  return selectedId;
}

export function setSelectedId(id) {
  selectedId = id;
}

export function selectedProfile() {
  return profiles.find((p) => p.id === selectedId) || null;
}

export function selectedIndex() {
  return profiles.findIndex((p) => p.id === selectedId);
}

export function getPendingChanges() {
  return pendingChanges;
}

export function setPendingChanges(changes) {
  pendingChanges = changes;
}
