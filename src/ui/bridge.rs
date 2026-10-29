//! Runs service calls on the tokio runtime and hands their results to the nsh
//! thread. See the module docs on [`crate::ui`].

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::core::audit::Category;
use crate::dialogs::{self, Kind};
use crate::host::{runtime, state};
use crate::service::{self, LogsView, ProfileView};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Banner {
    pub title: String,
    pub body: String,
}

#[derive(Default)]
struct Inner {
    profiles: Vec<ProfileView>,
    /// Set when the loaded profiles replace what the editor holds (load, save,
    /// add, import, delete) rather than only refreshing live status.
    reset_edits: bool,
    select: Option<String>,
    ignored_flags: Vec<String>,
    logs: Option<LogsView>,
    banner: Option<Banner>,
    note: String,
    /// A save went through: the editor's copy is what is on disk now.
    saved: bool,
}

static INNER: Mutex<Inner> = Mutex::new(Inner {
    profiles: Vec::new(),
    reset_edits: false,
    select: None,
    ignored_flags: Vec::new(),
    logs: None,
    banner: None,
    note: String::new(),
    saved: false,
});

/// Whether the editor holds unsaved changes. Mirrored from the nsh signal so
/// the window's close handler, which is on the main thread, can read it.
static DIRTY: AtomicBool = AtomicBool::new(false);

pub fn set_dirty(dirty: bool) {
    DIRTY.store(dirty, Ordering::Release);
}

pub fn is_dirty() -> bool {
    DIRTY.load(Ordering::Acquire)
}

/// Forget the previous window's banner and note, so a reopened window does not
/// start by showing an old error or "Saved.".
pub fn reset_session() {
    write(|i| {
        i.banner = None;
        i.note.clear();
        i.ignored_flags.clear();
        i.saved = false;
    });
    set_dirty(false);
}
/// Bumped on every write; a component re-reads when it changes.
static VERSION: AtomicU64 = AtomicU64::new(0);

fn write(f: impl FnOnce(&mut Inner)) {
    f(&mut INNER.lock().unwrap_or_else(|e| e.into_inner()));
    VERSION.fetch_add(1, Ordering::Release);
}

pub fn version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

/// Everything a view needs, cloned out.
#[derive(Clone, Default)]
pub struct Data {
    pub profiles: Vec<ProfileView>,
    pub reset_edits: bool,
    pub select: Option<String>,
    pub ignored_flags: Vec<String>,
    pub logs: Option<LogsView>,
    pub banner: Option<Banner>,
    pub note: String,
    pub saved: bool,
}

/// The current data. Consumes the one-shot flags (`reset_edits`, `select`,
/// `saved`).
pub fn take() -> Data {
    let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
    let data = Data {
        profiles: inner.profiles.clone(),
        reset_edits: inner.reset_edits,
        select: inner.select.take(),
        ignored_flags: inner.ignored_flags.clone(),
        logs: inner.logs.clone(),
        banner: inner.banner.clone(),
        note: inner.note.clone(),
        saved: inner.saved,
    };
    inner.reset_edits = false;
    inner.saved = false;
    data
}

pub fn set_note(note: &str) {
    write(|i| i.note = note.to_string());
}

pub fn clear_banner() {
    write(|i| i.banner = None);
}

fn fail(title: &str, error: impl std::fmt::Display) {
    let body = error.to_string();
    write(|i| {
        i.note.clear();
        i.banner = Some(Banner {
            title: title.to_string(),
            body,
        });
    });
}

/// Reload the profiles. `reset_edits` replaces the editor's copy; `select`
/// chooses which profile the detail pane shows afterwards.
pub fn load_profiles(reset_edits: bool, select: Option<String>) {
    runtime().spawn(async move {
        match service::list_profiles(state()).await {
            Ok(profiles) => write(|i| {
                i.profiles = profiles;
                i.reset_edits |= reset_edits;
                if select.is_some() {
                    i.select = select;
                }
                i.banner = None;
            }),
            Err(e) => fail("Could not load profiles", e),
        }
    });
}

/// Refresh live status only; never touches the editor's copy.
pub fn refresh_status() {
    runtime().spawn(async {
        if let Ok(profiles) = service::list_profiles(state()).await {
            write(|i| i.profiles = profiles);
        }
    });
}

