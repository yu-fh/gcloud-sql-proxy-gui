//! Ownership of the `cloud-sql-proxy` child processes.
//!
//! Each running profile binds local ports (15432 primary, 15433 replica by
//! convention). A leaked child keeps holding those ports, so the next start
//! fails with "address already in use" and the user has to hunt down a stray
//! PID. Preventing that is the whole reason this module drives
//! `tokio::process` directly instead of delegating to `tauri-plugin-shell`:
//! it can set [`kill_on_drop`] and add an explicit [`Drop`] that signals every
//! surviving child.
//!
//! [`kill_on_drop`]: tokio::process::Command::kill_on_drop
//!
//! Status and logs are published through `Arc<Mutex<..>>` handles so the UI
//! layer can read them without going through this type.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::log_watcher::{self, Diagnosis, ProxyEvent};
use super::profile::Profile;

/// How many log lines are retained before the oldest are dropped. A proxy left
/// running all day would otherwise grow this buffer without bound.
const DEFAULT_LOG_CAP: usize = 2000;

/// Where a profile is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyStatus {
    Stopped,
    /// Spawned, but the ready line has not appeared yet.
    Starting,
    Running,
    Failed(Diagnosis),
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("failed to spawn {binary}: {source}")]
    Spawn {
        binary: String,
        source: std::io::Error,
    },
    #[error("profile '{0}' is already running")]
    AlreadyRunning(String),
}

/// A line of output kept for the log view.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub profile_id: String,
    pub text: String,
}

type Statuses = Arc<Mutex<HashMap<String, ProxyStatus>>>;
type Logs = Arc<Mutex<Vec<LogLine>>>;

pub struct ProxyManager {
    /// Path to the `cloud-sql-proxy` binary. Injectable so tests substitute a
    /// fake that never binds a port.
    binary: PathBuf,
    /// Extra environment applied to every child. Scoped to this manager rather
    /// than the process, so tests can vary child behaviour without the data
    /// race that `std::env::set_var` would introduce.
    env: Vec<(OsString, OsString)>,
    /// Live children, keyed by profile id. Owning the [`Child`] here (rather
    /// than moving it into the reader task) is what makes dropping the manager
    /// drop the children, and therefore kill them.
    children: HashMap<String, Child>,
    statuses: Statuses,
    logs: Logs,
    log_cap: usize,
}

