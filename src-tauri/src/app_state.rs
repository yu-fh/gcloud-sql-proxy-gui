//! The shared state every Tauri command (and, from Task 10, the tray menu)
//! reads and mutates: the in-memory profile config, where it came from on
//! disk, and the manager owning the live proxy children.
//!
//! Both fields are `tokio::sync::Mutex` rather than `std::sync::Mutex` because
//! `ProxyManager`'s methods are `async` and the guard has to be held across
//! `.await` points; a `std` guard is not `Send` and cannot cross one.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use fh_cloud_sql_proxy_gui::core::audit::Logger;
use fh_cloud_sql_proxy_gui::core::profile::ProfileConfig;
use fh_cloud_sql_proxy_gui::core::proxy::ProxyManager;

/// Everything the command layer shares.
///
/// # Lock ordering
///
/// **Always acquire `config` before `manager`, and never the reverse.** Two
/// mutexes taken in inconsistent orders by two concurrent commands is a
/// textbook deadlock (Tauri runs commands on a thread pool, so two IPC calls
/// really do overlap), and the only cheap defence is a total order everyone
/// obeys.
///
/// Concretely, the permitted shapes are:
/// - take `config` alone (e.g. `save_profiles`, `add_profile`),
/// - take `manager` alone (e.g. `stop_profile`, `read_logs`),
/// - take `config`, then `manager` while still holding `config`
///   (e.g. `list_profiles`, `start_profile`).
///
/// A command that holds `manager` and then wants `config` must instead drop
/// the manager guard first, or clone the data it needs out of `config` before
/// taking `manager`.
///
/// [`Shared::audit`] is deliberately **not** part of that order. It is not a
/// `tokio::sync::Mutex` at all: internally it is a `std::sync::Mutex` held for
/// the duration of a `Vec::push`, with no `.await` inside it and no other lock
/// taken while it is held. A task can therefore write to it while holding
/// either or both of the two above without any risk of a cycle, which is what
/// makes it callable from everywhere a thing worth recording happens.
pub struct Shared {
    /// The profile config as last loaded or saved. Guarded so that
    /// `store::save` — whose docs require callers to serialize saves — can run
    /// while the guard is held, making validate+write+update one atomic step.
    pub config: Mutex<ProfileConfig>,
    /// Where `config` is persisted. Immutable for the process lifetime, so it
    /// needs no lock of its own.
    pub config_path: PathBuf,
    /// Owns the `cloud-sql-proxy` children. Most of its useful methods take
    /// `&mut self` (they reap exited children first), hence a mutex rather
    /// than an `RwLock`.
    pub manager: Mutex<ProxyManager>,
    /// The audit trail. Not behind a mutex here because it is one internally
    /// and is `Clone` -- see the lock-ordering note above on why it stands
    /// outside the `config`/`manager` order.
    pub audit: Logger,
}

/// What `tauri::Builder::manage` holds and commands receive as
/// `State<'_, SharedState>`.
pub type SharedState = Arc<Shared>;

impl Shared {
    /// The audit logger is taken from the manager rather than passed
    /// separately, so there is structurally only one: a second logger would
    /// mean two files, or two halves of one trail that nothing interleaves.
    pub fn new(config: ProfileConfig, config_path: PathBuf, manager: ProxyManager) -> SharedState {
        let audit = manager.audit();
        Arc::new(Self {
            config: Mutex::new(config),
            config_path,
            manager: Mutex::new(manager),
            audit,
        })
    }
}
