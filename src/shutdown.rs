//! The ways the app can die that `applicationWillTerminate:` does not see.
//!
//! A quit from the menu, Cmd+Q and logout all reach `applicationWillTerminate:`,
//! which stops every child. These do not:
//!
//! - **A signal** (`kill <pid>`, `pkill`, a launchd stop, Ctrl+C from a
//!   terminal): the default action ends the process at once. [`install`]
//!   catches SIGTERM, SIGINT and SIGHUP and turns each into an ordinary
//!   `terminate:`, so it goes through the clean path.
//! - **A panic**: the release profile uses `panic = "abort"`, so no `Drop` or
//!   `kill_on_drop` runs. A panic hook still does, and it SIGKILLs every child
//!   it can find in the registry in `core::proxy`.
//!
//! What this cannot cover is SIGKILL of the app itself, and a crash that skips
//! the hook (a fault in AppKit, say). There the children are orphaned and keep
//! their ports until stopped by hand.

use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use tokio::signal::unix::{signal, SignalKind};

use crate::core::proxy::kill_live_children_now;
use crate::host::{on_main, runtime};

/// How long a requested quit may take before the children are killed and the
/// process exits regardless. The clean path stops each child with a SIGKILL, so
/// it takes milliseconds; this only matters if the main thread is wedged.
const QUIT_DEADLINE: Duration = Duration::from_secs(3);

/// Install the panic hook and the signal handlers. After `host::init`.
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        kill_live_children_now();
        previous(info);
    }));

    runtime().spawn(async {
        let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
        ) else {
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
            _ = hup.recv() => {}
        }
        on_main(|| {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).terminate(None);
            }
        });
        tokio::time::sleep(QUIT_DEADLINE).await;
        // The clean path did not finish: do the part that matters ourselves.
        kill_live_children_now();
        std::process::exit(0);
    });
}
