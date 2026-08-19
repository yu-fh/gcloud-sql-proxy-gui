# Cloud SQL Proxy GUI — Design

**Date:** 2026-08-18
**Status:** Approved (pending implementation plan)

## Problem

Connecting to Cloud SQL Postgres databases requires running `cloud-sql-proxy` by hand:

```bash
cloud-sql-proxy --auto-iam-authn --private-ip \
  "my-project-dev:us-central1:primary-instance?port=15432" \
  "my-project-dev:us-central1:replica-instance?port=15433"
```

Three problems with the status quo:

1. **Switching environments means editing a long command.** Connection names are opaque Terraform-generated IDs, and dev/stg/prd differ only by project.
2. **Connection names go stale.** Terraform replaces instances, so a working command silently breaks. Shell history shows dev's replica ID changing from `replica-instance` to `replica-instance-v2`, and prd's primary from `terraform-2023...` to `primary-instance-v2`.
3. **Failures are opaque.** The three real failure modes — expired ADC, VPN down, port already bound — all present as "the proxy is running but I can't connect", and diagnosing them means reading `/tmp/cloud-sql-proxy-dev.log` and cross-referencing the troubleshooting section of `fh-mono/docs/claude/cloud-sql-access.md`.

## Goal

A macOS menu bar app, modelled on Tailscale, that starts and stops `cloud-sql-proxy` for named environment profiles with one click, keeps connection names fresh, and names the fix when a connection fails.

Non-goal: replacing the documented launchd/shell-function patterns for keeping a proxy alive in the background. The app owns the proxy's lifetime — quitting the app stops the proxy.

## Background: existing conventions

From `fh-mono/docs/claude/cloud-sql-access.md`:

- Authentication is Cloud SQL IAM group auth via `--auto-iam-authn`; the user authenticates as their own Google identity, with permissions from `cloud-sql-users@example.com`.
- Instances are private-IP only, so **the corporate VPN is required**.
- Port convention: **15432 = primary, 15433 = read replica**. Every environment reuses these ports.
- Each environment exposes a primary and a read replica, both passed to a single proxy process.
- Discovery: `gcloud sql instances list --project=<p> --billing-project=<p> --format='value(name,instanceType,connectionName)'`, where `instanceType` is `CLOUD_SQL_INSTANCE` for the primary and `READ_REPLICA_INSTANCE` for the replica.
- Environment projects: dev `my-project-dev`, stg `my-project-stg`, prd `my-project-prd`.

Verified on this machine: a single gcloud account (`you@example.com`), a single gcloud configuration, and one ADC file. Per-profile credentials are therefore not needed. An `impersonateServiceAccount` field is reserved in the schema for future users with multi-account setups, but is unused and hidden.

## Architecture

Tauri v2. The Rust backend owns all state and child processes. The primary UI is a native tray menu; a webview window opens on demand for profile editing and log viewing.

```
Tray (NSStatusItem, ActivationPolicy::Accessory — no Dock icon)
  native NSMenu, rebuilt whenever state changes

Rust core
  ProfileStore     load and save profiles.json
  ProxyManager     spawn and kill cloud-sql-proxy children; owns every child
  LogWatcher       parse child stderr, classify into known failure modes
  Preflight        VPN reachable? ADC valid? ports free?
  GcloudDiscovery  run `gcloud sql instances list`, reconcile connection names

Webview window (opened on demand only)
  Profiles editor, log viewer
```

Tauri is a good fit for a menu bar utility: `TrayIconBuilder` provides the native menu, and the runtime call `app.set_activation_policy(ActivationPolicy::Accessory)` removes the Dock icon and app-switcher entry. That call is the only mechanism — Tauri 2.11 has no equivalent config field, so this cannot be set declaratively in `tauri.conf.json`.

Child processes use `tokio::process` rather than `tauri-plugin-shell`. The plugin would work, but `tokio::process::Command` offers `kill_on_drop(true)` — which is the orphan guarantee that matters here, since a leaked child keeps holding port 15432 — and it lets `ProxyManager` be unit-tested without constructing a Tauri app or navigating the plugin's permission scoping.

