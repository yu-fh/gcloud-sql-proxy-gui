//! Start-decision logic: given the configured profiles and which profile ids
//! are currently running, decide whether a profile can start immediately or
//! must stop conflicting profiles first.
//!
//! This module is a pure decision function -- no processes, no I/O -- so it
//! is unit-testable standalone. Task 9's commands and Task 10's tray menu
//! both call into [`plan_start`] and [`requires_confirmation`], which is what
//! keeps their behavior from drifting apart.

use crate::core::profile::{Profile, ProfileConfig};

/// What must happen before a profile can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartPlan {
    /// Nothing is in the way.
    Start,
    /// These running profiles share a port and must stop first.
    /// The UI confirms before proceeding.
    StopThenStart(Vec<String>),
}

/// Decide how to start `target` given the profile ids currently running.
///
/// The app is exclusive-by-default: because dev/stg/prd conventionally all
/// reuse ports 15432/15433, starting one normally requires stopping any
/// other running profile that shares a port. A profile given non-default
/// ports (e.g. prd on 25432/25433) does not overlap and so runs concurrently
/// with no prompt -- this is the opt-in escape hatch from the shared-port
/// default.
///
/// Rules:
/// - A running id equal to `target.id` is not a conflict (restarting itself).
/// - A running id not found in `config.profiles` is a stale id: its ports
///   can't be compared, so it can't be shown to conflict.
/// - Otherwise, any running profile whose ports overlap `target`'s must stop
///   first.
///
/// The returned ids preserve the order of `running_ids` (not sorted) and are
/// deduplicated, so a repeated id cannot produce `StopThenStart(["dev",
/// "dev"])` — which would render as "Stop dev, dev" and issue two stop calls
/// for one process. Deduplicating last keeps first-occurrence ordering.
pub fn plan_start(config: &ProfileConfig, target: &Profile, running_ids: &[String]) -> StartPlan {
    let mut seen = std::collections::HashSet::new();
    let conflicts: Vec<String> = running_ids
        .iter()
        .filter(|id| id.as_str() != target.id.as_str())
        .filter_map(|id| config.profiles.iter().find(|p| &p.id == id))
        .filter(|running| ProfileConfig::ports_overlap(running, target))
        .map(|running| running.id.clone())
        .filter(|id| seen.insert(id.clone()))
        .collect();

    if conflicts.is_empty() {
        StartPlan::Start
    } else {
        StartPlan::StopThenStart(conflicts)
    }
}

