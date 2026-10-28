//! Native alerts: the one way this app talks to the user outside the tray menu
//! and the settings window. Every one is a real `NSAlert`.
//!
//! Both entry points are callable from any thread. The alert itself runs on the
//! main thread; `confirm` hands the answer back through a oneshot so the caller
//! parks no thread while the user thinks.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication};
use objc2_foundation::NSString;

use crate::host::on_main;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Warning,
    Error,
}

fn run_modal(
    mtm: MainThreadMarker,
    title: &str,
    message: &str,
    kind: Kind,
    affirmative: &str,
    cancel: Option<&str>,
) -> bool {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    alert.setAlertStyle(match kind {
        Kind::Warning => NSAlertStyle::Warning,
        Kind::Error => NSAlertStyle::Critical,
    });
    alert.addButtonWithTitle(&NSString::from_str(affirmative));
    if let Some(cancel) = cancel {
        alert.addButtonWithTitle(&NSString::from_str(cancel));
    }
    // An accessory app is not frontmost, so an alert raised from the menu bar
    // would otherwise open behind whatever window the user is in.
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    alert.runModal() == NSAlertFirstButtonReturn
}

/// A two-button modal. True means the affirmative button.
pub async fn confirm(title: String, message: String, affirmative: &str, kind: Kind) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let affirmative = affirmative.to_string();
    on_main(move || {
        let mtm = MainThreadMarker::new().expect("on the main queue");
        let answer = run_modal(mtm, &title, &message, kind, &affirmative, Some("Cancel"));
        let _ = tx.send(answer);
    });
    // A dropped sender means the alert went away unanswered; treating that as
    // "no" is the safe default when the question is "start production?".
    rx.await.unwrap_or(false)
}

/// A modal carrying an operator-facing failure. The core's errors are already
/// user-facing text, so they are shown verbatim.
pub fn report_error(title: &str, message: &str) {
    let (title, message) = (title.to_string(), message.to_string());
    on_main(move || {
        let mtm = MainThreadMarker::new().expect("on the main queue");
        run_modal(mtm, &title, &message, Kind::Error, "OK", None);
    });
}

/// Report a config load failure discovered before the tray existed.
pub fn report_startup_error(message: &str) {
    report_error("Could not load your profiles", message);
}