impl ProxyManager {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            env: Vec::new(),
            children: HashMap::new(),
            statuses: Arc::new(Mutex::new(HashMap::new())),
            logs: Arc::new(Mutex::new(Vec::new())),
            log_cap: DEFAULT_LOG_CAP,
        }
    }

    /// Set an environment variable on every child this manager spawns.
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Override the retained-log-line cap. Primarily a testing seam.
    pub fn with_log_cap(mut self, cap: usize) -> Self {
        self.log_cap = cap;
        self
    }

    /// Status map shared with the UI.
    pub fn status_handle(&self) -> Statuses {
        Arc::clone(&self.statuses)
    }

    /// Log buffer shared with the UI.
    pub fn logs_handle(&self) -> Logs {
        Arc::clone(&self.logs)
    }

    /// The profile's status, or [`ProxyStatus::Stopped`] if it has never run.
    pub async fn status_of(&self, profile_id: &str) -> ProxyStatus {
        self.statuses
            .lock()
            .await
            .get(profile_id)
            .cloned()
            .unwrap_or(ProxyStatus::Stopped)
    }

    /// Whether a live child is registered for this profile. Reads the child map
    /// rather than the status map, so it is true throughout `Starting` too.
    ///
    /// Takes `&mut self` because it first reaps children that exited on their
    /// own: a child that crashed is not running, and leaving it in the map
    /// would both misreport that and block a restart with `AlreadyRunning`.
    pub fn is_running(&mut self, profile_id: &str) -> bool {
        self.reap_exited();
        self.children.contains_key(profile_id)
    }

    pub fn running_ids(&mut self) -> Vec<String> {
        self.reap_exited();
        self.children.keys().cloned().collect()
    }

    /// Drop children that have already exited.
    ///
    /// `try_wait` reaps without blocking and returns `Ok(Some(_))` once the
    /// process is gone, so this cannot stall on a healthy child. Status is left
    /// to the exit watcher, which has seen every log line and can tell a
    /// diagnosed failure from a plain exit.
    fn reap_exited(&mut self) {
        self.children
            .retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
    }

    /// The OS pid of a running child, if any. Used by tests to assert against
    /// the OS that no child outlives its manager.
    pub fn pid_of(&mut self, profile_id: &str) -> Option<u32> {
        self.reap_exited();
        self.children.get(profile_id)?.id()
    }

    /// Append one line to the shared log buffer, enforcing the cap.
    pub async fn push_log_line(&self, profile_id: &str, text: impl Into<String>) {
        push_log(&self.logs, self.log_cap, profile_id, text.into()).await;
    }

    /// Spawn the proxy for `profile`.
    ///
    /// Returns [`ProxyError::AlreadyRunning`] without spawning anything if a
    /// live child for this profile already exists, and [`ProxyError::Spawn`] if
    /// the binary could not be executed. A profile whose previous child has
    /// already exited can be started again.
    pub async fn start(&mut self, profile: &Profile) -> Result<(), ProxyError> {
        self.reap_exited();
        if self.children.contains_key(&profile.id) {
            return Err(ProxyError::AlreadyRunning(profile.id.clone()));
        }

        let mut command = Command::new(&self.binary);
        command
            .args(args_for(profile))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // The reason this module exists: a child must never outlive the
            // manager and keep holding the profile's ports.
            .kill_on_drop(true);
        for (key, value) in &self.env {
            command.env(key, value);
        }

        let mut child = command.spawn().map_err(|source| ProxyError::Spawn {
            binary: self.binary.display().to_string(),
            source,
        })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        self.set_status(&profile.id, ProxyStatus::Starting).await;

        // Read both streams: the real proxy logs to stderr, but do not assume.
        // Each reader ends when its stream closes, which happens on child exit.
        let streams: Vec<StreamKind> = [
            stdout.map(StreamKind::Stdout),
            stderr.map(StreamKind::Stderr),
        ]
        .into_iter()
        .flatten()
        .collect();

        let mut readers = Vec::with_capacity(streams.len());
        for stream in streams {
            readers.push(spawn_reader(
                stream,
                profile.id.clone(),
                Arc::clone(&self.statuses),
                Arc::clone(&self.logs),
                self.log_cap,
            ));
        }

        // Once every stream has closed the child has exited. If no line
        // diagnosed the exit, the profile is simply no longer running and
        // there is nothing to show the user, so report Stopped rather than
        // inventing a failure. A `Failed` status set from a real diagnosis, or
        // a `Stopped` already set by `stop`, is left untouched.
        spawn_exit_watcher(readers, profile.id.clone(), Arc::clone(&self.statuses));

        self.children.insert(profile.id.clone(), child);
        Ok(())
    }

    /// Kill the profile's child and mark it stopped. A no-op if it is not
    /// running, except that the status is still normalised to `Stopped`.
    pub async fn stop(&mut self, profile_id: &str) {
        if let Some(mut child) = self.children.remove(profile_id) {
            // `kill` sends SIGKILL and awaits the child, so no zombie is left.
            let _ = child.kill().await;
        }
        self.set_status(profile_id, ProxyStatus::Stopped).await;
    }

    /// Kill every child. Called on quit, and by tests to guarantee cleanup.
    pub async fn stop_all(&mut self) {
        for id in self.running_ids() {
            self.stop(&id).await;
        }
    }

    async fn set_status(&self, profile_id: &str, status: ProxyStatus) {
        self.statuses
            .lock()
            .await
            .insert(profile_id.to_string(), status);
    }
}

