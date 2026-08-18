#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Process entry point: resolve the config and the proxy binary, build the
//! shared state, register the command surface, and guarantee no child outlives
//! the app.

mod app_state;
mod commands;

use std::path::{Path, PathBuf};

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

    let manager = ProxyManager::new(proxy_binary());
    let shared = app_state::Shared::new(config, config_path.clone(), manager);

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
            commands::start_profile,
            commands::stop_profile,
            commands::refresh_connection_names,
            commands::apply_changes,
            commands::read_logs,
        ])
        .setup(move |_app| {
            // Menu-bar-only: no Dock icon, no app switcher entry. There is no
            // tauri.conf.json field for this, so it has to happen here.
            #[cfg(target_os = "macos")]
            _app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Task 10 builds the tray menu here: `tray::build(_app, &shared)?;`

            if let Some(message) = &load_error {
                // With no tray and no window yet there is nowhere to render
                // this, so stderr is the honest channel. Task 10/11 should
                // surface it as a menu item or a dialog.
                eprintln!("{message}");
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building application");

    let on_exit = shared.clone();
    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            // A leaked child keeps holding 15432/15433 and breaks the next
            // launch. `kill_on_drop` + `Drop` on `ProxyManager` are a
            // backstop; this is the clean path.
            tauri::async_runtime::block_on(async {
                on_exit.manager.lock().await.stop_all().await;
            });
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
