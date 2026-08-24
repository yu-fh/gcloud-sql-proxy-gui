//! Parse a pasted `cloud-sql-proxy` invocation back into a [`Profile`].
//!
//! This module has no Tauri dependency and is unit-testable standalone.
//!
//! # Why this exists
//!
//! Everyone who wants this app already has a working `cloud-sql-proxy` command
//! in their shell history — that command is the problem the app exists to
//! solve. Without this module the first thing the app asks is that they
//! transcribe it into six form fields, which is the same copying, just moved.
//!
//! # This is the inverse of `proxy::args_for`
//!
//! [`crate::core::proxy::args_for`] renders a `Profile` into an argv. This
//! parses an argv back. They are a matched pair, and the round-trip test in
//! this module's tests is what keeps them matched: a change to the emitter that
//! this parser does not understand fails there rather than in front of a user.
//!
//! # Where the argv is not enough
//!
//! Four `Profile` fields have no representation in a `cloud-sql-proxy` command
//! line, so no parser can recover them:
//!
//! - `name` — the user types it; that is the one thing this feature asks for.
//! - `id` — allocated by the caller from the name, so ids stay unique.
//! - `danger` and `vpn_probe_host` — app concepts the proxy knows nothing about.
//!
//! `role` is a fifth, and it is the interesting one: `args_for` emits nothing
//! that distinguishes a primary from a replica. It is assigned here by
//! position — first instance primary, second replica. That guess is cheap to
//! get wrong, because the role never reaches the argv: it drives the display
//! label and the one-of-each rule in `ProfileConfig::validate`, nothing more.
//! Guessing from the port instead would bake this app's 15432/15433 convention
//! into the reading of a command that need not follow it.
//!
//! # The parser is deliberately permissive
//!
//! The real CLI is much wider than the three flags this app emits, and a paste
//! is whatever the user actually runs. So an unrecognised flag is **collected,
//! not rejected** — see [`Imported::ignored_flags`], which the caller shows the
//! user so they know the profile is not equivalent to what they pasted.
//! Rejecting on the first unknown flag would turn a stray `--quiet` into a dead
//! end, and the user would be back to transcribing by hand.

use thiserror::Error;

use crate::core::profile::{Instance, InstanceRole, Profile, ProxyFlags};

/// The result of parsing a command line: a profile, and an honest account of
/// what was thrown away getting there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The parsed profile. `id` and `name` are empty and `danger` is false --
    /// the argv cannot supply them, so the caller fills them in.
    pub profile: Profile,
    /// Flags that were understood well enough to skip but which this app has
    /// no field for, in the order they appeared, rendered as the user wrote
    /// them (`--address 0.0.0.0` becomes `"--address 0.0.0.0"`).
    ///
    /// Not an error: the import still happens. But the resulting profile does
    /// *not* reproduce the pasted command, and the user is the only one who can
    /// judge whether the difference matters.
    pub ignored_flags: Vec<String>,
}

/// Why a paste could not become a profile.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImportError {
    #[error("nothing to import -- paste a cloud-sql-proxy command")]
    Empty,

    #[error(
        "no instance connection names found. Expected at least one argument \
         shaped like 'project:region:instance'."
    )]
    NoInstances,

    #[error(
        "this command has {found} instances; a profile holds at most two (a \
         primary and a read replica). Import them as separate profiles."
    )]
    TooManyInstances { found: usize },

    #[error(
        "'{value}' is not an instance connection name. Expected \
         'project:region:instance'."
    )]
    MalformedConnectionName { value: String },

    #[error("'{value}' is not a valid port")]
    BadPort { value: String },

    #[error("both instances would use port {port}; they must differ")]
    DuplicatePort { port: u16 },

    #[error("unbalanced quote in the pasted command")]
    UnbalancedQuote,
}

/// The most instances one profile can hold: one primary, one replica.
///
/// Enforced here rather than left to `ProfileConfig::validate` so the message
/// can say what to do about it. `validate` would report `DuplicateRole`, which
/// is true but describes an internal invariant rather than the user's problem.
const MAX_INSTANCES: usize = 2;

