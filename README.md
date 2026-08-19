# Cloud SQL Proxy GUI

A macOS menu bar app for running `cloud-sql-proxy` against your Cloud SQL
instances. Click an environment to connect; click it again to disconnect.

You define the environments yourself — the app ships with none. Every project
ID, connection name and hostname in this README is a placeholder.

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
  are opaque, and environments often differ only by project.
- **Connection names go stale.** Provisioning tools generate the instance name,
  so when an instance is replaced your working command silently stops working.
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
- IAM permission to connect to the instances (typically via a group such as
  `cloud-sql-users@example.com`)
- Network reach to the instances — if they are private-IP only, that usually
  means a VPN, and nothing connects without it

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

The app starts with no environments — it has no way to guess which projects are
yours, and a pre-filled environment would invite a connection you never chose.
Open **Profiles…**, add one per environment you use, then set its project and
type its connection names into the Primary and Read Replica fields. A connection
name looks like `project:region:instance`; to find them:

```sh
gcloud sql instances list --project=<project> \
  --format='value(name,instanceType,connectionName)'
```

Do the same whenever a connection that used to work starts reporting that the
instance does not exist — that is the instance having been replaced, which
changes its connection name. Provisioning tools do this routinely: Terraform,
for example, names instances with a generated `terraform-<timestamp>` suffix
that changes every time the instance is recreated.

### Environments are yours to define

In **Profiles…** you can add, rename, and delete environments, mark any of them
as production, and set their ports and connection names. Ids are stable across a
rename, so the app never loses track of a running proxy because you retitled it.

### Only one environment at a time, by default

Ports default to **15432 for the primary, 15433 for the read replica**. Every
environment defaults to that same pair, so two cannot bind at
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
| Database | whichever database you mean to open |
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
| Instance not found | The instance was replaced. Look the new connection name up and type it into **Profiles…**. |

**Logs…** shows the full audit trail if you need more than that.

## The audit log

**Logs…** is not just the proxy's output. It is an append-only trail of four
things, in one ordered stream:

- **what you did** — started or stopped an environment, what a save changed
  field by field, profiles added and deleted, production-start confirmations
  (both the confirmed and the cancelled ones), the window being opened;
- **what the app did** — preflight outcomes, the exact `cloud-sql-proxy` argv,
  the child's pid and exit code, every status transition and what caused it,
  tray menu rebuilds;
- **what the machine is** — recorded once per launch: app version, macOS
  version, the resolved `cloud-sql-proxy` path and its version, the `gcloud`
  account, and the config path;
- **what the proxy printed** — every line, tagged with its environment.

Filter it by environment, by severity, or both. Errors are called out in the
view, so a failed start is findable without reading the whole thing.

It is also written to disk, so a crash or a restart does not lose it:

```
~/Library/Logs/ai.firsthand.fh-cloud-sql-proxy-gui/audit.log
```

That is the macOS convention and where Console.app looks. **Show in Finder** in
the Logs section reveals it. The file rotates at 2 MiB and keeps three older
generations, so it is capped at 8 MiB and small enough to mail to a colleague.

**It is not redacted.** Your `gcloud` account email, your GCP project ids, and
your full connection names are written in the clear — a log that elides the
connection name cannot answer the question you opened it for. Credentials are
the exception and are never recorded: the app reads the account email, never the
token beside it. The file is local and readable only by you; treat it as you
would `profiles.json` before sending it anywhere.

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

Tauri v2 — a Rust core plus one small webview window, with a sidebar for the two
things a menu cannot do: a form and a log buffer.

The core (`src-tauri/src/core/`) has no Tauri dependency, so it tests
standalone:

| Module | Responsibility |
| --- | --- |
| `audit` | The append-only trail: in-memory view plus a rotating file |
| `profile` | Profile types, validation, port and role uniqueness |
| `store` | `profiles.json` load and save, atomic writes |
| `log_watcher` | Turns proxy output into a diagnosis with a fix |
| `preflight` | Port, credential, and connection-name checks before spawn |
| `proxy` | Owns the child processes; kills them on exit |
| `state` | Decides what must stop before a profile can start |

The Tauri layer above it is deliberately thin, and split by responsibility:
`commands.rs` is the IPC surface, `tray.rs` builds and polls the menu,
`window.rs` opens the settings window and remembers its geometry, `dialogs.rs`
raises the native alerts both of them need. The tray and the window route start
decisions through `core::state`, so they cannot disagree about what starting an
environment means.

The frontend (`src/`) has no bundler — plain ES modules with relative imports,
under a `default-src 'self'` CSP. `main.js` wires them together; `ipc.js` is the
only module that touches `window.__TAURI__`; `shell.js` owns the sidebar and the
window keyboard; `profiles-view.js` and `logs-view.js` are the two sections.
Styles are split under `src/css/` and pulled in by `styles.css` in cascade
order.

Configuration lives in
`~/Library/Application Support/ai.firsthand.fh-cloud-sql-proxy-gui/profiles.json`.
It is plain JSON on purpose — small enough to read, edit by hand, back up, or
send to a colleague. Writes go through a temp file and `fsync` before being
renamed into place, so an interrupted write cannot leave a truncated config.