pub fn load_logs(profile: Option<String>, severity: Option<String>) {
    runtime().spawn(async move {
        match service::read_logs(state(), profile, severity).await {
            Ok(view) => write(|i| i.logs = Some(view)),
            Err(e) => fail("Could not read logs", e),
        }
    });
}

pub fn reveal_log() {
    runtime().spawn(async {
        if let Err(e) = service::reveal_log_file(state()).await {
            fail("Could not show the log file", e);
        }
    });
}

pub fn save(profiles: Vec<crate::core::profile::Profile>) {
    set_note("Saving…");
    runtime().spawn(async move {
        match service::save_profiles(state(), profiles).await {
            Ok(()) => {
                // The editor already holds what was saved, so it is left alone:
                // replacing it would rebuild the pane under the user's cursor.
                write(|i| {
                    i.note = "Saved.".to_string();
                    i.saved = true;
                });
                refresh_status();
            }
            Err(e) => fail("Could not save profiles", e),
        }
    });
}

const NEW_PROFILE_NAME: &str = "New Environment";

pub fn add() {
    set_note("Adding…");
    runtime().spawn(async {
        match service::add_profile(state(), NEW_PROFILE_NAME.to_string()).await {
            Ok(created) => {
                write(|i| i.note = "Added.".to_string());
                load_profiles(true, Some(created.id));
            }
            Err(e) => fail("Could not add an environment", e),
        }
    });
}

pub fn import(command: String) {
    set_note("Importing…");
    runtime().spawn(async move {
        match service::import_profile(state(), NEW_PROFILE_NAME.to_string(), command).await {
            Ok(created) => {
                write(|i| {
                    i.note = "Imported.".to_string();
                    i.ignored_flags = created.ignored_flags;
                });
                load_profiles(true, Some(created.profile.id));
            }
            Err(e) => fail("Could not import that command", e),
        }
    });
}

pub fn clear_ignored_flags() {
    write(|i| i.ignored_flags.clear());
}

/// Delete after a native confirmation that spells out the consequences.
pub fn delete(view: ProfileView) {
    runtime().spawn(async move {
        let label = if view.profile.name.is_empty() {
            view.profile.id.clone()
        } else {
            view.profile.name.clone()
        };
        let mut consequences = Vec::new();
        if view.status == "running" || view.status == "starting" {
            consequences.push(
                "Its proxy is running and will be stopped first, which will disconnect \
                 anything using its ports."
                    .to_string(),
            );
        }
        if view.profile.danger {
            consequences.push("This environment is marked as production.".to_string());
        }
        consequences.push("This cannot be undone.".to_string());

        let ok = dialogs::confirm(
            format!("Delete the environment “{label}”?"),
            consequences.join("\n\n"),
            "Delete",
            Kind::Warning,
        )
        .await;
        if !ok {
            return;
        }
        set_note("Deleting…");
        match service::delete_profile(state(), view.profile.id.clone()).await {
            Ok(()) => {
                write(|i| i.note = "Deleted.".to_string());
                load_profiles(true, None);
            }
            Err(e) => fail("Could not delete the environment", e),
        }
    });
}

/// Run `then` now, or after the user agrees to discard unsaved edits when
/// `dirty`. Every action that replaces the editor's copy goes through this.
pub fn guarded(dirty: bool, action: &'static str, then: impl FnOnce() + Send + 'static) {
    runtime().spawn(async move {
        if !dirty || confirm_discard(action).await {
            then();
        }
    });
}

/// Ask before throwing away unsaved edits. True to proceed.
pub async fn confirm_discard(action: &str) -> bool {
    dialogs::confirm(
        format!("{action} will discard your unsaved changes. Continue?"),
        "Your unsaved changes will be lost.".to_string(),
        "Discard",
        Kind::Warning,
    )
    .await
}

pub fn copy(text: String) {
    runtime().spawn(async move {
        use tokio::io::AsyncWriteExt;
        let child = tokio::process::Command::new("/usr/bin/pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn();
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes()).await;
            }
            let _ = child.wait().await;
        } else {
            state()
                .audit
                .warn(Category::Action, None, "could not run pbcopy");
        }
    });
}

/// Show an error banner for a problem found in the view itself.
pub fn fail_input(title: &str, body: &str) {
    fail(title, body);
}
