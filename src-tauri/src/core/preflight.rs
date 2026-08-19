//! Checks that run before spawning a `cloud-sql-proxy` process, so the app
//! can refuse to start with a useful message instead of letting the proxy
//! fail obscurely later.
//!
//! This module reuses [`Diagnosis`] and [`FailureKind`] from `log_watcher`
//! so the two modules never drift on wording or classification.

use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::log_watcher::{Diagnosis, FailureKind, ADC_FIX};
use crate::core::profile::Profile;

/// Everything checked before spawning a proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preflight {
    Ok,
    Blocked(Diagnosis),
}

/// Timeout for the best-effort VPN reachability probe.
const VPN_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Whether `port` is free to bind on localhost right now.
///
/// Binds and immediately drops the listener; this is inherently a
/// time-of-check/time-of-use race (something else could grab the port a
/// moment later), but it's the best local signal available before spawning.
pub fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// The expected path to Google's Application Default Credentials file.
///
/// Returns `None` only if the home directory cannot be determined.
pub fn adc_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join(".config")
            .join("gcloud")
            .join("application_default_credentials.json")
    })
}

/// Whether the ADC file exists at `path`.
///
/// This is only weak evidence of a working credential: the file can exist
/// while the token inside it is expired. That failure mode can only be
/// detected once the proxy actually runs and `log_watcher::classify` reads
/// its output — this check merely catches the "never logged in at all" case
/// up front.
pub fn adc_present(path: &Path) -> bool {
    path.exists()
}

/// The host to probe for VPN connectivity when diagnosing a failed start, or
/// `None` if this profile has no probe host configured.
///
/// Nothing is derived here: the host is whatever the user put in the
/// profile's `vpnProbeHost` field. Profiles are user-created and freely
/// named, so there is no name from which a hostname could be inferred.
///
/// `None` means "unknown, cannot probe" — callers must never treat that as
/// a failure, only as "no signal available."
pub fn vpn_probe_host_for(profile: &Profile) -> Option<String> {
    profile.vpn_probe_host.clone()
}

/// Best-effort TCP reachability probe against `host:port`, used to diagnose
/// whether the corporate VPN is connected.
///
/// - `Some(true)` / `Some(false)`: a definite answer.
/// - `None`: inconclusive (e.g. DNS resolution returned no addresses at
///   all), so callers should not draw a conclusion either way.
///
/// A DNS resolution *failure* (`Err`) on a `.private.` hostname generally
/// means the VPN isn't routing that name at all, so it's treated as a
/// confident "not reachable" rather than "unknown".
pub fn vpn_reachable(host: &str, port: u16) -> Option<bool> {
    let addrs: Vec<_> = match (host, port).to_socket_addrs() {
        Ok(iter) => iter.collect(),
        Err(_) => return Some(false),
    };

    let addr = addrs.first()?;
    Some(TcpStream::connect_timeout(addr, VPN_PROBE_TIMEOUT).is_ok())
}

