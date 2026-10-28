//! Native alerts: the one way this app talks to the user outside the tray menu
//! and the settings window. Every one is a real `NSAlert`.
//!
//! Both entry points are callable from any thread. The alert itself runs on the
//! main thread; `confirm` hands the answer back through a oneshot so the caller
//! parks no thread while the user thinks.
//!
//! # Why not a GCD block
//!
//! `NSAlert::runModal` runs a nested run loop. Inside a block on the main
//! dispatch queue that starves the queue: it is serial, so no other main-queue
//! block runs until the block returns. The tray's poll updates, nsh's frame
//! wakeups and any second alert all use that queue, so the icon would freeze
//! and the settings window stop redrawing for as long as an alert was up. So
//! the modal is run from `performSelectorOnMainThread:`, which is a run-loop
//! source and leaves the queue free.

use std::collections::VecDeque;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use crate::host::on_main;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Warning,
    Error,
}

/// Run one modal and return whether the first (affirmative) button was chosen.
/// Main thread only.
pub fn run_modal(
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

struct Job {
    title: String,
    message: String,
    kind: Kind,
    affirmative: String,
    cancel: Option<&'static str>,
    reply: Option<tokio::sync::oneshot::Sender<bool>>,
}

/// Alerts waiting to be shown, oldest first.
static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "AlertRunner"]
    #[ivars = ()]
    struct Runner;

    unsafe impl NSObjectProtocol for Runner {}

    impl Runner {
        /// Show the oldest queued alert. One call per submitted job.
        #[unsafe(method(drainAlerts:))]
        fn drain_alerts(&self, _arg: Option<&AnyObject>) {
            let job = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
            let Some(job) = job else { return };
            let answer = run_modal(
                self.mtm(),
                &job.title,
                &job.message,
                job.kind,
                &job.affirmative,
                job.cancel,
            );
            if let Some(reply) = job.reply {
                let _ = reply.send(answer);
            }
        }
    }
);

thread_local! {
    static RUNNER: std::cell::OnceCell<Retained<Runner>> = const { std::cell::OnceCell::new() };
}

/// Queue a job and have the main run loop show it. Any thread.
fn submit(job: Job) {
    QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(job);
    on_main(|| {
        let mtm = MainThreadMarker::new().expect("on the main queue");
        RUNNER.with(|cell| {
            let runner = cell.get_or_init(|| {
                let this = mtm.alloc::<Runner>().set_ivars(());
                unsafe { msg_send![super(this), init] }
            });
            // Returns at once; the alert is shown from the run loop, not from
            // this block.
            let _: () = unsafe {
                msg_send![
                    &**runner,
                    performSelectorOnMainThread: sel!(drainAlerts:),
                    withObject: std::ptr::null::<AnyObject>(),
                    waitUntilDone: false
                ]
            };
        });
    });
}

/// A two-button modal. True means the affirmative button.
pub async fn confirm(title: String, message: String, affirmative: &str, kind: Kind) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    submit(Job {
        title,
        message,
        kind,
        affirmative: affirmative.to_string(),
        cancel: Some("Cancel"),
        reply: Some(tx),
    });
    // A dropped sender means the alert went away unanswered; treating that as
    // "no" is the safe default when the question is "start production?".
    rx.await.unwrap_or(false)
}

/// A modal carrying an operator-facing failure. The core's errors are already
/// user-facing text, so they are shown verbatim.
pub fn report_error(title: &str, message: &str) {
    submit(Job {
        title: title.to_string(),
        message: message.to_string(),
        kind: Kind::Error,
        affirmative: "OK".to_string(),
        cancel: None,
        reply: None,
    });
}

/// Report a config load failure discovered before the tray existed.
pub fn report_startup_error(message: &str) {
    report_error("Could not load your profiles", message);
}
