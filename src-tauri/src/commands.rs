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

use fh_cloud_sql_proxy_gui::core::audit::{Category, Record, Severity};
use fh_cloud_sql_proxy_gui::core::profile::{Profile, ProfileConfig, CURRENT_SCHEMA_VERSION};
use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;
use fh_cloud_sql_proxy_gui::core::{audit, preflight, state, store};

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

    // Diff before validating, so a rejected save still records what was
    // attempted -- "I typed a bad port and it would not save" is exactly the
    // kind of thing the trail should be able to answer.
    let changes = describe_changes(&config.profiles, &next.profiles);

    if let Err(error) = next.validate() {
        state.audit.error(
            Category::Action,
            None,
            format!("save rejected: {error}; attempted changes: {changes}"),
        );
        return Err(error.to_string());
    }
    if let Err(error) = store::save(&state.config_path, &next) {
        state.audit.error(
            Category::Action,
            None,
            format!("save failed to write: {error}"),
        );
        return Err(error.to_string());
    }

    state
        .audit
        .info(Category::Action, None, format!("saved profiles: {changes}"));

    *config = next;
    Ok(())
}

/// Describe what a save changed, field by field, for the audit trail.
///
/// A per-field diff rather than "saved 3 profiles": the question the log has to
/// answer is "when did this port change, and to what", and a count cannot. Full
/// values on both sides, no redaction -- these are the connection names and
/// project ids the user explicitly asked to see.
///
/// Returns `"no changes"` when the save was a no-op, which happens whenever the
/// user presses Done without editing anything.
fn describe_changes(before: &[Profile], after: &[Profile]) -> String {
    let mut changes: Vec<String> = Vec::new();

    for old in before {
        if !after.iter().any(|p| p.id == old.id) {
            changes.push(format!("removed '{}'", old.id));
        }
    }

    for new in after {
        let Some(old) = before.iter().find(|p| p.id == new.id) else {
            changes.push(format!("added '{}' (name '{}')", new.id, new.name));
            continue;
        };
        if old == new {
            continue;
        }

        let mut fields: Vec<String> = Vec::new();
        if old.name != new.name {
            fields.push(format!("name '{}' -> '{}'", old.name, new.name));
        }
        if old.project != new.project {
            fields.push(format!("project '{}' -> '{}'", old.project, new.project));
        }
        if old.region != new.region {
            fields.push(format!("region '{}' -> '{}'", old.region, new.region));
        }
        if old.danger != new.danger {
            fields.push(format!("danger {} -> {}", old.danger, new.danger));
        }
        if old.flags != new.flags {
            fields.push(format!(
                "flags autoIamAuthn {} -> {}, privateIp {} -> {}",
                old.flags.auto_iam_authn,
                new.flags.auto_iam_authn,
                old.flags.private_ip,
                new.flags.private_ip
            ));
        }
        if old.impersonate_service_account != new.impersonate_service_account {
            fields.push(format!(
                "impersonateServiceAccount {:?} -> {:?}",
                old.impersonate_service_account, new.impersonate_service_account
            ));
        }
        if old.vpn_probe_host != new.vpn_probe_host {
            fields.push(format!(
                "vpnProbeHost {:?} -> {:?}",
                old.vpn_probe_host, new.vpn_probe_host
            ));
        }

        // Instances are compared positionally, which is how the editor presents
        // them: the form has a fixed row per instance, so index is stable and a
        // set-difference would report a changed port as a remove plus an add.
        if old.instances.len() != new.instances.len() {
            fields.push(format!(
                "instance count {} -> {}",
                old.instances.len(),
                new.instances.len()
            ));
        }
        for (index, new_instance) in new.instances.iter().enumerate() {
            let Some(old_instance) = old.instances.get(index) else {
                fields.push(format!(
                    "instance {index} added ({} port {})",
                    new_instance.connection_name, new_instance.port
                ));
                continue;
            };
            if old_instance.connection_name != new_instance.connection_name {
                fields.push(format!(
                    "instance {index} connectionName '{}' -> '{}'",
                    old_instance.connection_name, new_instance.connection_name
                ));
            }
            if old_instance.port != new_instance.port {
                fields.push(format!(
                    "instance {index} port {} -> {}",
                    old_instance.port, new_instance.port
                ));
            }
            if old_instance.role != new_instance.role {
                fields.push(format!(
                    "instance {index} role {:?} -> {:?}",
                    old_instance.role, new_instance.role
                ));
            }
        }

        if fields.is_empty() {
            // Equality already said they differ, so something changed that
            // nothing above names. Say so rather than reporting "no changes".
            fields.push("changed".to_string());
        }
        changes.push(format!("'{}' {}", new.id, fields.join(", ")));
    }

    if changes.is_empty() {
        return "no changes".to_string();
    }
    changes.join("; ")
}

