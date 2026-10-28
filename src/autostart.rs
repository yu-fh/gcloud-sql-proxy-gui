//! Launch at login, as a per-user LaunchAgent.
//!
//! This registers **the app**, not any proxy: nothing starts a profile on
//! login, so a machine that reboots does not come up holding 15432.
//!
//! The agent is a plist in `~/Library/LaunchAgents`. Its existence is the
//! state: enabling writes it, disabling removes it, and launchd picks it up at
//! the next login, so there is no `launchctl` call to fail.

use std::path::PathBuf;

const NAME: &str = "fh-cloud-sql-proxy-gui";

/// The name the Tauri build registered under: its autostart plugin named the
/// agent after the product name. A user who enabled Launch at Login before the
/// rewrite has this file, and it points at the same bundle path, so it still
/// works. It has to be recognised, or the menu would show the item unchecked
/// while the app launches anyway, and enabling it again would add a second
/// agent.
const NAME_DISPLAY: &str = "Cloud SQL Proxy";

const LEGACY_NAME: &str = "Cloud SQL Proxy";

fn plist_path_named(name: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{name}.plist")))
}

fn plist_path() -> Option<PathBuf> {
    plist_path_named(NAME)
}

fn legacy_plist_path() -> Option<PathBuf> {
    plist_path_named(LEGACY_NAME)
}

/// Whether the app is registered to launch at login, under either name. Not
/// being able to tell reads as "not enabled": an unchecked box is the honest
/// default.
pub fn is_enabled() -> bool {
    [plist_path(), legacy_plist_path()]
        .into_iter()
        .flatten()
        .any(|p| p.exists())
}

/// Whether `bundle` is somewhere a login launch can still find it. A bundle run
/// from a mounted DMG (`/Volumes/...`) or from an App Translocation path is
/// temporary: registering it would write a path that stops existing, and the
/// login launch would then fail with nothing to say why.
fn is_installed_location(bundle: &std::path::Path) -> bool {
    let in_user_apps = dirs::home_dir().is_some_and(|h| bundle.starts_with(h.join("Applications")));
    bundle.starts_with("/Applications") || in_user_apps
}

/// What launchd should run: `open -a` on the bundle when this process lives in
/// one, so the app launches as an app, else the bare executable.
fn program_arguments() -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the app: {e}"))?;
    let bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"));
    if let Some(bundle) = bundle {
        if !is_installed_location(bundle) {
            return Err(format!(
                "Move {NAME_DISPLAY} to the Applications folder first. It is running from {}, \
                 which will not exist at login.",
                bundle.display()
            ));
        }
    }
    Ok(match bundle {
        Some(bundle) => vec![
            "/usr/bin/open".into(),
            "-a".into(),
            bundle.display().to_string(),
        ],
        None => vec![exe.display().to_string()],
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn plist(args: &[String]) -> String {
    let args: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n  <key>Label</key>\n  <string>{NAME}</string>\n  <key>ProgramArguments</key>\n  <array>\n{args}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n\
         </plist>\n"
    )
}

/// Register the app. Replaces a legacy registration rather than adding a second.
pub fn enable() -> Result<(), String> {
    let path = plist_path().ok_or("no home directory")?;
    let dir = path.parent().ok_or("no LaunchAgents directory")?;
    let contents = plist(&program_arguments()?);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    remove(legacy_plist_path())
}

/// Unregister the app, whichever name it was registered under.
pub fn disable() -> Result<(), String> {
    remove(plist_path())?;
    remove(legacy_plist_path())
}

fn remove(path: Option<PathBuf>) -> Result<(), String> {
    let path = path.ok_or("no home directory")?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_runs_at_load_and_carries_every_argument() {
        let p = plist(&[
            "/usr/bin/open".into(),
            "-a".into(),
            "/Applications/X.app".into(),
        ]);
        assert!(p.contains("<key>RunAtLoad</key>\n  <true/>"));
        for arg in ["/usr/bin/open", "-a", "/Applications/X.app"] {
            assert!(p.contains(&format!("<string>{arg}</string>")), "{p}");
        }
    }

    #[test]
    fn only_a_bundle_in_an_applications_folder_can_be_registered() {
        use std::path::Path;
        assert!(is_installed_location(Path::new(
            "/Applications/Cloud SQL Proxy.app"
        )));
        let home = dirs::home_dir().expect("home");
        assert!(is_installed_location(&home.join("Applications/X.app")));
        // The two temporary places an unsigned, downloaded app really runs from.
        assert!(!is_installed_location(Path::new(
            "/Volumes/Cloud SQL Proxy/X.app"
        )));
        assert!(!is_installed_location(Path::new(
            "/private/var/folders/ab/T/AppTranslocation/UUID/d/X.app"
        )));
        assert!(!is_installed_location(&home.join("Downloads/X.app")));
    }

    #[test]
    fn the_legacy_name_is_the_one_the_tauri_build_wrote() {
        let legacy = legacy_plist_path().expect("home");
        assert!(legacy.ends_with("Library/LaunchAgents/Cloud SQL Proxy.plist"));
        assert_ne!(Some(legacy), plist_path());
    }

    #[test]
    fn arguments_are_xml_escaped() {
        let p = plist(&["/a&b/<c>".into()]);
        assert!(p.contains("/a&amp;b/&lt;c&gt;"), "{p}");
    }
}
