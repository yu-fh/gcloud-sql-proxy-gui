//! Discovery of Cloud SQL instance connection names via `gcloud`.
//!
//! Terraform regenerates the trailing suffix of a connection name
//! (`...:terraform-<timestamp><random>`) whenever the underlying instance is
//! replaced, silently breaking any previously-working profile. This module
//! parses the output of:
//!
//! ```text
//! gcloud sql instances list --project=<project> --billing-project=<project> \
//!   --format='value(name,instanceType,connectionName)'
//! ```
//!
//! and reconciles it against a [`Profile`]'s stored instances, producing a
//! list of proposed [`Change`]s for the UI to confirm before rewriting
//! config. Running `gcloud` itself is deliberately kept out of this module
//! (see Task 9) so parsing and reconciliation are testable without the
//! network.

use crate::core::profile::{InstanceRole, Profile};

/// A single Cloud SQL instance as reported by `gcloud sql instances list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInstance {
    pub name: String,
    pub role: InstanceRole,
    pub connection_name: String,
}

/// Errors produced while discovering or parsing instance data.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("gcloud returned no instances for project '{0}'")]
    NoInstances(String),
    #[error("could not parse gcloud output: {0}")]
    Parse(String),
    #[error("gcloud failed: {0}")]
    Command(String),
}

/// A proposed change to a profile's stored connection name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub profile_id: String,
    pub role: InstanceRole,
    pub from: String,
    pub to: String,
}

/// Parse the tab-separated `value(name,instanceType,connectionName)` output
/// of `gcloud sql instances list`.
///
/// Blank/whitespace-only lines are skipped. A row with fewer than 3
/// tab-separated columns is a [`DiscoveryError::Parse`]. A row whose
/// `instanceType` is neither `CLOUD_SQL_INSTANCE` nor `READ_REPLICA_INSTANCE`
/// is silently skipped, not an error -- Cloud SQL may report instance types
/// this app doesn't care about.
pub fn parse_instances(output: &str) -> Result<Vec<DiscoveredInstance>, DiscoveryError> {
    let mut result = Vec::new();

    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let columns: Vec<&str> = line.split('\t').collect();
        if columns.len() < 3 {
            return Err(DiscoveryError::Parse(format!(
                "expected 3 tab-separated columns, got {}: {line:?}",
                columns.len()
            )));
        }

        let name = columns[0];
        let instance_type = columns[1];
        let connection_name = columns[2];

        let role = match instance_type {
            "CLOUD_SQL_INSTANCE" => InstanceRole::Primary,
            "READ_REPLICA_INSTANCE" => InstanceRole::Replica,
            _ => continue,
        };

        result.push(DiscoveredInstance {
            name: name.to_string(),
            role,
            connection_name: connection_name.to_string(),
        });
    }

    Ok(result)
}

/// Compare a profile's stored instances against freshly discovered ones,
/// returning only the differences. Instances whose role has no matching
/// discovered instance are skipped (not an error) so partial discovery
/// results don't block reconciliation of the roles that were found.
///
/// When discovery returns several instances with the same role — Cloud SQL
/// permits more than one read replica per project — the first one gcloud
/// listed wins, silently. Note the asymmetry with stored profiles, where
/// `ProfileConfig::validate` rejects a duplicate role outright
/// (`ValidationError::DuplicateRole`) precisely because role matching would be
/// ambiguous. If multi-replica projects become real here, surface both
/// candidates so the confirmation step can show the choice instead of
/// resolving it invisibly.
pub fn reconcile(profile: &Profile, discovered: &[DiscoveredInstance]) -> Vec<Change> {
    let mut changes = Vec::new();

    for instance in &profile.instances {
        let Some(found) = discovered.iter().find(|d| d.role == instance.role) else {
            continue;
        };

        if found.connection_name != instance.connection_name {
            changes.push(Change {
                profile_id: profile.id.clone(),
                role: instance.role,
                from: instance.connection_name.clone(),
                to: found.connection_name.clone(),
            });
        }
    }

    changes
}

/// Apply confirmed changes to a profile in place. Changes whose
/// `profile_id` does not match this profile's id are ignored -- callers
/// pass a flat list of changes covering all profiles.
pub fn apply(profile: &mut Profile, changes: &[Change]) {
    for change in changes {
        if change.profile_id != profile.id {
            continue;
        }

        for instance in &mut profile.instances {
            if instance.role == change.role {
                instance.connection_name = change.to.clone();
            }
        }
    }
}