/// Create a new profile named `name` and persist it.
///
/// The id is derived from the name and disambiguated against the ids already
/// in the config, so two profiles can share a display name without colliding.
/// The profile starts blank — empty project, empty connection names, the
/// conventional ports — and the caller edits it from there.
///
/// The config guard is held across generate + validate + write + update, so a
/// concurrent add cannot pick the same id.
#[tauri::command]
pub async fn add_profile(state: State<'_, SharedState>, name: String) -> Result<Profile, String> {
    let mut config = state.config.lock().await;

    let taken: Vec<String> = config.profiles.iter().map(|p| p.id.clone()).collect();
    let profile = store::new_profile(&name, &taken);

    let mut next = config.clone();
    next.profiles.push(profile.clone());
    next.validate().map_err(|e| e.to_string())?;
    store::save(&state.config_path, &next).map_err(|e| e.to_string())?;
    *config = next;

    state.audit.info(
        Category::Action,
        Some(&profile.id),
        format!("added profile '{}' (name '{}')", profile.id, profile.name),
    );

    Ok(profile)
}

/// Delete the profile `id` and persist.
///
/// **Stops it first if it is running.** Removing a running profile from the
/// config without stopping it would strand its `cloud-sql-proxy` child: the
/// manager keys everything by id, so nothing would ever be able to name that
/// process again and it would hold its ports until the app quits.
///
/// The stop happens before the write, so a failed save leaves the profile
/// stopped-but-present rather than deleted-but-running.
#[tauri::command]
pub async fn delete_profile(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    // config -> manager, per the lock order.
    let mut config = state.config.lock().await;

    let Some(doomed) = config.profiles.iter().find(|p| p.id == id).cloned() else {
        return Err(format!("no profile with id '{id}'"));
    };

    state.audit.warn(
        Category::Action,
        Some(&id),
        format!(
            "deleting profile '{}' (name '{}', project '{}', ports {:?}, danger {})",
            doomed.id,
            doomed.name,
            doomed.project,
            doomed.ports(),
            doomed.danger
        ),
    );

    {
        // `stop` is idempotent, so this is unconditional rather than guarded
        // on a status read that could go stale between the two calls.
        let mut manager = state.manager.lock().await;
        manager.stop(&id).await;
    }

    let mut next = config.clone();
    next.profiles.retain(|p| p.id != id);
    next.validate().map_err(|e| e.to_string())?;
    store::save(&state.config_path, &next).map_err(|e| e.to_string())?;
    *config = next;

    state
        .audit
        .info(Category::Action, Some(&id), format!("deleted profile '{id}'"));

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

    state.audit.info(
        Category::Action,
        Some(&id),
        format!(
            "start requested for '{}' (project '{}', ports {:?}, danger {})",
            profile.name,
            profile.project,
            profile.ports(),
            profile.danger
        ),
    );

    let mut manager = state.manager.lock().await;

    // Exclusive by default: anything sharing a port has to go first.
    let stopped_any = match state::plan_start(&config, &profile, &manager.running_ids()) {
        state::StartPlan::Start => false,
        state::StartPlan::StopThenStart(conflicts) => {
            if !conflicts.is_empty() {
                state.audit.info(
                    Category::Event,
                    Some(&id),
                    format!(
                        "port conflict: stopping {} first",
                        conflicts.join(", ")
                    ),
                );
            }
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
        state.audit.error(
            Category::Event,
            Some(&id),
            format!(
                "preflight blocked ({:?}): {}{}",
                diagnosis.kind,
                diagnosis.message,
                match &diagnosis.fix_command {
                    Some(fix) => format!(" [fix: {fix}]"),
                    None => String::new(),
                }
            ),
        );
        return Err(diagnosis.message);
    }

    state.audit.info(
        Category::Event,
        Some(&id),
        format!(
            "preflight passed (ports {:?} free, credentials present, connection names set)",
            profile.ports()
        ),
    );

    // The spawn itself, its argv, and every status transition after it are
    // audited inside `ProxyManager`, which shares this logger.
    match manager.start(&profile).await {
        Ok(()) => Ok(()),
        Err(error) => {
            state.audit.error(
                Category::Event,
                Some(&id),
                format!("start failed: {error}"),
            );
            Err(error.to_string())
        }
    }
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
    state
        .audit
        .info(Category::Action, Some(&id), "stop requested");
    let mut manager = state.manager.lock().await;
    manager.stop(&id).await;
    Ok(())
}

/// One audit record, as the Logs view renders it.
///
/// Both `at` (epoch milliseconds) and `atDisplay` (the UTC string that is also
/// what lands in the file) are sent. The view formats the wall-clock time it
/// shows from `at` using the webview's own locale and timezone — which is the
/// only place in the app that knows either — while `atDisplay` is what the user
/// would grep for if they opened the file, so the two must be able to be
/// matched up.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRecordView {
    pub at: u64,
    pub at_display: String,
    /// "info" | "warn" | "error"
    pub severity: String,
    /// "system" | "action" | "event" | "proxy"
    pub category: String,
    /// Absent for records that belong to no particular profile — the startup
    /// system-info block, a menu rebuild, the settings window opening.
    pub profile_id: Option<String>,
    pub message: String,
}

