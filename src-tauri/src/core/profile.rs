//! Profile data types: the pure-Rust core describing a named environment
//! (dev/stg/prd) and the two Cloud SQL instances (primary + read replica)
//! that its proxy process connects to.
//!
//! This module has no Tauri dependency and is unit-testable standalone.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The schema version this build of the app understands. Bump this whenever
/// the on-disk `ProfileConfig` shape changes in a way older builds can't read.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Which role a Cloud SQL instance plays within a profile.
///
/// Convention: primary always binds port 15432, replica always binds 15433.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceRole {
    Primary,
    Replica,
}

/// A single Cloud SQL instance passed to `cloud-sql-proxy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub role: InstanceRole,
    #[serde(rename = "connectionName")]
    pub connection_name: String,
    pub port: u16,
}

/// Flags forwarded to the `cloud-sql-proxy` invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyFlags {
    #[serde(rename = "autoIamAuthn")]
    pub auto_iam_authn: bool,
    #[serde(rename = "privateIp")]
    pub private_ip: bool,
}

impl Default for ProxyFlags {
    fn default() -> Self {
        Self {
            auto_iam_authn: true,
            private_ip: true,
        }
    }
}

/// A named environment profile (e.g. "dev", "stg", "prd") bundling the two
/// Cloud SQL instances (primary + replica) that a single proxy process
/// connects to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub project: String,
    pub region: String,
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub flags: ProxyFlags,
    #[serde(rename = "impersonateServiceAccount", default)]
    pub impersonate_service_account: Option<String>,
    #[serde(default)]
    pub danger: bool,
}

impl Profile {
    /// Render each instance as a `cloud-sql-proxy` positional argument, e.g.
    /// `"project:region:instance?port=15432"`.
    pub fn instance_args(&self) -> Vec<String> {
        self.instances
            .iter()
            .map(|i| format!("{}?port={}", i.connection_name, i.port))
            .collect()
    }

    /// The ports this profile's instances bind, in instance order.
    pub fn ports(&self) -> Vec<u16> {
        self.instances.iter().map(|i| i.port).collect()
    }
}

/// The on-disk collection of all configured profiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub version: u32,
    pub profiles: Vec<Profile>,
}

/// Errors produced while validating a `ProfileConfig`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    /// Two different profiles use the same port. Reserved for callers that
    /// want to treat cross-profile sharing as an error; `ProfileConfig::validate`
    /// does not use this variant, since cross-profile sharing is allowed by
    /// design (ports are exclusive-by-default, not exclusive-by-validation).
    #[error("port {port} is used by both '{first}' and '{second}'")]
    DuplicatePort {
        port: u16,
        first: String,
        second: String,
    },

    /// The same port appears twice within a single profile's instance list.
    /// Such a profile could never start, so this is always an error.
    #[error("port {port} appears twice in profile '{profile}'")]
    DuplicatePortInProfile { port: u16, profile: String },

    /// A profile has no instances configured.
    #[error("profile '{0}' has no instances")]
    NoInstances(String),

    /// Two profiles share the same id.
    #[error("duplicate profile id '{0}'")]
    DuplicateId(String),

    /// The config's schema version is not one this build understands.
    #[error("unsupported schema version {found} (expected {expected})")]
    UnsupportedVersion { found: u32, expected: u32 },
}

impl ProfileConfig {
    /// Validate structural invariants of the config:
    /// - schema version matches [`CURRENT_SCHEMA_VERSION`]
    /// - every profile has at least one instance
    /// - profile ids are unique across the config
    /// - within a single profile, no two instances share a port
    ///
    /// Sharing a port ACROSS profiles is allowed: the app is exclusive-by-default
    /// (only one profile normally runs at a time), so cross-profile port reuse
    /// is a valid, intentional default rather than a config error. Use
    /// [`ProfileConfig::conflicting_ports`] to surface that as a concurrency fact.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version != CURRENT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                found: self.version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }

