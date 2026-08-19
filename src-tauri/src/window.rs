//! The settings window: one native window with a sidebar, and the sidecar that
//! remembers where the user left it.
//!
//! Split out of [`crate::tray`] because these are two different concerns that
//! merely happen to be reachable from the same menu: the tray builds and polls
//! an `NSMenu`, this opens and positions a webview. Neither needs to know how
//! the other works.
//!
//! # One window, not two
//!
//! There used to be two windows, `profiles` and `logs`, both loading
//! `index.html` with the hash choosing the view. Two windows both titled "Cloud
//! SQL Proxy" appeared twice in Mission Control and read as two apps rather
//! than one settings panel. They are now two sections of one window with a
//! source list on the left, which is what a macOS settings window is.
//!
//! # The ACL
//!
//! [`WINDOW_SETTINGS`] must match `capabilities/default.json` exactly. A window
//! under any other label gets no plugin access at all -- no autostart, no native
//! dialogs -- and it fails at runtime rather than at build time, so there is a
//! test pinning the constant and another reading the capability file itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

use crate::dialogs::report_error;

/// The one window label. See the module docs on the ACL.
pub const WINDOW_SETTINGS: &str = "settings";

/// The window's title. One window, one label: the sidebar names the section, so
/// the title bar names the app.
const WINDOW_TITLE: &str = "Cloud SQL Proxy";

/// Default size. Wider than the old profiles window because the logs pane wants
/// width, and tall enough for the profile form's full height plus the footer;
/// the sidebar adds its own width on top of the old content width.
const WINDOW_WIDTH: f64 = 900.0;
const WINDOW_HEIGHT: f64 = 600.0;

/// Remembered geometry for one window label.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct WindowGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Where the remembered geometry lives: a small sidecar next to the profile
/// config.
///
/// Deliberately *not* `tauri-plugin-window-state`. That plugin would do this
/// job, but it pulls in a dependency tree and has to be registered in
/// `main.rs`, and what it buys over ~40 lines here is machinery this app does
/// not need: it tracks maximised/fullscreen/visibility for every window in the
/// app, where this one has a single window that is only ever plain and
/// resizable. A sidecar file keeps the whole feature inside the window code that
/// owns it.
///
/// The file is still keyed by label rather than holding one bare geometry: it
/// already has entries under `profiles` and `logs` on every machine that ran an
/// earlier build, and a map means those are simply ignored rather than
/// misparsed.
fn window_state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("fh-cloud-sql-proxy-gui").join("window-state.json"))
}

/// Read every remembered window geometry. A missing or corrupt file is not an
/// error worth reporting -- the windows simply open at their default size,
/// which is exactly what happens on first run.
fn load_window_state() -> HashMap<String, WindowGeometry> {
    window_state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Persist one window's geometry, merging into whatever is already stored so a
/// write does not discard entries under other labels.
///
/// Errors are swallowed on purpose: failing to remember a window position is
/// not worth a modal, and the next launch just uses the default.
fn save_window_geometry(label: &str, geometry: WindowGeometry) {
    let Some(path) = window_state_path() else {
        return;
    };
    let mut all = load_window_state();
    all.insert(label.to_string(), geometry);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// Whether a remembered position still lands on a screen that exists.
///
/// Restoring blind is how a window ends up entirely off-canvas after someone
/// unplugs the external display it was last on -- a window you cannot reach is
/// worse than one in the wrong place. Requiring the saved origin to fall
/// inside some current monitor is the cheap version of the check AppKit does.
fn position_is_visible<R: Runtime>(app: &AppHandle<R>, geometry: &WindowGeometry) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };
    monitors.iter().any(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        let scale = monitor.scale_factor();
        // Monitor geometry is physical; the saved geometry is logical.
        let left = position.x as f64 / scale;
        let top = position.y as f64 / scale;
        let right = left + size.width as f64 / scale;
        let bottom = top + size.height as f64 / scale;
        // The title bar is what the user grabs, so it is the part that has to
        // be on screen. Requiring the whole window would refuse to restore a
        // window the user had deliberately hanging off an edge.
        geometry.x >= left - 8.0
            && geometry.x < right - 40.0
            && geometry.y >= top - 8.0
            && geometry.y < bottom - 40.0
    })
}

/// Which section of the settings window a tray item opens.
///
/// The page reads `location.hash` to decide, so the enum's whole job is to name
/// the fragment in one place rather than spelling `"#logs"` at each call site.
#[derive(Clone, Copy)]
pub enum Section {
    Profiles,
    Logs,
}

impl Section {
    /// The URL fragment, without the `#`.
    fn hash(self) -> &'static str {
        match self {
            Section::Profiles => "profiles",
            Section::Logs => "logs",
        }
    }
}