impl From<Record> for LogRecordView {
    fn from(record: Record) -> Self {
        Self {
            at: record.at_ms,
            at_display: audit::format_utc(record.at_ms),
            severity: record.severity.as_str().to_string(),
            category: record.category.as_str().to_string(),
            profile_id: record.profile_id,
            message: record.message,
        }
    }
}

/// What the Logs view needs in one round trip: the records, and where the file
/// they are also written to lives.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsView {
    pub records: Vec<LogRecordView>,
    /// The audit log's path, for the "reveal in Finder" action and for the
    /// copyable text beside it. `None` only when there is nowhere to write.
    pub file_path: Option<String>,
    /// How many records the file could not be given. Non-zero means the
    /// in-memory view below is complete but the file is not, which is worth
    /// saying rather than hiding.
    pub write_failures: u64,
}

/// Parse a severity filter from the wire. An unrecognised value is treated as
/// no filter rather than an error: a filter that silently shows nothing would
/// look like an empty log.
fn parse_severity(name: Option<&str>) -> Option<Severity> {
    match name {
        Some("info") => Some(Severity::Info),
        Some("warn") => Some(Severity::Warn),
        Some("error") => Some(Severity::Error),
        _ => None,
    }
}

/// The audit trail, newest last, optionally filtered by profile and by minimum
/// severity.
///
/// Replaces the old `Vec<String>` of proxy output. That answered only "what did
/// `cloud-sql-proxy` print"; this carries the user actions, the preflight
/// outcomes, the spawn argv and the startup system info alongside it, which is
/// what makes it an audit trail rather than a console.
///
/// Reads only the audit logger, which takes no async lock at all — so unlike
/// the old implementation this needs neither the manager guard nor the log
/// buffer's, and cannot contend with the proxy reader tasks.
#[tauri::command]
pub async fn read_logs(
    state: State<'_, SharedState>,
    id: Option<String>,
    severity: Option<String>,
) -> Result<LogsView, String> {
    let records = state
        .audit
        .filtered(parse_severity(severity.as_deref()), id.as_deref())
        .into_iter()
        .map(LogRecordView::from)
        .collect();

    Ok(LogsView {
        records,
        file_path: state
            .audit
            .path()
            .map(|path| path.display().to_string()),
        write_failures: state.audit.write_failures(),
    })
}