        let mut seen_ids = std::collections::HashSet::new();
        for profile in &self.profiles {
            if profile.instances.is_empty() {
                return Err(ValidationError::NoInstances(profile.id.clone()));
            }

            if !seen_ids.insert(profile.id.as_str()) {
                return Err(ValidationError::DuplicateId(profile.id.clone()));
            }

            let mut ports_seen = std::collections::HashSet::new();
            for port in profile.ports() {
                if !ports_seen.insert(port) {
                    return Err(ValidationError::DuplicatePortInProfile {
                        port,
                        profile: profile.id.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    /// For every unordered pair of profiles, list each port they share as
    /// `(a.id, b.id, port)`. This is informational: it reports the
    /// concurrency consequence of the shared-port default, not a config
    /// error. Iteration order is deterministic: outer loop over profiles in
    /// order, inner loop over later profiles, ports in profile order.
    pub fn conflicting_ports(&self) -> Vec<(String, String, u16)> {
        let mut result = Vec::new();
        for (i, a) in self.profiles.iter().enumerate() {
            for b in self.profiles.iter().skip(i + 1) {
                let b_ports: std::collections::HashSet<u16> = b.ports().into_iter().collect();
                for port in a.ports() {
                    if b_ports.contains(&port) {
                        result.push((a.id.clone(), b.id.clone(), port));
                    }
                }
            }
        }
        result
    }

    /// True if profiles `a` and `b` share at least one port.
    pub fn ports_overlap(a: &Profile, b: &Profile) -> bool {
        let b_ports: std::collections::HashSet<u16> = b.ports().iter().copied().collect();
        a.ports().iter().any(|p| b_ports.contains(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(role: InstanceRole, connection_name: &str, port: u16) -> Instance {
        Instance {
            role,
            connection_name: connection_name.to_string(),
            port,
        }
    }

    fn primary(connection_name: &str, port: u16) -> Instance {
        instance(InstanceRole::Primary, connection_name, port)
    }

    fn replica(connection_name: &str, port: u16) -> Instance {
        instance(InstanceRole::Replica, connection_name, port)
    }

    /// Build a profile with the standard 15432/15433 port convention.
    fn standard_profile(id: &str) -> Profile {
        profile_with_ports(id, 15432, 15433)
    }

    fn profile_with_ports(id: &str, primary_port: u16, replica_port: u16) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            project: format!("fh-{id}-project"),
            region: "us-central1".to_string(),
            instances: vec![
                primary(&format!("proj:us-central1:{id}-primary"), primary_port),
                replica(&format!("proj:us-central1:{id}-replica"), replica_port),
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
        }
    }

    fn config(profiles: Vec<Profile>) -> ProfileConfig {
        ProfileConfig {
            version: CURRENT_SCHEMA_VERSION,
            profiles,
        }
    }

    #[test]
    fn instance_args_appends_port_query() {
        let p = standard_profile("dev");
        assert_eq!(
            p.instance_args(),
            vec![
                "proj:us-central1:dev-primary?port=15432".to_string(),
                "proj:us-central1:dev-replica?port=15433".to_string(),
            ]
        );
    }

    #[test]
    fn validate_accepts_distinct_ports_within_profile() {
        let cfg = config(vec![standard_profile("dev")]);
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn validate_allows_same_port_across_different_profiles() {
        // This is the exclusive-by-default seeded state: dev/stg/prd all
        // reuse 15432/15433. Cross-profile sharing must NOT be an error.
        let cfg = config(vec![standard_profile("dev"), standard_profile("stg")]);
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_duplicate_port_within_one_profile() {
        let mut bad = standard_profile("dev");
        // Force both instances onto the same port -- this profile could
        // never start.
        bad.instances[1].port = 15432;
        let cfg = config(vec![bad]);
        assert_eq!(
            cfg.validate(),
            Err(ValidationError::DuplicatePortInProfile {
                port: 15432,
                profile: "dev".to_string(),
            })
        );
    }

    #[test]
    fn validate_rejects_unsupported_schema_version() {
        let mut cfg = config(vec![standard_profile("dev")]);
        cfg.version = 999;
        assert_eq!(
            cfg.validate(),
            Err(ValidationError::UnsupportedVersion {
                found: 999,
                expected: CURRENT_SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn validate_rejects_profile_with_no_instances() {
        let mut empty = standard_profile("dev");
        empty.instances.clear();
        let cfg = config(vec![empty]);
        assert_eq!(cfg.validate(), Err(ValidationError::NoInstances("dev".to_string())));
    }

    #[test]
    fn validate_rejects_duplicate_profile_ids() {
        let cfg = config(vec![standard_profile("dev"), standard_profile("dev")]);
        assert_eq!(cfg.validate(), Err(ValidationError::DuplicateId("dev".to_string())));
    }

    #[test]
    fn conflicting_ports_lists_shared_ports_between_profiles() {
        let cfg = config(vec![standard_profile("dev"), standard_profile("stg")]);
        assert_eq!(
            cfg.conflicting_ports(),
            vec![
                ("dev".to_string(), "stg".to_string(), 15432),
                ("dev".to_string(), "stg".to_string(), 15433),
            ]
        );
    }

    #[test]
    fn conflicting_ports_empty_when_offsets_differ() {
        let cfg = config(vec![
            standard_profile("dev"),
            profile_with_ports("prd", 25432, 25433),
        ]);
        assert_eq!(cfg.conflicting_ports(), Vec::new());
    }

    #[test]
    fn conflicting_ports_three_profiles_sharing_ports_yields_six_entries() {
        // 3 profiles all on the standard ports -> C(3,2) = 3 pairs, each
        // sharing both ports -> 6 total entries. This is the real seeded
        // dev/stg/prd state.
        let cfg = config(vec![
            standard_profile("dev"),
            standard_profile("stg"),
            standard_profile("prd"),
        ]);
        assert_eq!(cfg.conflicting_ports().len(), 6);
    }

    #[test]
    fn ports_overlap_detects_shared_port() {
        let dev = standard_profile("dev");
        let stg = standard_profile("stg");
        assert!(ProfileConfig::ports_overlap(&dev, &stg));
    }

    #[test]
    fn ports_overlap_rejects_non_overlapping() {
        let dev = standard_profile("dev");
        let prd = profile_with_ports("prd", 25432, 25433);
        assert!(!ProfileConfig::ports_overlap(&dev, &prd));
    }

    #[test]
    fn profile_config_round_trips_through_json() {
        let cfg = config(vec![standard_profile("dev")]);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: ProfileConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, back);
    }

    #[test]
    fn serialized_json_uses_camel_case_field_names() {
        let mut p = standard_profile("dev");
        p.impersonate_service_account = Some("sa@example.iam.gserviceaccount.com".to_string());
        let json = serde_json::to_value(&p).expect("serialize");

        assert!(json["instances"][0]["connectionName"].is_string());
        assert!(json["flags"]["autoIamAuthn"].is_boolean());
        assert!(json["flags"]["privateIp"].is_boolean());
        assert!(json["impersonateServiceAccount"].is_string());

        // snake_case must NOT be present
        assert!(json["instances"][0].get("connection_name").is_none());
        assert!(json["flags"].get("auto_iam_authn").is_none());
        assert!(json.get("impersonate_service_account").is_none());
    }
}
