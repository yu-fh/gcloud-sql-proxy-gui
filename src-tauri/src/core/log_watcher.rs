//! Pure classification of `cloud-sql-proxy` stderr/stdout lines into meaningful
//! events. No I/O, no dependencies on other core modules — Task 6's
//! `ProxyManager` feeds it lines from the child process, and Task 5's
//! `preflight.rs` reuses `Diagnosis`/`FailureKind`.

/// The fix command for expired Application Default Credentials, shared with
/// `preflight.rs` so the two modules never drift on the exact string.
pub const ADC_FIX: &str = "gcloud auth application-default login";

/// A meaningful event parsed out of cloud-sql-proxy's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyEvent {
    Ready,
    Failure(Diagnosis),
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
    #[allow(dead_code)] // Reserved for Task 5/6 when classification is inconclusive.
    Unknown,
}

/// Classify one line of cloud-sql-proxy output.
///
/// Matching is case-insensitive and substring-based on stable fragments,
/// since exact phrasing varies between proxy versions. Order matters: the
/// port-in-use check runs before the network/VPN checks because a bind
/// error line (`listen tcp 127.0.0.1:15432: bind: address already in use`)
/// contains `listen tcp`, which would otherwise be misread as a dial/network
/// failure.
pub fn classify(line: &str) -> ProxyEvent {
    let lower = line.to_lowercase();

    if lower.contains("ready for new connections") {
        return ProxyEvent::Ready;
    }

    // Must run before network/VPN patterns: bind errors mention "listen tcp"
    // which would otherwise look like a dial failure.
    if lower.contains("address already in use") {
        let message = match extract_port(line) {
            Some(port) => format!(
                "Port {port} is already in use — another profile or a stray proxy holds it."
            ),
            None => "A local port is already in use — another profile or a stray proxy holds it."
                .to_string(),
        };
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::PortInUse,
            message,
            fix_command: None,
        });
    }

    if lower.contains("pam authentication failed")
        || lower.contains("invalid_grant")
        || lower.contains("could not find default credentials")
        || lower.contains("reauthentication is needed")
    {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::AdcExpired,
            message:
                "Your Google credentials have expired — run the command below to log in again."
                    .to_string(),
            fix_command: Some(ADC_FIX.to_string()),
        });
    }

    if lower.contains("does not exist") {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::StaleInstance,
            message: "This instance connection name is stale — use \"Refresh connection names\" in the app to fetch the current one.".to_string(),
            fix_command: None,
        });
    }

    if lower.contains("i/o timeout")
        || lower.contains("dial tcp")
        || lower.contains("context deadline exceeded")
    {
        return ProxyEvent::Failure(Diagnosis {
            kind: FailureKind::OffVpn,
            message: "Cloud SQL instances are private-IP only — connect to the corporate VPN and try again.".to_string(),
            fix_command: None,
        });
    }

    ProxyEvent::Noise
}