### Unit responsibilities

Each unit has one purpose, a narrow interface, and can be tested without the others.

**ProfileStore** — reads and writes `profiles.json`; validates port uniqueness within the config. Depends on the filesystem only.

**ProxyManager** — given a profile, spawns one `cloud-sql-proxy` process with all of that profile's instances as positional arguments; tracks PID and state; kills on request and on app exit. Depends on an injectable "binary path" so tests can substitute a fake. Owns no policy — it does not decide whether a start is allowed.

**LogWatcher** — consumes a line stream, emits either `Ready` or a classified `ProxyError`. Pure function of its input; no I/O.

**Preflight** — answers "can this profile start right now?" and returns either `Ok` or a specific reason. Depends on network probes and the ADC file, both injectable.

**GcloudDiscovery** — shells out to `gcloud`, parses the output, and returns instance descriptors. Parsing is separated from the shell call so it can be tested against recorded fixtures.

The tray layer holds no logic beyond translating menu events into core calls and rendering state.

### Process ownership

One running profile equals one `cloud-sql-proxy` child holding both of that profile's instances. Killing the process takes both instances down together.

On app exit every child is killed. Because a leaked child would hold port 15432 and block the next start, cleanup is belt-and-braces: a `Drop` implementation on `ProxyManager`, plus signal handlers for SIGTERM and SIGINT, plus a panic hook. On startup the app also detects an already-bound configured port and reports it rather than failing opaquely.

## Data model

`~/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json`:

```jsonc
{
  "version": 1,
  "profiles": [
    {
      "id": "dev",
      "name": "dev",
      "project": "my-project-dev",
      "region": "us-central1",
      "instances": [
        {
          "role": "primary",
          "connectionName": "my-project-dev:us-central1:primary-instance",
          "port": 15432
        },
        {
          "role": "replica",
          "connectionName": "my-project-dev:us-central1:replica-instance",
          "port": 15433
        }
      ],
      "flags": { "autoIamAuthn": true, "privateIp": true },
      "impersonateServiceAccount": null,
      "danger": false
    }
  ]
}
```

Ports are per-instance and default to the documented 15432/15433. `danger: true` marks prd, which drives a red indicator in the menu and a confirmation prompt before starting.

`version` exists so a future schema change can migrate rather than silently misread; on encountering an unknown version the app refuses to load and says so.

## Concurrency model: exclusive by default

Because every environment uses 15432/15433 by convention, two profiles cannot run simultaneously on their default ports.

Default behavior: starting a profile while another is running prompts **"Stop dev and start prd?"**. This preserves the documented port convention, so existing TablePlus/DBeaver/psql configurations keep working unchanged regardless of which environment is up.

Opt-in concurrency: a profile may be assigned non-default ports (for example prd on 25432/25433). Profiles whose ports do not overlap run concurrently with no prompt. This makes the simultaneous dev-and-prd case possible without making divergent client configs the default.

Port handling is split between config integrity and runtime availability, because sharing a port across profiles is intentional here while sharing one *within* a profile is always a bug:

1. **Save time** — the profile editor rejects a duplicate port within a single profile (such a profile could never start). It permits the same port across different profiles, since that is exactly what exclusive-by-default means; the app instead records those profiles as mutually exclusive.
2. **Start time** — a bind test immediately before spawn catches ports held by another profile, a stray terminal proxy, Postgres.app, or a previous leaked child. `address already in use` on stderr is also classified as a fallback.

## Failure diagnosis

This is the core value of the app over a shell alias. Checks run before spawn and on failure, and the result names the fix instead of showing a log.

| Condition | Detection | Surfaced as |
| --- | --- | --- |
| ADC expired | token expiry in `application_default_credentials.json`; or `PAM authentication failed` / token rejection on stderr | "ADC expired — run `gcloud auth application-default login`", with a copy-to-clipboard button |
| Off VPN | TCP reachability probe to the environment's private DNS name (`pg.<env>.internal.example.com`); or connection timeouts after the proxy reports ready | "Off VPN — Cloud SQL is private-IP only" |
| Port in use | bind test before spawn; `address already in use` on stderr | "Port 15432 in use — dev is already running, or a stray proxy holds it" |
| Stale connection name | `instance does not exist` on stderr | "Instance was replaced by Terraform — run Refresh connection names" |
| Password set in client | `FATAL: empty password returned by client` observed in the client, not the proxy | Documented in README; not detectable by the app |