/// Show the audit log file in Finder.
///
/// `open -R` rather than a plugin: revealing a path is not something
/// `tauri-plugin-dialog` does — that is `tauri-plugin-opener`'s
/// `reveal_item_in_dir` — and adding a whole plugin plus its ACL entry to run
/// one `open(1)` is a poor trade when the app already spawns processes as its
/// entire purpose. The path is this process's own, never user input, so there is
/// nothing here to escape.
///
/// The file is created on the first record, which the startup system-info block
/// guarantees has already happened, so there is normally something to reveal.
/// If there is not, Finder is opened on the directory instead of failing.
#[tauri::command]
pub async fn reveal_log_file(state: State<'_, SharedState>) -> Result<(), String> {
    let Some(path) = state.audit.path().map(|p| p.to_path_buf()) else {
        return Err("There is no log file: this session is logging to memory only.".to_string());
    };

    state
        .audit
        .info(Category::Action, None, "revealed the log file in Finder");

    let (flag, target) = if path.exists() {
        ("-R", path.clone())
    } else {
        // Nothing written yet. Opening the enclosing directory is more useful
        // than an error about a file the user was not asking about directly.
        ("", path.parent().unwrap_or(&path).to_path_buf())
    };

    let mut command = tokio::process::Command::new("/usr/bin/open");
    if !flag.is_empty() {
        command.arg(flag);
    }
    command.arg(&target);

    match command.status().await {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("Finder could not open {} ({status})", target.display())),
        Err(error) => Err(format!("Could not open Finder: {error}")),
    }
}

