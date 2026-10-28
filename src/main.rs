//! Process entry point: resolve the config and the proxy binary, build the
//! shared state, put the tray up, and run the AppKit loop. Children are
//! stopped in `applicationWillTerminate:` (see `tray::shut_down`), so no path
//! out of the app leaves one behind.

use std::path::{Path, PathBuf};

use fh_cloud_sql_proxy_gui::app_state::Shared;
use fh_cloud_sql_proxy_gui::core::audit::{Category, Logger, SystemInfoInputs};
use fh_cloud_sql_proxy_gui::core::profile::ProfileConfig;
use fh_cloud_sql_proxy_gui::core::proxy::ProxyManager;
use fh_cloud_sql_proxy_gui::core::store;
use fh_cloud_sql_proxy_gui::{dialogs, host, tray};
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;

/// Homebrew's install location, preferred when present so the app does not
/// depend on the `PATH` a GUI process happens to inherit (a `.app` launched
/// from Finder gets a minimal one, typically without `/opt/homebrew/bin`).
const HOMEBREW_PROXY: &str = "/opt/homebrew/bin/cloud-sql-proxy";

fn main() {
    let mtm = MainThreadMarker::new().expect("main runs on the main thread");

    let config_path = store::default_config_path()
        // `dirs::config_dir()` only fails without a home directory. Falling
        // back to a relative path keeps the app usable rather than panicking
        // before it can report anything.
        .unwrap_or_else(|| PathBuf::from("profiles.json"));

    let (config, load_error) = load_config(&config_path);

    // The audit logger is built first and lives for the whole process: its
    // writer is a plain OS thread, so it is available before the run loop
    // starts and still available while it shuts down, which is what lets
    // startup and exit both be recorded.
    let audit = Logger::at_default_path();
    audit.info(
        Category::System,
        None,
        format!("--- app starting (pid {}) ---", std::process::id()),
    );
    if let Some(message) = &load_error {
        audit.error(Category::System, None, message.clone());
    }

    let binary = proxy_binary();
    let manager = ProxyManager::new(binary.clone()).with_audit(audit.clone());
    let shared = Shared::new(config, config_path.clone(), manager);

    // Category 3, off the startup path. `sw_vers`, `cloud-sql-proxy --version`
    // and `gcloud config get-value account` together take on the order of a
    // second, and a menu bar app whose icon takes a second to appear looks
    // broken. A detached OS thread: this is blocking subprocess I/O that has
    // no business on an async executor.
    let info_audit = audit.clone();
    let info_inputs = SystemInfoInputs {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        proxy_binary: binary,
        config_path: config_path.clone(),
    };
    // A machine that cannot spawn a thread has larger problems, and losing the
    // system-info block is not a reason to refuse to launch.
    let _ = std::thread::Builder::new()
        .name("audit-system-info".to_string())
        .spawn(move || info_audit.system_info(&info_inputs));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("proxy-runtime")
        .enable_all()
        .build()
        .expect("tokio runtime");
    host::init(runtime, shared);

    tray::install(mtm);

    if let Some(message) = &load_error {
        // stderr for the operator, and a dialog for the user: with no Dock icon
        // and no window, a menu bar app that silently fell back to defaults
        // gives no clue that it did.
        eprintln!("{message}");
        dialogs::report_startup_error(message);
    }

    NSApplication::sharedApplication(mtm).run();
}

/// Load the config, degrading to in-memory defaults on failure.
///
/// A malformed or invalid `profiles.json` is user-fixable, so panicking here
/// would be the worst possible response: a menu bar app that dies at launch has
/// no way to tell anyone why. Instead the app starts with no environments held
/// **in memory only**. The bad file is left untouched, so the user can still fix
/// or recover it, and nothing overwrites their (possibly hand-edited) config
/// behind their back. The first explicit save from the UI is what replaces the
/// file, which is a deliberate user action.
///
/// Returns the config plus an operator-facing message when the load failed.
fn load_config(path: &Path) -> (ProfileConfig, Option<String>) {
    match store::load_or_init(path) {
        Ok(config) => (config, None),
        Err(error) => (
            store::empty_config(),
            Some(format!(
                "Could not load {}: {error}. Starting with no environments; \
                 the file on disk was left unchanged and will only be replaced \
                 when you save.",
                path.display()
            )),
        ),
    }
}

/// Prefer the Homebrew install if it is actually there, else let the OS
/// resolve `cloud-sql-proxy` from `PATH`.
fn proxy_binary() -> PathBuf {
    let homebrew = Path::new(HOMEBREW_PROXY);
    if homebrew.exists() {
        homebrew.to_path_buf()
    } else {
        PathBuf::from("cloud-sql-proxy")
    }
}
