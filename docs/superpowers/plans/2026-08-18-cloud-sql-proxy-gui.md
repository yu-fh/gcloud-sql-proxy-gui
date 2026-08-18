# Cloud SQL Proxy GUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a macOS menu bar app that starts and stops `cloud-sql-proxy` for named environment profiles with one click, refreshes Terraform-drifted connection names from gcloud, and names the fix when a connection fails.

**Architecture:** Tauri v2 app with `ActivationPolicy::Accessory` (no Dock icon). All logic lives in a pure-Rust core library (`src-tauri/src/core/`) with no Tauri dependencies, so it is unit-testable without launching an app. A thin Tauri layer (tray menu + commands) adapts the core to the UI. Child processes are managed with `tokio::process` rather than `tauri-plugin-shell` for guaranteed kill-on-drop and no permission-scope indirection.

**Tech Stack:** Rust (edition 2021, toolchain 1.95), Tauri v2, tokio, serde/serde_json, vanilla HTML/CSS/JS frontend (no framework — the only webview screens are a profile form and a log view).

---

## Reference: spec

Read `docs/superpowers/specs/2026-08-18-cloud-sql-proxy-gui-design.md` before starting. Key facts you will need repeatedly:

- Port convention: **15432 = primary, 15433 = read replica**.
- Environment projects: dev `my-project-dev`, stg `my-project-stg`, prd `my-project-prd`. All region `us-central1`.
- Proxy invocation shape:
  ```
  cloud-sql-proxy --auto-iam-authn --private-ip \
    "<project>:<region>:<instance>?port=15432" \
    "<project>:<region>:<instance>?port=15433"
  ```
- Readiness line on stderr contains `ready for new connections`.
- Instances are private-IP only; the corporate VPN is required.

## File structure

Each file has one responsibility. Core files have no `tauri::` imports so they compile and test standalone.

| File | Responsibility |
| --- | --- |
| `src-tauri/src/core/mod.rs` | Re-exports the core modules |
| `src-tauri/src/core/profile.rs` | `Profile`, `Instance`, `InstanceRole`, `ProfileConfig` types + validation |
| `src-tauri/src/core/store.rs` | Load/save `profiles.json`; seed defaults; schema version guard |
| `src-tauri/src/core/log_watcher.rs` | Classify a proxy output line into `ProxyEvent` (pure, no I/O) |
| `src-tauri/src/core/preflight.rs` | Port bind test, ADC file check, VPN reachability probe |
| `src-tauri/src/core/proxy.rs` | `ProxyManager`: spawn/kill children, track state |
| `src-tauri/src/core/discovery.rs` | Parse `gcloud sql instances list` output; reconcile against stored profiles |
| `src-tauri/src/core/state.rs` | `AppState`: profiles + running set; decides whether a start is allowed |
| `src-tauri/src/tray.rs` | Build and update the tray menu; translate menu events into core calls |
| `src-tauri/src/commands.rs` | Tauri commands invoked by the webview (profile CRUD, logs, refresh) |
| `src-tauri/src/main.rs` | Wire everything; set activation policy; install exit cleanup |
| `src/index.html`, `src/profiles.js`, `src/styles.css` | Profile editor + log viewer webview |
| `tests/fixtures/gcloud_instances_list.txt` | Recorded gcloud output for discovery tests |
| `tests/fixtures/fake-proxy.sh` | Fake proxy binary for ProxyManager tests |

Tests live inline in `#[cfg(test)]` modules for pure units (the Rust convention), except ProxyManager, which gets an integration test in `src-tauri/tests/proxy_manager.rs` because it spawns real processes.

---

## Task 1: Scaffold the Tauri project

**Files:**
- Create: `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/build.rs`, `src-tauri/src/main.rs`
- Create: `package.json`, `src/index.html`
- Create: `.gitignore`

- [ ] **Step 1: Create `.gitignore`**

```
target/
node_modules/
dist/
.DS_Store
```

- [ ] **Step 2: Create `package.json`**

```json
{
  "name": "fh-cloud-sql-proxy-gui",
  "private": true,
  "version": "0.1.0",
  "scripts": {
    "tauri": "tauri"
  },
  "devDependencies": {
    "@tauri-apps/cli": "^2"
  }
}
```

- [ ] **Step 3: Create `src/index.html`** (placeholder; the real editor comes in Task 9)

```html
<!doctype html>
<html>
  <head><meta charset="utf-8" /><title>Cloud SQL Proxy</title></head>
  <body><div id="app">Loading…</div><script type="module" src="/profiles.js"></script></body>
</html>
```

- [ ] **Step 4: Create `src/profiles.js`** (placeholder)

```javascript
document.getElementById('app').textContent = 'Profiles';
```

- [ ] **Step 5: Create `src/styles.css`** (placeholder)

```css
body { font: 13px -apple-system, system-ui, sans-serif; margin: 0; padding: 12px; }
```

- [ ] **Step 6: Create `src-tauri/Cargo.toml`**

```toml
[package]
name = "fh-cloud-sql-proxy-gui"
version = "0.1.0"
edition = "2021"
rust-version = "1.77"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-autostart = "2"
tauri-plugin-dialog = "2"
tokio = { version = "1", features = ["process", "io-util", "rt-multi-thread", "macros", "sync", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
dirs = "5"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 7: Create `src-tauri/build.rs`**

```rust
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 8: Create `src-tauri/tauri.conf.json`**

`dockVisibility: false` is what makes this a menu-bar-only app. `withGlobalTauri` lets the plain-JS frontend call commands without a bundler.

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Cloud SQL Proxy",
  "version": "0.1.0",
  "identifier": "ai.firsthand.fh-cloud-sql-proxy-gui",
  "build": {
    "frontendDist": "../src"
  },
  "app": {
    "withGlobalTauri": true,
    "windows": [],
    "security": {
      "csp": "default-src 'self'"
    }
  },
  "bundle": {
    "active": true,
    "targets": ["app", "dmg"],
    "icon": ["icons/icon.icns"],
    "macOS": {
      "dockVisibility": false,
      "minimumSystemVersion": "11.0"
    }
  }
}
```

Note: `windows: []` — no window is created at launch. Windows are created on demand in Task 9.

- [ ] **Step 9: Create a placeholder icon**

Run:
```bash
mkdir -p src-tauri/icons
npm install
npx tauri icon --help >/dev/null 2>&1 || true
```

Then generate a minimal 1024×1024 PNG and convert it:
```bash
/usr/bin/python3 -c "
import zlib, struct
def chunk(t, d):
    c = t + d
    return struct.pack('>I', len(d)) + c + struct.pack('>I', zlib.crc32(c) & 0xffffffff)
w = h = 1024
raw = b''.join(b'\x00' + b'\x4a\x90\xd0\xff' * w for _ in range(h))
png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')
open('src-tauri/icons/icon.png','wb').write(png)
"
npx tauri icon src-tauri/icons/icon.png
```
Expected: writes `icon.icns`, `icon.ico`, and several PNGs into `src-tauri/icons/`.

- [ ] **Step 10: Create minimal `src-tauri/src/main.rs`**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running application");
}
```

- [ ] **Step 11: Verify it compiles**

Run: `cd src-tauri && cargo build`
Expected: `Finished dev [unoptimized + debuginfo] target(s)`. First build downloads many crates and may take several minutes.

- [ ] **Step 12: Commit**

```bash
git add -A
git commit -m "Scaffold Tauri v2 project with menu-bar-only configuration"
```

---

## Task 2: Profile types and validation

**Files:**
- Create: `src-tauri/src/core/mod.rs`
- Create: `src-tauri/src/core/profile.rs`

- [ ] **Step 1: Create `src-tauri/src/core/mod.rs`**

```rust
pub mod profile;
```

- [ ] **Step 2: Write the failing tests in `src-tauri/src/core/profile.rs`**

Write the whole file with types and tests; the tests fail to compile until the impl block exists, which is the point.

```rust
use serde::{Deserialize, Serialize};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceRole {
    Primary,
    Replica,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Instance {
    pub role: InstanceRole,
    #[serde(rename = "connectionName")]
    pub connection_name: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyFlags {
    #[serde(rename = "autoIamAuthn")]
    pub auto_iam_authn: bool,
    #[serde(rename = "privateIp")]
    pub private_ip: bool,
}

impl Default for ProxyFlags {
    fn default() -> Self {
        Self { auto_iam_authn: true, private_ip: true }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub version: u32,
    pub profiles: Vec<Profile>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("port {port} is used by both '{first}' and '{second}'")]
    DuplicatePort { port: u16, first: String, second: String },
    #[error("profile '{0}' has no instances")]
    NoInstances(String),
    #[error("duplicate profile id '{0}'")]
    DuplicateId(String),
    #[error("config schema version {found} is not supported (expected {expected})")]
    UnsupportedVersion { found: u32, expected: u32 },
}

impl Profile {
    /// Build the positional instance arguments for cloud-sql-proxy,
    /// e.g. "project:region:instance?port=15432".
    pub fn instance_args(&self) -> Vec<String> {
        self.instances
            .iter()
            .map(|i| format!("{}?port={}", i.connection_name, i.port))
            .collect()
    }

    pub fn ports(&self) -> Vec<u16> {
        self.instances.iter().map(|i| i.port).collect()
    }
}

impl ProfileConfig {
    /// Validate the whole config: schema version, per-profile shape,
    /// and global port uniqueness across all profiles.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version != CURRENT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                found: self.version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }

        let mut seen_ids: Vec<&str> = Vec::new();
        for p in &self.profiles {
            if p.instances.is_empty() {
                return Err(ValidationError::NoInstances(p.id.clone()));
            }
            if seen_ids.contains(&p.id.as_str()) {
                return Err(ValidationError::DuplicateId(p.id.clone()));
            }
            seen_ids.push(&p.id);
        }

        // Port owner map across all profiles.
        let mut owner: Vec<(u16, &str)> = Vec::new();
        for p in &self.profiles {
            for port in p.ports() {
                if let Some((_, first)) = owner.iter().find(|(seen, _)| *seen == port) {
                    return Err(ValidationError::DuplicatePort {
                        port,
                        first: (*first).to_string(),
                        second: p.id.clone(),
                    });
                }
                owner.push((port, &p.id));
            }
        }
        Ok(())
    }

    /// Profiles whose ports do not overlap may run concurrently.
    pub fn ports_overlap(a: &Profile, b: &Profile) -> bool {
        a.ports().iter().any(|p| b.ports().contains(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(role: InstanceRole, port: u16) -> Instance {
        Instance {
            role,
            connection_name: format!("proj:us-central1:inst-{port}"),
            port,
        }
    }

    fn profile(id: &str, ports: [u16; 2]) -> Profile {
        Profile {
            id: id.to_string(),
            name: id.to_string(),
            project: "proj".to_string(),
            region: "us-central1".to_string(),
            instances: vec![
                instance(InstanceRole::Primary, ports[0]),
                instance(InstanceRole::Replica, ports[1]),
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
        }
    }

    #[test]
    fn instance_args_appends_port_query() {
        let p = profile("dev", [15432, 15433]);
        assert_eq!(
            p.instance_args(),
            vec![
                "proj:us-central1:inst-15432?port=15432".to_string(),
                "proj:us-central1:inst-15433?port=15433".to_string(),
            ]
        );
    }

    #[test]
    fn validate_accepts_distinct_ports() {
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15433]), profile("prd", [25432, 25433])],
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_duplicate_port_across_profiles() {
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15433]), profile("prd", [15432, 25433])],
        };
        assert_eq!(
            config.validate(),
            Err(ValidationError::DuplicatePort {
                port: 15432,
                first: "dev".to_string(),
                second: "prd".to_string()
            })
        );
    }

    #[test]
    fn validate_rejects_duplicate_port_within_one_profile() {
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15432])],
        };
        assert!(matches!(
            config.validate(),
            Err(ValidationError::DuplicatePort { port: 15432, .. })
        ));
    }

    #[test]
    fn validate_rejects_unsupported_version() {
        let config = ProfileConfig { version: 99, profiles: vec![] };
        assert_eq!(
            config.validate(),
            Err(ValidationError::UnsupportedVersion { found: 99, expected: 1 })
        );
    }

    #[test]
    fn validate_rejects_profile_with_no_instances() {
        let mut p = profile("dev", [15432, 15433]);
        p.instances.clear();
        let config = ProfileConfig { version: 1, profiles: vec![p] };
        assert_eq!(config.validate(), Err(ValidationError::NoInstances("dev".to_string())));
    }

    #[test]
    fn ports_overlap_detects_shared_port() {
        let dev = profile("dev", [15432, 15433]);
        let prd_default = profile("prd", [15432, 15433]);
        let prd_offset = profile("prd", [25432, 25433]);
        assert!(ProfileConfig::ports_overlap(&dev, &prd_default));
        assert!(!ProfileConfig::ports_overlap(&dev, &prd_offset));
    }

    #[test]
    fn config_round_trips_through_json() {
        let config = ProfileConfig { version: 1, profiles: vec![profile("dev", [15432, 15433])] };
        let json = serde_json::to_string(&config).unwrap();
        let back: ProfileConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn json_uses_camel_case_field_names() {
        let config = ProfileConfig { version: 1, profiles: vec![profile("dev", [15432, 15433])] };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"connectionName\""));
        assert!(json.contains("\"autoIamAuthn\""));
    }
}
```