/// Show the settings window on the given section, creating it on first use.
///
/// The window is created lazily rather than at launch so the app costs nothing
/// until asked, and reused rather than duplicated afterwards: `build` with an
/// existing label fails, and two windows editing the same config would be a way
/// to lose edits.
///
/// `Profiles…` and `Logs…` both land here. When the window already exists the
/// section is switched by writing `location.hash` from Rust — the page's
/// `hashchange` handler does the rest, so a tray click and a sidebar click take
/// exactly the same path through the JS.
pub fn open_settings<R: Runtime>(app: &AppHandle<R>, section: Section) {
    if let Some(window) = app.get_webview_window(WINDOW_SETTINGS) {
        // Set the section before showing, so the window never appears on the
        // wrong one and then flips.
        //
        // The hash is a fixed string from `Section::hash`, never user input, so
        // there is nothing here to escape. An `eval` failure means the webview
        // is gone, in which case showing it is the only useful thing left to
        // try.
        let _ = window.eval(format!("window.location.hash = '#{}';", section.hash()));
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }

    let remembered = load_window_state().get(WINDOW_SETTINGS).copied();

    let url = format!("index.html#{}", section.hash());
    let mut builder = WebviewWindowBuilder::new(app, WINDOW_SETTINGS, WebviewUrl::App(url.into()))
        .title(WINDOW_TITLE)
        // A unified title bar, as native settings windows have: the page draws
        // under the traffic lights instead of below a separate bar. The page
        // pays for this with a header strip that supplies the clearance and
        // the drag region -- see `.titlebar` in styles.css.
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .resizable(true)
        // The old 420pt floor was for a window with no sidebar. The sidebar
        // collapses to glyphs below 632pt (see the media query in styles.css),
        // so the content pane keeps the same usable width it always had at this
        // minimum. A native window declares its minimum rather than letting the
        // user drag it into an unusable shape.
        .min_inner_size(520.0, 360.0);

    // Restore size and position together: a remembered size at a default
    // position would put the window somewhere the user never left it.
    builder = match remembered {
        Some(geometry) if position_is_visible(app, &geometry) => builder
            .inner_size(geometry.width, geometry.height)
            .position(geometry.x, geometry.y),
        // No memory, or the screen it was on is gone: default size, and let
        // the OS place it.
        _ => builder.inner_size(WINDOW_WIDTH, WINDOW_HEIGHT).center(),
    };

    match builder.build() {
        Ok(window) => {
            // With `ActivationPolicy::Accessory` the app is not the active
            // application, so a new window can open behind whatever the user
            // was in. Focusing it explicitly is what makes the click feel
            // like it did something.
            let _ = window.set_focus();
            remember_geometry_on_close(&window, WINDOW_SETTINGS.to_string());
        }
        Err(error) => report_error(
            app,
            &format!("Could not open {WINDOW_TITLE}"),
            &error.to_string(),
        ),
    }
}

/// Keep the window's geometry, and turn closing it into hiding it.
///
/// Two jobs, both hanging off the same event stream.
///
/// Geometry: saving on close alone is not enough, because on macOS a window
/// that is going away does not reliably report its final frame. Tracking the
/// last-seen good geometry as it changes and writing that is what survives.
///
/// Closing: the app lives in the menu bar, so ⌘W and the red traffic light
/// mean "put this away", not "quit". The close is prevented and the window
/// hidden instead, which also keeps the webview alive so reopening is instant
/// and does not discard scroll position or a half-typed field.
fn remember_geometry_on_close<R: Runtime>(window: &tauri::WebviewWindow<R>, label: String) {
    let tracked: Arc<Mutex<Option<WindowGeometry>>> = Arc::new(Mutex::new(None));

    let handle = window.clone();
    let seen = tracked.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            // A minimised or zoomed window reports a frame that is not the one
            // to restore, so only plain states are recorded.
            if handle.is_minimized().unwrap_or(false) {
                return;
            }
            if let (Ok(position), Ok(size), Ok(scale)) = (
                handle.outer_position(),
                handle.inner_size(),
                handle.scale_factor(),
            ) {
                if size.width == 0 || size.height == 0 {
                    return;
                }
                if let Ok(mut slot) = seen.lock() {
                    *slot = Some(WindowGeometry {
                        x: position.x as f64 / scale,
                        y: position.y as f64 / scale,
                        width: size.width as f64 / scale,
                        height: size.height as f64 / scale,
                    });
                }
            }
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            let geometry = seen.lock().ok().and_then(|slot| *slot);
            if let Some(geometry) = geometry {
                save_window_geometry(&label, geometry);
            }

            // Hide rather than destroy. The app lives in the menu bar, so
            // closing a window means "put it away", not "quit" — the same
            // thing the red button does in Tailscale or System Settings while
            // the app keeps running. Destroying it would also throw away the
            // webview, so the next `Profiles…` would pay a rebuild and lose
            // scroll position and any half-typed field.
            //
            // `open_window` already prefers `show()` on an existing window, so
            // a hidden window is what makes reopening instant.
            api.prevent_close();
            let _ = handle.hide();
        }
        tauri::WindowEvent::Destroyed => {
            let geometry = seen.lock().ok().and_then(|slot| *slot);
            if let Some(geometry) = geometry {
                save_window_geometry(&label, geometry);
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_label_matches_the_acl() {
        // `capabilities/default.json` grants plugin permissions to exactly this
        // one label; a typo here means a window with no plugin access, which
        // fails at runtime rather than at build time.
        assert_eq!(WINDOW_SETTINGS, "settings");
    }

    #[test]
    fn the_acl_file_actually_lists_the_window_label() {
        // The assertion above only pins the constant. This one reads the ACL
        // itself, so renaming the label without editing the capability file is
        // a test failure rather than a window whose `invoke` calls are silently
        // denied.
        let acl = include_str!("../capabilities/default.json");
        let listed: serde_json::Value = serde_json::from_str(acl).expect("capability file is JSON");
        let windows = listed["windows"]
            .as_array()
            .expect("capability file lists windows");
        assert!(
            windows.iter().any(|w| w == WINDOW_SETTINGS),
            "capabilities/default.json does not grant permissions to \
             '{WINDOW_SETTINGS}'; it lists {windows:?}"
        );
    }

    #[test]
    fn each_section_names_its_hash() {
        // The page dispatches on `location.hash`, so these strings are a
        // contract with applyView() in shell.js.
        assert_eq!(Section::Profiles.hash(), "profiles");
        assert_eq!(Section::Logs.hash(), "logs");
    }
}
