//! Integration tests for [`ProxyManager`]. These spawn real child processes
//! (the `tests/fixtures/fake-proxy.sh` stand-in), so they live outside the
//! unit tests.
//!
//! Two conventions matter here:
//!
//! * **No fixed sleeps.** Status transitions are awaited with [`poll_until`],
//!   which polls on a deadline and reports the last-seen status on timeout.
//!   Fixed sleeps are both slower and flakier.
//!
//! * **No process-global env mutation.** The fake proxy's behaviour is chosen
//!   with `FAKE_PROXY_MODE`, but `std::env::set_var` is process-global and
//!   would race across concurrently-running tests. Instead each test builds
//!   its own `ProxyManager` with [`ProxyManager::with_env`], which applies the
//!   variable to that manager's children only. The suite therefore needs no
//!   `--test-threads=1` and runs with plain `cargo test`.
//!
//! No test binds or asserts on ports 15432/15433 on this machine: the fake
//! proxy never opens a listener, and the one port assertion is against the
//! text of a canned log line, not a real socket.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fh_cloud_sql_proxy_gui::core::log_watcher::FailureKind;
use fh_cloud_sql_proxy_gui::core::profile::{Instance, InstanceRole, Profile, ProxyFlags};
use fh_cloud_sql_proxy_gui::core::proxy::{LogLine, ProxyError, ProxyManager, ProxyStatus};

/// Absolute path to the fake proxy script.
fn fake_proxy() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-proxy.sh")
}

/// A manager wired to the fake proxy, running in the given fixture mode.
/// The mode is scoped to this manager's children, not the test process.
fn manager(mode: &str) -> ProxyManager {
    ProxyManager::new(fake_proxy()).with_env("FAKE_PROXY_MODE", mode)
}

fn test_profile(id: &str) -> Profile {
    Profile {
        id: id.to_string(),
        name: id.to_string(),
        project: format!("fh-{id}-project"),
        region: "us-central1".to_string(),
        instances: vec![
            Instance {
                role: InstanceRole::Primary,
                connection_name: format!("proj:us-central1:{id}-primary"),
                port: 15432,
            },
            Instance {
                role: InstanceRole::Replica,
                connection_name: format!("proj:us-central1:{id}-replica"),
                port: 15433,
            },
        ],
        flags: ProxyFlags::default(),
        impersonate_service_account: None,
        danger: false,
        vpn_probe_host: None,
    }
}