- [ ] **Step 3: Register the module in `src-tauri/src/main.rs`**

Add as the first line of the file, before `#![cfg_attr...]`? No — module declarations go after inner attributes. Insert after the `#![cfg_attr(...)]` line:

```rust
mod core;
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test core::profile`
Expected: 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Add profile types with port-uniqueness validation"
```

---

## Task 3: Profile store with seeded defaults

**Files:**
- Create: `src-tauri/src/core/store.rs`
- Modify: `src-tauri/src/core/mod.rs`

- [ ] **Step 1: Add the module to `src-tauri/src/core/mod.rs`**

```rust
pub mod profile;
pub mod store;
```

- [ ] **Step 2: Write `src-tauri/src/core/store.rs` with tests**

The store takes an explicit path so tests use a tempdir instead of the real Application Support directory.

```rust
use std::path::{Path, PathBuf};

use crate::core::profile::{
    Instance, InstanceRole, Profile, ProfileConfig, ProxyFlags, ValidationError,
    CURRENT_SCHEMA_VERSION,
};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file is not valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Invalid(#[from] ValidationError),
}

/// Default config location: ~/Library/Application Support/<identifier>/profiles.json
pub fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| {
        d.join("ai.firsthand.fh-cloud-sql-proxy-gui")
            .join("profiles.json")
    })
}

/// The three known Firsthand environments, with the documented port
/// convention (15432 primary / 15433 replica) and empty connection names.
/// Connection names are filled in by discovery, because they contain
/// Terraform-generated suffixes that change over time.
pub fn seed_profiles() -> ProfileConfig {
    let envs = [
        ("dev", "my-project-dev", false),
        ("stg", "my-project-stg", false),
        ("prd", "my-project-prd", true),
    ];
    ProfileConfig {
        version: CURRENT_SCHEMA_VERSION,
        profiles: envs
            .iter()
            .map(|(id, project, danger)| Profile {
                id: (*id).to_string(),
                name: (*id).to_string(),
                project: (*project).to_string(),
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
                danger: *danger,
            })
            .collect(),
    }
}

/// Load the config, or seed and write defaults if the file does not exist.
/// A malformed or unsupported file is an error, never silently replaced —
/// overwriting a user's config on a parse bug would lose their edits.
pub fn load_or_seed(path: &Path) -> Result<ProfileConfig, StoreError> {
    if !path.exists() {
        let seeded = seed_profiles();
        save(path, &seeded)?;
        return Ok(seeded);
    }
    let text = std::fs::read_to_string(path)?;
    let config: ProfileConfig = serde_json::from_str(&text)?;
    config.validate()?;
    Ok(config)
}

pub fn save(path: &Path, config: &ProfileConfig) -> Result<(), StoreError> {
    config.validate()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a temp file then rename, so a crash mid-write cannot
    // leave a truncated config behind.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_has_three_environments_with_convention_ports() {
        let config = seed_profiles();
        let ids: Vec<&str> = config.profiles.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["dev", "stg", "prd"]);
        for p in &config.profiles {
            assert_eq!(p.ports(), vec![15432, 15433]);
        }
    }

    #[test]
    fn seed_marks_only_prd_as_danger() {
        let config = seed_profiles();
        let danger: Vec<&str> = config
            .profiles
            .iter()
            .filter(|p| p.danger)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(danger, vec!["prd"]);
    }

    #[test]
    fn seed_config_fails_validation_only_on_duplicate_ports() {
        // All three seeded profiles share 15432/15433 by design (exclusive
        // by default), so the seeded set intentionally does NOT pass the
        // global uniqueness check. Validation is for user-authored configs
        // where concurrency is requested.
        let config = seed_profiles();
        assert!(config.validate().is_err());
    }

    #[test]
    fn load_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        assert!(!path.exists());
        let config = load_or_seed(&path).unwrap();
        assert_eq!(config.profiles.len(), 3);
        assert!(path.exists());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let mut config = seed_profiles();
        // Give profiles distinct ports so the config validates.
        config.profiles[1].instances[0].port = 25432;
        config.profiles[1].instances[1].port = 25433;
        config.profiles[2].instances[0].port = 35432;
        config.profiles[2].instances[1].port = 35433;
        save(&path, &config).unwrap();
        let back = load_or_seed(&path).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn load_rejects_malformed_json_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(load_or_seed(&path), Err(StoreError::Parse(_))));
        // File is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    #[test]
    fn load_rejects_future_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        std::fs::write(&path, r#"{"version": 99, "profiles": []}"#).unwrap();
        assert!(matches!(load_or_seed(&path), Err(StoreError::Invalid(_))));
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let mut config = seed_profiles();
        config.profiles.truncate(1);
        save(&path, &config).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
    }
}
```

Note the deliberate design point captured in `seed_config_fails_validation_only_on_duplicate_ports`: the seeded three-environment config shares ports because exclusive-by-default is the intent. Therefore `load_or_seed` must **not** call `validate()` on the seeded value it writes. Verify the code above does not — `save()` does call validate, so this is a real conflict.

- [ ] **Step 3: Resolve the seed/validate conflict**

Split validation: global port uniqueness is a *concurrency* property, not a config-integrity property. Replace `validate` usage so `save` only checks integrity.

In `src-tauri/src/core/profile.rs`, split the method — replace the `validate` method with these two:

```rust
    /// Integrity checks that must hold for any config: schema version,
    /// unique ids, non-empty instances, and no duplicate port *within*
    /// a single profile (which could never start).
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version != CURRENT_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                found: self.version,
                expected: CURRENT_SCHEMA_VERSION,
            });
        }

        let mut seen_ids: Vec<&str> = Vec::new();
        for p in &self.profiles {
            if p.instances.is_empty() {
                return Err(ValidationError::NoInstances(p.id.clone()));
            }
            if seen_ids.contains(&p.id.as_str()) {
                return Err(ValidationError::DuplicateId(p.id.clone()));
            }
            seen_ids.push(&p.id);

            let mut ports = p.ports();
            ports.sort_unstable();
            let before = ports.len();
            ports.dedup();
            if ports.len() != before {
                let dup = p
                    .ports()
                    .iter()
                    .find(|port| p.ports().iter().filter(|q| *q == *port).count() > 1)
                    .copied()
                    .unwrap_or_default();
                return Err(ValidationError::DuplicatePort {
                    port: dup,
                    first: p.id.clone(),
                    second: p.id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Which profiles could never run at the same time because they
    /// share a port. Used to decide whether starting one must stop another.
    pub fn conflicting_ports(&self) -> Vec<(String, String, u16)> {
        let mut conflicts = Vec::new();
        for (i, a) in self.profiles.iter().enumerate() {
            for b in self.profiles.iter().skip(i + 1) {
                for port in a.ports() {
                    if b.ports().contains(&port) {
                        conflicts.push((a.id.clone(), b.id.clone(), port));
                    }
                }
            }
        }
        conflicts
    }
```

- [ ] **Step 4: Update the profile tests for the split**

In `src-tauri/src/core/profile.rs`, replace `validate_rejects_duplicate_port_across_profiles` with:

```rust
    #[test]
    fn validate_allows_duplicate_ports_across_profiles() {
        // Sharing 15432 across environments is the documented default;
        // it means they cannot run concurrently, not that the config is invalid.
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15433]), profile("prd", [15432, 15433])],
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn conflicting_ports_lists_shared_ports_between_profiles() {
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15433]), profile("prd", [15432, 25433])],
        };
        assert_eq!(
            config.conflicting_ports(),
            vec![("dev".to_string(), "prd".to_string(), 15432)]
        );
    }

    #[test]
    fn conflicting_ports_empty_when_offsets_differ() {
        let config = ProfileConfig {
            version: 1,
            profiles: vec![profile("dev", [15432, 15433]), profile("prd", [25432, 25433])],
        };
        assert!(config.conflicting_ports().is_empty());
    }
```

Also update `validate_accepts_distinct_ports` to keep passing (it still should) and change `seed_config_fails_validation_only_on_duplicate_ports` in `store.rs` to:

```rust
    #[test]
    fn seeded_config_is_valid_and_all_profiles_conflict() {
        // Exclusive by default: the three environments share 15432/15433,
        // so the config is valid but no two can run together.
        let config = seed_profiles();
        assert_eq!(config.validate(), Ok(()));
        assert_eq!(config.conflicting_ports().len(), 6); // 3 pairs x 2 ports
    }
```

- [ ] **Step 5: Run the tests**

Run: `cd src-tauri && cargo test core::`
Expected: all profile and store tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Add profile store with seeded environments and atomic writes"
```

