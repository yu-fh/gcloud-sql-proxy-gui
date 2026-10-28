//! JSON-backed persistence for [`ProfileConfig`]: default path resolution,
//! initialising a fresh empty config, and load/save with validation and
//! atomic writes.
//!
//! This module has no Tauri dependency and is unit-testable standalone.

use std::path::{Path, PathBuf};

use crate::core::profile::{
    Instance, InstanceRole, Profile, ProfileConfig, ProxyFlags, ValidationError,
    CURRENT_SCHEMA_VERSION,
};

/// Errors produced while loading or saving a [`ProfileConfig`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file is not valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] ValidationError),
}

/// The application's config directory name under the OS config root.
const APP_CONFIG_DIR: &str = "ai.firsthand.fh-cloud-sql-proxy-gui";

/// The default on-disk location for `profiles.json`, e.g.
/// `~/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json`
/// on macOS. Returns `None` if the OS config directory can't be resolved.
pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join(APP_CONFIG_DIR).join("profiles.json"))
}

/// The conventional ports every profile starts on: primary 15432, replica
/// 15433.
///
/// Every profile defaults to the same pair, which is precisely what makes the
/// app exclusive-by-default: two profiles cannot both bind 15432, so starting
/// one stops the other. A user who genuinely wants two proxies at once edits
/// one profile's ports.
pub const DEFAULT_PRIMARY_PORT: u16 = 15432;
pub const DEFAULT_REPLICA_PORT: u16 = 15433;

/// Build a standard-shaped profile: one primary on 15432, one replica on
/// 15433, both with an empty connection name (the user types those in),
/// default proxy flags, and no impersonation.
///
/// `vpn_probe_host` is the optional diagnostic host — see [`Profile`].
fn standard_profile(
    id: &str,
    name: &str,
    project: &str,
    danger: bool,
    vpn_probe_host: Option<&str>,
) -> Profile {
    Profile {
        id: id.to_string(),
        name: name.to_string(),
        project: project.to_string(),
        region: "us-central1".to_string(),
        instances: vec![
            Instance {
                role: InstanceRole::Primary,
                connection_name: String::new(),
                port: DEFAULT_PRIMARY_PORT,
            },
            Instance {
                role: InstanceRole::Replica,
                connection_name: String::new(),
                port: DEFAULT_REPLICA_PORT,
            },
        ],
        flags: ProxyFlags::default(),
        impersonate_service_account: None,
        danger,
        vpn_probe_host: vpn_probe_host.map(str::to_string),
    }
}

/// A blank user-created profile: unique `id` derived from `name`, empty
/// project and connection names, and the conventional ports.
///
/// Everything else is left for the user to fill in — this is deliberately not
/// a copy of any seeded environment, because a new profile that silently
/// inherited a real project would be a trap.
pub fn new_profile(name: &str, taken_ids: &[String]) -> Profile {
    let id = crate::core::profile::unique_id_from_name(name, taken_ids);
    standard_profile(&id, name, "", false, None)
}

/// An empty config, at the current schema version.
///
/// This is what a first run starts from, and it is deliberately **empty**
/// rather than a set of starter profiles. An earlier version seeded `dev`,
/// `stg` and `prd` pre-filled with one particular deployment's projects and
/// probe hosts, which suited that deployment and nobody else: this is a
/// general-purpose tool, so there is no project it could seed that would be
/// right for the person running it. Stripped of those values the seeds were
/// three blank rows asserting a naming convention the user may not share, and
/// rows they would have to delete.
///
/// A profile that arrives pre-filled is also the trap `new_profile` avoids: it
/// looks configured, so it invites a Connect against a project the user never
/// chose. The front end already renders an empty environment list as "No
/// environments." beside the + button, so starting empty needs no other
/// handling.
pub fn empty_config() -> ProfileConfig {
    ProfileConfig {
        version: CURRENT_SCHEMA_VERSION,
        profiles: Vec::new(),
    }
}

