# Cloud SQL Proxy GUI

A macOS menu bar app for running `cloud-sql-proxy` against Firsthand's Cloud SQL
instances. Click an environment to connect; click it again to disconnect.

## Why

Connecting by hand means running a command like this and leaving it in a
terminal:

```bash
cloud-sql-proxy --auto-iam-authn --private-ip \
  "my-project-dev:us-central1:primary-instance?port=15432" \
  "my-project-dev:us-central1:replica-instance?port=15433"
```

Three things make that worse than it looks:

- **Switching environments means editing a long command.** The connection names
  are opaque, and dev, stg, and prd differ only by project.
- **Connection names go stale.** The `terraform-…` suffix is generated, so when
  an instance is replaced your working command silently stops working.
- **Failures are opaque.** Expired credentials, a disconnected VPN, and an
  already-bound port all present as "the proxy is running but I can't connect",
  and telling them apart means reading the proxy's log.

This app keeps the connection names fresh, switches environments in one click,
and — when a connection fails — tells you which of those three it was and what
to run.

## Requirements

- macOS 11 or later
- `cloud-sql-proxy` — `brew install cloud-sql-proxy`
- `gcloud`, authenticated once with `gcloud auth application-default login`
- Membership in `cloud-sql-users@example.com`
- The corporate VPN — the instances are private-IP only, so nothing connects
  without it

## Build

```bash
npm install
npx tauri build
```

The app bundle lands in `src-tauri/target/release/bundle/macos/`. For
development, `npx tauri dev` runs it with hot reload.

There is no signed release and no Apple Developer account behind this, so
distribution is source-only for now.

## Using it

The app lives in the menu bar and has no Dock icon. Click the icon for the menu:

```
● Cloud SQL Proxy
──────────────────
dev — 127.0.0.1:15432, :15433
──────────────────
✓ dev  (15432/15433)
  stg  (15432/15433)
⚠ prd  (15432/15433)
──────────────────
Profiles…
Logs…
Refresh connection names
Launch at Login        ✓
──────────────────
Quit
```

Clicking an environment starts or stops it. The status line at the top shows
what is running and on which ports, and every row carries its ports — with all
environments on the same ports by default, the port alone never tells you which
environment you are talking to.

Environments marked as production show a `⚠` and ask for confirmation before
starting.

**Quitting the app stops every proxy it started.** The app owns their lifetime
deliberately: it does not install a launchd agent or leave anything running in
the background.

### First run

The app seeds three environments — dev, stg, and prd — with empty connection
names, so nothing can start until you fill them in. Open **Profiles…** and
choose **Refresh Connection Names**: it queries `gcloud sql instances list` for
each project, shows what it proposes to change, and writes only what you
approve.

Do the same whenever a connection that used to work starts reporting that the
instance does not exist. That is Terraform having replaced the instance.

### Environments are yours to define

The seeded three are a convenience, not a fixed set. In **Profiles…** you can
add, rename, and delete environments, mark any of them as production, and set
their ports and connection names. Ids are stable across a rename, so the app
never loses track of a running proxy because you retitled it.

### Only one environment at a time, by default

Ports follow the team convention: **15432 for the primary, 15433 for the read
replica**. Every environment defaults to that same pair, so two cannot bind at
once — starting one stops the other, and the app tells you before it does.

That is deliberate. It means your TablePlus, DBeaver, and `psql` settings keep
working no matter which environment is up, and it makes it hard to be connected
to production while believing you are on dev.

If you genuinely need two at once, give one of them different ports in
**Profiles…** — say 25432 and 25433 for prd. Environments whose ports do not
overlap run side by side with no prompt. The cost is that your client
configuration now has to distinguish them, which is exactly the trade you are
making.

### Connecting

| Field | Value |
| --- | --- |
| Host | `127.0.0.1` |
| Port | `15432` (primary) or `15433` (read replica) |
| User | your Google account email |
| Password | leave blank — the proxy injects an IAM token |
| Database | `fh_ui_<env>` or `fh_knowledge_<env>` |
| SSL mode | disable — the proxy already wraps the connection in TLS |

Use the replica for heavy read queries.

If your client reports `FATAL: empty password returned by client`, it has a
password saved. Clear it.

## When something fails

The app names the cause rather than showing you a log:

| What you see | What it means |
| --- | --- |
| Credentials expired | Run `gcloud auth application-default login`. The app offers the command to copy. |
| Off VPN | Cloud SQL is private-IP only. Connect to the VPN. |
| Port in use | Another environment or a stray proxy holds it. Stop it, or change this environment's ports. |
| Instance not found | Terraform replaced it. Run **Refresh Connection Names**. |

**Logs…** shows the proxy's raw output if you need more than that.

## Tests

```bash
cd src-tauri && cargo test
```

Process management is tested against a fake proxy script in `tests/fixtures/`,
so the suite needs neither GCP nor the VPN. That includes the orphan-prevention
test, which checks a real PID against the OS after dropping the manager: a
leaked proxy would keep holding port 15432 and break the next launch, so it is
verified rather than assumed.

## How it is built

Tauri v2 — a Rust core with a small webview for the windows that need a form.

The core (`src-tauri/src/core/`) has no Tauri dependency, so it tests
standalone:

| Module | Responsibility |
| --- | --- |
| `profile` | Profile types, validation, port and role uniqueness |
| `store` | `profiles.json` load and save, atomic writes, seeding |
| `log_watcher` | Turns proxy output into a diagnosis with a fix |
| `preflight` | Port, credential, and connection-name checks before spawn |
| `proxy` | Owns the child processes; kills them on exit |
| `discovery` | Reads `gcloud sql instances list`, reconciles drift |
| `state` | Decides what must stop before a profile can start |

The Tauri layer above it is deliberately thin: `commands.rs` for the webview,
`tray.rs` for the menu. Both route start decisions through `core::state` so the
menu and the window cannot disagree about what starting an environment means.

Configuration lives in
`~/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json`.
It is plain JSON on purpose — small enough to read, edit by hand, back up, or
send to a colleague. Writes go through a temp file and `fsync` before being
renamed into place, so an interrupted write cannot leave a truncated config.