---

## Task 4: Log watcher — classify proxy output

**Files:**
- Create: `src-tauri/src/core/log_watcher.rs`
- Modify: `src-tauri/src/core/mod.rs`

- [ ] **Step 1: Add the module to `src-tauri/src/core/mod.rs`**

```rust
pub mod log_watcher;
pub mod profile;
pub mod store;
```

- [ ] **Step 2: Write `src-tauri/src/core/log_watcher.rs` with tests**

Classification is a pure function over a line, so it is table-tested against the real strings from the troubleshooting docs.

```rust
/// A meaningful event parsed out of cloud-sql-proxy's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyEvent {
    /// The proxy is listening and ready to accept connections.
    Ready,
    /// A classified failure, with a message that names the fix.
    Failure(Diagnosis),
    /// Nothing actionable; keep for the log view only.
    Noise,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    pub kind: FailureKind,
    /// User-facing explanation, phrased as the fix.
    pub message: String,
    /// A command the user can copy, if one fixes it.
    pub fix_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    AdcExpired,
    PortInUse,
    StaleInstance,
    OffVpn,
    Unknown,
}

const ADC_FIX: &str = "gcloud auth application-default login";

/// Classify a single line of proxy output.
///
/// Matching is case-insensitive and substring-based because the proxy's
/// exact phrasing varies by version; we key on the stable fragments.
pub fn classify(line: &str) -> ProxyEvent {
    let lower = line.to_lowercase();

    if lower.contains("ready for new connections") {
        return ProxyEvent::Ready;
    }

    if lower.contains("address already in use") {
        let port = extract_port(line);
        let message = match port {
            Some(p) => format!(
                "Port {p} is already in use — another profile or a stray proxy holds it."
            ),
            None => "A local port is already in use — another profile or a stray proxy holds it."
                .to_string(),
        };
        return ProxyEvent::Failure(Diagnosis { kind: FailureKind::PortInUse, message, fix_command: None });
    }

    if lower.contains("pam authentication failed")
        || lower.contains("invalid_grant")
        || lower.contains("could not find default credentials")
        || lower.contains("reauthentication is needed")
    {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::AdcExpired,
            message: "Application-default credentials expired — re-authenticate, then start again."
                .to_string(),
            fix_command: Some(ADC_FIX.to_string()),
        });
    }

    if lower.contains("does not exist") || lower.contains("instance does not exist") {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::StaleInstance,
            message: "Instance not found — it was probably replaced by Terraform. Run \"Refresh connection names\"."
                .to_string(),
            fix_command: None,
        });
    }

    if lower.contains("i/o timeout")
        || lower.contains("dial tcp")
        || lower.contains("context deadline exceeded")
    {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::OffVpn,
            message: "Cannot reach the instance — Cloud SQL is private-IP only, so you must be on the VPN."
                .to_string(),
            fix_command: None,
        });
    }

    ProxyEvent::Noise
}

/// Pull a port number out of a message like
/// "listen tcp 127.0.0.1:15432: bind: address already in use".
fn extract_port(line: &str) -> Option<u16> {
    let after_colon = line.split(':').collect::<Vec<_>>();
    for part in after_colon {
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            if let Ok(p) = digits.parse::<u16>() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ready_line() {
        assert_eq!(
            classify("2026/08/18 10:00:00 The proxy has started successfully and is ready for new connections!"),
            ProxyEvent::Ready
        );
    }

    #[test]
    fn detects_port_in_use_and_extracts_port() {
        let event = classify(
            "failed to start listener: listen tcp 127.0.0.1:15432: bind: address already in use",
        );
        match event {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15432"), "message was: {}", d.message);
            }
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn detects_adc_expiry_and_offers_fix_command() {
        for line in [
            "FATAL: PAM authentication failed for user \"you@example.com\"",
            "oauth2: cannot fetch token: 400 Bad Request: invalid_grant",
            "could not find default credentials",
            "Reauthentication is needed. Please run `gcloud auth login`",
        ] {
            match classify(line) {
                ProxyEvent::Failure(d) => {
                    assert_eq!(d.kind, FailureKind::AdcExpired, "line: {line}");
                    assert_eq!(d.fix_command.as_deref(), Some(ADC_FIX));
                }
                other => panic!("expected ADC failure for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn detects_stale_instance() {
        match classify("Cloud SQL instance \"proj:us-central1:terraform-123\" does not exist") {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::StaleInstance);
                assert!(d.message.contains("Refresh connection names"));
            }
            other => panic!("expected stale instance, got {other:?}"),
        }
    }

    #[test]
    fn detects_network_unreachable_as_vpn_problem() {
        for line in [
            "dial tcp 10.1.2.3:3307: i/o timeout",
            "context deadline exceeded",
        ] {
            match classify(line) {
                ProxyEvent::Failure(d) => assert_eq!(d.kind, FailureKind::OffVpn, "line: {line}"),
                other => panic!("expected VPN failure for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn ordinary_lines_are_noise() {
        assert_eq!(classify("2026/08/18 Authorizing with Application Default Credentials"), ProxyEvent::Noise);
        assert_eq!(classify(""), ProxyEvent::Noise);
    }

    #[test]
    fn port_in_use_takes_precedence_over_network_words() {
        // A bind error line also contains "listen tcp"; it must classify
        // as PortInUse, not OffVpn.
        match classify("listen tcp 127.0.0.1:15433: bind: address already in use") {
            ProxyEvent::Failure(d) => assert_eq!(d.kind, FailureKind::PortInUse),
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cd src-tauri && cargo test core::log_watcher`
Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add proxy output classifier mapping failures to their fixes"
```

---

## Task 5: Preflight checks

**Files:**
- Create: `src-tauri/src/core/preflight.rs`
- Modify: `src-tauri/src/core/mod.rs`

- [ ] **Step 1: Add the module to `src-tauri/src/core/mod.rs`**

```rust
pub mod discovery;
pub mod log_watcher;
pub mod preflight;
pub mod profile;
pub mod proxy;
pub mod state;
pub mod store;
```

(Declare all now; the files come in Tasks 5–8. If `cargo` complains about missing modules, create empty placeholder files, then fill them.)

- [ ] **Step 2: Create empty placeholders so the crate keeps compiling**

```bash
cd src-tauri/src/core && touch discovery.rs proxy.rs state.rs && cd -
```

- [ ] **Step 3: Write `src-tauri/src/core/preflight.rs` with tests**

```rust
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::log_watcher::{Diagnosis, FailureKind};
use crate::core::profile::Profile;

const ADC_FIX: &str = "gcloud auth application-default login";
const VPN_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Can this port be bound on loopback right now?
pub fn port_is_free(port: u16) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpListener::bind(addr).is_ok()
}

/// Path to the application-default credentials file.
pub fn adc_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| {
        h.join(".config")
            .join("gcloud")
            .join("application_default_credentials.json")
    })
}

/// Does the ADC file exist? Absence is a definite failure; presence is
/// only weak evidence, because the token inside may still be expired —
/// that case is caught by the log watcher once the proxy runs.
pub fn adc_present(path: &Path) -> bool {
    path.exists()
}

/// Private DNS name for an environment's primary, per the access doc:
/// pg.<env>.internal.example.com
pub fn private_dns_for(profile: &Profile) -> Option<String> {
    let env = profile.name.as_str();
    if matches!(env, "dev" | "stg" | "prd") {
        Some(format!("pg.{env}.internal.example.com"))
    } else {
        None
    }
}

/// Best-effort VPN reachability probe. Returns `None` when we cannot
/// tell (no known DNS name for this profile), which must not be
/// reported as a failure.
pub fn vpn_reachable(host: &str, port: u16) -> Option<bool> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<SocketAddr> = match (host, port).to_socket_addrs() {
        Ok(it) => it.collect(),
        // DNS failure on a .private. name generally means no VPN routing.
        Err(_) => return Some(false),
    };
    let addr = addrs.first()?;
    Some(TcpStream::connect_timeout(addr, VPN_PROBE_TIMEOUT).is_ok())
}

/// Everything checked before spawning a proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    Ok,
    Blocked(Diagnosis),
}

