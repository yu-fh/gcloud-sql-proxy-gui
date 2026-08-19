// Small DOM helpers, plus the two places the page reports to the user: the
// error banner and the footer status line.
//
// Everything is built with createElement rather than innerHTML: connection
// names and backend error strings are interpolated all over this page, and
// textContent means none of it can ever be parsed as markup.

export const $ = (id) => document.getElementById(id);

export function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = String(text);
  return node;
}

export function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

/// Show a backend or unexpected error as visible text. Every rejected invoke
/// routes here: a failure the user cannot see is worse than no feature.
export function showError(context, error) {
  const banner = $('banner');
  const message = error && error.message ? error.message : String(error);
  banner.textContent = `${context}: ${message}`;
  banner.hidden = false;
  console.error(context, error);
}

export function clearError() {
  const banner = $('banner');
  banner.hidden = true;
  banner.textContent = '';
}

/// Transient status text in the footer ("Saved.", "Unsaved changes").
export function note(id, text) {
  $(id).textContent = text || '';
}

/// How long an operation may take before it is worth telling the user it is
/// running. Under this, a native app shows nothing and simply commits the
/// result when it lands; a placeholder makes a fast operation feel slow.
const BUSY_THRESHOLD_MS = 200;

/// Run `work`, showing `message` only if it is still running after 200ms.
/// Returns whatever `work` resolves to.
///
/// This replaces an older unconditional placeholder row, which was a loading
/// skeleton by another name: it appeared for a backend call that usually
/// answers in well under a frame's worth of perceptible delay.
export async function withBusyNote(id, message, work) {
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

/// Copy text to the clipboard, reporting into the button that asked.
export async function copyText(text, button) {
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
