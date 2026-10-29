//! Process-wide handles the AppKit side needs: the tokio runtime, the shared
//! state, and a way to run something on the main thread.
//!
//! AppKit callbacks are plain Objective-C methods with no place to carry
//! context, so the things they need live here, set once in `main` before the
//! run loop starts.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

use crate::app_state::SharedState;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static STATE: OnceLock<SharedState> = OnceLock::new();

/// Install the runtime and state. Call once, before the run loop starts.
pub fn init(runtime: Runtime, state: SharedState) {
    assert!(RUNTIME.set(runtime).is_ok(), "host::init called twice");
    assert!(STATE.set(state).is_ok(), "host::init called twice");
}

pub fn runtime() -> &'static Runtime {
    RUNTIME.get().expect("host::init has not run")
}

pub fn state() -> &'static SharedState {
    STATE.get().expect("host::init has not run")
}

/// Run `f` on the main thread, later. AppKit is main-thread-only and the poll
/// loop and the proxy tasks are not on it.
pub fn on_main(f: impl FnOnce() + Send + 'static) {
    dispatch2::DispatchQueue::main().exec_async(f);
}