/// Load the config at `path`, writing a fresh empty config if the file does
/// not yet exist.
///
/// A file that exists but fails to parse as JSON, or parses but fails
/// [`ProfileConfig::validate`], is always an error -- it is never silently
/// replaced. Overwriting a user's (possibly hand-edited) config on a parse
/// bug would be a silent data-loss bug.
pub fn load_or_init(path: &Path) -> Result<ProfileConfig, StoreError> {
    if !path.exists() {
        let config = empty_config();
        save(path, &config)?;
        return Ok(config);
    }

    let contents = std::fs::read_to_string(path)?;
    let config: ProfileConfig = serde_json::from_str(&contents)?;
    config.validate()?;
    Ok(config)
}

/// Validate and persist `config` to `path`, creating parent directories if
/// needed.
///
/// Writes to a temp file, flushes it to disk, then renames onto the target, so
/// a crash mid-write leaves either the old config or the new one — never a
/// truncated file. `rename` alone would only make the *swap* atomic to other
/// processes; without the `sync_all` the rename can survive a power loss while
/// the data blocks behind it do not.
///
/// The temp name carries the process id so two processes cannot interleave
/// their writes into one temp file. That does NOT serialize concurrent saves
/// within a process: callers must hold the config behind a mutex and save
/// while holding the guard (Tauri commands run on a thread pool, so two IPC
/// calls can otherwise overlap).
pub fn save(path: &Path, config: &ProfileConfig) -> Result<(), StoreError> {
    config.validate()?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(config)?;

    // Sibling of the target, so the rename stays within one filesystem.
    let tmp_path = path.with_extension(format!("json.tmp.{}", std::process::id()));

    {
        use std::io::Write;
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }

    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three profiles built the way the UI builds them, for the tests that
    /// need a populated config. This stands in for the starter profiles an
    /// earlier version shipped; see `empty_config` on why nothing is seeded.
    fn three_profiles() -> ProfileConfig {
        let mut config = empty_config();
        for (id, danger) in [("dev", false), ("stg", false), ("prd", true)] {
            config
                .profiles
                .push(standard_profile(id, id, "", danger, None));
        }
        config
    }

    #[test]
    fn a_fresh_config_has_no_profiles() {
        // The whole point of not seeding: a first run presents an empty list,
        // not someone else's environments.
        let cfg = empty_config();
        assert!(cfg.profiles.is_empty());
        assert_eq!(cfg.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn new_profiles_all_use_standard_ports() {
        for profile in &three_profiles().profiles {
            assert_eq!(
                profile.ports(),
                vec![DEFAULT_PRIMARY_PORT, DEFAULT_REPLICA_PORT],
                "profile {}",
                profile.id
            );
        }
    }

    #[test]
    fn profiles_sharing_the_standard_ports_all_mutually_conflict() {
        // The exclusive-by-default invariant: profiles created through the UI
        // all reuse 15432/15433, so such a config must validate cleanly while
        // still reporting that every pair shares both ports.
        let cfg = three_profiles();
        assert_eq!(cfg.validate(), Ok(()));
        assert_eq!(cfg.conflicting_ports().len(), 6);
    }

    #[test]
    fn new_profile_uses_the_conventional_ports_and_is_blank() {
        let p = new_profile("Analytics", &[]);
        assert_eq!(p.id, "analytics");
        assert_eq!(p.name, "Analytics");
        assert_eq!(p.ports(), vec![DEFAULT_PRIMARY_PORT, DEFAULT_REPLICA_PORT]);
        assert_eq!(p.project, "");
        assert!(!p.danger);
        assert_eq!(p.vpn_probe_host, None);
        for instance in &p.instances {
            assert_eq!(instance.connection_name, "");
        }
    }

    #[test]
    fn new_profile_avoids_ids_already_in_the_config() {
        let cfg = three_profiles();
        let taken: Vec<String> = cfg.profiles.iter().map(|p| p.id.clone()).collect();

        let p = new_profile("dev", &taken);
        assert_eq!(p.id, "dev-2");

        // And the resulting config must validate -- a duplicate id would be a
        // validation error, so this is the invariant that matters.
        let mut next = cfg;
        next.profiles.push(p);
        assert_eq!(next.validate(), Ok(()));
    }

    #[test]
    fn load_or_init_accepts_a_config_written_before_vpn_probe_host_existed() {
        // Backward compatibility at the file level: an existing profiles.json
        // with no vpnProbeHost anywhere must load rather than erroring, which
        // is what #[serde(default)] on the field buys.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        std::fs::write(
            &path,
            r#"{
              "version": 1,
              "profiles": [
                {
                  "id": "dev",
                  "name": "dev",
                  "project": "my-project-dev",
                  "region": "us-central1",
                  "instances": [
                    {"role": "primary", "connectionName": "a:b:c", "port": 15432},
                    {"role": "replica", "connectionName": "a:b:d", "port": 15433}
                  ],
                  "flags": {"autoIamAuthn": true, "privateIp": true},
                  "impersonateServiceAccount": null,
                  "danger": false
                }
              ]
            }"#,
        )
        .expect("write legacy config");

        let cfg = load_or_init(&path).expect("legacy config should still load");
        assert_eq!(cfg.profiles.len(), 1);
        assert_eq!(cfg.profiles[0].vpn_probe_host, None);
        assert_eq!(cfg.profiles[0].ports(), vec![15432, 15433]);
    }

    #[test]
    fn load_or_init_creates_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        assert!(!path.exists());

        let cfg = load_or_init(&path).expect("load_or_init");

        // The file is created so later saves have a target, but it holds no
        // profiles: a first run starts empty. See `empty_config`.
        assert!(path.exists());
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn save_then_load_or_init_round_trips_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let original = three_profiles();
        save(&path, &original).expect("save");

        let loaded = load_or_init(&path).expect("load_or_init");
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_or_init_rejects_malformed_json_and_leaves_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        let garbage = "{ this is not valid json";
        std::fs::write(&path, garbage).expect("write garbage");

        let result = load_or_init(&path);
        assert!(matches!(result, Err(StoreError::Parse(_))));

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, garbage);
    }

    #[test]
    fn load_or_init_rejects_future_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, r#"{"version": 99, "profiles": []}"#).expect("write");

        let result = load_or_init(&path);
        assert!(matches!(
            result,
            Err(StoreError::Invalid(ValidationError::UnsupportedVersion {
                found: 99,
                expected: CURRENT_SCHEMA_VERSION,
            }))
        ));
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        save(&path, &three_profiles()).expect("save");

        // Assert on the whole directory rather than one expected temp name:
        // a name-specific check passes even when a differently-named temp
        // file is leaked.
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["profiles.json".to_string()]);
    }

    #[test]
    fn save_overwrites_existing_file_without_leaving_stale_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        // First write the full three-environment config, then overwrite with a
        // shorter one. A non-atomic write could leave trailing bytes of the
        // longer JSON behind, producing invalid output.
        save(&path, &three_profiles()).expect("save long");
        let long_len = std::fs::read(&path).expect("read long").len();

        let mut short = three_profiles();
        short.profiles.truncate(1);
        save(&path, &short).expect("save short");

        let after = std::fs::read_to_string(&path).expect("read short");
        assert!(after.len() < long_len, "expected a shorter file");
        let reloaded: ProfileConfig = serde_json::from_str(&after).expect("valid JSON");
        assert_eq!(reloaded, short);
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("dirs").join("profiles.json");
        assert!(!path.parent().unwrap().exists());

        save(&path, &three_profiles()).expect("save");

        assert!(path.exists());
    }

    #[test]
    fn save_rejects_invalid_config_and_does_not_write_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let mut bad = three_profiles();
        // Force a duplicate port within the first profile's instances.
        bad.profiles[0].instances[1].port = bad.profiles[0].instances[0].port;

        let result = save(&path, &bad);
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