/// Long flags that take a value, and which this app has no field for. Listed so
/// their value is consumed rather than mistaken for a connection name.
///
/// From `cloud-sql-proxy --help`. This does not need to be exhaustive -- an
/// unlisted value-taking flag degrades to its value being reported as a
/// malformed connection name, which is a legible failure rather than a silent
/// wrong import.
const IGNORED_FLAGS_WITH_VALUE: &[&str] = &[
    "--address",
    "--admin-port",
    "--config-file",
    "--credentials-file",
    "--fuse",
    "--fuse-tmp-dir",
    "--http-address",
    "--http-port",
    "--json-credentials",
    "--login-token",
    "--max-connections",
    "--max-sigterm-delay",
    "--min-sigterm-delay",
    "--prometheus-namespace",
    "--quota-project",
    "--run-connection-test",
    "--telemetry-prefix",
    "--telemetry-project",
    "--telemetry-sample-rate",
    "--token",
    "--unix-socket",
    "--universe-domain",
    "--user-agent",
];

/// Short flags that take a value. `-p` is handled separately (it is `--port`,
/// which this parser *does* understand).
const IGNORED_SHORT_FLAGS_WITH_VALUE: &[&str] = &["-a", "-c", "-t", "-u"];

/// Flags whose *value* must never be repeated back.
///
/// [`Imported::ignored_flags`] is shown in the window and written to the audit
/// log, and the audit log is deliberately unredacted and small enough to mail to
/// a colleague (see the README). A bearer token or a credentials path pasted
/// into the import box would otherwise land in both. The flag name is still
/// reported -- the user needs to know their credentials flag was dropped, which
/// is exactly the case where the imported profile will fail to start in a way
/// that looks unrelated to the import.
///
/// This is a paste box: it is the one place in the app where user-supplied text
/// can contain a live credential.
const SECRET_FLAGS: &[&str] = &[
    "--credentials-file",
    "--json-credentials",
    "--login-token",
    "--token",
    "-c",
    "-t",
];

/// How a dropped flag is rendered for the user, with secrets held back.
fn describe_ignored(name: &str, value: Option<&str>) -> String {
    match value {
        Some(_) if SECRET_FLAGS.contains(&name) => format!("{name} (value not shown)"),
        Some(value) => format!("{name} {value}"),
        None => name.to_string(),
    }
}

