//! The Tauri IPC surface. Both the tray menu (Task 10) and the webview
//! profile editor (Task 11) call these, which is what keeps their behaviour
//! from drifting apart.
//!
//! Every command returns `Result<_, String>`: Tauri serializes the `Err` arm
//! straight to the frontend, and the core's error types already render
//! user-facing text via `thiserror`, so `to_string()` is the whole mapping.
//!
//! # Confirmation is the caller's job
//!
//! [`start_profile`] deliberately does **not** prompt. A command cannot show a
//! modal — it runs on a worker thread with no window handle, and blocking IPC
//! on a dialog would wedge the thread pool. So the split is:
//!
//! - the caller (tray or webview) asks [`plan_for`] what starting a profile
//!   entails — whether it is dangerous, and which profiles would be stopped —
//!   and shows whatever confirmation it wants;
//! - [`start_profile`] then executes that plan unconditionally.
//!
//! `core::state::requires_confirmation` exists for exactly the first half, and
//! [`plan_for`] surfaces it so both callers ask the same question.
//!
//! # Locking
//!
//! See the lock-ordering contract on [`crate::app_state::Shared`]: config
//! before manager, never the reverse.

use tauri::State;

use fh_cloud_sql_proxy_gui::core::profile::{
    InstanceRole, Profile, ProfileConfig, CURRENT_SCHEMA_VERSION,
};
use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;
use fh_cloud_sql_proxy_gui::core::{discovery, preflight, state, store};

use crate::app_state::SharedState;

/// A profile plus its live status, as the UI renders it.
///
/// `profile` is flattened, so the JSON is the profile's own camelCase shape
/// with `status`/`detail`/`fixCommand` alongside it rather than nested.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    #[serde(flatten)]
    pub profile: Profile,
    /// "stopped" | "starting" | "running" | "failed"
    pub status: String,
    /// The `Diagnosis` message when `status == "failed"`.
    pub detail: Option<String>,
    /// The `Diagnosis` fix command when one exists, e.g.
    /// `gcloud auth application-default login`, so the UI can offer a copy
    /// button instead of making the user retype it.
    pub fix_command: Option<String>,
}

/// What starting a profile would entail, for the caller to confirm.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPlanView {
    /// Profiles that share a port and would be stopped first.
    pub stop_first: Vec<String>,
    /// True when the profile is marked `danger` (production), which warrants
    /// its own confirmation regardless of `stop_first`.
    pub requires_confirmation: bool,
}

/// The proposed connection-name changes from a `gcloud` refresh. Nothing has
/// been written when this is returned.
#[derive(serde::Serialize)]
pub struct RefreshResult {
    pub changes: Vec<ChangeView>,
}

/// One proposed connection-name change, round-tripped through the UI so the
/// user can confirm before [`apply_changes`] writes it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeView {
    pub profile_id: String,
    /// "primary" | "replica"
    pub role: String,
    pub from: String,
    pub to: String,
}

fn role_str(role: InstanceRole) -> &'static str {
    match role {
        InstanceRole::Primary => "primary",
        InstanceRole::Replica => "replica",
    }
}

fn role_from_str(role: &str) -> Option<InstanceRole> {
    match role {
        "primary" => Some(InstanceRole::Primary),
        "replica" => Some(InstanceRole::Replica),
        _ => None,
    }
}

/// Flatten a status into the three UI-facing fields.
fn status_fields(status: &ProxyStatus) -> (String, Option<String>, Option<String>) {
    match status {
        ProxyStatus::Stopped => ("stopped".to_string(), None, None),
        ProxyStatus::Starting => ("starting".to_string(), None, None),
        ProxyStatus::Running => ("running".to_string(), None, None),
        ProxyStatus::Failed(diagnosis) => (
            "failed".to_string(),
            Some(diagnosis.message.clone()),
            diagnosis.fix_command.clone(),
        ),
    }
}