/// Run the checks that must pass before spawn. Ports are checked here
/// because binding is cheap and gives a better message than the proxy's
/// own stderr. The VPN probe is deliberately *not* blocking: it is slow,
/// and a false negative would stop a working start.
pub fn check(profile: &Profile, adc: Option<&Path>) -> Preflight {
    for port in profile.ports() {
        if !port_is_free(port) {
            return Preflight::Blocked(Diagnosis {
                kind: FailureKind::PortInUse,
                message: format!(
                    "Port {port} is already in use — stop the profile using it, or change this profile's ports."
                ),
                fix_command: None,
            });
        }
    }

    match adc {
        Some(p) if !adc_present(p) => {
            return Preflight::Blocked(Diagnosis {
                kind: FailureKind::AdcExpired,
                message: "No application-default credentials found — authenticate first."
                    .to_string(),
                fix_command: Some(ADC_FIX.to_string()),
            });
        }
        _ => {}
    }

    if profile.instances.iter().any(|i| i.connection_name.is_empty()) {
        return Preflight::Blocked(Diagnosis {
            kind: FailureKind::StaleInstance,
            message: "This profile has no instance connection names yet. Run \"Refresh connection names\"."
                .to_string(),
            fix_command: None,
        });
    }

    Preflight::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn profile_with(ports: [u16; 2], connection_names: bool) -> Profile {
        Profile {
            id: "dev".into(),
            name: "dev".into(),
            project: "proj".into(),
            region: "us-central1".into(),
            instances: vec![
                Instance {
                    role: InstanceRole::Primary,
                    connection_name: if connection_names { "proj:us-central1:a".into() } else { String::new() },
                    port: ports[0],
                },
                Instance {
                    role: InstanceRole::Replica,
                    connection_name: if connection_names { "proj:us-central1:b".into() } else { String::new() },
                    port: ports[1],
                },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
        }
    }

    /// Bind an ephemeral port and return it, keeping the listener alive.
    fn occupied_port() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn free_port_reports_free() {
        let (listener, port) = occupied_port();
        drop(listener);
        assert!(port_is_free(port));
    }

    #[test]
    fn occupied_port_reports_not_free() {
        let (_listener, port) = occupied_port();
        assert!(!port_is_free(port));
    }

    #[test]
    fn check_blocks_when_port_occupied() {
        let (_listener, port) = occupied_port();
        let profile = profile_with([port, 15433], true);
        match check(&profile, None) {
            Preflight::Blocked(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains(&port.to_string()));
            }
            Preflight::Ok => panic!("expected block on occupied port"),
        }
    }

    #[test]
    fn check_blocks_when_adc_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("application_default_credentials.json");
        let (listener, port) = occupied_port();
        drop(listener);
        let profile = profile_with([port, port + 1], true);
        match check(&profile, Some(&missing)) {
            Preflight::Blocked(d) => {
                assert_eq!(d.kind, FailureKind::AdcExpired);
                assert_eq!(d.fix_command.as_deref(), Some(ADC_FIX));
            }
            Preflight::Ok => panic!("expected block on missing ADC"),
        }
    }

    #[test]
    fn check_blocks_when_connection_names_empty() {
        let (listener, port) = occupied_port();
        drop(listener);
        let profile = profile_with([port, port + 1], false);
        match check(&profile, None) {
            Preflight::Blocked(d) => assert_eq!(d.kind, FailureKind::StaleInstance),
            Preflight::Ok => panic!("expected block on empty connection names"),
        }
    }

    #[test]
    fn check_passes_when_everything_ready() {
        let dir = tempfile::tempdir().unwrap();
        let adc = dir.path().join("application_default_credentials.json");
        std::fs::write(&adc, "{}").unwrap();
        let (l1, p1) = occupied_port();
        let (l2, p2) = occupied_port();
        drop(l1);
        drop(l2);
        let profile = profile_with([p1, p2], true);
        assert_eq!(check(&profile, Some(&adc)), Preflight::Ok);
    }

    #[test]
    fn private_dns_known_for_three_envs_only() {
        let mut p = profile_with([15432, 15433], true);
        for env in ["dev", "stg", "prd"] {
            p.name = env.to_string();
            assert_eq!(private_dns_for(&p), Some(format!("pg.{env}.internal.example.com")));
        }
        p.name = "sandbox".to_string();
        assert_eq!(private_dns_for(&p), None);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test core::preflight`
Expected: 7 tests pass. These bind only ephemeral loopback ports, so they are safe to run anywhere.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "Add preflight checks for ports, credentials, and missing connection names"
```

---

## Task 6: ProxyManager — spawn and kill children

**Files:**
- Create: `src-tauri/src/core/proxy.rs`
- Create: `src-tauri/tests/fixtures/fake-proxy.sh`
- Create: `src-tauri/tests/proxy_manager.rs`

- [ ] **Step 1: Create the fake proxy binary `src-tauri/tests/fixtures/fake-proxy.sh`**

It mimics the real proxy's behavior: prints a ready line to stderr, then blocks. Modes let tests drive failure paths.

```bash
#!/bin/bash
# Fake cloud-sql-proxy for tests.
# FAKE_PROXY_MODE=ready   : print ready line, then sleep forever
# FAKE_PROXY_MODE=bind    : print an address-in-use error, exit 1
# FAKE_PROXY_MODE=crash   : print ready line, then exit 1 after a moment
set -u
mode="${FAKE_PROXY_MODE:-ready}"
echo "fake-proxy args: $*" >&2
case "$mode" in
  bind)
    echo "failed to start listener: listen tcp 127.0.0.1:15432: bind: address already in use" >&2
    exit 1
    ;;
  crash)
    echo "The proxy has started successfully and is ready for new connections!" >&2
    sleep 0.2
    echo "unexpected shutdown" >&2
    exit 1
    ;;
  *)
    echo "The proxy has started successfully and is ready for new connections!" >&2
    while true; do sleep 1; done
    ;;
esac
```

Make it executable:
```bash
chmod +x src-tauri/tests/fixtures/fake-proxy.sh
```

- [ ] **Step 2: Write `src-tauri/src/core/proxy.rs`**

`kill_on_drop(true)` plus an explicit `kill_all` is the orphan guarantee. Children are also killed when the manager is dropped, which covers panics.

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::core::log_watcher::{classify, Diagnosis, ProxyEvent};
use crate::core::profile::Profile;

/// Where a profile is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyStatus {
    Stopped,
    /// Spawned but the ready line has not appeared yet.
    Starting,
    Running,
    Failed(Diagnosis),
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("failed to spawn {binary}: {source}")]
    Spawn { binary: String, source: std::io::Error },
    #[error("profile '{0}' is already running")]
    AlreadyRunning(String),
}

/// A line of output kept for the log view.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub profile_id: String,
    pub text: String,
}

struct Running {
    child: Child,
}

/// Owns every cloud-sql-proxy child process.
///
/// Status and logs are shared with the UI through `Arc<Mutex<..>>`
/// because output arrives on background tasks.
pub struct ProxyManager {
    binary: PathBuf,
    running: HashMap<String, Running>,
    status: Arc<Mutex<HashMap<String, ProxyStatus>>>,
    logs: Arc<Mutex<Vec<LogLine>>>,
    max_log_lines: usize,
}

impl ProxyManager {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            running: HashMap::new(),
            status: Arc::new(Mutex::new(HashMap::new())),
            logs: Arc::new(Mutex::new(Vec::new())),
            max_log_lines: 2000,
        }
    }

    pub fn status_handle(&self) -> Arc<Mutex<HashMap<String, ProxyStatus>>> {
        Arc::clone(&self.status)
    }

    pub fn logs_handle(&self) -> Arc<Mutex<Vec<LogLine>>> {
        Arc::clone(&self.logs)
    }

    pub async fn status_of(&self, profile_id: &str) -> ProxyStatus {
        self.status
            .lock()
            .await
            .get(profile_id)
            .cloned()
            .unwrap_or(ProxyStatus::Stopped)
    }

    pub fn is_running(&self, profile_id: &str) -> bool {
        self.running.contains_key(profile_id)
    }

    pub fn running_ids(&self) -> Vec<String> {
        self.running.keys().cloned().collect()
    }

    /// Build the argument list for a profile.
    fn args_for(profile: &Profile) -> Vec<String> {
        let mut args = Vec::new();
        if profile.flags.auto_iam_authn {
            args.push("--auto-iam-authn".to_string());
        }
        if profile.flags.private_ip {
            args.push("--private-ip".to_string());
        }
        if let Some(sa) = &profile.impersonate_service_account {
            if !sa.is_empty() {
                args.push("--impersonate-service-account".to_string());
                args.push(sa.clone());
            }
        }
        args.extend(profile.instance_args());
        args
    }

    /// Spawn the proxy for a profile and start reading its output.
    pub async fn start(&mut self, profile: &Profile) -> Result<(), ProxyError> {
        if self.running.contains_key(&profile.id) {
            return Err(ProxyError::AlreadyRunning(profile.id.clone()));
        }

        let args = Self::args_for(profile);
        let mut child = Command::new(&self.binary)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ProxyError::Spawn {
                binary: self.binary.display().to_string(),
                source,
            })?;

        self.status
            .lock()
            .await
            .insert(profile.id.clone(), ProxyStatus::Starting);

        // The proxy writes almost everything to stderr; read both anyway.
        let stderr = child.stderr.take();
        let stdout = child.stdout.take();
        for stream in [stderr.map(Streams::Err), stdout.map(Streams::Out)] {
            if let Some(stream) = stream {
                self.spawn_reader(profile.id.clone(), stream);
            }
        }

        self.running.insert(profile.id.clone(), Running { child });
        Ok(())
    }

    fn spawn_reader(&self, profile_id: String, stream: Streams) {
        let status = Arc::clone(&self.status);
        let logs = Arc::clone(&self.logs);
        let max = self.max_log_lines;
        tokio::spawn(async move {
            let reader: Box<dyn AsyncBufReadExt + Unpin + Send> = match stream {
                Streams::Err(s) => Box::new(BufReader::new(s)),
                Streams::Out(s) => Box::new(BufReader::new(s)),
            };
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                {
                    let mut log = logs.lock().await;
                    log.push(LogLine { profile_id: profile_id.clone(), text: line.clone() });
                    let overflow = log.len().saturating_sub(max);
                    if overflow > 0 {
                        log.drain(0..overflow);
                    }
                }
                match classify(&line) {
                    ProxyEvent::Ready => {
                        status.lock().await.insert(profile_id.clone(), ProxyStatus::Running);
                    }
                    ProxyEvent::Failure(d) => {
                        status
                            .lock()
                            .await
                            .insert(profile_id.clone(), ProxyStatus::Failed(d));
                    }
                    ProxyEvent::Noise => {}
                }
            }
        });
    }

    /// Stop one profile's proxy.
    pub async fn stop(&mut self, profile_id: &str) {
        if let Some(mut running) = self.running.remove(profile_id) {
            let _ = running.child.kill().await;
        }
        self.status
            .lock()
            .await
            .insert(profile_id.to_string(), ProxyStatus::Stopped);
    }

    /// Stop everything. Called on app exit; must not leave a child
    /// holding a port.
    pub async fn stop_all(&mut self) {
        let ids: Vec<String> = self.running.keys().cloned().collect();
        for id in ids {
            self.stop(&id).await;
        }
    }
}

enum Streams {
    Err(tokio::process::ChildStderr),
    Out(tokio::process::ChildStdout),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn profile() -> Profile {
        Profile {
            id: "dev".into(),
            name: "dev".into(),
            project: "proj".into(),
            region: "us-central1".into(),
            instances: vec![
                Instance { role: InstanceRole::Primary, connection_name: "proj:us-central1:a".into(), port: 15432 },
                Instance { role: InstanceRole::Replica, connection_name: "proj:us-central1:b".into(), port: 15433 },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
        }
    }

    #[test]
    fn args_include_flags_and_instances_in_order() {
        let args = ProxyManager::args_for(&profile());
        assert_eq!(
            args,
            vec![
                "--auto-iam-authn",
                "--private-ip",
                "proj:us-central1:a?port=15432",
                "proj:us-central1:b?port=15433",
            ]
        );
    }

    #[test]
    fn args_omit_disabled_flags() {
        let mut p = profile();
        p.flags = ProxyFlags { auto_iam_authn: false, private_ip: false };
        let args = ProxyManager::args_for(&p);
        assert_eq!(args, vec!["proj:us-central1:a?port=15432", "proj:us-central1:b?port=15433"]);
    }

    #[test]
    fn args_include_impersonation_when_set() {
        let mut p = profile();
        p.impersonate_service_account = Some("sa@proj.iam.gserviceaccount.com".into());
        let args = ProxyManager::args_for(&p);
        assert!(args.contains(&"--impersonate-service-account".to_string()));
        assert!(args.contains(&"sa@proj.iam.gserviceaccount.com".to_string()));
    }

    #[test]
    fn args_omit_impersonation_when_empty_string() {
        let mut p = profile();
        p.impersonate_service_account = Some(String::new());
        let args = ProxyManager::args_for(&p);
        assert!(!args.iter().any(|a| a == "--impersonate-service-account"));
    }
}
```

- [ ] **Step 3: Run the unit tests**

Run: `cd src-tauri && cargo test core::proxy`
Expected: 4 tests pass.

- [ ] **Step 4: Write the integration test `src-tauri/tests/proxy_manager.rs`**

These spawn real processes, so they live outside the unit tests.

```rust
use std::path::PathBuf;

use fh_cloud_sql_proxy_gui::core::profile::{Instance, InstanceRole, Profile, ProxyFlags};
use fh_cloud_sql_proxy_gui::core::proxy::{ProxyManager, ProxyStatus};

fn fake_proxy() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-proxy.sh")
}