/// Extract the listener port from a bind-failure line such as
/// `listen tcp 127.0.0.1:15432: bind: address already in use`.
///
/// Anchors on the `listen tcp` token rather than scanning the line for any
/// `:digits` group. Every proxy line is timestamped, so a left-to-right scan
/// matches the timestamp first: `10:23:45` yields port 23, and `10:00:00`
/// yields port 0 — the app would name a port the user does not hold.
/// Bracketed IPv6 (`[::1]:15432`) is likewise mis-read as port 1.
///
/// Returns the digit run after the address token's final colon, or `None`
/// when the line has no `listen tcp` address or the port is out of range.
fn extract_port(line: &str) -> Option<u16> {
    let lower = line.to_lowercase();
    let idx = lower.find("listen tcp")?;

    // Skip the "listen tcp" token and any address-family suffix (tcp4/tcp6).
    let after = idx + "listen tcp".len();
    let start = after
        + lower[after..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .count();

    let token = line[start..].split_whitespace().next()?;
    let token = token.trim_end_matches(':');

    // rsplit_once takes the digits after the FINAL colon, which is the port
    // for both "127.0.0.1:15432" and bracketed IPv6 "[::1]:15432".
    let port_str = token.rsplit_once(':')?.1;
    if port_str.is_empty() || !port_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    port_str.parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_is_recognized() {
        let line =
            "2026/08/18 10:00:00 The proxy has started successfully and is ready for new connections!";
        assert_eq!(classify(line), ProxyEvent::Ready);
    }

    #[test]
    fn port_in_use_is_detected_and_port_extracted() {
        let line =
            "failed to start listener: listen tcp 127.0.0.1:15432: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15432"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn port_is_extracted_from_timestamped_lines() {
        // Proxy output is timestamped. A left-to-right scan for ":digits"
        // finds the ":00" in "10:00:00" and reports port 0, so the extraction
        // must anchor on the listener address instead.
        let line = "2026/08/18 10:00:00 listen tcp 127.0.0.1:15433: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15433"), "message: {}", d.message);
                assert!(
                    !d.message.contains("Port 0"),
                    "extracted the timestamp, not the port: {}",
                    d.message
                );
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn port_is_not_taken_from_the_timestamps_minutes_field() {
        // A 10:23:45 timestamp made the old scan report "Port 23".
        let line = "2024/01/15 10:23:45 failed to start listener: listen tcp 127.0.0.1:15432: \
                    bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert!(d.message.contains("15432"), "message: {}", d.message);
                assert!(!d.message.contains("Port 23"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn port_is_extracted_from_tcp6_zone_scoped_address() {
        let line = "listen tcp6 [fe80::1%en0]:15433: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15433"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn port_is_extracted_from_bracketed_ipv6_address() {
        let line = "listen tcp [::1]:15432: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15432"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn out_of_range_port_falls_back_to_generic_message() {
        // 70000 does not fit in u16; the message must still be useful.
        let line = "listen tcp 127.0.0.1:70000: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("A local port"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn port_in_use_takes_precedence_over_network_wording() {
        // A line carrying BOTH a bind failure and network wording. Only the
        // ordering of the checks decides the answer here, so this test fails
        // if the port-in-use block is moved below the VPN block.
        let line = "listen tcp 127.0.0.1:15433: bind: address already in use (dial tcp \
                    10.1.2.3:3307: i/o timeout)";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(d.message.contains("15433"), "message: {}", d.message);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }

    #[test]
    fn stale_instance_outranks_network_wording() {
        // A dial failure against a replaced instance mentions both. The stale
        // name is the actionable cause; the timeout is only a symptom, so
        // StaleInstance must win.
        let line = "Cloud SQL instance \"proj:us-central1:terraform-123\" does not exist: \
                    dial tcp 10.1.2.3:3307: i/o timeout";
        match classify(line) {
            ProxyEvent::Failure(d) => assert_eq!(d.kind, FailureKind::StaleInstance),
            other => panic!("expected Failure(StaleInstance), got {other:?}"),
        }
    }

    #[test]
    fn adc_variants_map_to_adc_expired_with_fix_command() {
        let lines = [
            r#"FATAL: PAM authentication failed for user "you@example.com""#,
            "oauth2: cannot fetch token: 400 Bad Request: invalid_grant",
            "could not find default credentials",
            "Reauthentication is needed. Please run `gcloud auth login`",
        ];
        for line in lines {
            match classify(line) {
                ProxyEvent::Failure(d) => {
                    assert_eq!(d.kind, FailureKind::AdcExpired, "line: {line}");
                    assert_eq!(d.fix_command, Some(ADC_FIX.to_string()), "line: {line}");
                }
                other => panic!("expected Failure(AdcExpired) for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn stale_instance_is_detected() {
        let line = r#"Cloud SQL instance "proj:us-central1:terraform-123" does not exist"#;
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::StaleInstance);
                assert!(
                    d.message
                        .to_lowercase()
                        .contains("refresh connection names"),
                    "message: {}",
                    d.message
                );
            }
            other => panic!("expected Failure(StaleInstance), got {other:?}"),
        }
    }

    #[test]
    fn network_lines_map_to_off_vpn() {
        let lines = [
            "dial tcp 10.1.2.3:3307: i/o timeout",
            "context deadline exceeded",
        ];
        for line in lines {
            match classify(line) {
                ProxyEvent::Failure(d) => {
                    assert_eq!(d.kind, FailureKind::OffVpn, "line: {line}");
                    assert!(
                        d.message.to_lowercase().contains("vpn"),
                        "message: {}",
                        d.message
                    );
                }
                other => panic!("expected Failure(OffVpn) for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn ordinary_and_empty_lines_are_noise() {
        assert_eq!(classify(""), ProxyEvent::Noise);
        // Deliberate false-positive trap: contains "Default Credentials" but
        // is normal startup output, not an auth failure.
        assert_eq!(
            classify("2026/08/18 Authorizing with Application Default Credentials"),
            ProxyEvent::Noise
        );
        assert_eq!(
            classify("2026/08/18 10:00:00 Listening on 127.0.0.1:15432"),
            ProxyEvent::Noise
        );
    }

    #[test]
    fn classification_is_case_insensitive() {
        let line = "COULD NOT FIND DEFAULT CREDENTIALS";
        match classify(line) {
            ProxyEvent::Failure(d) => assert_eq!(d.kind, FailureKind::AdcExpired),
            other => panic!("expected Failure(AdcExpired), got {other:?}"),
        }
    }

    #[test]
    fn port_in_use_without_extractable_port_still_classifies() {
        let line = "bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert!(!d.message.is_empty());
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
        }
    }
}
