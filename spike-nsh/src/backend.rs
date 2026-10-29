//! What the tray and the window share: a tokio runtime that owns the proxy
//! children, and a plain-data snapshot of every environment's state.
//!
//! Both UIs read [`snapshot`] (a `std` mutex, never an await) and call
//! [`toggle`] (a spawn, never a block), so neither the AppKit main thread nor
//! nsh's engine thread ever waits on the runtime.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::sync::Mutex as AsyncMutex;

use crate::core::profile::ProfileConfig;
use crate::core::proxy::{ProxyManager, ProxyStatus};
use crate::core::{state, store};

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub ports: String,
    pub phase: Phase,
}

struct Backend {
    rt: Runtime,
    config: AsyncMutex<ProfileConfig>,
    manager: AsyncMutex<ProxyManager>,
    rows: Mutex<Vec<Row>>,
    /// Called (on a runtime thread) when `rows` changed.
    on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

static BACKEND: OnceLock<Arc<Backend>> = OnceLock::new();

fn backend() -> &'static Arc<Backend> {
    BACKEND.get().expect("backend::init ran")
}

/// A config of three fake environments, for a spike that must not touch real
/// Cloud SQL instances. `dev` and `stg` share ports, as the app's own do.
const SEED: &str = r#"{
  "version": 1,
  "profiles": [
    { "id": "dev", "name": "dev", "project": "spike-dev", "region": "us-central1",
      "instances": [
        { "role": "primary", "connectionName": "spike-dev:us-central1:primary", "port": 15432 },
        { "role": "replica", "connectionName": "spike-dev:us-central1:replica", "port": 15433 } ] },
    { "id": "stg", "name": "stg", "project": "spike-stg", "region": "us-central1",
      "instances": [
        { "role": "primary", "connectionName": "spike-stg:us-central1:primary", "port": 15432 },
        { "role": "replica", "connectionName": "spike-stg:us-central1:replica", "port": 15433 } ] },
    { "id": "prd", "name": "prd", "project": "spike-prd", "region": "us-central1",
      "instances": [
        { "role": "primary", "connectionName": "spike-prd:us-central1:primary", "port": 25432 } ] }
  ]
}"#;

fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPIKE_CONFIG") {
        return path.into();
    }
    let dir = std::env::temp_dir().join("nsh-spike");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("profiles.json");
    if !path.exists() {
        std::fs::write(&path, SEED).expect("seed config");
    }
    path
}

/// The binary to spawn. The default is the test fake, which prints the ready
/// line and sleeps: the spike never runs the real proxy unless asked to.
fn proxy_binary() -> PathBuf {
    std::env::var_os("SPIKE_PROXY_BIN").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src-tauri/tests/fixtures/fake-proxy.sh"
        ))
    })
}

pub fn init() {
    let config = store::load_or_init(&config_path()).expect("load config");
    let rt = tokio::runtime::Builder::new_multi_thread()
        // The app is idle nearly all the time; two workers are plenty.
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let manager = ProxyManager::new(proxy_binary());
    let backend = Arc::new(Backend {
        rt,
        config: AsyncMutex::new(config),
        manager: AsyncMutex::new(manager),
        rows: Mutex::new(Vec::new()),
        on_change: Mutex::new(None),
    });
    BACKEND.set(backend.clone()).ok();
    backend.rt.spawn(poll(backend.clone()));
}

pub fn on_change(f: impl Fn() + Send + Sync + 'static) {
    *backend().on_change.lock().unwrap() = Some(Box::new(f));
}

pub fn snapshot() -> Vec<Row> {
    backend().rows.lock().unwrap().clone()
}

/// Connect `id`, or disconnect it if it is running or starting.
pub fn toggle(id: String) {
    let b = backend().clone();
    backend().rt.spawn(async move {
        let config = b.config.lock().await;
        let Some(profile) = config.profiles.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let mut manager = b.manager.lock().await;
        let status = manager.status_of(&id).await;
        if matches!(status, ProxyStatus::Running | ProxyStatus::Starting) {
            manager.stop(&id).await;
        } else {
            // The spike skips the confirmation and the preflight (which needs
            // real ADC credentials); the plan itself is the app's own.
            if let state::StartPlan::StopThenStart(conflicts) =
                state::plan_start(&config, &profile, &manager.running_ids())
            {
                for conflict in &conflicts {
                    manager.stop(conflict).await;
                }
            }
            let _ = manager.start(&profile).await;
        }
        drop(manager);
        drop(config);
        refresh(&b).await;
    });
}

/// Stop every child; called from the main thread on quit.
pub fn stop_all_blocking() {
    let b = backend().clone();
    b.rt.block_on(async { b.manager.lock().await.stop_all().await });
}

async fn refresh(b: &Backend) {
    let config = b.config.lock().await;
    let statuses = b.manager.lock().await.status_handle();
    let statuses = statuses.lock().await;
    let rows: Vec<Row> = config
        .profiles
        .iter()
        .map(|p| Row {
            id: p.id.clone(),
            name: p.name.clone(),
            ports: p.ports().iter().map(u16::to_string).collect::<Vec<_>>().join(", "),
            phase: match statuses.get(&p.id) {
                None | Some(ProxyStatus::Stopped) => Phase::Stopped,
                Some(ProxyStatus::Starting) => Phase::Starting,
                Some(ProxyStatus::Running) => Phase::Running,
                Some(ProxyStatus::Failed(d)) => Phase::Failed(d.message.clone()),
            },
        })
        .collect();
    let changed = {
        let mut slot = b.rows.lock().unwrap();
        let changed = *slot != rows;
        *slot = rows;
        changed
    };
    if changed {
        if let Some(f) = b.on_change.lock().unwrap().as_ref() {
            f();
        }
    }
}

/// Re-reads statuses: quickly while something is starting, slowly otherwise
/// (a child can still die on its own, and the tray must notice).
async fn poll(b: Arc<Backend>) {
    loop {
        refresh(&b).await;
        let starting = b.rows.lock().unwrap().iter().any(|r| r.phase == Phase::Starting);
        tokio::time::sleep(Duration::from_millis(if starting { 200 } else { 2000 })).await;
    }
}