/// The gate called before spawning a proxy for `profile`.
///
/// Checks run in this fixed order, each one short-circuiting the rest:
/// 1. Every port the profile needs must be free.
/// 2. If `adc` is `Some(path)`, the ADC file must exist at `path` (pass
///    `None` to skip this check entirely — e.g. a caller that already knows
///    it wants to skip credential checking).
/// 3. Every instance must have a non-empty `connection_name` (seeded
///    profiles start with empty names until gcloud discovery fills them in).
///
/// Deliberately NOT checked here: VPN reachability. [`vpn_reachable`] does a
/// network probe with a ~1.5s timeout, which is both slow to run on every
/// start attempt and inherently unreliable as a pre-check — a false
/// negative (e.g. a transient hiccup) would block a start that actually
/// would have worked. It's exposed separately for the UI to call when
/// diagnosing an already-failed start, not as a gate here.
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

    if let Some(path) = adc {
        if !adc_present(path) {
            return Preflight::Blocked(Diagnosis {
                kind: FailureKind::AdcExpired,
                message: "You need to authenticate with Google before starting this profile."
                    .to_string(),
                fix_command: Some(ADC_FIX.to_string()),
            });
        }
    }

    if profile
        .instances
        .iter()
        .any(|i| i.connection_name.is_empty())
    {
        return Preflight::Blocked(Diagnosis {
            kind: FailureKind::StaleInstance,
            message: "This profile is missing connection names — run \"Refresh connection names\" before starting.".to_string(),
            fix_command: None,
        });
    }

    Preflight::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::profile::{Instance, InstanceRole, ProxyFlags};

    fn instance(role: InstanceRole, connection_name: &str, port: u16) -> Instance {
        Instance {
            role,
            connection_name: connection_name.to_string(),
            port,
        }
    }

    /// Build a profile using two freshly-bound ephemeral ports (so tests
    /// never collide with a real proxy that might be running on this
    /// machine's 15432/15433). The listeners are returned so the caller can
    /// keep them alive (to simulate "occupied") or drop them (to free the
    /// port) as needed.
    fn profile_with_free_ports(name: &str) -> (Profile, TcpListener, TcpListener) {
        let l1 = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port 1");
        let l2 = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port 2");
        let port1 = l1.local_addr().expect("local_addr").port();
        let port2 = l2.local_addr().expect("local_addr").port();

        let profile = Profile {
            id: name.to_string(),
            name: name.to_string(),
            project: "proj".to_string(),
            region: "us-central1".to_string(),
            instances: vec![
                instance(InstanceRole::Primary, "proj:us-central1:primary", port1),
                instance(InstanceRole::Replica, "proj:us-central1:replica", port2),
            ],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
            vpn_probe_host: None,
        };

        (profile, l1, l2)
    }

    #[test]
    fn port_with_no_listener_is_free() {
        // Bind then drop immediately to reserve and release an ephemeral
        // port, so we know the OS considers it free afterward.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("local_addr").port();
        drop(listener);

        assert!(port_is_free(port));
    }

    #[test]
    fn port_held_by_live_listener_is_not_free() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("local_addr").port();

        assert!(!port_is_free(port));
        // Keep `listener` alive until here so the port stays occupied for
        // the assertion above.
        drop(listener);
    }

    #[test]
    fn check_blocks_on_occupied_port_with_port_number_in_message() {
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        // Keep l1 alive (port occupied), drop l2 (irrelevant to this test).
        let occupied_port = l1.local_addr().unwrap().port();
        drop(l2);
        profile.instances[0].connection_name = "proj:us-central1:primary".to_string();
        profile.instances[1].connection_name = "proj:us-central1:replica".to_string();

        match check(&profile, None) {
            Preflight::Blocked(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(
                    d.message.contains(&occupied_port.to_string()),
                    "message: {}",
                    d.message
                );
            }
            other => panic!("expected Blocked(PortInUse), got {other:?}"),
        }
        drop(l1);
    }

    #[test]
    fn check_blocks_when_adc_file_missing() {
        let (profile, l1, l2) = profile_with_free_ports("dev");
        drop(l1);
        drop(l2);

        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_adc = tmp.path().join("does_not_exist.json");

        match check(&profile, Some(&missing_adc)) {
            Preflight::Blocked(d) => {
                assert_eq!(d.kind, FailureKind::AdcExpired);
                assert_eq!(d.fix_command, Some(ADC_FIX.to_string()));
            }
            other => panic!("expected Blocked(AdcExpired), got {other:?}"),
        }
    }

    #[test]
    fn check_skips_adc_check_when_adc_is_none() {
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        drop(l1);
        drop(l2);
        profile.instances[0].connection_name = "proj:us-central1:primary".to_string();
        profile.instances[1].connection_name = "proj:us-central1:replica".to_string();

        // No ADC file anywhere passed (None) — even though no real ADC
        // exists at this fabricated path, `check` should never look at it.
        assert_eq!(check(&profile, None), Preflight::Ok);
    }

    #[test]
    fn check_blocks_when_first_instance_missing_connection_name() {
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        drop(l1);
        drop(l2);
        // Clear the first (primary) instance's connection name; leave the
        // second populated, so only the first is stale.
        profile.instances[0].connection_name = String::new();
        profile.instances[1].connection_name = "proj:us-central1:replica".to_string();

        let tmp = tempfile::tempdir().expect("tempdir");
        let adc = tmp.path().join("adc.json");
        std::fs::write(&adc, "{}").expect("write adc");

        match check(&profile, Some(&adc)) {
            Preflight::Blocked(d) => assert_eq!(d.kind, FailureKind::StaleInstance),
            other => panic!("expected Blocked(StaleInstance), got {other:?}"),
        }
    }

    #[test]
    fn check_blocks_when_second_instance_missing_connection_name() {
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        drop(l1);
        drop(l2);
        profile.instances[0].connection_name = "proj:us-central1:primary".to_string();
        profile.instances[1].connection_name = String::new();

        let tmp = tempfile::tempdir().expect("tempdir");
        let adc = tmp.path().join("adc.json");
        std::fs::write(&adc, "{}").expect("write adc");

        match check(&profile, Some(&adc)) {
            Preflight::Blocked(d) => assert_eq!(d.kind, FailureKind::StaleInstance),
            other => panic!("expected Blocked(StaleInstance), got {other:?}"),
        }
    }

    #[test]
    fn check_returns_ok_when_everything_is_fine() {
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        drop(l1);
        drop(l2);
        profile.instances[0].connection_name = "proj:us-central1:primary".to_string();
        profile.instances[1].connection_name = "proj:us-central1:replica".to_string();

        let tmp = tempfile::tempdir().expect("tempdir");
        let adc = tmp.path().join("adc.json");
        std::fs::write(&adc, "{}").expect("write adc");

        assert_eq!(check(&profile, Some(&adc)), Preflight::Ok);
    }

    #[test]
    fn port_check_precedes_adc_check() {
        // Both a port conflict AND a missing ADC file are present. The
        // result must be PortInUse, pinning the documented check order.
        let (mut profile, l1, l2) = profile_with_free_ports("dev");
        let occupied_port = l1.local_addr().unwrap().port();
        drop(l2);
        profile.instances[0].connection_name = "proj:us-central1:primary".to_string();
        profile.instances[1].connection_name = "proj:us-central1:replica".to_string();

        let tmp = tempfile::tempdir().expect("tempdir");
        let missing_adc = tmp.path().join("does_not_exist.json");

        match check(&profile, Some(&missing_adc)) {
            Preflight::Blocked(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains(&occupied_port.to_string()));
            }
            other => panic!("expected Blocked(PortInUse), got {other:?}"),
        }
        drop(l1);
    }

    fn profile_named(name: &str) -> Profile {
        Profile {
            id: name.to_string(),
            name: name.to_string(),
            project: "proj".to_string(),
            region: "us-central1".to_string(),
            instances: vec![instance(InstanceRole::Primary, "proj:us-central1:x", 1)],
            flags: ProxyFlags::default(),
            impersonate_service_account: None,
            danger: false,
            vpn_probe_host: None,
        }
    }

    #[test]
    fn vpn_probe_host_is_returned_verbatim_from_the_profile() {
        let mut profile = profile_named("dev");
        profile.vpn_probe_host = Some("pg.dev.internal.example.com".to_string());
        assert_eq!(
            vpn_probe_host_for(&profile),
            Some("pg.dev.internal.example.com".to_string())
        );

        // Nothing is derived from the name: renaming the profile must not
        // change the host, and an arbitrary host is passed through unchanged.
        profile.name = "my staging box".to_string();
        profile.vpn_probe_host = Some("db.internal.example".to_string());
        assert_eq!(
            vpn_probe_host_for(&profile),
            Some("db.internal.example".to_string())
        );
    }

    #[test]
    fn vpn_probe_host_is_none_when_unset_regardless_of_name() {
        // Previously "dev"/"stg"/"prd" produced a host purely from the name.
        // They must not any more: a user-created profile called "dev" with no
        // probe host configured has no signal available.
        for name in ["dev", "stg", "prd", "staging-2", ""] {
            assert_eq!(vpn_probe_host_for(&profile_named(name)), None, "name {name}");
        }
    }

    #[test]
    fn adc_present_reflects_file_existence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("application_default_credentials.json");
        assert!(!adc_present(&path));

        std::fs::write(&path, "{}").expect("write");
        assert!(adc_present(&path));
    }

    #[test]
    fn adc_path_ends_with_expected_suffix() {
        let path = adc_path().expect("home dir should be resolvable in test env");
        assert!(path.ends_with(".config/gcloud/application_default_credentials.json"));
    }
}