/// Parse a pasted command line into a profile.
///
/// Accepts what a user would actually paste: line continuations, quotes, a
/// leading `cloud-sql-proxy` (bare or as a path) or no program word at all,
/// `--flag=value` as well as `--flag value`, and short flags.
pub fn parse_command(input: &str) -> Result<Imported, ImportError> {
    let tokens = tokenize(input)?;
    let tokens = strip_program_word(tokens);

    if tokens.is_empty() {
        return Err(ImportError::Empty);
    }

    let mut flags = ProxyFlags {
        // Not `ProxyFlags::default()`: that defaults both to true, which is
        // right for a new profile but wrong here. A pasted command that omits
        // `--private-ip` is a command that does not use private IP, and
        // importing it as if it did would silently change what runs.
        auto_iam_authn: false,
        private_ip: false,
    };
    let mut impersonate: Option<String> = None;
    let mut ignored_flags = Vec::new();
    let mut base_port: Option<u16> = None;
    let mut positionals: Vec<String> = Vec::new();

    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        index += 1;

        if !token.starts_with('-') || token == "-" {
            positionals.push(token.clone());
            continue;
        }

        // `--flag=value` and `-p=15432` both split here, so the two spellings
        // converge before anything looks at the flag name.
        let (name, inline_value) = match token.split_once('=') {
            Some((name, value)) => (name.to_string(), Some(value.to_string())),
            None => (token.clone(), None),
        };

        // Takes the flag's value from `--flag=value` if it was written that
        // way, else consumes the next token.
        let take_value = |index: &mut usize| -> Option<String> {
            match &inline_value {
                Some(value) => Some(value.clone()),
                None => {
                    let value = tokens.get(*index).cloned();
                    if value.is_some() {
                        *index += 1;
                    }
                    value
                }
            }
        };

        match name.as_str() {
            "--auto-iam-authn" | "-i" => flags.auto_iam_authn = true,
            "--private-ip" => flags.private_ip = true,

            "--impersonate-service-account" => {
                if let Some(value) = take_value(&mut index) {
                    impersonate = Some(value);
                }
            }

            "--port" | "-p" => {
                if let Some(value) = take_value(&mut index) {
                    base_port = Some(
                        value
                            .parse::<u16>()
                            .map_err(|_| ImportError::BadPort { value })?,
                    );
                }
            }

            _ => {
                let takes_value = IGNORED_FLAGS_WITH_VALUE.contains(&name.as_str())
                    || IGNORED_SHORT_FLAGS_WITH_VALUE.contains(&name.as_str());
                let value = if takes_value {
                    take_value(&mut index)
                } else {
                    // A bare unknown flag, or an unknown flag written as
                    // `--flag=value`: in the latter case the value came with
                    // the token, so nothing is consumed.
                    inline_value.clone()
                };
                ignored_flags.push(describe_ignored(&name, value.as_deref()));
            }
        }
    }

    if positionals.is_empty() {
        return Err(ImportError::NoInstances);
    }
    if positionals.len() > MAX_INSTANCES {
        return Err(ImportError::TooManyInstances {
            found: positionals.len(),
        });
    }

    let mut instances = Vec::with_capacity(positionals.len());
    for (ordinal, positional) in positionals.iter().enumerate() {
        let (connection_name, explicit_port) = split_port(positional)?;
        check_connection_name(&connection_name)?;

        // `--port N` is a *base*: the CLI assigns N to the first listener and
        // increments for each one after it ("All subsequent listeners will
        // increment from the provided value"). A per-instance `?port=`
        // overrides it. Reading `--port` as a flat port for every instance
        // would produce two listeners on one port -- exactly the silently-wrong
        // import this feature exists to prevent.
        let port = match (explicit_port, base_port) {
            (Some(port), _) => port,
            (None, Some(base)) => base.saturating_add(ordinal as u16),
            (None, None) => default_port_for(ordinal),
        };

        instances.push(Instance {
            role: role_for(ordinal),
            connection_name,
            port,
        });
    }

    if let [first, second] = instances.as_slice() {
        if first.port == second.port {
            return Err(ImportError::DuplicatePort { port: first.port });
        }
    }

    // Both are carried on the connection name (`project:region:instance`) and
    // nowhere else in the argv, so the first instance is the only source. A
    // second instance in another project is possible to write but is not a
    // shape this app models -- the profile holds one project.
    let (project, region) = project_and_region(&instances[0].connection_name);

    Ok(Imported {
        profile: Profile {
            id: String::new(),
            name: String::new(),
            project,
            region,
            instances,
            flags,
            impersonate_service_account: impersonate,
            danger: false,
            vpn_probe_host: None,
        },
        ignored_flags,
    })
}

/// Split a command line into tokens the way a shell would, minus the parts a
/// pasted proxy invocation never contains (variable expansion, pipes, globs).
///
/// Handles line continuations, single and double quotes, and backslash escapes
/// inside double quotes. A trailing backslash before a newline joins the lines;
/// this is the single most common thing in a pasted command, because that is
/// how the README itself formats one.
fn tokenize(input: &str) -> Result<Vec<String>, ImportError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut has_current = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.peek() {
                // Line continuation: the backslash and the newline both vanish.
                Some('\n') => {
                    chars.next();
                }
                // Inside single quotes a backslash is literal, as in a shell.
                Some(&next) if quote != Some('\'') => {
                    chars.next();
                    current.push(next);
                    has_current = true;
                }
                _ => {
                    current.push(ch);
                    has_current = true;
                }
            },
            '\'' | '"' => match quote {
                Some(open) if open == ch => quote = None,
                Some(_) => {
                    current.push(ch);
                    has_current = true;
                }
                None => {
                    quote = Some(ch);
                    // An empty quoted string is still a token, so record that
                    // one has started even though nothing was pushed.
                    has_current = true;
                }
            },
            ch if ch.is_whitespace() && quote.is_none() => {
                if has_current {
                    tokens.push(std::mem::take(&mut current));
                    has_current = false;
                }
            }
            ch => {
                current.push(ch);
                has_current = true;
            }
        }
    }

    if quote.is_some() {
        return Err(ImportError::UnbalancedQuote);
    }
    if has_current {
        tokens.push(current);
    }

    Ok(tokens)
}

