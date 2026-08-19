// The Tauri bridge: the only module that reaches for `window.__TAURI__`.
//
// Everything else imports from here, so what the page is allowed to ask the
// backend for is one import list rather than a global reached for from six
// places. It also means the plugin-absent case (the page driven headless in a
// plain browser during development) is handled once.
//
// No bundler: `withGlobalTauri` puts the whole API on the window and the CSP is
// `default-src 'self'`, so this is a plain ES module served same-origin.

/// Call a Rust command. Rejections are the caller's to surface -- see
/// `showError` in dom.js; every call site routes failures to visible text.
///
/// Field names on the wire are camelCase (`connectionName`, `profileId`,
/// `autoIamAuthn`, `impersonateServiceAccount`, `fixCommand`): the Rust structs
/// carry serde renames and the snake_case spelling is rejected by the
/// deserializer, silently dropping the value in the best case.
export const invoke = window.__TAURI__.core.invoke;

/// The native window this page is drawn in, for ⌘W and ⌘M.
///
/// Guarded because the page is also driven headless in a plain browser during
/// development, where there is no window to control.
export const tauriWindow = window.__TAURI__.window
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
export async function confirmDestructive(message, title, okLabel) {
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
