//! The menu bar tray: the app's entire primary UI, modelled on Tailscale.
//!
//! A native `NSMenu` hanging off the tray icon lists every profile with its
//! ports and live status; clicking one toggles it. Richer interactions
//! (`Profiles…`, `Logs…`) open real webview windows rather than cramming an
//! editor into a popover.
//!
//! # No duplicated policy
//!
//! Every action here calls the corresponding function in [`crate::commands`]
//! rather than reimplementing it. Those are plain `async fn`s — the
//! `#[tauri::command]` attribute adds an IPC wrapper but leaves the function
//! itself callable — and [`tauri::Manager::state`] hands us the same
//! `State<'_, SharedState>` an IPC call would receive. So the tray and the
//! webview cannot drift apart on the start policy, the preflight gate, or the
//! stop-then-start conflict resolution: there is one implementation.
//!
//! # Locking
//!
//! This module takes no locks of its own beyond what it needs to render the
//! menu, and where it does it follows the contract on
//! [`crate::app_state::Shared`]: `config` before `manager`, never the reverse.
//! [`snapshot`] is the only place that takes both, and it takes them in that
//! order, clones what it needs, and drops both guards before touching a menu
//! item — so the poll loop can never be found holding a lock while blocked on
//! the main thread, and a concurrent click handler can never be blocked on the
//! poll loop for longer than a clone.
//!
//! # One icon, not four
//!
//! The tray keeps a single icon; aggregate state is carried by the status line
//! at the top of the menu and the per-profile suffixes, not by the glyph in
//! the menu bar. Four icon variants would mean four hand-drawn assets that a
//! designer has not been asked for, and getting them wrong (a colour that
//! vanishes against a dark menu bar, say) is worse than not having them. The
//! menu is one click away and says more than a colour could. If distinct
//! icons are wanted later, `TrayIcon::set_icon` is the hook and the poll loop
//! is where the call belongs.
//!
//! # Why polling
//!
//! A profile reaches `Running` a second or so after spawn, asynchronously,
//! when the log watcher sees the ready line. The core deliberately exposes no
//! change notification (it has no Tauri dependency), so rather than thread an
//! event channel through it, the tray re-reads the state once a second. One
//! second is well under human notice for a status line and keeps the tray the
//! only thing that knows a tray exists.
//!
//! The same poll carries profile *set* changes, which arrive by the same route
//! — the Profiles window writes the config through the command layer, and the
//! core has no way to announce it. Each tick compares the ids the current menu
//! was built from against the snapshot's, and rebuilds the menu only when they
//! differ. Rebuilding unconditionally would flicker the menu once a second for
//! the sake of something that happens when a user adds a profile; rebuilding on
//! change costs nothing in the common case and is the only way an added profile
//! ever appears or a deleted one ever goes away.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;

use crate::app_state::SharedState;
use crate::commands;

/// How often the menu re-reads live status. See the module docs.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Menu item ids. Profile toggles get `profile:{id}`, so a profile named
/// `logs` cannot collide with the `Logs…` item.
const ID_STATUS: &str = "status";
const ID_PROFILES: &str = "open-profiles";
const ID_LOGS: &str = "open-logs";
const ID_REFRESH: &str = "refresh";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const PROFILE_PREFIX: &str = "profile:";

/// The tray's own id, used to look the menu back up when a click has to
/// correct a checkbox the OS already flipped.
const TRAY_ID: &str = "main";

/// Window labels. These two are the only labels the ACL in
/// `capabilities/default.json` grants plugin permissions to, so they must
/// match it exactly — a window under any other label gets no plugin access
/// and its JS would fail at runtime rather than at build time.
const WINDOW_PROFILES: &str = "profiles";
const WINDOW_LOGS: &str = "logs";

/// One profile's rendering inputs, cloned out from behind the locks.
struct ProfileRow {
    id: String,
    name: String,
    ports: Vec<u16>,
    danger: bool,
    status: ProxyStatus,
}

impl ProfileRow {
    /// The `CheckMenuItem` label, e.g. `"⚠ prd  (15432/15433)"` or
    /// `"dev  (15432/15433) — starting…"`.
    ///
    /// The ports are always present because all three environments share
    /// 15432/15433 by convention and prd is one click from on: a bare "prd"
    /// next to a bare "dev" would make the user guess which environment owns
    /// the port their psql session is pointed at. The `⚠` marks danger
    /// profiles; menu items carry no colour, so a glyph is the available
    /// channel.
    fn label(&self) -> String {
        let mut label = String::new();
        if self.danger {
            label.push_str("⚠ ");
        }
        label.push_str(&self.name);
        label.push_str("  (");
        label.push_str(&join_ports(&self.ports));
        label.push(')');
        match &self.status {
            ProxyStatus::Starting => label.push_str(" — starting…"),
            ProxyStatus::Failed(_) => label.push_str(" — failed"),
            ProxyStatus::Running | ProxyStatus::Stopped => {}
        }
        label
    }

    /// Checked while `Running` or `Starting`: the check mark reflects "you
    /// asked for this to be on", so it must appear the moment a start is
    /// underway rather than a second later when the ready line lands.
    fn checked(&self) -> bool {
        matches!(self.status, ProxyStatus::Running | ProxyStatus::Starting)
    }
}

fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// The whole menu's rendering inputs, read under the locks in one pass.
struct Snapshot {
    rows: Vec<ProfileRow>,
}

