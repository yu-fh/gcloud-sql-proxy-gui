#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// The core modules now live in the library target (`src/lib.rs`) so integration
// tests can import them. The binary will pull them in as
// `fh_cloud_sql_proxy_gui::core::*` once the Tauri command layer needs them
// (Task 9); nothing here references them yet, so importing now would only
// produce an unused-import warning.

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running application");
}