fn profile(id: &str) -> Profile {
    Profile {
        id: id.into(),
        name: id.into(),
        project: "proj".into(),
        region: "us-central1".into(),
        instances: vec![
            Instance { role: InstanceRole::Primary, connection_name: "proj:us-central1:a".into(), port: 15432 },
            Instance { role: InstanceRole::Replica, connection_name: "proj:us-central1:b".into(), port: 15433 },
        ],
        flags: ProxyFlags::default(),
        impersonate_service_account: None,
        danger: false,
    }
}

/// Poll until the status matches, or fail after a deadline. Avoids a
/// fixed sleep, which is both slower and flakier.
async fn await_status(manager: &ProxyManager, id: &str, want: impl Fn(&ProxyStatus) -> bool) -> ProxyStatus {
    for _ in 0..100 {
        let status = manager.status_of(id).await;
        if want(&status) {
            return status;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("status never matched; last was {:?}", manager.status_of(id).await);
}

#[tokio::test]
async fn start_reaches_running_after_ready_line() {
    let mut manager = ProxyManager::new(fake_proxy());
    manager.start(&profile("dev")).await.unwrap();
    let status = await_status(&manager, "dev", |s| matches!(s, ProxyStatus::Running)).await;
    assert_eq!(status, ProxyStatus::Running);
    manager.stop_all().await;
}

#[tokio::test]
async fn stop_transitions_to_stopped_and_clears_running() {
    let mut manager = ProxyManager::new(fake_proxy());
    manager.start(&profile("dev")).await.unwrap();
    await_status(&manager, "dev", |s| matches!(s, ProxyStatus::Running)).await;
    manager.stop("dev").await;
    assert_eq!(manager.status_of("dev").await, ProxyStatus::Stopped);
    assert!(!manager.is_running("dev"));
}

#[tokio::test]
async fn starting_same_profile_twice_is_rejected() {
    let mut manager = ProxyManager::new(fake_proxy());
    manager.start(&profile("dev")).await.unwrap();
    let err = manager.start(&profile("dev")).await;
    assert!(err.is_err());
    manager.stop_all().await;
}

#[tokio::test]
async fn bind_failure_is_classified_as_port_in_use() {
    std::env::set_var("FAKE_PROXY_MODE", "bind");
    let mut manager = ProxyManager::new(fake_proxy());
    manager.start(&profile("dev")).await.unwrap();
    let status = await_status(&manager, "dev", |s| matches!(s, ProxyStatus::Failed(_))).await;
    match status {
        ProxyStatus::Failed(d) => assert!(d.message.contains("15432"), "got: {}", d.message),
        other => panic!("expected failure, got {other:?}"),
    }
    manager.stop_all().await;
    std::env::remove_var("FAKE_PROXY_MODE");
}

#[tokio::test]
async fn missing_binary_returns_spawn_error() {
    let mut manager = ProxyManager::new("/nonexistent/cloud-sql-proxy");
    let err = manager.start(&profile("dev")).await;
    assert!(err.is_err());
}

#[tokio::test]
async fn stop_all_kills_every_child() {
    let mut manager = ProxyManager::new(fake_proxy());
    manager.start(&profile("dev")).await.unwrap();
    manager.start(&profile("stg")).await.unwrap();
    assert_eq!(manager.running_ids().len(), 2);
    manager.stop_all().await;
    assert!(manager.running_ids().is_empty());
}

#[tokio::test]
async fn logs_are_captured_for_the_profile() {
    let mut manager = ProxyManager::new(fake_proxy());
    let logs = manager.logs_handle();
    manager.start(&profile("dev")).await.unwrap();
    await_status(&manager, "dev", |s| matches!(s, ProxyStatus::Running)).await;
    let captured = logs.lock().await;
    assert!(captured.iter().any(|l| l.profile_id == "dev" && l.text.contains("ready for new connections")));
    drop(captured);
    manager.stop_all().await;
}
```

- [ ] **Step 5: Expose the crate as a library so integration tests can import it**

Create `src-tauri/src/lib.rs`:

```rust
pub mod core;
```

Then in `src-tauri/Cargo.toml`, add above `[build-dependencies]`:

```toml
[lib]
name = "fh_cloud_sql_proxy_gui"
path = "src/lib.rs"

[[bin]]
name = "fh-cloud-sql-proxy-gui"
path = "src/main.rs"
```

And change `src-tauri/src/main.rs` to use the library instead of its own module tree — replace `mod core;` with:

```rust
use fh_cloud_sql_proxy_gui::core;
```

- [ ] **Step 6: Run the integration tests**

Run: `cd src-tauri && cargo test --test proxy_manager`
Expected: 7 tests pass. The `FAKE_PROXY_MODE` test sets a process-wide env var, so run with `--test-threads=1` if it interferes:
`cargo test --test proxy_manager -- --test-threads=1`

- [ ] **Step 7: Verify no orphan processes remain**

Run: `pgrep -f fake-proxy.sh`
Expected: no output (exit code 1). If PIDs are listed, `kill_on_drop` is not working — fix before continuing.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "Add ProxyManager with kill-on-drop child ownership and log capture"
```

---

## Task 7: gcloud discovery and reconciliation

**Files:**
- Create: `src-tauri/src/core/discovery.rs`
- Create: `src-tauri/tests/fixtures/gcloud_instances_list.txt`

- [ ] **Step 1: Create the fixture `src-tauri/tests/fixtures/gcloud_instances_list.txt`**

This is the tab-separated shape of `--format='value(name,instanceType,connectionName)'`.

```
primary-instance	CLOUD_SQL_INSTANCE	my-project-dev:us-central1:primary-instance
replica-instance	READ_REPLICA_INSTANCE	my-project-dev:us-central1:replica-instance
```

- [ ] **Step 2: Write `src-tauri/src/core/discovery.rs` with tests**

Parsing is separated from the `gcloud` call so it is testable without the network.

```rust
use crate::core::profile::{InstanceRole, Profile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInstance {
    pub name: String,
    pub role: InstanceRole,
    pub connection_name: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    #[error("gcloud returned no instances for project '{0}'")]
    NoInstances(String),
    #[error("could not parse gcloud output: {0}")]
    Parse(String),
    #[error("gcloud failed: {0}")]
    Command(String),
}

/// Parse `gcloud sql instances list --format='value(name,instanceType,connectionName)'`.
///
/// Columns are tab-separated. Rows with an unrecognized instanceType are
/// skipped rather than failing the whole parse, because Cloud SQL may add
/// new types we do not care about.
pub fn parse_instances(output: &str) -> Result<Vec<DiscoveredInstance>, DiscoveryError> {
    let mut found = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        if cols.len() < 3 {
            return Err(DiscoveryError::Parse(format!("expected 3 columns, got {}: {line}", cols.len())));
        }
        let role = match cols[1] {
            "CLOUD_SQL_INSTANCE" => InstanceRole::Primary,
            "READ_REPLICA_INSTANCE" => InstanceRole::Replica,
            _ => continue,
        };
        found.push(DiscoveredInstance {
            name: cols[0].to_string(),
            role,
            connection_name: cols[2].to_string(),
        });
    }
    Ok(found)
}

/// A proposed change to a profile's stored connection name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub profile_id: String,
    pub role: InstanceRole,
    pub from: String,
    pub to: String,
}

/// Compare what gcloud reports against what the profile stores.
/// Returns only the differences, so the UI can show them for confirmation
/// instead of silently rewriting config.
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

/// Apply confirmed changes to a profile in place.
pub fn apply(profile: &mut Profile, changes: &[Change]) {
    for change in changes {
        if change.profile_id != profile.id {
            continue;
        }
        if let Some(instance) = profile.instances.iter_mut().find(|i| i.role == change.role) {
            instance.connection_name = change.to.clone();
        }
    }
}

/// Build the gcloud argument list for a project.
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

    fn profile_with(primary: &str, replica: &str) -> Profile {
        Profile {
            id: "dev".into(),
            name: "dev".into(),
            project: "my-project-dev".into(),
            region: "us-central1".into(),
            instances: vec![
                Instance { role: InstanceRole::Primary, connection_name: primary.into(), port: 15432 },
                Instance { role: InstanceRole::Replica, connection_name: replica.into(), port: 15433 },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
        }
    }

    #[test]
    fn parses_primary_and_replica_from_fixture() {
        let found = parse_instances(FIXTURE).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].role, InstanceRole::Primary);
        assert_eq!(found[1].role, InstanceRole::Replica);
        assert!(found[0].connection_name.ends_with("primary-instance"));
    }

    #[test]
    fn skips_unknown_instance_types() {
        let output = "a\tCLOUD_SQL_INSTANCE\tproj:r:a\nb\tSOMETHING_NEW\tproj:r:b\n";
        let found = parse_instances(output).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn rejects_rows_with_too_few_columns() {
        let output = "a\tCLOUD_SQL_INSTANCE\n";
        assert!(matches!(parse_instances(output), Err(DiscoveryError::Parse(_))));
    }

    #[test]
    fn empty_output_parses_to_empty_list() {
        assert_eq!(parse_instances("").unwrap(), vec![]);
        assert_eq!(parse_instances("\n  \n").unwrap(), vec![]);
    }

    #[test]
    fn reconcile_reports_no_changes_when_names_match() {
        let discovered = parse_instances(FIXTURE).unwrap();
        let profile = profile_with(
            "my-project-dev:us-central1:primary-instance",
            "my-project-dev:us-central1:replica-instance",
        );
        assert!(reconcile(&profile, &discovered).is_empty());
    }

    #[test]
    fn reconcile_detects_terraform_drift() {
        let discovered = parse_instances(FIXTURE).unwrap();
        let profile = profile_with("stale-primary", "stale-replica");
        let changes = reconcile(&profile, &discovered);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].from, "stale-primary");
        assert!(changes[0].to.contains("terraform-20231109"));
    }

    #[test]
    fn reconcile_fills_empty_connection_names() {
        let discovered = parse_instances(FIXTURE).unwrap();
        let profile = profile_with("", "");
        let changes = reconcile(&profile, &discovered);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].from, "");
    }

    #[test]
    fn apply_writes_changes_into_profile() {
        let discovered = parse_instances(FIXTURE).unwrap();
        let mut profile = profile_with("", "");
        let changes = reconcile(&profile, &discovered);
        apply(&mut profile, &changes);
        assert!(profile.instances[0].connection_name.contains("terraform-20231109"));
        assert!(profile.instances[1].connection_name.contains("terraform-20260107"));
        assert!(reconcile(&profile, &discovered).is_empty());
    }

    #[test]
    fn apply_ignores_changes_for_other_profiles() {
        let mut profile = profile_with("keep", "keep2");
        let changes = vec![Change {
            profile_id: "other".into(),
            role: InstanceRole::Primary,
            from: "keep".into(),
            to: "overwritten".into(),
        }];
        apply(&mut profile, &changes);
        assert_eq!(profile.instances[0].connection_name, "keep");
    }

    #[test]
    fn gcloud_args_include_project_and_billing_project() {
        let args = gcloud_args("my-project-dev");
        assert!(args.contains(&"--project=my-project-dev".to_string()));
        assert!(args.contains(&"--billing-project=my-project-dev".to_string()));
        assert!(args.iter().any(|a| a.starts_with("--format=value(")));
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cd src-tauri && cargo test core::discovery`
Expected: 10 tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add gcloud instance discovery with drift reconciliation"
```

---

## Task 8: AppState — start policy and exclusivity

**Files:**
- Create: `src-tauri/src/core/state.rs`

- [ ] **Step 1: Write `src-tauri/src/core/state.rs` with tests**

This is where exclusive-by-default lives. It is a pure decision function, so it tests without processes.

```rust
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

/// Decide how to start `target` given what is already running.
///
/// Exclusive by default: because every environment uses the documented
/// 15432/15433 ports, starting one normally requires stopping another.
/// Profiles given non-overlapping ports start concurrently with no prompt.
pub fn plan_start(config: &ProfileConfig, target: &Profile, running_ids: &[String]) -> StartPlan {
    let conflicting: Vec<String> = running_ids
        .iter()
        .filter(|id| id.as_str() != target.id)
        .filter_map(|id| config.profiles.iter().find(|p| &p.id == id))
        .filter(|running| ProfileConfig::ports_overlap(running, target))
        .map(|p| p.id.clone())
        .collect();

    if conflicting.is_empty() {
        StartPlan::Start
    } else {
        StartPlan::StopThenStart(conflicting)
    }
}

/// Should starting this profile ask for confirmation on its own merits?
/// True for production, so prd is never one unguarded click away.
pub fn requires_confirmation(target: &Profile) -> bool {
    target.danger
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn profile(id: &str, ports: [u16; 2], danger: bool) -> Profile {
        Profile {
            id: id.into(),
            name: id.into(),
            project: "proj".into(),
            region: "us-central1".into(),
            instances: vec![
                Instance { role: InstanceRole::Primary, connection_name: "proj:r:a".into(), port: ports[0] },
                Instance { role: InstanceRole::Replica, connection_name: "proj:r:b".into(), port: ports[1] },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger,
        }
    }

    fn config() -> ProfileConfig {
        ProfileConfig {
            version: 1,
            profiles: vec![
                profile("dev", [15432, 15433], false),
                profile("stg", [15432, 15433], false),
                profile("prd", [25432, 25433], true),
            ],
        }
    }

    #[test]
    fn starts_directly_when_nothing_running() {
        let c = config();
        let target = c.profiles[0].clone();
        assert_eq!(plan_start(&c, &target, &[]), StartPlan::Start);
    }

    #[test]
    fn requires_stopping_a_profile_that_shares_ports() {
        let c = config();
        let target = c.profiles[1].clone(); // stg, same ports as dev
        assert_eq!(
            plan_start(&c, &target, &["dev".to_string()]),
            StartPlan::StopThenStart(vec!["dev".to_string()])
        );
    }

    #[test]
    fn starts_concurrently_when_ports_do_not_overlap() {
        let c = config();
        let target = c.profiles[2].clone(); // prd on 25432/25433
        assert_eq!(plan_start(&c, &target, &["dev".to_string()]), StartPlan::Start);
    }

    #[test]
    fn ignores_the_target_itself_in_running_list() {
        let c = config();
        let target = c.profiles[0].clone();
        assert_eq!(plan_start(&c, &target, &["dev".to_string()]), StartPlan::Start);
    }

    #[test]
    fn lists_every_conflicting_profile() {
        let mut c = config();
        c.profiles[2] = profile("prd", [15432, 15433], true);
        let target = c.profiles[2].clone();
        let plan = plan_start(&c, &target, &["dev".to_string(), "stg".to_string()]);
        match plan {
            StartPlan::StopThenStart(mut ids) => {
                ids.sort();
                assert_eq!(ids, vec!["dev".to_string(), "stg".to_string()]);
            }
            other => panic!("expected StopThenStart, got {other:?}"),
        }
    }

    #[test]
    fn production_requires_confirmation() {
        assert!(requires_confirmation(&profile("prd", [25432, 25433], true)));
        assert!(!requires_confirmation(&profile("dev", [15432, 15433], false)));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd src-tauri && cargo test core::state`
Expected: 6 tests pass.

- [ ] **Step 3: Run the whole core suite**

Run: `cd src-tauri && cargo test`
Expected: all unit and integration tests pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "Add start-planning policy for exclusive-by-default profiles"
```

---

## Task 9: Tauri commands for the webview

**Files:**
- Create: `src-tauri/src/commands.rs`
- Create: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/main.rs`

- [ ] **Step 1: Create `src-tauri/src/app_state.rs`**

The managed state the Tauri layer shares between the tray and the webview.

```rust
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use fh_cloud_sql_proxy_gui::core::profile::ProfileConfig;
use fh_cloud_sql_proxy_gui::core::proxy::ProxyManager;

pub struct Shared {
    pub config: Mutex<ProfileConfig>,
    pub config_path: PathBuf,
    pub manager: Mutex<ProxyManager>,
}

pub type SharedState = Arc<Shared>;
```

- [ ] **Step 2: Create `src-tauri/src/commands.rs`**

```rust
use fh_cloud_sql_proxy_gui::core::discovery::{self, Change};
use fh_cloud_sql_proxy_gui::core::preflight::{self, Preflight};
use fh_cloud_sql_proxy_gui::core::profile::{Profile, ProfileConfig};
use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;
use fh_cloud_sql_proxy_gui::core::state::{plan_start, StartPlan};
use fh_cloud_sql_proxy_gui::core::store;
use serde::Serialize;
use tauri::State;

use crate::app_state::SharedState;

#[derive(Serialize)]
pub struct ProfileView {
    #[serde(flatten)]
    pub profile: Profile,
    pub status: String,
    pub detail: Option<String>,
}

fn status_label(status: &ProxyStatus) -> (String, Option<String>) {
    match status {
        ProxyStatus::Stopped => ("stopped".into(), None),
        ProxyStatus::Starting => ("starting".into(), None),
        ProxyStatus::Running => ("running".into(), None),
        ProxyStatus::Failed(d) => ("failed".into(), Some(d.message.clone())),
    }
}

#[tauri::command]
pub async fn list_profiles(state: State<'_, SharedState>) -> Result<Vec<ProfileView>, String> {
    let config = state.config.lock().await;
    let manager = state.manager.lock().await;
    let mut views = Vec::new();
    for profile in &config.profiles {
        let (status, detail) = status_label(&manager.status_of(&profile.id).await);
        views.push(ProfileView { profile: profile.clone(), status, detail });
    }
    Ok(views)
}

#[tauri::command]
pub async fn save_profiles(
    state: State<'_, SharedState>,
    profiles: Vec<Profile>,
) -> Result<(), String> {
    let next = ProfileConfig {
        version: fh_cloud_sql_proxy_gui::core::profile::CURRENT_SCHEMA_VERSION,
        profiles,
    };
    next.validate().map_err(|e| e.to_string())?;
    store::save(&state.config_path, &next).map_err(|e| e.to_string())?;
    *state.config.lock().await = next;
    Ok(())
}

#[tauri::command]
pub async fn start_profile(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    let config = state.config.lock().await.clone();
    let profile = config
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("no profile '{id}'"))?
        .clone();

    let mut manager = state.manager.lock().await;

    // Stop anything sharing a port first (exclusive by default).
    if let StartPlan::StopThenStart(conflicts) = plan_start(&config, &profile, &manager.running_ids()) {
        for other in conflicts {
            manager.stop(&other).await;
        }
    }

    let adc = preflight::adc_path();
    if let Preflight::Blocked(d) = preflight::check(&profile, adc.as_deref()) {
        return Err(d.message);
    }

    manager.start(&profile).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_profile(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    state.manager.lock().await.stop(&id).await;
    Ok(())
}

#[derive(Serialize)]
pub struct RefreshResult {
    pub changes: Vec<ChangeView>,
}

#[derive(Serialize)]
pub struct ChangeView {
    pub profile_id: String,
    pub role: String,
    pub from: String,
    pub to: String,
}

/// Query gcloud for every profile's project and report proposed changes.
/// Nothing is written; `apply_changes` commits after the user confirms.
#[tauri::command]
pub async fn refresh_connection_names(
    state: State<'_, SharedState>,
) -> Result<RefreshResult, String> {
    let config = state.config.lock().await.clone();
    let mut all = Vec::new();
    for profile in &config.profiles {
        let output = tokio::process::Command::new("gcloud")
            .args(discovery::gcloud_args(&profile.project))
            .output()
            .await
            .map_err(|e| format!("could not run gcloud: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "gcloud failed for {}: {}",
                profile.project,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let discovered = discovery::parse_instances(&text).map_err(|e| e.to_string())?;
        all.extend(discovery::reconcile(profile, &discovered));
    }
    Ok(RefreshResult {
        changes: all
            .into_iter()
            .map(|c| ChangeView {
                profile_id: c.profile_id,
                role: format!("{:?}", c.role).to_lowercase(),
                from: c.from,
                to: c.to,
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn apply_changes(
    state: State<'_, SharedState>,
    changes: Vec<ChangeViewInput>,
) -> Result<(), String> {
    let mut config = state.config.lock().await.clone();
    let core_changes: Vec<Change> = changes
        .into_iter()
        .filter_map(|c| {
            let role = match c.role.as_str() {
                "primary" => fh_cloud_sql_proxy_gui::core::profile::InstanceRole::Primary,
                "replica" => fh_cloud_sql_proxy_gui::core::profile::InstanceRole::Replica,
                _ => return None,
            };
            Some(Change { profile_id: c.profile_id, role, from: c.from, to: c.to })
        })
        .collect();

    for profile in &mut config.profiles {
        discovery::apply(profile, &core_changes);
    }
    store::save(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().await = config;
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct ChangeViewInput {
    pub profile_id: String,
    pub role: String,
    pub from: String,
    pub to: String,
}

#[tauri::command]
pub async fn read_logs(state: State<'_, SharedState>, id: Option<String>) -> Result<Vec<String>, String> {
    let manager = state.manager.lock().await;
    let logs = manager.logs_handle();
    let lines = logs.lock().await;
    Ok(lines
        .iter()
        .filter(|l| id.as_ref().is_none_or(|want| &l.profile_id == want))
        .map(|l| format!("[{}] {}", l.profile_id, l.text))
        .collect())
}
```

Note: `is_none_or` requires Rust 1.82+; the toolchain is 1.95, so it is available.

- [ ] **Step 3: Wire commands into `src-tauri/src/main.rs`**

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod tray;

use std::sync::Arc;

use fh_cloud_sql_proxy_gui::core::proxy::ProxyManager;
use fh_cloud_sql_proxy_gui::core::store;
use tokio::sync::Mutex;

use app_state::Shared;

fn main() {
    let config_path = store::default_config_path().expect("no config directory available");
    let config = store::load_or_seed(&config_path).expect("could not load profiles.json");

    let proxy_binary = which_cloud_sql_proxy();
    let shared = Arc::new(Shared {
        config: Mutex::new(config),
        config_path,
        manager: Mutex::new(ProxyManager::new(proxy_binary)),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            commands::list_profiles,
            commands::save_profiles,
            commands::start_profile,
            commands::stop_profile,
            commands::refresh_connection_names,
            commands::apply_changes,
            commands::read_logs,
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            tray::build(app.handle())?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building application")
        .run(move |_app, event| {
            // Killing children on exit is the orphan guarantee: a leaked
            // proxy would keep holding port 15432.
            if let tauri::RunEvent::Exit = event {
                let shared = shared.clone();
                tauri::async_runtime::block_on(async move {
                    shared.manager.lock().await.stop_all().await;
                });
            }
        });
}

/// Locate the proxy binary. Homebrew's path first, then PATH.
fn which_cloud_sql_proxy() -> std::path::PathBuf {
    let homebrew = std::path::PathBuf::from("/opt/homebrew/bin/cloud-sql-proxy");
    if homebrew.exists() {
        return homebrew;
    }
    std::path::PathBuf::from("cloud-sql-proxy")
}
```

- [ ] **Step 4: Verify it compiles (tray module does not exist yet, so expect an error)**

Run: `cd src-tauri && cargo build 2>&1 | tail -20`
Expected: an error about the missing `tray` module. That is fine — Task 10 creates it. Do not commit a non-compiling tree; proceed straight to Task 10 and commit there.

---

## Task 10: Tray menu

**Files:**
- Create: `src-tauri/src/tray.rs`

- [ ] **Step 1: Write `src-tauri/src/tray.rs`**

Menu items are mutated (`set_text`, `set_checked`) rather than rebuilt, which avoids flicker and is the API's supported path.

```rust
use std::sync::Arc;

use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};

use crate::app_state::SharedState;

/// Build the tray icon and its menu, then start a task that keeps the
/// menu's text and checkmarks in sync with proxy status.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let state = app.state::<SharedState>().inner().clone();
    let profiles = tauri::async_runtime::block_on(async {
        state.config.lock().await.profiles.clone()
    });

    let status_item = MenuItem::with_id(app, "status", "Nothing running", false, None::<&str>)?;

    let mut profile_items = Vec::new();
    for profile in &profiles {
        let label = if profile.danger {
            format!("⚠ {}  ({}:{})", profile.name, "127.0.0.1", profile.instances[0].port)
        } else {
            format!("{}  ({}:{})", profile.name, "127.0.0.1", profile.instances[0].port)
        };
        let item = CheckMenuItem::with_id(
            app,
            format!("toggle:{}", profile.id),
            label,
            true,
            false,
            None::<&str>,
        )?;
        profile_items.push(item);
    }

    let profiles_window = MenuItem::with_id(app, "open:profiles", "Profiles…", true, None::<&str>)?;
    let logs_window = MenuItem::with_id(app, "open:logs", "Logs…", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "Refresh connection names", true, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(app, "autostart", "Launch at Login", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, Some("CmdOrCtrl+Q"))?;
    let sep = || PredefinedMenuItem::separator(app);

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<R>> = vec![&status_item];
    let s1 = sep()?;
    items.push(&s1);
    for item in &profile_items {
        items.push(item);
    }
    let s2 = sep()?;
    items.push(&s2);
    items.push(&profiles_window);
    items.push(&logs_window);
    items.push(&refresh);
    items.push(&autostart);
    let s3 = sep()?;
    items.push(&s3);
    items.push(&quit);

    let menu = Menu::with_items(app, &items)?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_menu_event(&app, &id).await;
            });
        })
        .build(app)?;

    start_status_sync(app.clone(), status_item, profile_items);
    Ok(())
}

async fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    if id == "quit" {
        // Children are killed by the Exit run-event handler in main.rs.
        app.exit(0);
        return;
    }
    if id == "open:profiles" {
        open_window(app, "profiles", "Profiles", "index.html");
        return;
    }
    if id == "open:logs" {
        open_window(app, "logs", "Logs", "index.html#logs");
        return;
    }
    if let Some(profile_id) = id.strip_prefix("toggle:") {
        toggle_profile(app, profile_id).await;
    }
}

async fn toggle_profile<R: Runtime>(app: &AppHandle<R>, profile_id: &str) {
    let state = app.state::<SharedState>().inner().clone();
    let running = {
        let manager = state.manager.lock().await;
        manager.is_running(profile_id)
    };
    if running {
        state.manager.lock().await.stop(profile_id).await;
    } else {
        // Reuse the same code path the webview uses, so behavior cannot drift.
        let config = state.config.lock().await.clone();
        if let Some(profile) = config.profiles.iter().find(|p| p.id == profile_id) {
            let mut manager = state.manager.lock().await;
            use fh_cloud_sql_proxy_gui::core::state::{plan_start, StartPlan};
            if let StartPlan::StopThenStart(conflicts) =
                plan_start(&config, profile, &manager.running_ids())
            {
                for other in conflicts {
                    manager.stop(&other).await;
                }
            }
            let adc = fh_cloud_sql_proxy_gui::core::preflight::adc_path();
            use fh_cloud_sql_proxy_gui::core::preflight::{check, Preflight};
            match check(profile, adc.as_deref()) {
                Preflight::Blocked(_) => { /* surfaced in the status line by sync */ }
                Preflight::Ok => {
                    let _ = manager.start(profile).await;
                }
            }
        }
    }
}

fn open_window<R: Runtime>(app: &AppHandle<R>, label: &str, title: &str, url: &str) {
    if let Some(existing) = app.get_webview_window(label) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return;
    }
    let _ = tauri::WebviewWindowBuilder::new(app, label, tauri::WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(720.0, 520.0)
        .build();
}

/// Poll status and update the menu. Polling (rather than a channel) keeps
/// the tray layer free of core plumbing; 1s is well under human notice.
fn start_status_sync<R: Runtime>(
    app: AppHandle<R>,
    status_item: MenuItem<R>,
    profile_items: Vec<CheckMenuItem<R>>,
) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<SharedState>().inner().clone();
        loop {
            let profiles = state.config.lock().await.profiles.clone();
            let manager = state.manager.lock().await;

            let mut summary_parts = Vec::new();
            for (item, profile) in profile_items.iter().zip(profiles.iter()) {
                let status = manager.status_of(&profile.id).await;
                let _ = item.set_checked(matches!(status, ProxyStatus::Running | ProxyStatus::Starting));
                let mark = match &status {
                    ProxyStatus::Running => "",
                    ProxyStatus::Starting => "  (starting…)",
                    ProxyStatus::Failed(_) => "  (failed)",
                    ProxyStatus::Stopped => "",
                };
                let danger = if profile.danger { "⚠ " } else { "" };
                let ports: Vec<String> = profile.instances.iter().map(|i| i.port.to_string()).collect();
                let _ = item.set_text(format!("{danger}{}  ({}){mark}", profile.name, ports.join("/")));

                match &status {
                    ProxyStatus::Running => summary_parts.push(format!(
                        "{} — 127.0.0.1:{}",
                        profile.name,
                        ports.join(", :")
                    )),
                    ProxyStatus::Failed(d) => summary_parts.push(format!("{}: {}", profile.name, d.message)),
                    _ => {}
                }
            }
            drop(manager);

            let _ = status_item.set_text(if summary_parts.is_empty() {
                "Nothing running".to_string()
            } else {
                summary_parts.join("  |  ")
            });

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}
```

- [ ] **Step 2: Add `Clone` where the plan needs it**

`SharedState` is `Arc<Shared>`, which is already `Clone`. Verify `app.state::<SharedState>().inner().clone()` compiles; if the borrow checker objects, bind it as `let state: SharedState = Arc::clone(app.state::<SharedState>().inner());`.

- [ ] **Step 3: Build**

Run: `cd src-tauri && cargo build 2>&1 | tail -30`
Expected: compiles. Fix any type errors reported; the likely ones are menu-item trait-object lifetimes (`IsMenuItem`) — if `Vec<&dyn IsMenuItem<R>>` fights the borrow checker, build the menu with `MenuBuilder` instead:

```rust
use tauri::menu::MenuBuilder;
let mut builder = MenuBuilder::new(app).item(&status_item).separator();
for item in &profile_items { builder = builder.item(item); }
let menu = builder
    .separator()
    .item(&profiles_window)
    .item(&logs_window)
    .item(&refresh)
    .item(&autostart)
    .separator()
    .item(&quit)
    .build()?;
```

- [ ] **Step 4: Run the app and verify the menu appears**

Run: `cd .. && npx tauri dev`
Expected: no Dock icon; a tray icon appears in the menu bar; clicking it shows the menu with dev/stg/prd, "Nothing running" at top, and Quit at the bottom.

- [ ] **Step 5: Verify Quit leaves no orphan**

With the app running, click a profile to start it (it will fail without connection names — that is expected at this stage), then Quit. Run:
```bash
pgrep -f cloud-sql-proxy
```
Expected: no output.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Add tray menu with per-profile toggles and live status sync"
```

---

## Task 11: Profiles and logs webview

**Files:**
- Modify: `src/index.html`, `src/profiles.js`, `src/styles.css`

- [ ] **Step 1: Write `src/index.html`**

```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <title>Cloud SQL Proxy</title>
    <link rel="stylesheet" href="/styles.css" />
  </head>
  <body>
    <nav>
      <button data-view="profiles" class="active">Profiles</button>
      <button data-view="logs">Logs</button>
    </nav>

    <section id="profiles-view">
      <div id="profile-list"></div>
      <div class="row">
        <button id="refresh">Refresh connection names</button>
        <button id="save" class="primary">Save</button>
      </div>
      <p id="message" role="status"></p>
    </section>

    <section id="logs-view" hidden>
      <pre id="log-output"></pre>
    </section>

    <script type="module" src="/profiles.js"></script>
  </body>
</html>
```

- [ ] **Step 2: Write `src/styles.css`**

```css
:root { color-scheme: light dark; }
body {
  font: 13px -apple-system, system-ui, sans-serif;
  margin: 0;
  padding: 16px;
}
nav { display: flex; gap: 4px; margin-bottom: 16px; }
nav button {
  background: transparent;
  border: none;
  padding: 6px 10px;
  border-radius: 6px;
  font: inherit;
}
nav button.active { background: color-mix(in srgb, currentColor 12%, transparent); }
.profile {
  border: 1px solid color-mix(in srgb, currentColor 15%, transparent);
  border-radius: 8px;
  padding: 12px;
  margin-bottom: 12px;
}
.profile.danger { border-color: #c0392b; }
.profile h3 { margin: 0 0 8px; display: flex; align-items: center; gap: 8px; }
.badge { font-size: 11px; padding: 2px 6px; border-radius: 4px; background: color-mix(in srgb, currentColor 12%, transparent); }
.badge.running { background: #1e8e3e; color: white; }
.badge.failed { background: #c0392b; color: white; }
.badge.starting { background: #f39c12; color: white; }
.field { display: grid; grid-template-columns: 90px 1fr 80px; gap: 8px; align-items: center; margin-bottom: 6px; }
input { font: inherit; padding: 4px 6px; border-radius: 4px; border: 1px solid color-mix(in srgb, currentColor 25%, transparent); background: transparent; color: inherit; }
.row { display: flex; gap: 8px; margin-top: 8px; }
button.primary { background: #1a73e8; color: white; border: none; padding: 6px 14px; border-radius: 6px; font: inherit; }
#message { min-height: 1.4em; }
#message.error { color: #c0392b; }
pre { white-space: pre-wrap; word-break: break-all; font-size: 11px; }
```

- [ ] **Step 3: Write `src/profiles.js`**

`withGlobalTauri` exposes `window.__TAURI__.core.invoke`, so no bundler is needed.

```javascript
const { invoke } = window.__TAURI__.core;

const listEl = document.getElementById('profile-list');
const messageEl = document.getElementById('message');
const logEl = document.getElementById('log-output');

let profiles = [];

function say(text, isError = false) {
  messageEl.textContent = text;
  messageEl.classList.toggle('error', isError);
}

function instanceFields(profile, index) {
  const instance = profile.instances[index];
  return `
    <div class="field">
      <label>${instance.role}</label>
      <input data-profile="${profile.id}" data-index="${index}" data-key="connectionName"
             value="${instance.connectionName ?? ''}" placeholder="project:region:instance" />
      <input data-profile="${profile.id}" data-index="${index}" data-key="port"
             type="number" value="${instance.port}" />
    </div>`;
}

function render() {
  listEl.innerHTML = profiles
    .map((p) => `
      <div class="profile ${p.danger ? 'danger' : ''}">
        <h3>${p.danger ? '⚠' : ''} ${p.name}
          <span class="badge ${p.status}">${p.status}</span>
        </h3>
        <div class="field"><label>Project</label>
          <input data-profile="${p.id}" data-key="project" value="${p.project}" /><span></span></div>
        ${p.instances.map((_, i) => instanceFields(p, i)).join('')}
        ${p.detail ? `<p class="error">${p.detail}</p>` : ''}
      </div>`)
    .join('');
}

function readEdits() {
  for (const input of listEl.querySelectorAll('input')) {
    const profile = profiles.find((p) => p.id === input.dataset.profile);
    if (!profile) continue;
    const { key, index } = input.dataset;
    if (index === undefined) {
      profile[key] = input.value;
    } else {
      const instance = profile.instances[Number(index)];
      instance[key] = key === 'port' ? Number(input.value) : input.value;
    }
  }
}

async function load() {
  try {
    profiles = await invoke('list_profiles');
    render();
  } catch (err) {
    say(String(err), true);
  }
}

document.getElementById('save').addEventListener('click', async () => {
  readEdits();
  // Strip the view-only fields the backend does not accept.
  const payload = profiles.map(({ status, detail, ...rest }) => rest);
  try {
    await invoke('save_profiles', { profiles: payload });
    say('Saved.');
    await load();
  } catch (err) {
    say(String(err), true);
  }
});

document.getElementById('refresh').addEventListener('click', async () => {
  say('Querying gcloud…');
  try {
    const { changes } = await invoke('refresh_connection_names');
    if (changes.length === 0) {
      say('All connection names are current.');
      return;
    }
    const summary = changes
      .map((c) => `${c.profile_id} ${c.role}: ${c.from || '(empty)'} → ${c.to}`)
      .join('\n');
    if (confirm(`Apply these changes?\n\n${summary}`)) {
      await invoke('apply_changes', { changes });
      say(`Applied ${changes.length} change(s).`);
      await load();
    } else {
      say('No changes applied.');
    }
  } catch (err) {
    say(String(err), true);
  }
});

for (const button of document.querySelectorAll('nav button')) {
  button.addEventListener('click', async () => {
    for (const other of document.querySelectorAll('nav button')) {
      other.classList.toggle('active', other === button);
    }
    const showLogs = button.dataset.view === 'logs';
    document.getElementById('profiles-view').hidden = showLogs;
    document.getElementById('logs-view').hidden = !showLogs;
    if (showLogs) {
      logEl.textContent = (await invoke('read_logs', { id: null })).join('\n');
    }
  });
}

if (location.hash === '#logs') {
  document.querySelector('nav button[data-view="logs"]').click();
}

load();
```

- [ ] **Step 4: Run the app and exercise the editor**

Run: `npx tauri dev`

Then: click the tray icon → Profiles… A window opens listing dev/stg/prd with empty connection names.

- [ ] **Step 5: Verify save rejects duplicate ports within a profile**

In the Profiles window, set dev's primary and replica ports both to 15432 and click Save.
Expected: a red message reading `port 15432 is used by both 'dev' and 'dev'`, and no file written.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Add profile editor and log viewer webview"
```

---

## Task 12: End-to-end verification against real infrastructure

This task has no code. It confirms the app works against actual Cloud SQL, which no unit test can establish.

**Prerequisites:** on the corporate VPN, and `gcloud auth application-default login` completed.

- [ ] **Step 1: Refresh connection names for real**

Run the app, open Profiles…, click "Refresh connection names".
Expected: proposed changes filling in dev/stg/prd primary and replica connection names with `terraform-…` instance IDs. Accept them.

- [ ] **Step 2: Verify the config on disk**

Run:
```bash
cat "$HOME/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json"
```
Expected: all six connection names populated; dev/stg/prd all on ports 15432/15433.

- [ ] **Step 3: Start dev and confirm it reaches Running**

Click the tray icon → click `dev`.
Expected: the item shows `(starting…)` briefly, then the status line reads `dev — 127.0.0.1:15432, :15433`.

- [ ] **Step 4: Connect with psql**

Run:
```bash
psql -h 127.0.0.1 -p 15432 -U "$(gcloud config get-value account)" -d fh_ui_dev -c 'select 1'
```
Expected: `1`. Leave the password prompt empty if asked — the proxy injects the token.

- [ ] **Step 5: Verify exclusivity**

With dev running, click `stg` in the tray menu.
Expected: dev stops, stg starts, and only stg appears in the status line. Confirm with:
```bash
pgrep -fl cloud-sql-proxy
```
Expected: exactly one process.

- [ ] **Step 6: Verify the port-in-use diagnosis**

Stop everything in the app. In a terminal, run:
```bash
python3 -m http.server 15432
```
Then click `dev` in the tray.
Expected: the status line reports `Port 15432 is already in use…`. Stop the Python server.

- [ ] **Step 7: Verify the off-VPN diagnosis**

Disconnect from the VPN, then click `dev`.
Expected: the profile goes to failed with a message naming the VPN. Reconnect afterwards.

- [ ] **Step 8: Verify Quit kills the proxy**

Start dev, confirm `pgrep -fl cloud-sql-proxy` lists a process, then Quit from the tray menu. Run:
```bash
pgrep -fl cloud-sql-proxy
```
Expected: no output.

- [ ] **Step 9: Commit any fixes discovered**

```bash
git add -A
git commit -m "Fix issues found in end-to-end verification"
```

---

## Task 13: README and build instructions

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write `README.md`**

```markdown
# Cloud SQL Proxy GUI

A macOS menu bar app for starting and stopping `cloud-sql-proxy` against
Firsthand's dev, stg, and prd Cloud SQL instances.

## Why

The proxy is normally run by hand with a long command containing
Terraform-generated instance IDs that change over time. This app keeps
those IDs fresh, switches environments in one click, and names the fix
when a connection fails (expired credentials, VPN down, port in use).

## Requirements

- macOS 11+
- `cloud-sql-proxy` (`brew install cloud-sql-proxy`)
- `gcloud`, authenticated: `gcloud auth application-default login`
- Membership in `cloud-sql-users@example.com`
- Corporate VPN — the instances are private-IP only

## Build

```bash
npm install
npx tauri build
```

The app bundle lands in `src-tauri/target/release/bundle/macos/`.

For development: `npx tauri dev`.

## Usage

The app lives in the menu bar and has no Dock icon. Click the icon to
toggle a profile on or off. The status line at the top shows what is
running and on which ports.

Ports follow the team convention: **15432 = primary, 15433 = read
replica**.

### Exclusive by default

All three environments use 15432/15433, so starting one stops any other
that shares those ports. To run two environments at once, give one of
them different ports in Profiles… (for example prd on 25432/25433) —
then they run concurrently.

### Connecting

| Field | Value |
| --- | --- |
| Host | `127.0.0.1` |
| Port | `15432` (primary) or `15433` (replica) |
| User | your Google account email |
| Password | leave blank — the proxy injects an IAM token |
| Database | `fh_ui_<env>` or `fh_knowledge_<env>` |
| SSL mode | disable |

If your client reports `FATAL: empty password returned by client`, you
have a password set; clear it.

## Scope

The app owns the proxy's lifetime: quitting the app stops every proxy it
started. It deliberately does not install a launchd agent or keep
proxies running in the background.

## Tests

```bash
cd src-tauri && cargo test
```

Process management is tested against a fake proxy binary in
`tests/fixtures/`, so the suite needs neither GCP nor the VPN.
```

- [ ] **Step 2: Verify the build instructions work from a clean state**

Run:
```bash
cd src-tauri && cargo clean && cd .. && npx tauri build 2>&1 | tail -5
```
Expected: a bundle is produced. This takes several minutes.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "Add README with build, usage, and connection instructions"
```

---

## Self-review notes

**Spec coverage.** Every spec section maps to a task: architecture and unit boundaries (file structure table, Tasks 2–8), data model (Task 2), profile store and seeding (Task 3), concurrency model and exclusivity (Tasks 2 and 8), failure diagnosis table (Tasks 4 and 5), connection name refresh (Tasks 7 and 9), first run (Task 3 seeding + Task 12 step 1), tray menu layout (Task 10), testing strategy (Tasks 2–8 inline tests, Task 6 integration tests), out-of-scope items (absent by construction, and restated in the README).

**Deviation from the spec's first draft, now reconciled.** The spec originally named `tauri-plugin-shell` for process spawning; the plan uses `tokio::process` for `kill_on_drop` and testability without a Tauri app. The spec's architecture section has been updated to match, and `tauri-plugin-shell` is not a dependency.

**Contradiction found and resolved.** The spec originally said the profile editor "rejects a port already claimed by another profile", which contradicts exclusive-by-default — all three environments intentionally share 15432/15433. Task 3 Step 3 implements the resolution: `validate()` enforces uniqueness only *within* a profile, while cross-profile sharing is reported by `conflicting_ports()` as a concurrency fact rather than an error. The spec's port-handling section has been corrected to match.

**Known risk.** Task 10's menu construction may need `MenuBuilder` instead of a `Vec<&dyn IsMenuItem>`; both forms are given so the implementer is not stuck. Task 10 Step 3 names this explicitly.