/// Poll `profile_id`'s status every 50ms for up to 5s until `pred` holds.
/// Returns the matching status; panics with the last-seen status on timeout.
async fn poll_until(
    manager: &ProxyManager,
    profile_id: &str,
    label: &str,
    pred: impl Fn(&ProxyStatus) -> bool,
) -> ProxyStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = manager.status_of(profile_id).await;
    loop {
        if pred(&last) {
            return last;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last status was {last:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        last = manager.status_of(profile_id).await;
    }
}

async fn poll_running(manager: &ProxyManager, profile_id: &str) -> ProxyStatus {
    poll_until(manager, profile_id, "Running", |s| {
        matches!(s, ProxyStatus::Running)
    })
    .await
}

/// Poll the shared log buffer until `pred` holds over the lines belonging to
/// `profile_id`. Panics with the collected lines on timeout.
async fn poll_logs(
    manager: &ProxyManager,
    profile_id: &str,
    label: &str,
    pred: impl Fn(&[String]) -> bool,
) -> Vec<String> {
    let handle = manager.logs_handle();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lines: Vec<String> = handle
            .lock()
            .await
            .iter()
            .filter(|l| l.profile_id == profile_id)
            .map(|l| l.text.clone())
            .collect();
        if pred(&lines) {
            return lines;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}; lines so far: {lines:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn start_reaches_running_after_ready_line() {
    let mut m = manager("ready");
    let p = test_profile("dev");

    m.start(&p).await.expect("start should succeed");
    // Immediately after spawn the ready line has not been read yet.
    assert!(
        matches!(
            m.status_of("dev").await,
            ProxyStatus::Starting | ProxyStatus::Running
        ),
        "expected Starting or Running right after spawn"
    );

    poll_running(&m, "dev").await;
    assert!(m.is_running("dev"));
    assert_eq!(m.running_ids(), vec!["dev".to_string()]);

    m.stop_all().await;
}

#[tokio::test]
async fn stop_transitions_to_stopped_and_clears_is_running() {
    let mut m = manager("ready");
    let p = test_profile("dev");

    m.start(&p).await.expect("start");
    poll_running(&m, "dev").await;

    m.stop("dev").await;

    assert_eq!(m.status_of("dev").await, ProxyStatus::Stopped);
    assert!(!m.is_running("dev"));
    assert!(m.running_ids().is_empty());
}

#[tokio::test]
async fn starting_the_same_profile_twice_returns_already_running() {
    let mut m = manager("ready");
    let p = test_profile("dev");

    m.start(&p).await.expect("first start");
    poll_running(&m, "dev").await;

    match m.start(&p).await {
        Err(ProxyError::AlreadyRunning(id)) => assert_eq!(id, "dev"),
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }

    // The second call must not have spawned a second child.
    assert_eq!(m.running_ids(), vec!["dev".to_string()]);

    m.stop_all().await;
}

#[tokio::test]
async fn bind_failure_is_classified_as_failed_naming_the_port() {
    // Exercises the timestamped-line path through `log_watcher::classify`:
    // the fixture's bind error carries a `2026/08/18 10:00:00` prefix, which
    // a naive port scan would misread as port 0.
    let mut m = manager("bind");
    let p = test_profile("dev");

    m.start(&p)
        .await
        .expect("spawn succeeds even though the proxy then fails");

    let status = poll_until(&m, "dev", "Failed", |s| matches!(s, ProxyStatus::Failed(_))).await;
    let ProxyStatus::Failed(diagnosis) = status else {
        unreachable!("poll_until only returns on Failed");
    };
    assert_eq!(diagnosis.kind, FailureKind::PortInUse);
    assert!(
        diagnosis.message.contains("15432"),
        "message should name the port: {}",
        diagnosis.message
    );
    assert!(
        !diagnosis.message.contains("Port 0"),
        "extracted the timestamp instead of the port: {}",
        diagnosis.message
    );

    // The bind fixture exits immediately after printing the error, so the exit
    // watcher fires while this status is set. A diagnosis must survive it --
    // being demoted to `Stopped` would throw away the only thing the app has
    // to show the user. Waiting for the child to be reaped proves the exit path
    // has run, without guessing at a sleep duration.
    let deadline = Instant::now() + Duration::from_secs(5);
    while m.is_running("dev") {
        assert!(Instant::now() < deadline, "bind-mode child never exited");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        matches!(m.status_of("dev").await, ProxyStatus::Failed(_)),
        "the exit watcher must not overwrite a real diagnosis; got {:?}",
        m.status_of("dev").await
    );

    m.stop_all().await;
}

#[tokio::test]
async fn missing_binary_returns_spawn_error() {
    let mut m = ProxyManager::new("/nonexistent/path/to/cloud-sql-proxy");
    let p = test_profile("dev");

    match m.start(&p).await {
        Err(ProxyError::Spawn { binary, .. }) => {
            assert!(binary.contains("cloud-sql-proxy"), "binary: {binary}");
        }
        other => panic!("expected Spawn error, got {other:?}"),
    }

    // A failed spawn must leave no phantom child registered.
    assert!(!m.is_running("dev"));
    assert!(m.running_ids().is_empty());
}

#[tokio::test]
async fn stop_all_kills_every_child_and_empties_running_ids() {
    let mut m = manager("ready");
    for id in ["dev", "stg", "prd"] {
        m.start(&test_profile(id)).await.expect("start");
    }
    for id in ["dev", "stg", "prd"] {
        poll_running(&m, id).await;
    }
    assert_eq!(m.running_ids().len(), 3);

    m.stop_all().await;

    assert!(m.running_ids().is_empty());
    for id in ["dev", "stg", "prd"] {
        assert_eq!(m.status_of(id).await, ProxyStatus::Stopped);
        assert!(!m.is_running(id));
    }
}

#[tokio::test]
async fn logs_are_captured_and_tagged_with_the_profile_id() {
    let mut m = manager("ready");
    m.start(&test_profile("dev")).await.expect("start");

    let lines = poll_logs(&m, "dev", "the ready line", |lines| {
        lines
            .iter()
            .any(|l| l.contains("ready for new connections"))
    })
    .await;

    // The fixture echoes its argv, so the captured logs also prove the
    // instance arguments reached the child.
    assert!(
        lines
            .iter()
            .any(|l| l.contains("proj:us-central1:dev-primary?port=15432")),
        "logs should include the instance args: {lines:?}"
    );

    m.stop_all().await;
}

#[tokio::test]
async fn crashed_child_does_not_stay_running() {
    // The `crash` fixture prints the ready line, so the manager legitimately
    // reaches Running, then exits 1 with a line `classify` treats as Noise.
    // No line diagnoses the exit, so the reader task falls back to Stopped
    // when the streams close -- that is the assertion here. Reporting
    // `Failed(Unknown)` on a nonzero exit would be a plausible alternative,
    // but "Stopped" is the honest report: the app has no diagnosis to show,
    // and the tray should offer a plain restart rather than a fake error.
    let mut m = manager("crash");
    m.start(&test_profile("dev")).await.expect("start");

    let status = poll_until(&m, "dev", "not-Running", |s| {
        !matches!(s, ProxyStatus::Running | ProxyStatus::Starting)
    })
    .await;

    assert_eq!(
        status,
        ProxyStatus::Stopped,
        "an undiagnosed exit should report Stopped, not linger as Running"
    );
    assert!(!m.is_running("dev"));

    m.stop_all().await;
}

#[tokio::test]
async fn log_buffer_is_capped_by_draining_the_oldest_lines() {
    // Tested against the cap logic directly rather than by generating
    // thousands of real child-process lines: the behaviour under test is the
    // drain, and driving it through a subprocess would only add runtime and
    // flakiness.
    let m = ProxyManager::new(fake_proxy()).with_log_cap(4);
    let handle = m.logs_handle();

    for i in 0..10 {
        m.push_log_line("dev", format!("line {i}")).await;
    }

    let lines = handle.lock().await.clone();
    assert_eq!(lines.len(), 4, "buffer should be capped at 4");
    let texts: Vec<&str> = lines.iter().map(|l: &LogLine| l.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["line 6", "line 7", "line 8", "line 9"],
        "the oldest lines should be dropped, keeping the newest"
    );
}

#[tokio::test]
async fn dropping_the_manager_kills_its_children() {
    // Orphan prevention: a leaked child keeps holding 15432/15433, so the
    // next start fails with "address already in use". This asserts the
    // `kill_on_drop(true)` + `Drop` guarantee by observing the OS: the PID
    // must be gone after the manager is dropped without any `stop` call.
    let pid = {
        let mut m = manager("ready");
        m.start(&test_profile("dev")).await.expect("start");
        poll_running(&m, "dev").await;
        let pid = m.pid_of("dev").expect("a running child has a pid");
        assert!(pid_is_alive(pid), "child should be alive before drop");
        pid
        // `m` dropped here with no stop()/stop_all() call.
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_is_alive(pid) {
        if Instant::now() >= deadline {
            panic!("child pid {pid} survived the manager being dropped -- orphan leak");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The sequence `delete_profile` performs: stop the child, then drop the
/// profile from the config. Deleting without the stop would strand the child —
/// the manager keys everything by id, so once the profile is gone nothing
/// could ever name that process again and it would hold its ports until the
/// app quit.
///
/// This asserts on the real pid rather than on `status_of`, because a status
/// that says "stopped" while the process is still alive is exactly the bug.
#[tokio::test]
async fn stopping_before_removal_leaves_no_orphaned_child() {
    let mut m = manager("ready");
    let mut config = vec![test_profile("dev"), test_profile("keep")];

    m.start(&config[0]).await.expect("start");
    poll_running(&m, "dev").await;
    let pid = m.pid_of("dev").expect("a running child has a pid");
    assert!(pid_is_alive(pid), "child should be alive before delete");

    // What the command does, in order.
    m.stop("dev").await;
    config.retain(|p| p.id != "dev");

    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_is_alive(pid) {
        if Instant::now() >= deadline {
            panic!("child pid {pid} survived deletion of its profile -- orphan leak");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(!m.is_running("dev"));
    assert!(m.running_ids().is_empty());
    // The surviving profile is untouched by the delete.
    assert_eq!(config.len(), 1);
    assert_eq!(config[0].id, "keep");
}

/// Deleting a profile that was never started must not error or disturb the
/// manager: `stop` is idempotent, which is why the command calls it
/// unconditionally rather than reading a status that could go stale.
#[tokio::test]
async fn stopping_a_never_started_profile_before_removal_is_a_no_op() {
    let mut m = manager("ready");

    m.stop("never-started").await;

    assert_eq!(m.status_of("never-started").await, ProxyStatus::Stopped);
    assert!(m.running_ids().is_empty());
}

/// True while `pid` names a live process. `kill -0` only checks for existence
/// and permission; it delivers no signal.
fn pid_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
