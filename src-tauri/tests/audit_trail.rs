//! Integration tests for the audit trail as it is actually produced: a real
//! `ProxyManager` spawning a real child (the `tests/fixtures/fake-proxy.sh`
//! stand-in), writing to a real file on disk.
//!
//! The unit tests in `core::audit` cover the logger in isolation -- rotation,
//! the memory cap, filtering, degradation. What they cannot cover is whether the
//! wiring is actually connected: a logger with perfect rotation that nothing
//! calls is worth nothing. These tests assert on the file.
//!
//! Same conventions as `proxy_manager.rs`: no fixed sleeps where a condition can
//! be polled, and no process-global env mutation -- the fake proxy's mode is
//! scoped per manager with `with_env`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fh_cloud_sql_proxy_gui::core::audit::{Category, Logger, Severity};
use fh_cloud_sql_proxy_gui::core::profile::{Instance, InstanceRole, Profile, ProxyFlags};
use fh_cloud_sql_proxy_gui::core::proxy::{ProxyManager, ProxyStatus};

fn fake_proxy() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-proxy.sh")
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

/// Poll the audit file until `pred` holds over its contents. Panics with the
/// contents on timeout, which is what makes a failure debuggable.
fn poll_file(path: &Path, label: &str, pred: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        if pred(&contents) {
            return contents;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}; file so far:\n{contents}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn poll_status(
    manager: &ProxyManager,
    profile_id: &str,
    label: &str,
    pred: impl Fn(&ProxyStatus) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = manager.status_of(profile_id).await;
        if pred(&status) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {label}; last status was {status:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A real start writes the argv, the pid, the ready line, and the status
/// transition to the file, in that order.
#[tokio::test]
async fn a_real_start_writes_the_whole_sequence_to_the_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    let audit = Logger::to_file(path.clone());

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "ready")
        .with_audit(audit.clone());

    manager.start(&test_profile("dev")).await.expect("start");
    poll_status(&manager, "dev", "Running", |s| {
        matches!(s, ProxyStatus::Running)
    })
    .await;

    let contents = poll_file(&path, "the Running transition", |text| {
        text.contains("Starting -> Running")
    });

    // Category 2: the exact argv, before the spawn.
    assert!(
        contents.contains("spawning:") && contents.contains("fake-proxy.sh"),
        "the spawn argv should be recorded:\n{contents}"
    );
    // No redaction: the connection names are the point.
    assert!(
        contents.contains("proj:us-central1:dev-primary?port=15432"),
        "connection names should be recorded in full:\n{contents}"
    );
    assert!(
        contents.contains("--auto-iam-authn"),
        "flags should be recorded:\n{contents}"
    );
    assert!(
        contents.contains("spawned pid "),
        "the pid should be recorded:\n{contents}"
    );
    // Category 4: the child's own output.
    assert!(
        contents.contains("ready for new connections"),
        "the proxy's output should be recorded:\n{contents}"
    );
    // Category 2: the status transition, and which line caused it.
    assert!(
        contents.contains("status: Starting -> Running (ready line seen)"),
        "the transition should name its cause:\n{contents}"
    );
    // Every record is one physical line.
    for line in contents.lines() {
        assert!(
            line.starts_with("20"),
            "every line should start with a timestamp, got: {line}"
        );
    }

    manager.stop_all().await;
}

/// A diagnosed failure is recorded at error severity, so the Logs view's
/// "Errors Only" filter finds it.
#[tokio::test]
async fn a_bind_failure_is_recorded_as_an_error_with_its_diagnosis() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    let audit = Logger::to_file(path.clone());

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "bind")
        .with_audit(audit.clone());

    manager
        .start(&test_profile("dev"))
        .await
        .expect("spawn succeeds");
    poll_status(&manager, "dev", "Failed", |s| {
        matches!(s, ProxyStatus::Failed(_))
    })
    .await;

    let contents = poll_file(&path, "the Failed transition", |text| {
        text.contains("-> Failed")
    });
    assert!(
        contents.contains("PortInUse"),
        "the diagnosis kind should be recorded:\n{contents}"
    );
    assert!(
        contents.contains("15432"),
        "the diagnosis should name the port:\n{contents}"
    );

    // The severity is what the view filters on, so assert on it rather than on
    // the text alone.
    let errors = audit.filtered(Some(Severity::Error), Some("dev"));
    assert!(
        !errors.is_empty(),
        "the failure should be findable by an errors-only filter"
    );
    assert!(
        errors.iter().any(|r| r.category == Category::Proxy),
        "the failing proxy line itself should be an error: {errors:?}"
    );

    manager.stop_all().await;
}