/// Every profile with its live status.
#[tauri::command]
pub async fn list_profiles(state: State<'_, SharedState>) -> Result<Vec<ProfileView>, String> {
    // config -> manager, per the lock order.
    let config = state.config.lock().await;
    let manager = state.manager.lock().await;

    let mut views = Vec::with_capacity(config.profiles.len());
    for profile in &config.profiles {
        let (status, detail, fix_command) = status_fields(&manager.status_of(&profile.id).await);
        views.push(ProfileView {
            profile: profile.clone(),
            status,
            detail,
            fix_command,
        });
    }
    Ok(views)
}

/// What starting `id` would entail. Callers use this to decide whether to
/// confirm before calling [`start_profile`]; it changes nothing.
#[tauri::command]
pub async fn plan_for(state: State<'_, SharedState>, id: String) -> Result<StartPlanView, String> {
    let config = state.config.lock().await;
    let profile = find_profile(&config, &id)?.clone();

    let running = {
        let mut manager = state.manager.lock().await;
        manager.running_ids()
    };

    let stop_first = match state::plan_start(&config, &profile, &running) {
        state::StartPlan::Start => Vec::new(),
        state::StartPlan::StopThenStart(ids) => ids,
    };

    Ok(StartPlanView {
        stop_first,
        requires_confirmation: state::requires_confirmation(&profile),
    })
}

/// Validate and persist `profiles`, replacing the whole config.
///
/// The config guard is held across validate + write + in-memory update so two
/// concurrent saves cannot interleave — which is exactly what `store::save`'s
/// docs require of callers.
#[tauri::command]
pub async fn save_profiles(
    state: State<'_, SharedState>,
    profiles: Vec<Profile>,
) -> Result<(), String> {
    let mut config = state.config.lock().await;

    let next = ProfileConfig {
        version: CURRENT_SCHEMA_VERSION,
        profiles,
    };
    next.validate().map_err(|e| e.to_string())?;
    store::save(&state.config_path, &next).map_err(|e| e.to_string())?;
    *config = next;
    Ok(())
}

/// Start `id`: stop any port-conflicting profiles, run preflight, spawn.
///
/// Confirmation (danger / stop-then-start) is the caller's responsibility —
/// see the module docs and [`plan_for`].
#[tauri::command]
pub async fn start_profile(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    // config -> manager. The config guard is held throughout so the profile
    // cannot be edited out from under the start.
    let config = state.config.lock().await;
    let profile = find_profile(&config, &id)?.clone();

    let mut manager = state.manager.lock().await;

    // Exclusive by default: anything sharing a port has to go first.
    let stopped_any = match state::plan_start(&config, &profile, &manager.running_ids()) {
        state::StartPlan::Start => false,
        state::StartPlan::StopThenStart(conflicts) => {
            for conflict in &conflicts {
                manager.stop(conflict).await;
            }
            !conflicts.is_empty()
        }
    };

    let adc = preflight::adc_path();
    let mut check = preflight::check(&profile, adc.as_deref());

    // The kernel does not always release a just-killed child's listener by the
    // time the next `bind` runs, so a profile we ourselves just stopped can
    // still look like "port in use". Poll only in that case: when we stopped
    // nothing, a busy port belongs to some other process and waiting a second
    // to say so — while holding both locks — helps nobody.
    if stopped_any {
        for _ in 0..PORT_RELEASE_ATTEMPTS {
            if !is_port_block(&check) {
                break;
            }
            tokio::time::sleep(PORT_RELEASE_DELAY).await;
            check = preflight::check(&profile, adc.as_deref());
        }
    }

    if let preflight::Preflight::Blocked(diagnosis) = check {
        return Err(diagnosis.message);
    }

    manager.start(&profile).await.map_err(|e| e.to_string())
}

/// How many times to re-run preflight while a just-stopped child's port is
/// still held by the kernel, and how long to wait between attempts.
const PORT_RELEASE_ATTEMPTS: u32 = 10;
const PORT_RELEASE_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

fn is_port_block(check: &preflight::Preflight) -> bool {
    matches!(
        check,
        preflight::Preflight::Blocked(d)
            if d.kind == fh_cloud_sql_proxy_gui::core::log_watcher::FailureKind::PortInUse
    )
}