/// Last-resort orphan guard.
///
/// `kill_on_drop(true)` already arranges for each [`Child`] in `children` to be
/// killed as the map is dropped, but that path relies on a live Tokio runtime
/// to reap the process. Signalling every child explicitly here makes the kill
/// synchronous and unconditional, so a manager dropped during a panic or after
/// the runtime has shut down still cannot leave a process holding the ports.
impl Drop for ProxyManager {
    fn drop(&mut self) {
        for child in self.children.values_mut() {
            let _ = child.start_kill();
        }
    }
}

/// Which stream a reader is draining. Only needed because the two halves have
/// different types; both are treated identically once wrapped.
enum StreamKind {
    Stdout(tokio::process::ChildStdout),
    Stderr(tokio::process::ChildStderr),
}

/// Drain one stream line by line: record every line and act on
/// classifications. The returned handle completes when the stream closes.
fn spawn_reader(
    stream: StreamKind,
    profile_id: String,
    statuses: Statuses,
    logs: Logs,
    log_cap: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        match stream {
            StreamKind::Stdout(s) => {
                consume(BufReader::new(s).lines(), profile_id, statuses, logs, log_cap).await
            }
            StreamKind::Stderr(s) => {
                consume(BufReader::new(s).lines(), profile_id, statuses, logs, log_cap).await
            }
        }
    })
}

async fn consume<R: tokio::io::AsyncBufRead + Unpin>(
    mut lines: tokio::io::Lines<R>,
    profile_id: String,
    statuses: Statuses,
    logs: Logs,
    log_cap: usize,
) {
    while let Ok(Some(line)) = lines.next_line().await {
        push_log(&logs, log_cap, &profile_id, line.clone()).await;

        match log_watcher::classify(&line) {
            ProxyEvent::Ready => {
                statuses
                    .lock()
                    .await
                    .insert(profile_id.clone(), ProxyStatus::Running);
            }
            ProxyEvent::Failure(diagnosis) => {
                statuses
                    .lock()
                    .await
                    .insert(profile_id.clone(), ProxyStatus::Failed(diagnosis));
            }
            ProxyEvent::Noise => {}
        }
    }
}

/// Wait for every reader to finish -- which happens when the child's streams
/// close, i.e. when it exits -- then demote a still-`Running` (or still
/// `Starting`) status to `Stopped`.
///
/// Awaiting the readers rather than the child itself is deliberate: the manager
/// owns the [`Child`] so that dropping the manager kills it, which rules out
/// calling `Child::wait` here. Because the readers have already classified
/// every line by the time they complete, a genuine `Failed(..)` diagnosis is
/// visible and is left in place; so is a `Stopped` written by `stop`.
fn spawn_exit_watcher(
    readers: Vec<tokio::task::JoinHandle<()>>,
    profile_id: String,
    statuses: Statuses,
) {
    tokio::spawn(async move {
        for reader in readers {
            let _ = reader.await;
        }

        let mut guard = statuses.lock().await;
        if matches!(
            guard.get(&profile_id),
            Some(ProxyStatus::Running | ProxyStatus::Starting)
        ) {
            guard.insert(profile_id, ProxyStatus::Stopped);
        }
    });
}

/// Append `text`, then trim the buffer to `cap` by draining from the front so
/// the newest lines survive.
async fn push_log(logs: &Logs, cap: usize, profile_id: &str, text: String) {
    let mut guard = logs.lock().await;
    guard.push(LogLine {
        profile_id: profile_id.to_string(),
        text,
    });
    if guard.len() > cap {
        let excess = guard.len() - cap;
        guard.drain(..excess);
    }
}

