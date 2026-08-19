//! Native alerts: the one way this app talks to the user outside the tray menu
//! and the settings window.
//!
//! Every one is a real `NSAlert` via `tauri-plugin-dialog`, never a webview
//! `confirm()` -- a web alert carries the page's origin in its title, which is
//! the single most web-app-looking thing a native app can put on screen.
//!
//! Split out of [`crate::tray`] because both the tray and [`crate::window`]
//! report through here, and a shared helper module is the only way to have that
//! without one of them importing the other.
//!
//! # Non-blocking, deliberately
//!
//! Everything here is built on the callback `show` rather than `blocking_show`.
//! The dialog is displayed on the main thread either way, but the callback form
//! parks no thread at all while the user thinks about it, where `blocking_show`
//! would hold one for as long as the modal is up.

use tauri::{AppHandle, Runtime};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Show a two-button modal and await the answer. True means the affirmative
/// button.
pub async fn confirm<R: Runtime>(
    app: &AppHandle<R>,
    title: String,
    message: String,
    affirmative: &str,
    kind: MessageDialogKind,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();

    app.dialog()
        .message(message)
        .title(title)
        .kind(kind)
        .buttons(MessageDialogButtons::OkCancelCustom(
            affirmative.to_string(),
            "Cancel".to_string(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    // A dropped sender means the dialog went away without answering; treating
    // that as "no" is the safe default when the question is "start
    // production?".
    rx.await.unwrap_or(false)
}

/// A modal carrying an operator-facing failure. Errors from the command layer
/// are already user-facing text (the core renders them via `thiserror`), so
/// they are shown verbatim rather than paraphrased.
pub fn report_error<R: Runtime>(app: &AppHandle<R>, title: &str, message: &str) {
    show_message(app, title, message, MessageDialogKind::Error);
}

pub fn report_info<R: Runtime>(app: &AppHandle<R>, title: &str, message: &str) {
    show_message(app, title, message, MessageDialogKind::Info);
}

fn show_message<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: &str,
    kind: MessageDialogKind,
) {
    // Non-blocking: this is called from async contexts that have nothing left
    // to do but report, and blocking one of them on a click of "OK" would
    // hold a runtime thread for as long as the user ignores the dialog.
    app.dialog()
        .message(message.to_string())
        .title(title.to_string())
        .kind(kind)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

/// Report a config load failure that happened before the tray existed.
///
/// `main` discovers this at startup with nowhere to render it; this is the first
/// thing with a UI, so it takes the message.
pub fn report_startup_error<R: Runtime>(app: &AppHandle<R>, message: &str) {
    report_error(app, "Could not load your profiles", message);
}
