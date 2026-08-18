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

/// Extract a port number from a line like
/// `listen tcp 127.0.0.1:15432: bind: address already in use`.
///
/// Looks for the first `:<digits>` group that is immediately followed by a
/// non-digit (or end of string), scanning left to right.
fn extract_port(line: &str) -> Option<u16> {
    let bytes = line.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b != b':' {
            continue;
        }
        let rest = &line[i + 1..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        if let Ok(port) = digits.parse::<u16>() {
            return Some(port);
        }
    }
    None
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
    fn port_in_use_takes_precedence_over_network_wording() {
        // Contains "listen tcp" which could be mistaken for "dial tcp"/network
        // wording if the port-in-use check ran after the VPN checks.
        let line =
            "failed to start listener: listen tcp 127.0.0.1:15433: bind: address already in use";
        match classify(line) {
            ProxyEvent::Failure(d) => {
                assert_eq!(d.kind, FailureKind::PortInUse);
                assert_ne!(d.kind, FailureKind::OffVpn);
            }
            other => panic!("expected Failure(PortInUse), got {other:?}"),
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