impl Snapshot {
    /// Whether the menu bar icon should show its connected state.
    ///
    /// `Starting` counts: the proxy is up and the port is bound, and a filled
    /// icon that appears the moment you click is better feedback than one that
    /// waits a second for the ready line. `Failed` deliberately does not —
    /// nothing is listening, so showing "connected" would be a lie.
    fn any_active(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.status, ProxyStatus::Running | ProxyStatus::Starting))
    }

    /// The disabled line at the top of the menu: what is running, and where.
    ///
    /// It names ports rather than just profiles for the same reason the
    /// profile rows do — "dev — running" alone does not tell you what to point
    /// a client at.
    fn status_line(&self) -> String {
        let active: Vec<&ProfileRow> = self
            .rows
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    ProxyStatus::Running | ProxyStatus::Starting | ProxyStatus::Failed(_)
                )
            })
            .collect();

        if active.is_empty() {
            return "Nothing running".to_string();
        }

        active
            .iter()
            .map(|r| {
                let state = match &r.status {
                    ProxyStatus::Running => "running",
                    ProxyStatus::Starting => "starting…",
                    ProxyStatus::Failed(_) => "failed",
                    ProxyStatus::Stopped => "stopped",
                };
                format!("{} — {} on {}", r.name, state, join_ports(&r.ports))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Read the profiles and their live statuses, cloning everything out so both
/// guards drop before the caller touches a menu item.
///
/// Lock order: `config`, then `manager` while holding `config` — the
/// `list_profiles` shape, and one of the three permitted shapes documented on
/// [`crate::app_state::Shared`].
async fn snapshot(state: &SharedState) -> Snapshot {
    let config = state.config.lock().await;
    let manager = state.manager.lock().await;

    let mut rows = Vec::with_capacity(config.profiles.len());
    for profile in &config.profiles {
        rows.push(ProfileRow {
            id: profile.id.clone(),
            name: profile.name.clone(),
            ports: profile.ports(),
            danger: profile.danger,
            status: manager.status_of(&profile.id).await,
        });
    }
    Snapshot { rows }
}

/// The parts of one built menu the poll loop keeps mutating afterwards.
///
/// Returned by [`build_menu`] alongside the `Menu` itself so a rebuild can swap
/// in the new handles wholesale rather than reaching back into the menu — Tauri
/// 2.11 exposes no way to look an item back up off a `TrayIcon`.
struct MenuHandles<R: Runtime> {
    status_item: MenuItem<R>,
    profile_items: Vec<CheckMenuItem<R>>,
}

/// Build the whole tray menu for one snapshot of the profile set.
///
/// Both [`build`] and the poll loop's rebuild path go through here, so the
/// launch menu and every rebuilt menu are the same construction: same item
/// order, same ids — in particular the `{PROFILE_PREFIX}{id}` scheme the click
/// handler dispatches on — and no chance of the two drifting apart.
///
/// `autostart` is passed in rather than constructed here, and that is the whole
/// trick to surviving a rebuild. A Tauri `CheckMenuItem<R>` is an `Arc` around
/// one muda item, and muda keys its native `NSMenuItem`s by parent menu id, so
/// the *same* item can sit in a fresh menu and `set_checked` still writes
/// through to it. Carrying the item across a rebuild therefore preserves both
/// halves of the problem at once: the checkbox keeps whatever state it last
/// had (no launchctl re-read, no silent reset to `false` if the query fails),
/// and the clone handed to `on_menu_event` at launch stays the same underlying
/// item, so the click handler goes on toggling the box the user can see.
/// Constructing a fresh `CheckMenuItem` per rebuild would break both.
fn build_menu<R: Runtime>(
    app: &AppHandle<R>,
    snapshot: &Snapshot,
    autostart: &CheckMenuItem<R>,
) -> tauri::Result<(tauri::menu::Menu<R>, MenuHandles<R>)> {
    let status_item =
        MenuItem::with_id(app, ID_STATUS, snapshot.status_line(), false, None::<&str>)?;

    let profile_items: Vec<CheckMenuItem<R>> = snapshot
        .rows
        .iter()
        .map(|row| {
            CheckMenuItem::with_id(
                app,
                format!("{PROFILE_PREFIX}{}", row.id),
                row.label(),
                true,
                row.checked(),
                None::<&str>,
            )
        })
        .collect::<tauri::Result<_>>()?;

    let open_profiles = MenuItem::with_id(app, ID_PROFILES, "Profiles…", true, None::<&str>)?;
    let open_logs = MenuItem::with_id(app, ID_LOGS, "Logs…", true, None::<&str>)?;
    let refresh = MenuItem::with_id(
        app,
        ID_REFRESH,
        "Refresh connection names",
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit", true, Some("Cmd+Q"))?;

    let mut builder = MenuBuilder::new(app).item(&status_item).separator();
    for item in &profile_items {
        builder = builder.item(item);
    }
    let menu = builder
        .separator()
        .item(&open_profiles)
        .item(&open_logs)
        .item(&refresh)
        .item(autostart)
        .item(&PredefinedMenuItem::separator(app)?)
        .item(&quit)
        .build()?;

    Ok((
        menu,
        MenuHandles {
            status_item,
            profile_items,
        },
    ))
}

/// The menu bar icons, embedded rather than read from disk: a dev run and an
/// installed bundle resolve resource paths differently, and an icon that
/// silently fails to load leaves an invisible tray.
///
/// Both are template images (pure black on transparent) at 2x for the 22pt
/// slot. macOS handles light/dark inversion and the menu-open highlight.
const TRAY_ICON_IDLE: &[u8] = include_bytes!("../icons/trayTemplate@2x.png");
const TRAY_ICON_ACTIVE: &[u8] = include_bytes!("../icons/trayActiveTemplate@2x.png");

/// Decode the icon for the given connected state.
///
/// Returns `None` only if the embedded PNG fails to decode, which would mean a
/// corrupt build; callers fall back to a text title so the tray stays clickable.
fn tray_icon(active: bool) -> Option<tauri::image::Image<'static>> {
    let bytes = if active {
        TRAY_ICON_ACTIVE
    } else {
        TRAY_ICON_IDLE
    };
    tauri::image::Image::from_bytes(bytes).ok()
}

/// The identity of the profile set a menu was built from: the ids, in order.
///
/// Order is part of the identity, not just the set. `snapshot.rows` follows
/// config order and the items are paired with it positionally, so a reorder
/// with no additions or deletions would leave every label on the wrong item —
/// which is worse than a missing row, because it is not visibly wrong.
fn profile_ids(snapshot: &Snapshot) -> Vec<String> {
    snapshot.rows.iter().map(|row| row.id.clone()).collect()
}

/// Build the tray icon, its menu, and the poll loop that keeps the menu in
/// sync with live status.
///
/// The profile rows track the profile set as it changes: the poll loop rebuilds
/// the menu when — and only when — the ids it was built from stop matching the
/// snapshot. A status change, which happens on every start, is still an
/// in-place `set_text`/`set_checked` on the existing items, so the common case
/// does not flicker; a profile added or deleted in the Profiles window is rare
/// and deliberate, and pays for a rebuild. Rebuilding unconditionally once a
/// second would be the flickering trade; rebuilding on change is not.
pub fn build<R: Runtime>(app: &tauri::App<R>, state: &SharedState) -> tauri::Result<()> {
    let handle = app.handle().clone();

    // Read the initial state synchronously: the menu has to exist before the
    // tray icon is built, and `setup` is not async.
    //
    // `block_on` on the main thread is only safe because this runs during
    // `setup`, before the poll loop is spawned and before any menu click can
    // arrive — so nothing else can be holding either lock. Do not move this
    // call later in the app's life.
    let initial = tauri::async_runtime::block_on(snapshot(state));
    let initial_ids = profile_ids(&initial);

    // Read once here and never again except on click, as before — see the note
    // in `spawn_poll_loop` on why `Launch at Login` is not polled. From here on
    // this item outlives every menu it is placed in.
    let autostart = CheckMenuItem::with_id(
        app,
        ID_AUTOSTART,
        "Launch at Login",
        true,
        autostart_enabled(&handle),
        None::<&str>,
    )?;

    let (menu, handles) = build_menu(&handle, &initial, &autostart)?;

    let click_state = state.clone();
    let click_autostart = autostart.clone();
    let mut tray = TrayIconBuilder::with_id(TRAY_ID);
    // A tray with no icon is invisible, which would look exactly like the app
    // failing to launch, so fall back to a text title rather than a silent
    // no-op if the embedded image somehow fails to decode.
    match tray_icon(initial.any_active()) {
        Some(icon) => tray = tray.icon(icon),
        None => tray = tray.title("SQL"),
    }
    tray
        // These are template images: pure black on transparent. macOS inverts
        // them for light and dark menu bars and for the highlighted state
        // while the menu is open, which a colour icon would not get.
        .icon_as_template(true)
        .tooltip("Cloud SQL Proxy")
        .menu(&menu)
        // Without this, left-clicking a macOS tray icon does nothing visible
        // and the menu is right-click only — which nobody discovers.
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| {
            let app = app.clone();
            let state = click_state.clone();
            let autostart_item = click_autostart.clone();
            let id = event.id().0.clone();
            // Handlers run on the main thread. Anything that awaits a lock,
            // spawns a process, or shows a modal has to leave it, or the menu
            // stays stuck open and the app looks hung.
            tauri::async_runtime::spawn(async move {
                handle_menu_event(app, state, id, autostart_item).await;
            });
        })
        .build(app)?;

    spawn_poll_loop(
        handle,
        state.clone(),
        autostart,
        handles,
        initial_ids,
        initial.any_active(),
    );

    Ok(())
}

/// Keep the status line and the profile rows in step with live status, and the
/// profile rows themselves in step with the profile set.
///
/// Two paths, and which one runs is decided by [`profile_ids`]:
///
/// - The set is unchanged (essentially always): write only the items whose text
///   or checked state actually changed. `set_text` on macOS goes through to
///   `NSMenuItem`, and rewriting unchanged labels every second is churn the OS
///   does not need.
/// - The set changed (a profile added, deleted, or reordered in the Profiles
///   window): rebuild the menu, hand it to the tray, and adopt the new handles.
///   Without this an added profile would never render and a deleted one would
///   leave a row that clicks through to `no profile with id '…'`.
fn spawn_poll_loop<R: Runtime>(
    app: AppHandle<R>,
    state: SharedState,
    autostart: CheckMenuItem<R>,
    handles: MenuHandles<R>,
    initial_ids: Vec<String>,
    initial_active: bool,
) {
    tauri::async_runtime::spawn(async move {
        let mut handles = handles;
        let mut known_ids = initial_ids;
        let mut last_status = String::new();
        // Seeded from the state the tray was built with, so the first tick
        // does not redundantly rewrite an already-correct icon.
        let mut last_active: Option<bool> = Some(initial_active);
        let mut last_labels: Vec<String> = vec![String::new(); handles.profile_items.len()];
        let mut last_checked: Vec<Option<bool>> = vec![None; handles.profile_items.len()];

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            // Both guards are dropped inside `snapshot`, before any of the
            // menu writes below — see the module docs on locking. The rebuild
            // below is a menu write like any other and is held to the same
            // rule: no lock is live across it.
            let snapshot = snapshot(&state).await;

            let ids = profile_ids(&snapshot);
            if ids != known_ids {
                // `set_menu` replaces the tray's menu atomically as far as the
                // OS is concerned, and a click already in flight is dispatched
                // by id against the *live* config rather than against this
                // menu, so a stale click resolves correctly or reports a
                // missing profile — the same answer the webview would give.
                //
                // A `None` tray means the icon is gone and there is nothing to
                // render into; a failed build or `set_menu` means the old menu
                // is still installed. Either way the loop keeps its existing
                // handles and tries again next tick rather than exiting and
                // freezing the menu for good.
                let rebuilt = app.tray_by_id(TRAY_ID).and_then(|tray| {
                    let (menu, new_handles) = build_menu(&app, &snapshot, &autostart).ok()?;
                    tray.set_menu(Some(menu)).ok()?;
                    Some(new_handles)
                });

                if let Some(new_handles) = rebuilt {
                    last_status = snapshot.status_line();
                    last_labels = snapshot.rows.iter().map(|row| row.label()).collect();
                    last_checked = snapshot.rows.iter().map(|row| Some(row.checked())).collect();
                    handles = new_handles;
                    known_ids = ids;

                    // A set change can also change whether anything is
                    // connected — deleting the only running profile is exactly
                    // that — and the `continue` below skips the icon update
                    // further down, so it has to happen here or the icon stays
                    // stale until some later status change happens to move it.
                    let active = snapshot.any_active();
                    if last_active != Some(active) {
                        if let (Some(tray), Some(icon)) =
                            (app.tray_by_id(TRAY_ID), tray_icon(active))
                        {
                            if tray.set_icon(Some(icon)).is_ok() {
                                last_active = Some(active);
                            }
                        }
                    }

                    // The freshly built items already carry this snapshot's
                    // text and checked state, so there is nothing left to
                    // write this tick.
                    continue;
                }
                // Fall through on failure: the old menu is still installed and
                // still matches `known_ids`, so the in-place updates below are
                // the right thing to do with it.
            }

            let status_line = snapshot.status_line();
            if status_line != last_status {
                let _ = handles.status_item.set_text(&status_line);
                last_status = status_line;
            }

            // The icon is the only signal visible without opening the menu, so
            // it tracks whether anything is connected. Only written on change:
            // set_icon goes through to the OS, and rewriting the same image
            // every second is churn for no benefit.
            let active = snapshot.any_active();
            if last_active != Some(active) {
                if let (Some(tray), Some(icon)) = (app.tray_by_id(TRAY_ID), tray_icon(active)) {
                    if tray.set_icon(Some(icon)).is_ok() {
                        last_active = Some(active);
                    }
                }
            }

            // `snapshot.rows` follows config order, the same order the items
            // were built in, and `ids == known_ids` above establishes that the
            // two still line up one-for-one. `zip` stays as the pairing so a
            // failed rebuild degrades to the old truncating behaviour rather
            // than panicking on an index.
            for (index, (item, row)) in handles.profile_items.iter().zip(&snapshot.rows).enumerate()
            {
                let label = row.label();
                if label != last_labels[index] {
                    let _ = item.set_text(&label);
                    last_labels[index] = label;
                }
                let checked = row.checked();
                if last_checked[index] != Some(checked) {
                    let _ = item.set_checked(checked);
                    last_checked[index] = Some(checked);
                }
            }

            // `Launch at Login` is deliberately not polled. It can change from
            // outside the app (System Settings > Login Items), but reading it
            // means a launchctl query, and doing that every second for a
            // checkbox nobody is looking at is not worth it. It is read once
            // at launch and re-read on every click. A rebuild does not disturb
            // it: the same item is carried into the new menu — see
            // [`build_menu`].
        }
    });
}