/// A stop records the kill and the transition, and the exit code is recorded
/// once the child is reaped.
#[tokio::test]
async fn a_stop_records_the_kill_and_the_child_exit() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    let audit = Logger::to_file(path.clone());

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "ready")
        .with_audit(audit.clone());

    manager.start(&test_profile("dev")).await.expect("start");
    poll_status(&manager, "dev", "Running", |s| {
        matches!(s, ProxyStatus::Running)
    })
    .await;

    manager.stop("dev").await;

    let contents = poll_file(&path, "the kill record", |text| {
        text.contains("killed child pid")
    });
    assert!(
        contents.contains("status: Running -> Stopped"),
        "the stop transition should be recorded:\n{contents}"
    );
}

/// The `crash` fixture exits 1 on its own. The exit *code* is only observable
/// from `reap_exited`, so this is the test that the code actually lands.
#[tokio::test]
async fn an_unprompted_child_exit_records_its_exit_code() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    let audit = Logger::to_file(path.clone());

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "crash")
        .with_audit(audit.clone());

    manager.start(&test_profile("dev")).await.expect("start");

    // `reap_exited` is what reads the code, and it runs from `is_running`.
    let deadline = Instant::now() + Duration::from_secs(5);
    while manager.is_running("dev") {
        assert!(Instant::now() < deadline, "the crash fixture never exited");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let contents = poll_file(&path, "the exit code", |text| text.contains("child exited"));
    assert!(
        contents.contains("child exited with code 1"),
        "the exit code should be recorded:\n{contents}"
    );
    // A nonzero exit is a warning, not routine info.
    assert!(
        audit
            .filtered(Some(Severity::Warn), Some("dev"))
            .iter()
            .any(|r| r.message.contains("exited with code 1")),
        "a nonzero exit should carry at least warn severity"
    );
}

/// Rotation under a real spawn's volume: the trail keeps working and the newest
/// records stay in the active file.
#[tokio::test]
async fn the_trail_survives_rotation_while_a_proxy_is_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    // A threshold small enough that the start sequence alone crosses it.
    let audit = Logger::with_policy(path.clone(), 2000, 512, 2);

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "ready")
        .with_audit(audit.clone());

    manager.start(&test_profile("dev")).await.expect("start");
    poll_status(&manager, "dev", "Running", |s| {
        matches!(s, ProxyStatus::Running)
    })
    .await;

    // Push enough past the threshold to force at least one rotation.
    for i in 0..40 {
        audit.info(Category::Event, Some("dev"), format!("filler {i:03}"));
        audit.flush_blocking(Duration::from_secs(5));
    }
    audit.info(Category::Action, Some("dev"), "the last word");
    audit.flush_blocking(Duration::from_secs(5));

    let active = poll_file(&path, "the newest record in the active file", |text| {
        text.contains("the last word")
    });
    assert!(
        !active.is_empty(),
        "the active file must not be empty after rotation"
    );
    assert!(
        path.with_file_name("audit.log.1").exists(),
        "a generation should have been rotated out"
    );
    // The memory view is unaffected by rotation -- it is the whole session.
    assert!(
        audit.records().iter().any(|r| r.message == "the last word"),
        "the memory view should still hold the newest record"
    );
    assert_eq!(audit.write_failures(), 0);

    manager.stop_all().await;
}

/// A manager sharing one logger with the rest of the app writes into the same
/// trail, in order. This is the property that makes the log an audit trail
/// rather than four separate streams.
#[tokio::test]
async fn user_actions_and_proxy_output_interleave_in_one_ordered_trail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.log");
    let audit = Logger::to_file(path.clone());

    // What the command layer records around a start.
    audit.info(Category::Action, Some("dev"), "start requested for 'dev'");
    audit.info(Category::Event, Some("dev"), "preflight passed");

    let mut manager = ProxyManager::new(fake_proxy())
        .with_env("FAKE_PROXY_MODE", "ready")
        .with_audit(audit.clone());
    manager.start(&test_profile("dev")).await.expect("start");
    poll_status(&manager, "dev", "Running", |s| {
        matches!(s, ProxyStatus::Running)
    })
    .await;

    let contents = poll_file(&path, "the whole sequence", |text| {
        text.contains("Starting -> Running")
    });

    // Ordering is the point: the action precedes the preflight, which precedes
    // the spawn, which precedes the child's output.
    let index = |needle: &str| {
        contents
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle} in:\n{contents}"))
    };
    assert!(index("start requested") < index("preflight passed"));
    assert!(index("preflight passed") < index("spawning:"));
    assert!(index("spawning:") < index("ready for new connections"));

    manager.stop_all().await;
}
