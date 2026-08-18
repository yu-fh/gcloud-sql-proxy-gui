//! JSON-backed persistence for [`ProfileConfig`]: default path resolution,
//! seeding a fresh config with the three standard environments, and
//! load/save with validation and atomic writes.
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

/// Build a single standard-shaped profile: one primary on 15432, one replica
/// on 15433, both with an empty connection name (filled in later by gcloud
/// discovery), default proxy flags, no impersonation, and the given danger
/// flag.
fn seed_profile(id: &str, name: &str, project: &str, danger: bool) -> Profile {
    Profile {
        id: id.to_string(),
        name: name.to_string(),
        project: project.to_string(),
        region: "us-central1".to_string(),
        instances: vec![
            Instance {
                role: InstanceRole::Primary,
                connection_name: String::new(),
                port: 15432,
            },
            Instance {
                role: InstanceRole::Replica,
                connection_name: String::new(),
                port: 15433,
            },
        ],
        flags: ProxyFlags::default(),
        impersonate_service_account: None,
        danger,
    }
}

/// The default seeded config: dev, stg, prd, each with a primary (15432) and
/// replica (15433) instance and an empty connection name -- gcloud discovery
/// (a later task) fills those in. All three environments intentionally reuse
/// the same ports: the app is exclusive-by-default, so only one profile
/// normally runs its proxy process at a time.
pub fn seed_profiles() -> ProfileConfig {
    ProfileConfig {
        version: CURRENT_SCHEMA_VERSION,
        profiles: vec![
            seed_profile("dev", "dev", "my-project-dev", false),
            seed_profile("stg", "stg", "my-project-stg", false),
            seed_profile("prd", "prd", "my-project-prd", true),
        ],
    }
}

/// Load the config at `path`, seeding and writing a fresh default config if
/// the file does not yet exist.
///
/// A file that exists but fails to parse as JSON, or parses but fails
/// [`ProfileConfig::validate`], is always an error -- it is never silently
/// replaced. Overwriting a user's (possibly hand-edited) config on a parse
/// bug would be a silent data-loss bug.
pub fn load_or_seed(path: &Path) -> Result<ProfileConfig, StoreError> {
    if !path.exists() {
        let config = seed_profiles();
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

    #[test]
    fn seed_has_three_environments_in_order() {
        let cfg = seed_profiles();
        let ids: Vec<&str> = cfg.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["dev", "stg", "prd"]);
    }

    #[test]
    fn seed_profiles_all_use_standard_ports() {
        let cfg = seed_profiles();
        for profile in &cfg.profiles {
            assert_eq!(profile.ports(), vec![15432, 15433], "profile {}", profile.id);
        }
    }

    #[test]
    fn seed_marks_only_prd_as_danger() {
        let cfg = seed_profiles();
        for profile in &cfg.profiles {
            let expected_danger = profile.id == "prd";
            assert_eq!(profile.danger, expected_danger, "profile {}", profile.id);
        }
    }

    #[test]
    fn seed_connection_names_are_empty() {
        let cfg = seed_profiles();
        for profile in &cfg.profiles {
            for instance in &profile.instances {
                assert_eq!(instance.connection_name, "", "profile {}", profile.id);
            }
        }
    }

    #[test]
    fn seed_is_valid_and_all_profiles_mutually_conflict() {
        // This is the exclusive-by-default invariant: dev/stg/prd all reuse
        // 15432/15433, so the seeded config must validate cleanly while also
        // reporting that every pair of profiles shares both ports.
        let cfg = seed_profiles();
        assert_eq!(cfg.validate(), Ok(()));
        assert_eq!(cfg.conflicting_ports().len(), 6);
    }

    #[test]
    fn load_or_seed_creates_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        assert!(!path.exists());

        let cfg = load_or_seed(&path).expect("load_or_seed");

        assert!(path.exists());
        assert_eq!(cfg.profiles.len(), 3);
    }

    #[test]
    fn save_then_load_or_seed_round_trips_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let original = seed_profiles();
        save(&path, &original).expect("save");

        let loaded = load_or_seed(&path).expect("load_or_seed");
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_or_seed_rejects_malformed_json_and_leaves_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        let garbage = "{ this is not valid json";
        std::fs::write(&path, garbage).expect("write garbage");

        let result = load_or_seed(&path);
        assert!(matches!(result, Err(StoreError::Parse(_))));

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, garbage);
    }

    #[test]
    fn load_or_seed_rejects_future_schema_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, r#"{"version": 99, "profiles": []}"#).expect("write");

        let result = load_or_seed(&path);
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
        save(&path, &seed_profiles()).expect("save");

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
        save(&path, &seed_profiles()).expect("save long");
        let long_len = std::fs::read(&path).expect("read long").len();

        let mut short = seed_profiles();
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

        save(&path, &seed_profiles()).expect("save");

        assert!(path.exists());
    }

    #[test]
    fn save_rejects_invalid_config_and_does_not_write_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("profiles.json");

        let mut bad = seed_profiles();
        // Force a duplicate port within the first profile's instances.
        bad.profiles[0].instances[1].port = bad.profiles[0].instances[0].port;

        let result = save(&path, &bad);
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