/// Dispatch one menu click.
///
/// `autostart_item` is passed in rather than looked up: `TrayIcon` exposes no
/// getter for its menu in Tauri 2.11, and the handle is cheap to clone.
async fn handle_menu_event<R: Runtime>(
    app: AppHandle<R>,
    state: SharedState,
    id: String,
    autostart_item: CheckMenuItem<R>,
) {
    match id.as_str() {
        ID_QUIT => {
            // `RunEvent::Exit` in `main` stops every child, so this does not
            // leak a proxy holding 15432.
            app.exit(0);
        }
        ID_PROFILES => open_window(
            &app,
            WINDOW_PROFILES,
            "Profiles",
            "index.html",
            760.0,
            560.0,
        ),
        ID_LOGS => open_window(&app, WINDOW_LOGS, "Logs", "index.html#logs", 900.0, 600.0),
        ID_REFRESH => refresh_connection_names(&app).await,
        ID_AUTOSTART => toggle_autostart(&app, &autostart_item),
        ID_STATUS => {}
        other => {
            if let Some(profile_id) = other.strip_prefix(PROFILE_PREFIX) {
                toggle_profile(&app, &state, profile_id).await;
            }
        }
    }
}

/// Start or stop a profile, mirroring what the webview does through IPC.
///
/// Which way round it goes is decided from the live status rather than from
/// the check mark: the menu is at most a second stale, and acting on a stale
/// check mark could stop something the user just started.
async fn toggle_profile<R: Runtime>(app: &AppHandle<R>, state: &SharedState, profile_id: &str) {
    let running = {
        let mut manager = state.manager.lock().await;
        manager.is_running(profile_id)
    };

    if running {
        if let Err(message) = commands::stop_profile(app.state(), profile_id.to_string()).await {
            report_error(app, &format!("Could not stop {profile_id}"), &message);
        }
        return;
    }

    // Danger profiles get their own confirmation on their own merits: prd
    // should never be one unguarded click from live traffic. `plan_for` is the
    // same question the webview asks, so both agree on what is dangerous.
    let plan = match commands::plan_for(app.state(), profile_id.to_string()).await {
        Ok(plan) => plan,
        Err(message) => {
            report_error(app, &format!("Could not start {profile_id}"), &message);
            return;
        }
    };

    if plan.requires_confirmation && !confirm_danger(app, state, profile_id, &plan.stop_first).await
    {
        return;
    }

    if let Err(message) = commands::start_profile(app.state(), profile_id.to_string()).await {
        // The seeded profiles have empty connection names, so preflight
        // blocks here with "Refresh connection names" until a refresh has
        // run. Surfacing it as a modal is the point: a click that silently
        // did nothing would read as a broken app.
        report_error(app, &format!("Could not start {profile_id}"), &message);
    }
}

