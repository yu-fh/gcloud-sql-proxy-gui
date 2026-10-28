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

fn plist_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{NAME}.plist")))
}

/// Whether the app is registered to launch at login. Not being able to tell
/// reads as "not enabled": an unchecked box is the honest default.
pub fn is_enabled() -> bool {
    plist_path().is_some_and(|p| p.exists())
}

/// What launchd should run: `open -a` on the bundle when this process lives in
/// one, so the app launches as an app, else the bare executable.
fn program_arguments() -> Result<Vec<String>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the app: {e}"))?;
    let bundle = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"));
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

pub fn enable() -> Result<(), String> {
    let path = plist_path().ok_or("no home directory")?;
    let dir = path.parent().ok_or("no LaunchAgents directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(&path, plist(&program_arguments()?)).map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    let path = plist_path().ok_or("no home directory")?;
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
    fn arguments_are_xml_escaped() {
        let p = plist(&["/a&b/<c>".into()]);
        assert!(p.contains("/a&amp;b/&lt;c&gt;"), "{p}");
    }
}