/// Look a profile up by id, with an error the UI can show verbatim.
fn find_profile<'a>(config: &'a ProfileConfig, id: &str) -> Result<&'a Profile, String> {
    config
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("no profile with id '{id}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fh_cloud_sql_proxy_gui::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn profile(id: &str) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            project: "fh-dev".to_string(),
            region: "us-central1".to_string(),
            instances: vec![
                Instance {
                    role: InstanceRole::Primary,
                    connection_name: "proj:us-central1:primary".to_string(),
                    port: 15432,
                },
                Instance {
                    role: InstanceRole::Replica,
                    connection_name: "proj:us-central1:replica".to_string(),
                    port: 15433,
                },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
            vpn_probe_host: None,
        }
    }

    // --- the save diff -----------------------------------------------------

    #[test]
    fn an_unchanged_save_reports_no_changes() {
        // Pressing Done without editing is the common case; it must not fill
        // the trail with empty "saved" entries that look like edits.
        let before = vec![profile("dev"), profile("prd")];
        assert_eq!(describe_changes(&before, &before.clone()), "no changes");
    }

    #[test]
    fn a_renamed_profile_reports_both_names() {
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].name = "development".to_string();
        let changes = describe_changes(&before, &after);
        assert!(changes.contains("name 'dev' -> 'development'"), "{changes}");
    }

    #[test]
    fn a_changed_port_names_the_instance_and_both_values() {
        // The question the trail exists to answer: when did this port change,
        // and to what.
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].instances[1].port = 15999;
        let changes = describe_changes(&before, &after);
        assert!(changes.contains("instance 1 port 15433 -> 15999"), "{changes}");
        // The untouched instance must not be reported.
        assert!(!changes.contains("instance 0"), "{changes}");
    }

    #[test]
    fn a_changed_connection_name_is_recorded_in_full_without_redaction() {
        // The user explicitly asked for full connection names. A diff that
        // elided them could not answer what it was pointed at.
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].instances[0].connection_name = "other:eu-west1:box".to_string();
        let changes = describe_changes(&before, &after);
        assert!(
            changes.contains("'proj:us-central1:primary' -> 'other:eu-west1:box'"),
            "{changes}"
        );
    }

    #[test]
    fn toggling_production_is_recorded() {
        let before = vec![profile("prd")];
        let mut after = before.clone();
        after[0].danger = true;
        assert!(describe_changes(&before, &after).contains("danger false -> true"));
    }

    #[test]
    fn added_and_removed_profiles_are_both_reported() {
        let before = vec![profile("dev"), profile("stg")];
        let after = vec![profile("dev"), profile("uat")];
        let changes = describe_changes(&before, &after);
        assert!(changes.contains("removed 'stg'"), "{changes}");
        assert!(changes.contains("added 'uat'"), "{changes}");
        // `dev` is untouched, so it must not appear as a change of its own.
        assert!(!changes.contains("'dev' "), "{changes}");
    }

    #[test]
    fn changing_project_flags_impersonation_and_probe_host_are_all_reported() {
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].project = "fh-other".to_string();
        after[0].flags.private_ip = false;
        after[0].impersonate_service_account = Some("sa@example.com".to_string());
        after[0].vpn_probe_host = Some("db.internal".to_string());

        let changes = describe_changes(&before, &after);
        for expected in [
            "project 'fh-dev' -> 'fh-other'",
            "privateIp true -> false",
            "impersonateServiceAccount",
            "vpnProbeHost",
        ] {
            assert!(changes.contains(expected), "missing {expected} in: {changes}");
        }
    }

    #[test]
    fn an_instance_count_change_is_reported_rather_than_panicking_on_the_index() {
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].instances.pop();
        assert!(describe_changes(&before, &after).contains("instance count 2 -> 1"));

        // And the other direction, which walks past the end of `before`.
        let mut grown = before.clone();
        grown[0].instances.push(Instance {
            role: InstanceRole::Replica,
            connection_name: "proj:us-central1:third".to_string(),
            port: 15434,
        });
        let changes = describe_changes(&before, &grown);
        assert!(changes.contains("instance count 2 -> 3"), "{changes}");
        assert!(changes.contains("instance 2 added"), "{changes}");
    }

    #[test]
    fn a_difference_nothing_names_still_reports_a_change() {
        // Guards the fallback: if a field is ever added to `Profile` without a
        // branch in `describe_changes`, the diff must not claim "no changes"
        // for a save that really did change something.
        let before = vec![profile("dev")];
        let mut after = before.clone();
        after[0].region = "eu-west1".to_string();
        let changes = describe_changes(&before, &after);
        assert_ne!(changes, "no changes");
        assert!(changes.contains("region"), "{changes}");
    }

    // --- the severity filter parameter -------------------------------------

    #[test]
    fn severity_names_from_the_wire_parse_to_the_right_level() {
        assert_eq!(parse_severity(Some("info")), Some(Severity::Info));
        assert_eq!(parse_severity(Some("warn")), Some(Severity::Warn));
        assert_eq!(parse_severity(Some("error")), Some(Severity::Error));
    }

    #[test]
    fn an_absent_or_unknown_severity_means_no_filter_rather_than_no_records() {
        // A filter that silently matched nothing would look like an empty log.
        assert_eq!(parse_severity(None), None);
        assert_eq!(parse_severity(Some("")), None);
        assert_eq!(parse_severity(Some("nonsense")), None);
        // Not case-insensitive by design: the only caller is the app's own
        // <select>, whose values are these three literals.
        assert_eq!(parse_severity(Some("ERROR")), None);
    }

    // --- the wire shape ----------------------------------------------------

    #[test]
    fn a_log_record_serializes_with_camel_case_field_names() {
        // Verified empirically elsewhere in this app: snake_case is rejected by
        // the frontend's field access, silently dropping the value.
        let view = LogRecordView::from(Record {
            at_ms: 0,
            severity: Severity::Error,
            category: fh_cloud_sql_proxy_gui::core::audit::Category::Event,
            profile_id: Some("dev".to_string()),
            message: "boom".to_string(),
        });
        let json = serde_json::to_value(&view).expect("serializes");
        assert!(json.get("atDisplay").is_some(), "{json}");
        assert!(json.get("profileId").is_some(), "{json}");
        assert!(json.get("at_display").is_none(), "{json}");
        assert!(json.get("profile_id").is_none(), "{json}");
        assert_eq!(json["severity"], "error");
        assert_eq!(json["category"], "event");
        assert_eq!(json["atDisplay"], "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_record_with_no_profile_serializes_profile_id_as_null() {
        // The view distinguishes "not about a profile" from "about a profile",
        // so the field has to be present and null rather than absent.
        let view = LogRecordView::from(Record {
            at_ms: 0,
            severity: Severity::Info,
            category: fh_cloud_sql_proxy_gui::core::audit::Category::System,
            profile_id: None,
            message: "app version: 0.1.0".to_string(),
        });
        let json = serde_json::to_value(&view).expect("serializes");
        assert!(json["profileId"].is_null(), "{json}");
    }

    #[test]
    fn the_logs_view_wrapper_serializes_camel_case_too() {
        let view = LogsView {
            records: Vec::new(),
            file_path: Some("/tmp/audit.log".to_string()),
            write_failures: 3,
        };
        let json = serde_json::to_value(&view).expect("serializes");
        assert_eq!(json["filePath"], "/tmp/audit.log");
        assert_eq!(json["writeFailures"], 3);
        assert!(json.get("file_path").is_none(), "{json}");
        assert!(json.get("write_failures").is_none(), "{json}");
    }
}