Readiness is determined by parsing stderr for `ready for new connections`. Until that line appears the profile shows amber, not green, so the indicator never claims a connection is usable before it is.

Because the VPN probe and the ADC check can both pass at start and fail later, the app re-checks on failure rather than only at spawn.

## Connection name refresh

"Refresh connection names" runs, per profile:

```bash
gcloud sql instances list --project=<project> --billing-project=<project> \
  --format='value(name,instanceType,connectionName)'
```

It maps `CLOUD_SQL_INSTANCE` to `primary` and `READ_REPLICA_INSTANCE` to `replica`, diffs against the stored connection names, and presents the changes for confirmation before writing. This addresses the Terraform drift visible in shell history.

Refresh is explicit rather than automatic on every launch: it costs a network round trip per environment, and silently rewriting config is worse than telling the user what changed.

## First run

With no config file present, the app seeds the three known profiles using the project IDs above, marks prd as `danger`, then offers to run a refresh to populate connection names. The user gets a working app without pasting a Terraform instance ID.

If `gcloud` is missing or unauthenticated, seeding still succeeds and the refresh reports the specific problem.

## Tray menu

```
● Cloud SQL Proxy
──────────────────
dev — running
  primary  127.0.0.1:15432
  replica  127.0.0.1:15433
──────────────────
✓ dev
  stg
  ⚠ prd
──────────────────
Profiles…
Logs…
Refresh connection names
Launch at Login        ✓
──────────────────
Quit
```

Icon states: gray (nothing running), amber (starting, not yet ready), green (ready), red (failed). Clicking a profile toggles it. The status block lists every active profile with its ports, so the port a given environment is on is always visible next to the environment's name — this matters most for prd.

"Launch at Login" uses `tauri-plugin-autostart` and only launches the app; it starts no proxy, consistent with the app owning proxy lifetime.

## Testing

- **ProxyManager** against a fake binary — a script that emits proxy-like lines then sleeps — covering start, stop, crash mid-run, and orphan cleanup on exit. No GCP or VPN dependency.
- **LogWatcher** classification, table-driven over the real stderr strings documented in the troubleshooting section, asserting each maps to the right diagnosis.
- **Port collision**: bind a port inside the test, assert preflight refuses with the port-in-use message.
- **GcloudDiscovery** parsing against recorded `gcloud sql instances list` output, including the primary/replica disambiguation and a malformed-output case.
- **ProfileStore**: round-trip, rejection of duplicate ports, refusal of an unknown schema version.
- Tray and menu behavior is verified manually; automating macOS menu interaction is not worth the cost.

## Out of scope

- launchd supervision, sleep/wake restart, crash auto-restart — the app owns the proxy's lifetime and stopping the app stops the proxy.
- Path B (direct connection with a manually minted IAM token).
- Launching psql or TablePlus.
- Windows and Linux support.
- Apple notarization and a signed installer. Distribution to the team, and later open source, is intended to go through a Homebrew cask built from source, which avoids Gatekeeper without an Apple Developer account.

## Decisions and their reasons

**Tauri over Swift, Electron, or Go.** Tauri gives a native tray menu and a small binary while keeping a web frontend for the profile editor, and leaves a cross-platform path open. Swift would be more native but macOS-only and higher friction for outside contributors; Electron is too heavy for a menu bar utility; Go has the weakest UI story.

**Native menu plus a separate window, not a popover.** This mirrors what Tailscale actually does: the tray menu is the app, and richer interactions open real windows. Toggling is the daily action and belongs in the menu; profile editing is rare and needs a form.

**Exclusive by default.** The initial instinct was full concurrency with unique ports, but the documented 15432/15433 convention means concurrency forces divergent client configs. Exclusive-by-default preserves the convention; a port offset makes concurrency available when it is actually wanted.