/// Show a two-button modal and await the answer. True means the affirmative
/// button.
///
/// Deliberately built on the callback `show` rather than `blocking_show`: the
/// dialog is displayed on the main thread either way, but this parks no thread
/// at all while the user thinks about it, where `blocking_show` would hold one
/// for as long as the modal is up.
async fn confirm<R: Runtime>(
    app: &AppHandle<R>,
    title: String,
    message: String,
    affirmative: &str,
    kind: MessageDialogKind,
) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();

    app.dialog()
        .message(message)
        .title(title)
        .kind(kind)
        .buttons(MessageDialogButtons::OkCancelCustom(
            affirmative.to_string(),
            "Cancel".to_string(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    // A dropped sender means the dialog went away without answering; treating
    // that as "no" is the safe default when the question is "start
    // production?".
    rx.await.unwrap_or(false)
}

/// Ask before starting a `danger` profile. Returns true to proceed.
async fn confirm_danger<R: Runtime>(
    app: &AppHandle<R>,
    state: &SharedState,
    profile_id: &str,
    stop_first: &[String],
) -> bool {
    let (name, ports) = {
        let config = state.config.lock().await;
        match config.profiles.iter().find(|p| p.id == profile_id) {
            Some(profile) => (profile.name.clone(), join_ports(&profile.ports())),
            // Gone between the plan and here. Nothing to confirm.
            None => return false,
        }
    };

    let mut message = format!(
        "This starts a proxy to PRODUCTION ({name}) on {ports}.\n\n\
         Anything you connect to those ports will be talking to live data."
    );
    if !stop_first.is_empty() {
        message.push_str(&format!("\n\nStopping first: {}.", stop_first.join(", ")));
    }

    confirm(
        app,
        format!("Start {name}?"),
        message,
        "Start production",
        MessageDialogKind::Warning,
    )
    .await
}

/// Run a gcloud refresh and apply the proposed changes after confirmation.
///
/// The two-step (propose, then confirm, then write) is the core's design —
/// `refresh_connection_names` writes nothing — so the tray honours it rather
/// than auto-applying whatever gcloud said.
async fn refresh_connection_names<R: Runtime>(app: &AppHandle<R>) {
    let result = match commands::refresh_connection_names(app.state()).await {
        Ok(result) => result,
        Err(message) => {
            report_error(app, "Could not refresh connection names", &message);
            return;
        }
    };

    if result.changes.is_empty() {
        report_info(
            app,
            "Connection names",
            "Every profile already matches what gcloud reports. Nothing to change.",
        );
        return;
    }

    let summary = result
        .changes
        .iter()
        .map(|c| {
            let from = if c.from.is_empty() {
                "(empty)"
            } else {
                &c.from
            };
            format!("{} {}: {} → {}", c.profile_id, c.role, from, c.to)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let approved = confirm(
        app,
        "Refresh connection names".to_string(),
        format!("Apply these connection names?\n\n{summary}"),
        "Apply",
        MessageDialogKind::Info,
    )
    .await;

    if !approved {
        return;
    }

    match commands::apply_changes(app.state(), result.changes).await {
        Ok(()) => report_info(app, "Connection names", "Saved."),
        Err(message) => report_error(app, "Could not save connection names", &message),
    }
}

/// Remembered geometry for one window label.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct WindowGeometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Where the remembered geometry lives: a small sidecar next to the profile
/// config.
///
/// Deliberately *not* `tauri-plugin-window-state`. That plugin would do this
/// job, but it pulls in a dependency tree and has to be registered in
/// `main.rs`, and what it buys over ~40 lines here is machinery this app does
/// not need: it tracks maximised/fullscreen/visibility for every window in the
/// app, where this one has two windows that are only ever plain and resizable.
/// A sidecar file keeps the whole feature inside the window code that owns it.
fn window_state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| {
        dir.join("fh-cloud-sql-proxy-gui")
            .join("window-state.json")
    })
}

/// Read every remembered window geometry. A missing or corrupt file is not an
/// error worth reporting -- the windows simply open at their default size,
/// which is exactly what happens on first run.
fn load_window_state() -> HashMap<String, WindowGeometry> {
    window_state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Persist one window's geometry, merging into whatever is already stored so
/// the two windows do not overwrite each other.
///
/// Errors are swallowed on purpose: failing to remember a window position is
/// not worth a modal, and the next launch just uses the default.
fn save_window_geometry(label: &str, geometry: WindowGeometry) {
    let Some(path) = window_state_path() else {
        return;
    };
    let mut all = load_window_state();
    all.insert(label.to_string(), geometry);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&all) {
        let _ = std::fs::write(path, text);
    }
}

/// Whether a remembered position still lands on a screen that exists.
///
/// Restoring blind is how a window ends up entirely off-canvas after someone
/// unplugs the external display it was last on -- a window you cannot reach is
/// worse than one in the wrong place. Requiring the saved origin to fall
/// inside some current monitor is the cheap version of the check AppKit does.
fn position_is_visible<R: Runtime>(app: &AppHandle<R>, geometry: &WindowGeometry) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };
    monitors.iter().any(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        let scale = monitor.scale_factor();
        // Monitor geometry is physical; the saved geometry is logical.
        let left = position.x as f64 / scale;
        let top = position.y as f64 / scale;
        let right = left + size.width as f64 / scale;
        let bottom = top + size.height as f64 / scale;
        // The title bar is what the user grabs, so it is the part that has to
        // be on screen. Requiring the whole window would refuse to restore a
        // window the user had deliberately hanging off an edge.
        geometry.x >= left - 8.0
            && geometry.x < right - 40.0
            && geometry.y >= top - 8.0
            && geometry.y < bottom - 40.0
    })
}