/// Stop `id`. Idempotent: stopping something that is not running just
/// normalises its status to stopped.
#[tauri::command]
pub async fn stop_profile(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    let mut manager = state.manager.lock().await;
    manager.stop(&id).await;
    Ok(())
}

/// Ask `gcloud` for each project's Cloud SQL instances and return the proposed
/// connection-name changes. **Writes nothing** — [`apply_changes`] does, once
/// the user has confirmed.
#[tauri::command]
pub async fn refresh_connection_names(
    state: State<'_, SharedState>,
) -> Result<RefreshResult, String> {
    // Clone the profiles out and release the lock: the gcloud calls are slow
    // network round-trips and holding the config lock across them would block
    // every other command for seconds.
    let profiles: Vec<Profile> = state.config.lock().await.profiles.clone();

    let mut changes = Vec::new();
    for profile in &profiles {
        let discovered = discover(profile).await?;
        for change in discovery::reconcile(profile, &discovered) {
            changes.push(ChangeView {
                profile_id: change.profile_id,
                role: role_str(change.role).to_string(),
                from: change.from,
                to: change.to,
            });
        }
    }

    Ok(RefreshResult { changes })
}

/// Run `gcloud sql instances list` for one profile's project and parse it.
async fn discover(profile: &Profile) -> Result<Vec<discovery::DiscoveredInstance>, String> {
    let output = tokio::process::Command::new("gcloud")
        .args(discovery::gcloud_args(&profile.project))
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "gcloud was not found on your PATH — install the Google Cloud CLI \
                 (https://cloud.google.com/sdk/docs/install) and try again."
                    .to_string()
            } else {
                format!("could not run gcloud: {e}")
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "gcloud failed for project '{}': {}",
            profile.project,
            if stderr.is_empty() {
                "no error output".to_string()
            } else {
                stderr
            }
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    discovery::parse_instances(&stdout).map_err(|e| format!("project '{}': {e}", profile.project))
}

/// Apply confirmed connection-name changes and persist.
///
/// Unrecognised `role` strings are ignored rather than erroring: the list
/// round-trips through the frontend, and one bad entry should not block the
/// rest. The config guard is held across apply + write + update for the same
/// reason as [`save_profiles`].
#[tauri::command]
pub async fn apply_changes(
    state: State<'_, SharedState>,
    changes: Vec<ChangeView>,
) -> Result<(), String> {
    let core_changes: Vec<discovery::Change> = changes
        .into_iter()
        .filter_map(|c| {
            Some(discovery::Change {
                profile_id: c.profile_id,
                role: role_from_str(&c.role)?,
                from: c.from,
                to: c.to,
            })
        })
        .collect();

    let mut config = state.config.lock().await;

    // Apply to a copy so a validation failure cannot leave the in-memory
    // config half-updated relative to what is on disk.
    let mut next = config.clone();
    for profile in &mut next.profiles {
        discovery::apply(profile, &core_changes);
    }

    store::save(&state.config_path, &next).map_err(|e| e.to_string())?;
    *config = next;
    Ok(())
}

/// Retained log lines, newest last, formatted `"[{profile_id}] {text}"`.
/// Filtered to one profile when `id` is given.
#[tauri::command]
pub async fn read_logs(
    state: State<'_, SharedState>,
    id: Option<String>,
) -> Result<Vec<String>, String> {
    // Take the handle, then drop the manager guard before locking the log
    // buffer: the reader tasks hold the log lock, so holding the manager lock
    // while waiting on it would extend contention for no reason.
    let logs = {
        let manager = state.manager.lock().await;
        manager.logs_handle()
    };

    let buffer = logs.lock().await;
    Ok(buffer
        .iter()
        .filter(|line| match id.as_deref() {
            Some(want) => line.profile_id == want,
            None => true,
        })
        .map(|line| format!("[{}] {}", line.profile_id, line.text))
        .collect())
}

/// Look a profile up by id, with an error the UI can show verbatim.
fn find_profile<'a>(config: &'a ProfileConfig, id: &str) -> Result<&'a Profile, String> {
    config
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("no profile with id '{id}'"))
}