/// Drop a leading program word, if there is one.
///
/// A paste may start with `cloud-sql-proxy`, an absolute path to it, `./
/// cloud-sql-proxy`, or nothing at all when the user copied only the arguments.
/// Anything else is left alone: a first token that is neither a flag nor a
/// recognisable program name is more likely a connection name than a typo, and
/// dropping it would silently lose an instance.
fn strip_program_word(mut tokens: Vec<String>) -> Vec<String> {
    let is_program = tokens
        .first()
        .map(|first| {
            let stem = first.rsplit('/').next().unwrap_or(first);
            stem == "cloud-sql-proxy" || stem == "cloud_sql_proxy"
        })
        .unwrap_or(false);

    if is_program {
        tokens.remove(0);
    }
    tokens
}

/// Split `connection?port=N` into its parts.
///
/// Query parameters other than `port` are dropped: the proxy accepts several
/// instance-level ones and none map to a `Profile` field.
fn split_port(positional: &str) -> Result<(String, Option<u16>), ImportError> {
    let Some((connection, query)) = positional.split_once('?') else {
        return Ok((positional.to_string(), None));
    };

    let mut port = None;
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("port=") {
            port = Some(value.parse::<u16>().map_err(|_| ImportError::BadPort {
                value: value.to_string(),
            })?);
        }
    }

    Ok((connection.to_string(), port))
}

/// Reject anything that is not shaped like `project:region:instance`.
///
/// Checked here rather than left to the user to notice, because the failure it
/// prevents is silent: a stray value that lands in the positional list would
/// otherwise become an instance whose connection name cannot resolve, and the
/// error would surface much later as a failed start.
fn check_connection_name(value: &str) -> Result<(), ImportError> {
    let parts: Vec<&str> = value.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(ImportError::MalformedConnectionName {
            value: value.to_string(),
        });
    }
    Ok(())
}

/// The `project` and `region` halves of a `project:region:instance` name.
///
/// Only ever called on a name that [`check_connection_name`] has accepted.
fn project_and_region(connection_name: &str) -> (String, String) {
    let mut parts = connection_name.split(':');
    let project = parts.next().unwrap_or_default().to_string();
    let region = parts.next().unwrap_or_default().to_string();
    (project, region)
}

/// Role by position: first primary, second replica. See the module docs on why
/// position rather than port.
fn role_for(ordinal: usize) -> InstanceRole {
    if ordinal == 0 {
        InstanceRole::Primary
    } else {
        InstanceRole::Replica
    }
}