/// Show a webview window, creating it on first use.
///
/// A window is created lazily rather than at launch so the app costs nothing
/// until asked, and reused rather than duplicated afterwards: `build` with an
/// existing label fails, and two "Profiles" windows editing the same config
/// would be a way to lose edits.
fn open_window<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
    title: &str,
    url: &str,
    width: f64,
    height: f64,
) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }

    let remembered = load_window_state().get(label).copied();

    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(url.into()))
        .title(title)
        // A unified title bar, as native settings windows have: the page draws
        // under the traffic lights instead of below a separate bar. The page
        // pays for this with a header strip that supplies the clearance and
        // the drag region -- see `.titlebar` in styles.css.
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .resizable(true)
        // Below this the footer's three buttons stop fitting on one line and
        // the label/value rows collapse. A native window declares its minimum
        // rather than letting the user drag it into an unusable shape.
        .min_inner_size(420.0, 320.0);

    // Restore size and position together: a remembered size at a default
    // position would put the window somewhere the user never left it.
    builder = match remembered {
        Some(geometry) if position_is_visible(app, &geometry) => builder
            .inner_size(geometry.width, geometry.height)
            .position(geometry.x, geometry.y),
        // No memory, or the screen it was on is gone: default size, and let
        // the OS place it.
        _ => builder.inner_size(width, height).center(),
    };

    match builder.build() {
        Ok(window) => {
            // With `ActivationPolicy::Accessory` the app is not the active
            // application, so a new window can open behind whatever the user
            // was in. Focusing it explicitly is what makes the click feel
            // like it did something.
            let _ = window.set_focus();
            remember_geometry_on_close(&window, label.to_string());
        }
        Err(error) => report_error(app, &format!("Could not open {title}"), &error.to_string()),
    }
}