/// Build the `cloud-sql-proxy` argument list for `profile`: flags first, then
/// impersonation, then one positional argument per instance.
fn args_for(profile: &Profile) -> Vec<String> {
    let mut args = Vec::new();

    if profile.flags.auto_iam_authn {
        args.push("--auto-iam-authn".to_string());
    }
    if profile.flags.private_ip {
        args.push("--private-ip".to_string());
    }
    if let Some(sa) = profile
        .impersonate_service_account
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        args.push("--impersonate-service-account".to_string());
        args.push(sa.to_string());
    }

    args.extend(profile.instance_args());
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn profile() -> Profile {
        Profile {
            id: "dev".to_string(),
            name: "dev".to_string(),
            project: "fh-dev".to_string(),
            region: "us-central1".to_string(),
            instances: vec![
                Instance {
                    role: InstanceRole::Primary,
                    connection_name: "proj:us-central1:dev-primary".to_string(),
                    port: 15432,
                },
                Instance {
                    role: InstanceRole::Replica,
                    connection_name: "proj:us-central1:dev-replica".to_string(),
                    port: 15433,
                },
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
            vpn_probe_host: None,
        }
    }

    #[test]
    fn args_include_both_default_flags_then_instances() {
        // ProxyFlags::default() is auto_iam_authn + private_ip, so the default
        // profile exercises both flag branches.
        assert_eq!(
            args_for(&profile()),
            vec![
                "--auto-iam-authn".to_string(),
                "--private-ip".to_string(),
                "proj:us-central1:dev-primary?port=15432".to_string(),
                "proj:us-central1:dev-replica?port=15433".to_string(),
            ]
        );
    }

    #[test]
    fn flags_are_omitted_when_disabled() {
        let mut p = profile();
        p.flags = ProxyFlags {
            auto_iam_authn: false,
            private_ip: false,
        };
        assert_eq!(
            args_for(&p),
            vec![
                "proj:us-central1:dev-primary?port=15432".to_string(),
                "proj:us-central1:dev-replica?port=15433".to_string(),
            ]
        );
    }

    #[test]
    fn impersonation_adds_flag_and_value_before_instances() {
        let mut p = profile();
        p.impersonate_service_account = Some("sa@example.iam.gserviceaccount.com".to_string());
        let args = args_for(&p);
        let idx = args
            .iter()
            .position(|a| a == "--impersonate-service-account")
            .expect("flag present");
        assert_eq!(args[idx + 1], "sa@example.iam.gserviceaccount.com");
        // Positionals must come after every flag.
        assert!(args[idx + 2].contains("?port="));
    }

    #[test]
    fn empty_impersonation_string_is_treated_as_absent() {
        // An empty text field in the UI must not produce a bare
        // `--impersonate-service-account ""`, which the proxy rejects.
        let mut p = profile();
        p.impersonate_service_account = Some(String::new());
        assert!(!args_for(&p)
            .iter()
            .any(|a| a == "--impersonate-service-account"));
    }

    #[tokio::test]
    async fn status_of_unknown_profile_is_stopped() {
        let m = ProxyManager::new("/bin/true");
        assert_eq!(m.status_of("never-started").await, ProxyStatus::Stopped);
    }

    #[tokio::test]
    async fn log_cap_drains_oldest_lines_first() {
        let m = ProxyManager::new("/bin/true").with_log_cap(3);
        for i in 0..5 {
            m.push_log_line("dev", format!("line {i}")).await;
        }
        let logs = m.logs_handle();
        let texts: Vec<String> = logs.lock().await.iter().map(|l| l.text.clone()).collect();
        assert_eq!(texts, vec!["line 2", "line 3", "line 4"]);
    }

    #[tokio::test]
    async fn a_zero_cap_retains_nothing() {
        let m = ProxyManager::new("/bin/true").with_log_cap(0);
        m.push_log_line("dev", "a").await;
        assert!(m.logs_handle().lock().await.is_empty());
    }

    #[tokio::test]
    async fn log_lines_are_tagged_with_their_profile() {
        let m = ProxyManager::new("/bin/true");
        m.push_log_line("dev", "a").await;
        m.push_log_line("stg", "b").await;
        let logs = m.logs_handle();
        let guard = logs.lock().await;
        assert_eq!(guard[0].profile_id, "dev");
        assert_eq!(guard[1].profile_id, "stg");
    }
}