/// The port for an instance when the command names none at all.
///
/// This app's convention, not the proxy's -- the proxy would use the database
/// engine's default (5432 for Postgres) and increment. Using the app's
/// convention keeps an imported profile consistent with a hand-made one, and
/// the ports are visible and editable immediately after the import.
fn default_port_for(ordinal: usize) -> u16 {
    crate::core::store::DEFAULT_PRIMARY_PORT.saturating_add(ordinal as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command the README shows, as a user would paste it: line
    /// continuations, quoted positionals, both flags.
    const README_COMMAND: &str = r#"cloud-sql-proxy --auto-iam-authn --private-ip \
  "my-project-dev:us-central1:primary-instance?port=15432" \
  "my-project-dev:us-central1:replica-instance?port=15433""#;

    fn parse(input: &str) -> Imported {
        parse_command(input).expect("should parse")
    }

    #[test]
    fn parses_the_readme_command() {
        let imported = parse(README_COMMAND);
        let profile = &imported.profile;

        assert_eq!(profile.project, "my-project-dev");
        assert_eq!(profile.region, "us-central1");
        assert!(profile.flags.auto_iam_authn);
        assert!(profile.flags.private_ip);
        assert_eq!(profile.impersonate_service_account, None);
        assert!(imported.ignored_flags.is_empty());

        assert_eq!(profile.instances.len(), 2);
        assert_eq!(profile.instances[0].role, InstanceRole::Primary);
        assert_eq!(
            profile.instances[0].connection_name,
            "my-project-dev:us-central1:primary-instance"
        );
        assert_eq!(profile.instances[0].port, 15432);
        assert_eq!(profile.instances[1].role, InstanceRole::Replica);
        assert_eq!(profile.instances[1].port, 15433);
    }

    #[test]
    fn leaves_the_caller_to_supply_what_the_argv_cannot() {
        // Empty rather than guessed: an id invented here could collide with an
        // existing profile, and a name guessed from the project would be wrong
        // as often as not.
        let profile = parse(README_COMMAND).profile;
        assert_eq!(profile.id, "");
        assert_eq!(profile.name, "");
        assert!(!profile.danger);
        assert_eq!(profile.vpn_probe_host, None);
    }

    #[test]
    fn accepts_a_paste_with_no_program_word() {
        let imported = parse("--private-ip my-project-dev:us-central1:only");
        assert_eq!(imported.profile.instances.len(), 1);
        assert!(imported.profile.flags.private_ip);
    }

    #[test]
    fn accepts_the_program_as_a_path() {
        for program in [
            "/opt/homebrew/bin/cloud-sql-proxy",
            "./cloud-sql-proxy",
            "cloud_sql_proxy",
        ] {
            let imported = parse(&format!("{program} my-project-dev:us-central1:only"));
            assert_eq!(
                imported.profile.instances.len(),
                1,
                "{program} should be recognised as the program word"
            );
        }
    }

    #[test]
    fn a_first_token_that_is_not_the_program_is_kept() {
        // The guard against being too eager: a bare connection name first must
        // not be eaten as a program word, or the import silently loses it.
        let imported = parse("my-project-dev:us-central1:only");
        assert_eq!(
            imported.profile.instances[0].connection_name,
            "my-project-dev:us-central1:only"
        );
    }

    #[test]
    fn accepts_equals_and_short_flag_spellings() {
        let imported = parse(
            "-i --impersonate-service-account=sa@example.iam.gserviceaccount.com \
             my-project-dev:us-central1:only",
        );
        assert!(imported.profile.flags.auto_iam_authn);
        assert_eq!(
            imported.profile.impersonate_service_account.as_deref(),
            Some("sa@example.iam.gserviceaccount.com")
        );
    }

    #[test]
    fn omitted_flags_stay_off() {
        // `ProxyFlags::default()` is both-true, which is right for a new
        // profile and wrong for an import: a command without `--private-ip`
        // does not use private IP, and importing it as if it did would change
        // what runs.
        let imported = parse("my-project-dev:us-central1:only");
        assert!(!imported.profile.flags.auto_iam_authn);
        assert!(!imported.profile.flags.private_ip);
    }

    #[test]
    fn port_flag_is_a_base_that_increments() {
        let imported = parse(
            "--port 15432 my-project-dev:us-central1:primary \
             my-project-dev:us-central1:replica",
        );
        assert_eq!(imported.profile.instances[0].port, 15432);
        assert_eq!(imported.profile.instances[1].port, 15433);
    }

    #[test]
    fn per_instance_port_overrides_the_base() {
        let imported = parse(
            "--port 5000 my-project-dev:us-central1:primary?port=15432 \
             my-project-dev:us-central1:replica",
        );
        assert_eq!(imported.profile.instances[0].port, 15432);
        // The base still governs the instance that named no port of its own,
        // and it increments by ordinal rather than by how many defaulted.
        assert_eq!(imported.profile.instances[1].port, 5001);
    }

    #[test]
    fn falls_back_to_this_apps_port_convention() {
        let imported =
            parse("my-project-dev:us-central1:primary my-project-dev:us-central1:replica");
        assert_eq!(imported.profile.instances[0].port, 15432);
        assert_eq!(imported.profile.instances[1].port, 15433);
    }

    #[test]
    fn unknown_flags_are_reported_not_rejected() {
        let imported =
            parse("--private-ip --address 0.0.0.0 --quiet my-project-dev:us-central1:only");

        // The import still happened.
        assert!(imported.profile.flags.private_ip);
        assert_eq!(imported.profile.instances.len(), 1);

        // And the user is told what was dropped, valued flag and bare flag
        // alike.
        assert_eq!(imported.ignored_flags, vec!["--address 0.0.0.0", "--quiet"]);
    }

    #[test]
    fn credential_values_are_never_repeated_back() {
        // `ignored_flags` reaches the window and the audit log, and the log is
        // deliberately unredacted and mailable. This is the one place in the app
        // where pasted text can hold a live credential.
        let secret = "ya29.a0AfB_bytes_that_must_not_leak";
        let imported = parse(&format!(
            "--token {secret} --credentials-file /Users/someone/key.json \
             -t {secret} my-project-dev:us-central1:only"
        ));

        for entry in &imported.ignored_flags {
            assert!(
                !entry.contains(secret) && !entry.contains("key.json"),
                "ignored flag leaked a secret: {entry}"
            );
        }
        // The flag itself is still named -- dropping a credentials flag is
        // exactly the case that fails later in a way that looks unrelated.
        assert_eq!(
            imported.ignored_flags,
            vec![
                "--token (value not shown)",
                "--credentials-file (value not shown)",
                "-t (value not shown)",
            ]
        );
    }

    #[test]
    fn non_secret_flag_values_are_shown() {
        // The complement: withholding an address or a port would leave the user
        // guessing which `--address` they had set.
        let imported = parse("--address 0.0.0.0 my-project-dev:us-central1:only");
        assert_eq!(imported.ignored_flags, vec!["--address 0.0.0.0"]);
    }

    #[test]
    fn an_unknown_flags_value_is_not_mistaken_for_an_instance() {
        // The failure this prevents: `0.0.0.0` landing in the positional list
        // and becoming an instance, or tripping the too-many-instances guard.
        let imported = parse("--address 0.0.0.0 my-project-dev:us-central1:only");
        assert_eq!(imported.profile.instances.len(), 1);
        assert_eq!(
            imported.profile.instances[0].connection_name,
            "my-project-dev:us-central1:only"
        );
    }

    #[test]
    fn an_unknown_flag_with_an_inline_value_consumes_nothing() {
        let imported = parse("--address=0.0.0.0 my-project-dev:us-central1:only");
        assert_eq!(imported.ignored_flags, vec!["--address 0.0.0.0"]);
        assert_eq!(imported.profile.instances.len(), 1);
    }

    #[test]
    fn tokenizes_quotes_and_continuations() {
        let imported = parse(
            "cloud-sql-proxy \\\n  'my-project-dev:us-central1:primary' \\\n  \
             \"my-project-dev:us-central1:replica\"",
        );
        assert_eq!(imported.profile.instances.len(), 2);
        assert_eq!(
            imported.profile.instances[1].connection_name,
            "my-project-dev:us-central1:replica"
        );
    }

    #[test]
    fn rejects_nothing_to_import() {
        assert_eq!(parse_command(""), Err(ImportError::Empty));
        assert_eq!(parse_command("   \n  "), Err(ImportError::Empty));
        assert_eq!(parse_command("cloud-sql-proxy"), Err(ImportError::Empty));
    }

    #[test]
    fn rejects_a_command_with_no_instances() {
        assert_eq!(
            parse_command("cloud-sql-proxy --private-ip"),
            Err(ImportError::NoInstances)
        );
    }

    #[test]
    fn rejects_more_than_two_instances() {
        let result = parse_command(
            "my-project-dev:us-central1:a my-project-dev:us-central1:b \
             my-project-dev:us-central1:c",
        );
        assert_eq!(result, Err(ImportError::TooManyInstances { found: 3 }));
        // The message has to say what to do about it, not just what is wrong.
        let message = result.unwrap_err().to_string();
        assert!(message.contains("separate profiles"), "{message}");
    }

    #[test]
    fn rejects_a_malformed_connection_name() {
        for bad in ["not-a-connection-name", "only:two", "a:b:c:d", ":b:c"] {
            let result = parse_command(bad);
            assert!(
                matches!(result, Err(ImportError::MalformedConnectionName { .. })),
                "{bad} should be rejected, got {result:?}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_port() {
        assert!(matches!(
            parse_command("my-project-dev:us-central1:only?port=notanumber"),
            Err(ImportError::BadPort { .. })
        ));
        assert!(matches!(
            parse_command("--port 99999999 my-project-dev:us-central1:only"),
            Err(ImportError::BadPort { .. })
        ));
    }

    #[test]
    fn rejects_two_instances_on_one_port() {
        // `ProfileConfig::validate` would also catch this, but only after the
        // caller has allocated an id and built a config -- and its message
        // names an internal invariant rather than the pasted command.
        assert_eq!(
            parse_command(
                "my-project-dev:us-central1:a?port=15432 \
                 my-project-dev:us-central1:b?port=15432"
            ),
            Err(ImportError::DuplicatePort { port: 15432 })
        );
    }

    #[test]
    fn rejects_an_unbalanced_quote() {
        assert_eq!(
            parse_command("cloud-sql-proxy \"my-project-dev:us-central1:only"),
            Err(ImportError::UnbalancedQuote)
        );
    }

    #[test]
    fn drops_non_port_query_parameters() {
        let imported = parse("my-project-dev:us-central1:only?auto-iam-authn=true&port=15432");
        assert_eq!(
            imported.profile.instances[0].connection_name,
            "my-project-dev:us-central1:only"
        );
        assert_eq!(imported.profile.instances[0].port, 15432);
    }

    // --- the round trip --------------------------------------------------
    //
    // The property that keeps this parser and `proxy::args_for` honest with
    // each other. A change to the emitter that this parser does not understand
    // fails here, rather than in front of a user holding a command that no
    // longer imports.

    fn round_trip(profile: &Profile) -> Profile {
        let argv = crate::core::proxy::args_for(profile);
        // Quoted the way a shell would need, since a connection name contains
        // `?` and `&`.
        let command = std::iter::once("cloud-sql-proxy".to_string())
            .chain(argv.into_iter().map(|arg| format!("\"{arg}\"")))
            .collect::<Vec<_>>()
            .join(" ");
        parse_command(&command)
            .unwrap_or_else(|error| panic!("emitted argv should re-parse: {error}"))
            .profile
    }

    fn profile_for_round_trip(flags: ProxyFlags, impersonate: Option<&str>) -> Profile {
        Profile {
            id: "dev".to_string(),
            name: "dev".to_string(),
            project: "my-project-dev".to_string(),
            region: "us-central1".to_string(),
            instances: vec![
                Instance {
                    role: InstanceRole::Primary,
                    connection_name: "my-project-dev:us-central1:primary".to_string(),
                    port: 15432,
                },
                Instance {
                    role: InstanceRole::Replica,
                    connection_name: "my-project-dev:us-central1:replica".to_string(),
                    port: 15433,
                },
            ],
            flags,
            impersonate_service_account: impersonate.map(str::to_string),
            danger: false,
            vpn_probe_host: None,
        }
    }

    #[test]
    fn round_trips_every_flag_combination() {
        for auto_iam_authn in [false, true] {
            for private_ip in [false, true] {
                for impersonate in [None, Some("sa@example.iam.gserviceaccount.com")] {
                    let original = profile_for_round_trip(
                        ProxyFlags {
                            auto_iam_authn,
                            private_ip,
                        },
                        impersonate,
                    );
                    let parsed = round_trip(&original);

                    assert_eq!(parsed.instances, original.instances);
                    assert_eq!(parsed.flags, original.flags);
                    assert_eq!(
                        parsed.impersonate_service_account,
                        original.impersonate_service_account
                    );
                    assert_eq!(parsed.project, original.project);
                    assert_eq!(parsed.region, original.region);
                }
            }
        }
    }

    #[test]
    fn an_imported_profile_passes_config_validation() {
        // The seam between this module and `import_profile`: the command names
        // the id and pushes the profile into a `ProfileConfig`, which is
        // rejected wholesale if it does not validate. Everything `validate`
        // checks -- one instance minimum, one role each, distinct ports -- is
        // something this parser can produce, so it is worth proving rather than
        // discovering when a paste fails to save.
        use crate::core::profile::{ProfileConfig, CURRENT_SCHEMA_VERSION};

        for command in [
            README_COMMAND,
            "my-project-dev:us-central1:only",
            "--port 25432 my-project-dev:us-central1:a my-project-dev:us-central1:b",
        ] {
            let imported = parse(command);
            let profile = Profile {
                id: "imported".to_string(),
                name: "Imported".to_string(),
                ..imported.profile
            };
            let config = ProfileConfig {
                version: CURRENT_SCHEMA_VERSION,
                profiles: vec![profile],
            };
            assert_eq!(config.validate(), Ok(()), "command: {command}");
        }
    }

    #[test]
    fn round_trips_a_single_instance_profile() {
        let mut original = profile_for_round_trip(ProxyFlags::default(), None);
        original.instances.truncate(1);

        let parsed = round_trip(&original);
        assert_eq!(parsed.instances, original.instances);
    }

    #[test]
    fn round_trips_non_default_ports() {
        let mut original = profile_for_round_trip(ProxyFlags::default(), None);
        original.instances[0].port = 25432;
        original.instances[1].port = 25433;

        let parsed = round_trip(&original);
        assert_eq!(parsed.instances, original.instances);
    }
}
