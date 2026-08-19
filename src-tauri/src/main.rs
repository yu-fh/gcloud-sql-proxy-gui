#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Process entry point: resolve the config and the proxy binary, build the
//! shared state, register the command surface, and guarantee no child outlives
//! the app.

mod app_state;
mod commands;
mod dialogs;
mod tray;
mod window;

use std::path::{Path, PathBuf};

use fh_cloud_sql_proxy_gui::core::audit::{Category, Logger, SystemInfoInputs};
use fh_cloud_sql_proxy_gui::core::profile::ProfileConfig;
use fh_cloud_sql_proxy_gui::core::proxy::ProxyManager;
use fh_cloud_sql_proxy_gui::core::store;

/// Homebrew's install location, preferred when present so the app does not
/// depend on the `PATH` a GUI process happens to inherit (a `.app` launched
/// from Finder gets a minimal one, typically without `/opt/homebrew/bin`).
const HOMEBREW_PROXY: &str = "/opt/homebrew/bin/cloud-sql-proxy";

fn main() {
    let config_path = store::default_config_path()
        // `dirs::config_dir()` only fails without a home directory. Falling
        // back to a relative path keeps the app usable rather than panicking
        // before it can report anything.
        .unwrap_or_else(|| PathBuf::from("profiles.json"));

    let (config, load_error) = load_config(&config_path);

    // The audit logger is built first and lives for the whole process: its
    // writer is a plain OS thread, so it is available before the Tauri runtime
    // starts and still available while it shuts down -- which is what lets
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
    let shared = app_state::Shared::new(config, config_path.clone(), manager);

    // Category 3, off the startup path. `sw_vers`, `cloud-sql-proxy --version`
    // and `gcloud config get-value account` together take on the order of a
    // second, and a menu bar app whose icon takes a second to appear looks
    // broken. A detached OS thread rather than a tokio task: the runtime does
    // not exist yet at this point, and this work is blocking subprocess I/O
    // that has no business on an async executor anyway.
    let info_audit = audit.clone();
    let info_inputs = SystemInfoInputs {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        proxy_binary: binary,
        config_path: config_path.clone(),
    };
    std::thread::Builder::new()
        .name("audit-system-info".to_string())
        .spawn(move || info_audit.system_info(&info_inputs))
        // A machine that cannot spawn a thread has larger problems, and losing
        // the system-info block is not a reason to refuse to launch.
        .map(|_| ())
        .unwrap_or(());

    let shared_for_setup = shared.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::plan_for,
            commands::save_profiles,
            commands::add_profile,
            commands::delete_profile,
            commands::start_profile,
            commands::stop_profile,
            commands::read_logs,
            commands::reveal_log_file,
        ])
        .setup(move |app| {
            // Menu-bar-only: no Dock icon, no app switcher entry. There is no
            // tauri.conf.json field for this, so it has to happen here.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            tray::build(app, &shared_for_setup)?;

            if let Some(message) = &load_error {
                // stderr for the operator, and a dialog for the user: with no
                // Dock icon and no window, a menu bar app that silently fell
                // back to defaults gives no clue that it did.
                eprintln!("{message}");
                dialogs::report_startup_error(app.handle(), message);
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building application");

    let on_exit = shared.clone();
    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            on_exit
                .audit
                .info(Category::System, None, "--- app exiting ---");
            // A leaked child keeps holding 15432/15433 and breaks the next
            // launch. `kill_on_drop` + `Drop` on `ProxyManager` are a
            // backstop; this is the clean path.
            tauri::async_runtime::block_on(async {
                on_exit.manager.lock().await.stop_all().await;
            });
            // The whole point of persisting is surviving the process ending, so
            // give the writer a bounded moment to land the last records. The
            // bound matters more than the flush: an app that will not quit is
            // worse than a log missing its final line.
            on_exit
                .audit
                .flush_blocking(std::time::Duration::from_millis(500));
        }
    });
}

/// Load the config, degrading to in-memory seeded defaults on failure.
///
/// A malformed or invalid `profiles.json` is user-fixable, so panicking here
/// would be the worst possible response: a menu bar app that dies at launch
/// has no way to tell anyone why. Instead the app starts on seeded defaults
/// held **in memory only** — the bad file is left untouched, so the user can
/// still fix or recover it, and nothing overwrites their (possibly
/// hand-edited) config behind their back. The first explicit save from the UI
/// is what replaces the file, which is a deliberate user action.
///
/// Returns the config plus an operator-facing message when the load failed.
fn load_config(path: &Path) -> (ProfileConfig, Option<String>) {
    match store::load_or_seed(path) {
        Ok(config) => (config, None),
        Err(error) => (
            store::seed_profiles(),
            Some(format!(
                "Could not load {}: {error}. Starting with default profiles; \
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