/// Build the `gcloud` argument list for listing a project's Cloud SQL
/// instances in the tab-separated format [`parse_instances`] expects.
pub fn gcloud_args(project: &str) -> Vec<String> {
    vec![
        "sql".to_string(),
        "instances".to_string(),
        "list".to_string(),
        format!("--project={project}"),
        format!("--billing-project={project}"),
        "--format=value(name,instanceType,connectionName)".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, ProxyFlags};

    const FIXTURE: &str = include_str!("../../tests/fixtures/gcloud_instances_list.txt");

    fn discovered(role: InstanceRole, connection_name: &str) -> DiscoveredInstance {
        DiscoveredInstance {
            name: "irrelevant".to_string(),
            role,
            connection_name: connection_name.to_string(),
        }
    }

    fn instance(role: InstanceRole, connection_name: &str, port: u16) -> Instance {
        Instance {
            role,
            connection_name: connection_name.to_string(),
            port,
        }
    }

    fn profile_with(id: &str, instances: Vec<Instance>) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            project: format!("fh-{id}-project"),
            region: "us-central1".to_string(),
            instances,
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
            vpn_probe_host: None,
        }
    }

    // --- parse_instances ---

    #[test]
    fn parses_both_rows_from_fixture() {
        let result = parse_instances(FIXTURE).expect("should parse");
        assert_eq!(result.len(), 2);

        assert_eq!(result[0].role, InstanceRole::Primary);
        assert!(result[0]
            .connection_name
            .ends_with("primary-instance"));

        assert_eq!(result[1].role, InstanceRole::Replica);
        assert!(result[1]
            .connection_name
            .ends_with("replica-instance"));
    }

    #[test]
    fn unknown_instance_type_is_skipped_not_an_error() {
        let output = "some-name\tSOMETHING_NEW\tproj:region:some-name";
        let result = parse_instances(output).expect("should not error");
        assert!(result.is_empty());
    }

    #[test]
    fn row_with_too_few_columns_is_a_parse_error() {
        let output = "some-name\tCLOUD_SQL_INSTANCE";
        let result = parse_instances(output);
        assert!(matches!(result, Err(DiscoveryError::Parse(_))));
    }

    #[test]
    fn empty_output_parses_to_empty_list() {
        assert_eq!(parse_instances(""), Ok(Vec::new()));
    }

    #[test]
    fn whitespace_only_output_parses_to_empty_list() {
        assert_eq!(parse_instances("   \n\t\n   "), Ok(Vec::new()));
    }

    // --- reconcile ---

    #[test]
    fn reconcile_reports_no_changes_when_names_already_match() {
        let profile = profile_with(
            "dev",
            vec![
                instance(InstanceRole::Primary, "proj:region:primary-a", 15432),
                instance(InstanceRole::Replica, "proj:region:replica-a", 15433),
            ],
        );
        let discovered = vec![
            discovered(InstanceRole::Primary, "proj:region:primary-a"),
            discovered(InstanceRole::Replica, "proj:region:replica-a"),
        ];

        assert_eq!(reconcile(&profile, &discovered), Vec::new());
    }

    #[test]
    fn reconcile_detects_terraform_drift() {
        let profile = profile_with(
            "dev",
            vec![
                instance(InstanceRole::Primary, "stale-primary", 15432),
                instance(InstanceRole::Replica, "stale-replica", 15433),
            ],
        );
        let discovered = vec![
            discovered(InstanceRole::Primary, "fresh-primary"),
            discovered(InstanceRole::Replica, "fresh-replica"),
        ];

        let changes = reconcile(&profile, &discovered);
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes,
            vec![
                Change {
                    profile_id: "dev".to_string(),
                    role: InstanceRole::Primary,
                    from: "stale-primary".to_string(),
                    to: "fresh-primary".to_string(),
                },
                Change {
                    profile_id: "dev".to_string(),
                    role: InstanceRole::Replica,
                    from: "stale-replica".to_string(),
                    to: "fresh-replica".to_string(),
                },
            ]
        );
    }

    #[test]
    fn reconcile_fills_empty_connection_names() {
        // The seeded-profile case: a freshly created profile has no
        // connection name yet.
        let profile = profile_with(
            "dev",
            vec![instance(InstanceRole::Primary, "", 15432)],
        );
        let discovered = vec![discovered(InstanceRole::Primary, "proj:region:primary-a")];

        let changes = reconcile(&profile, &discovered);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].from, "");
        assert_eq!(changes[0].to, "proj:region:primary-a");
    }

    #[test]
    fn reconcile_skips_role_that_discovery_did_not_return() {
        let profile = profile_with(
            "dev",
            vec![
                instance(InstanceRole::Primary, "stale-primary", 15432),
                instance(InstanceRole::Replica, "stale-replica", 15433),
            ],
        );
        // Discovery only found a primary this time.
        let discovered = vec![discovered(InstanceRole::Primary, "fresh-primary")];

        let changes = reconcile(&profile, &discovered);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].role, InstanceRole::Primary);
    }

    // --- apply ---

    #[test]
    fn apply_writes_changes_and_is_idempotent() {
        let mut profile = profile_with(
            "dev",
            vec![
                instance(InstanceRole::Primary, "stale-primary", 15432),
                instance(InstanceRole::Replica, "stale-replica", 15433),
            ],
        );
        let discovered = vec![
            discovered(InstanceRole::Primary, "fresh-primary"),
            discovered(InstanceRole::Replica, "fresh-replica"),
        ];

        let changes = reconcile(&profile, &discovered);
        apply(&mut profile, &changes);

        assert_eq!(profile.instances[0].connection_name, "fresh-primary");
        assert_eq!(profile.instances[1].connection_name, "fresh-replica");

        // Second reconcile against the same discovery results is now empty.
        assert_eq!(reconcile(&profile, &discovered), Vec::new());
    }

    #[test]
    fn apply_ignores_changes_for_a_different_profile_id() {
        let mut profile = profile_with(
            "dev",
            vec![instance(InstanceRole::Primary, "stale-primary", 15432)],
        );
        let changes = vec![Change {
            profile_id: "stg".to_string(),
            role: InstanceRole::Primary,
            from: "stale-primary".to_string(),
            to: "fresh-primary".to_string(),
        }];

        apply(&mut profile, &changes);

        assert_eq!(profile.instances[0].connection_name, "stale-primary");
    }

    // --- gcloud_args ---

    #[test]
    fn gcloud_args_includes_project_and_format_flags() {
        let args = gcloud_args("my-project-dev");

        assert!(args
            .iter()
            .any(|a| a == "--project=my-project-dev"));
        assert!(args
            .iter()
            .any(|a| a == "--billing-project=my-project-dev"));
        assert!(args.iter().any(|a| a.starts_with("--format=value(")));
    }
}