/// Keep the window's geometry, and turn closing it into hiding it.
///
/// Two jobs, both hanging off the same event stream.
///
/// Geometry: saving on close alone is not enough, because on macOS a window
/// that is going away does not reliably report its final frame. Tracking the
/// last-seen good geometry as it changes and writing that is what survives.
///
/// Closing: the app lives in the menu bar, so ⌘W and the red traffic light
/// mean "put this away", not "quit". The close is prevented and the window
/// hidden instead, which also keeps the webview alive so reopening is instant
/// and does not discard scroll position or a half-typed field.
fn remember_geometry_on_close<R: Runtime>(window: &tauri::WebviewWindow<R>, label: String) {
    let tracked: Arc<Mutex<Option<WindowGeometry>>> = Arc::new(Mutex::new(None));

    let handle = window.clone();
    let seen = tracked.clone();
    window.on_window_event(move |event| match event {
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
            // A minimised or zoomed window reports a frame that is not the one
            // to restore, so only plain states are recorded.
            if handle.is_minimized().unwrap_or(false) {
                return;
            }
            if let (Ok(position), Ok(size), Ok(scale)) = (
                handle.outer_position(),
                handle.inner_size(),
                handle.scale_factor(),
            ) {
                if size.width == 0 || size.height == 0 {
                    return;
                }
                if let Ok(mut slot) = seen.lock() {
                    *slot = Some(WindowGeometry {
                        x: position.x as f64 / scale,
                        y: position.y as f64 / scale,
                        width: size.width as f64 / scale,
                        height: size.height as f64 / scale,
                    });
                }
            }
        }
        tauri::WindowEvent::CloseRequested { api, .. } => {
            let geometry = seen.lock().ok().and_then(|slot| *slot);
            if let Some(geometry) = geometry {
                save_window_geometry(&label, geometry);
            }

            // Hide rather than destroy. The app lives in the menu bar, so
            // closing a window means "put it away", not "quit" — the same
            // thing the red button does in Tailscale or System Settings while
            // the app keeps running. Destroying it would also throw away the
            // webview, so the next `Profiles…` would pay a rebuild and lose
            // scroll position and any half-typed field.
            //
            // `open_window` already prefers `show()` on an existing window, so
            // a hidden window is what makes reopening instant.
            api.prevent_close();
            let _ = handle.hide();
        }
        tauri::WindowEvent::Destroyed => {
            let geometry = seen.lock().ok().and_then(|slot| *slot);
            if let Some(geometry) = geometry {
                save_window_geometry(&label, geometry);
            }
        }
        _ => {}
    });
}