/// True when starting `target` should ask for confirmation first.
///
/// Production is dangerous enough that starting it should require an
/// explicit, separate confirmation on its own merits: it should never be one
/// unguarded click away, regardless of whether starting it also happens to
/// require stopping another profile.
pub fn requires_confirmation(target: &Profile) -> bool {
    target.danger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags, CURRENT_SCHEMA_VERSION};

    fn instance(role: InstanceRole, connection_name: &str, port: u16) -> Instance {
        Instance {
            role,
            connection_name: connection_name.to_string(),
            port,
        }
    }

    fn profile_with_ports(id: &str, primary_port: u16, replica_port: u16, danger: bool) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            project: format!("fh-{id}-project"),
            region: "us-central1".to_string(),
            instances: vec![
                instance(InstanceRole::Primary, &format!("proj:us-central1:{id}-primary"), primary_port),
                instance(InstanceRole::Replica, &format!("proj:us-central1:{id}-replica"), replica_port),
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger,
            vpn_probe_host: None,
        }
    }

    fn standard_profile(id: &str) -> Profile {
        profile_with_ports(id, 15432, 15433, false)
    }

    /// dev/stg/prd -- dev and stg share the real default ports, prd is given
    /// non-default ports and is marked dangerous.
    fn standard_config() -> ProfileConfig {
        ProfileConfig {
            version: CURRENT_SCHEMA_VERSION,
            profiles: vec![
                standard_profile("dev"),
                standard_profile("stg"),
                profile_with_ports("prd", 25432, 25433, true),
            ],
        }
    }

    fn ids(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_running_starts_cleanly() {
        let cfg = standard_config();
        let dev = cfg.profiles[0].clone();
        assert_eq!(plan_start(&cfg, &dev, &[]), StartPlan::Start);
    }

    #[test]
    fn starting_stg_while_dev_runs_requires_stopping_dev() {
        let cfg = standard_config();
        let stg = cfg.profiles[1].clone();
        let running = ids(&["dev"]);
        assert_eq!(
            plan_start(&cfg, &stg, &running),
            StartPlan::StopThenStart(vec!["dev".to_string()])
        );
    }

    #[test]
    fn starting_prd_with_non_default_ports_while_dev_runs_does_not_conflict() {
        let cfg = standard_config();
        let prd = cfg.profiles[2].clone();
        let running = ids(&["dev"]);
        assert_eq!(plan_start(&cfg, &prd, &running), StartPlan::Start);
    }

    #[test]
    fn targets_own_id_in_running_ids_is_ignored() {
        let cfg = standard_config();
        let dev = cfg.profiles[0].clone();
        let running = ids(&["dev"]);
        assert_eq!(plan_start(&cfg, &dev, &running), StartPlan::Start);
    }

    #[test]
    fn multiple_conflicting_profiles_are_all_listed() {
        // A prd variant that also uses the standard ports, so starting it
        // while dev and stg both run must list both.
        let mut cfg = standard_config();
        cfg.profiles[2] = standard_profile("prd");
        let prd = cfg.profiles[2].clone();
        let running = ids(&["dev", "stg"]);
        assert_eq!(
            plan_start(&cfg, &prd, &running),
            StartPlan::StopThenStart(vec!["dev".to_string(), "stg".to_string()])
        );
    }

    #[test]
    fn stale_running_id_not_in_config_is_ignored() {
        let cfg = standard_config();
        let dev = cfg.profiles[0].clone();
        let running = ids(&["ghost"]);
        assert_eq!(plan_start(&cfg, &dev, &running), StartPlan::Start);
    }

    #[test]
    fn returned_ids_follow_running_ids_order_not_sorted() {
        // A prd variant sharing the standard ports so both stg and dev
        // conflict; running_ids lists stg before dev, so the result must
        // preserve that order rather than sorting alphabetically.
        let mut cfg = standard_config();
        cfg.profiles[2] = standard_profile("prd");
        let prd = cfg.profiles[2].clone();
        let running = ids(&["stg", "dev"]);
        assert_eq!(
            plan_start(&cfg, &prd, &running),
            StartPlan::StopThenStart(vec!["stg".to_string(), "dev".to_string()])
        );
    }

    #[test]
    fn repeated_running_id_is_listed_once() {
        // Without dedupe this returns ["dev", "dev"], which would render as
        // "Stop dev, dev" and issue two stop calls for one process.
        let cfg = standard_config();
        let stg = cfg.profiles[1].clone();
        let running = ids(&["dev", "dev"]);
        assert_eq!(
            plan_start(&cfg, &stg, &running),
            StartPlan::StopThenStart(vec!["dev".to_string()])
        );
    }

    #[test]
    fn dedupe_keeps_first_occurrence_ordering() {
        let mut cfg = standard_config();
        cfg.profiles[2] = standard_profile("prd");
        let prd = cfg.profiles[2].clone();
        let running = ids(&["stg", "dev", "stg"]);
        assert_eq!(
            plan_start(&cfg, &prd, &running),
            StartPlan::StopThenStart(vec!["stg".to_string(), "dev".to_string()])
        );
    }

    #[test]
    fn requires_confirmation_true_for_danger_profile() {
        let cfg = standard_config();
        let prd = cfg.profiles[2].clone();
        assert!(requires_confirmation(&prd));
    }

    #[test]
    fn requires_confirmation_false_for_non_danger_profile() {
        let cfg = standard_config();
        let dev = cfg.profiles[0].clone();
        assert!(!requires_confirmation(&dev));
    }

    #[test]
    fn partial_port_overlap_still_conflicts() {
        // Shares only the primary port (15432) with dev; the replica port
        // (25433) differs. A naive "all ports equal" implementation would
        // miss this.
        let cfg = standard_config();
        let partial = profile_with_ports("partial", 15432, 25433, false);
        let mut cfg_with_partial = cfg.clone();
        cfg_with_partial.profiles.push(partial.clone());

        let running = ids(&["dev"]);
        assert_eq!(
            plan_start(&cfg_with_partial, &partial, &running),
            StartPlan::StopThenStart(vec!["dev".to_string()])
        );
    }

    #[test]
    fn empty_running_ids_and_empty_config_profiles_behave_sanely() {
        let empty_cfg = ProfileConfig {
            version: CURRENT_SCHEMA_VERSION,
            profiles: vec![],
        };
        let dev = standard_profile("dev");

        // Empty running_ids against a normal config.
        let cfg = standard_config();
        assert_eq!(plan_start(&cfg, &dev, &[]), StartPlan::Start);

        // Non-empty running_ids against an empty config: nothing to compare
        // against, so no conflicts.
        assert_eq!(
            plan_start(&empty_cfg, &dev, &ids(&["dev", "stg"])),
            StartPlan::Start
        );

        // Both empty.
        assert_eq!(plan_start(&empty_cfg, &dev, &[]), StartPlan::Start);
    }
}