/// Whether the app is registered to launch at login. A failed query reads as
/// "not enabled": showing the box unchecked when we cannot tell is the
/// honest default, and the click path re-reads before flipping.
fn autostart_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Flip the launch-at-login registration.
///
/// This registers **the app**, not any proxy: nothing starts a profile on
/// login, so a machine that reboots does not come up holding 15432.
///
/// The checkbox is not set optimistically. macOS flips it on click by itself,
/// so it has to be forced back to whatever the OS actually reports afterwards
/// — otherwise a failed `enable` leaves a ticked box claiming a registration
/// that does not exist.
fn toggle_autostart<R: Runtime>(app: &AppHandle<R>, item: &CheckMenuItem<R>) {
    use tauri_plugin_autostart::ManagerExt;

    let autolaunch = app.autolaunch();
    let currently = autolaunch.is_enabled().unwrap_or(false);

    let result = if currently {
        autolaunch.disable()
    } else {
        autolaunch.enable()
    };

    let _ = item.set_checked(autolaunch.is_enabled().unwrap_or(currently));

    if let Err(error) = result {
        report_error(app, "Could not change Launch at Login", &error.to_string());
    }
}

/// A modal carrying an operator-facing failure. Errors from the command layer
/// are already user-facing text (the core renders them via `thiserror`), so
/// they are shown verbatim rather than paraphrased.
fn report_error<R: Runtime>(app: &AppHandle<R>, title: &str, message: &str) {
    show_message(app, title, message, MessageDialogKind::Error);
}

fn report_info<R: Runtime>(app: &AppHandle<R>, title: &str, message: &str) {
    show_message(app, title, message, MessageDialogKind::Info);
}

fn show_message<R: Runtime>(
    app: &AppHandle<R>,
    title: &str,
    message: &str,
    kind: MessageDialogKind,
) {
    // Non-blocking: this is called from async contexts that have nothing left
    // to do but report, and blocking one of them on a click of "OK" would
    // hold a runtime thread for as long as the user ignores the dialog.
    app.dialog()
        .message(message.to_string())
        .title(title.to_string())
        .kind(kind)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

/// Report a config load failure that happened before the tray existed.
///
/// `main` discovers this at startup with nowhere to render it; the tray is the
/// first thing with a UI, so it takes the message.
pub fn report_startup_error<R: Runtime>(app: &AppHandle<R>, message: &str) {
    report_error(app, "Could not load your profiles", message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use fh_cloud_sql_proxy_gui::core::log_watcher::{Diagnosis, FailureKind};

    fn row(name: &str, danger: bool, status: ProxyStatus) -> ProfileRow {
        ProfileRow {
            id: name.to_string(),
            name: name.to_string(),
            ports: vec![15432, 15433],
            danger,
            status,
        }
    }

    fn diagnosis() -> Diagnosis {
        Diagnosis {
            kind: FailureKind::PortInUse,
            message: "port 15432 is already in use".to_string(),
            fix_command: None,
        }
    }

    fn snapshot_of(names: &[&str]) -> Snapshot {
        Snapshot {
            rows: names
                .iter()
                .map(|n| row(n, false, ProxyStatus::Stopped))
                .collect(),
        }
    }

    #[test]
    fn icon_is_idle_when_nothing_is_connected() {
        assert!(!snapshot_of(&["dev", "stg"]).any_active());
        assert!(!Snapshot { rows: vec![] }.any_active());
    }

    #[test]
    fn icon_is_active_while_running_or_starting() {
        // Starting counts: the port is bound, and feedback the instant you
        // click beats waiting a second for the ready line.
        for status in [ProxyStatus::Running, ProxyStatus::Starting] {
            let snapshot = Snapshot {
                rows: vec![row("dev", false, status.clone())],
            };
            assert!(snapshot.any_active(), "expected active for {status:?}");
        }
    }

    #[test]
    fn failed_profile_does_not_show_as_connected() {
        // Nothing is listening, so a filled icon would be a lie — the menu
        // reports the failure, the icon must not claim a connection.
        let snapshot = Snapshot {
            rows: vec![row("dev", false, ProxyStatus::Failed(diagnosis()))],
        };
        assert!(!snapshot.any_active());
    }

    #[test]
    fn one_running_profile_makes_the_icon_active() {
        let mut snapshot = snapshot_of(&["dev", "stg", "prd"]);
        snapshot.rows[1].status = ProxyStatus::Running;
        assert!(snapshot.any_active());
    }

    #[test]
    fn both_tray_icons_decode() {
        // A corrupt embedded PNG would leave the tray invisible, which looks
        // exactly like the app failing to launch.
        assert!(tray_icon(false).is_some(), "idle icon failed to decode");
        assert!(tray_icon(true).is_some(), "active icon failed to decode");
    }

    #[test]
    fn identical_profile_sets_need_no_rebuild() {
        let before = profile_ids(&snapshot_of(&["dev", "stg", "prd"]));
        let after = profile_ids(&snapshot_of(&["dev", "stg", "prd"]));
        assert_eq!(before, after);
    }

    #[test]
    fn added_or_deleted_profile_triggers_rebuild() {
        let before = profile_ids(&snapshot_of(&["dev", "stg"]));
        assert_ne!(before, profile_ids(&snapshot_of(&["dev", "stg", "uat"])));
        assert_ne!(before, profile_ids(&snapshot_of(&["dev"])));
    }

    #[test]
    fn reordering_triggers_rebuild() {
        // Items pair with rows positionally, so a reorder with no additions
        // would leave every label on the wrong item — worse than a missing
        // row, because nothing looks visibly wrong.
        assert_ne!(
            profile_ids(&snapshot_of(&["dev", "stg"])),
            profile_ids(&snapshot_of(&["stg", "dev"]))
        );
    }

    #[test]
    fn renaming_display_name_alone_does_not_trigger_rebuild() {
        // Ids are stable across a rename; the in-place set_text already
        // handles the new label, so a rebuild would be pure churn.
        let mut renamed = snapshot_of(&["dev", "stg"]);
        renamed.rows[0].name = "development".to_string();
        assert_eq!(
            profile_ids(&snapshot_of(&["dev", "stg"])),
            profile_ids(&renamed)
        );
    }

    #[test]
    fn status_change_alone_does_not_trigger_rebuild() {
        let mut running = snapshot_of(&["dev"]);
        running.rows[0].status = ProxyStatus::Running;
        assert_eq!(profile_ids(&snapshot_of(&["dev"])), profile_ids(&running));
    }

    #[test]
    fn label_always_shows_ports() {
        // The whole point: dev/stg/prd share 15432/15433, so a bare name
        // would leave the user guessing which environment owns the port.
        assert_eq!(
            row("dev", false, ProxyStatus::Stopped).label(),
            "dev  (15432/15433)"
        );
    }

    #[test]
    fn danger_profile_is_marked() {
        let label = row("prd", true, ProxyStatus::Stopped).label();
        assert!(label.starts_with("⚠ "), "got {label}");
        assert!(label.contains("15432/15433"));
    }

    #[test]
    fn transient_states_are_suffixed() {
        assert!(row("dev", false, ProxyStatus::Starting)
            .label()
            .ends_with("— starting…"));
        assert!(row("dev", false, ProxyStatus::Failed(diagnosis()))
            .label()
            .ends_with("— failed"));
    }

    #[test]
    fn running_and_stopped_labels_carry_no_suffix() {
        assert_eq!(
            row("dev", false, ProxyStatus::Running).label(),
            "dev  (15432/15433)"
        );
        assert_eq!(
            row("dev", false, ProxyStatus::Stopped).label(),
            "dev  (15432/15433)"
        );
    }

    #[test]
    fn checked_covers_starting_as_well_as_running() {
        // Checking only on Running would leave the box empty for the second
        // between the click and the ready line, reading as "the click did
        // nothing".
        assert!(row("dev", false, ProxyStatus::Running).checked());
        assert!(row("dev", false, ProxyStatus::Starting).checked());
        assert!(!row("dev", false, ProxyStatus::Stopped).checked());
        assert!(!row("dev", false, ProxyStatus::Failed(diagnosis())).checked());
    }

    #[test]
    fn status_line_says_nothing_running_when_idle() {
        let snapshot = Snapshot {
            rows: vec![
                row("dev", false, ProxyStatus::Stopped),
                row("prd", true, ProxyStatus::Stopped),
            ],
        };
        assert_eq!(snapshot.status_line(), "Nothing running");
    }

    #[test]
    fn status_line_names_the_running_profile_and_its_ports() {
        let snapshot = Snapshot {
            rows: vec![
                row("dev", false, ProxyStatus::Running),
                row("prd", true, ProxyStatus::Stopped),
            ],
        };
        assert_eq!(snapshot.status_line(), "dev — running on 15432/15433");
    }

    #[test]
    fn status_line_includes_starting_and_failed() {
        let snapshot = Snapshot {
            rows: vec![
                row("dev", false, ProxyStatus::Starting),
                row("stg", false, ProxyStatus::Failed(diagnosis())),
            ],
        };
        assert_eq!(
            snapshot.status_line(),
            "dev — starting… on 15432/15433, stg — failed on 15432/15433"
        );
    }

    #[test]
    fn single_port_profile_renders_without_a_slash() {
        let mut single = row("dev", false, ProxyStatus::Stopped);
        single.ports = vec![15432];
        assert_eq!(single.label(), "dev  (15432)");
    }

    #[test]
    fn profile_menu_ids_are_prefixed_so_they_cannot_collide_with_fixed_ids() {
        // A profile whose id happened to be "quit" or "logs" must not hijack
        // those menu items.
        for reserved in [
            ID_QUIT,
            ID_LOGS,
            ID_PROFILES,
            ID_REFRESH,
            ID_AUTOSTART,
            ID_STATUS,
        ] {
            assert_ne!(format!("{PROFILE_PREFIX}{reserved}"), reserved);
        }
    }

    #[test]
    fn window_labels_match_the_acl() {
        // `capabilities/default.json` grants plugin permissions to exactly
        // these two labels; a typo here means a window with no plugin access.
        assert_eq!(WINDOW_PROFILES, "profiles");
        assert_eq!(WINDOW_LOGS, "logs");
    }
}
