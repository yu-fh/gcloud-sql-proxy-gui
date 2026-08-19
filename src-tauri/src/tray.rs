//! The menu bar tray: the app's entire primary UI, modelled on Tailscale.
//!
//! A native `NSMenu` hanging off the tray icon lists every profile with its
//! ports and live status; clicking one toggles it. Richer interactions
//! (`Profiles…`, `Logs…`) open a real webview window rather than cramming an
//! editor into a popover — one window with a sidebar, two sections, as a macOS
//! settings window is.
//!
//! This module is the menu and nothing else: it builds the `NSMenu`, polls it
//! against live status, and dispatches clicks. The window it opens lives in
//! [`crate::window`] and the alerts it raises in [`crate::dialogs`], because
//! neither is about menus and both are needed by more than one caller.
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
//! # Four glyphs, by aggregate state
//!
//! The menu bar glyph is the only part of this UI visible without opening the
//! menu, so it carries the aggregate state: `disconnected`, `connecting`,
//! `connected`, `error`. The status line and the per-profile suffixes still
//! carry the detail — which profile, which port, which failure — because a
//! single glyph cannot say "dev is up, prd failed".
//!
//! One glyph has to stand for many profiles, so the four states are a strict
//! precedence rather than a tally; see [`Snapshot::icon_state`] for the order
//! and why failure outranks success.
//!
//! These are designer-supplied colour assets, *not* template images: a
//! translucent database glyph with a status dot whose colour is the signal.
//! `icon_as_template(false)` is therefore load-bearing — as a template image
//! macOS would flatten the whole thing to a black silhouette and throw away
//! both the translucency and the red error dot, which is the entire point of
//! the set.
//!
//! # Two ink sets, by system appearance
//!
//! Opting out of template rendering means opting out of the inversion macOS
//! does for template images. The artwork is white, so on a light menu bar it is
//! very nearly invisible — the red error dot is the only part that survives.
//! The fix is a second set: the same geometry recoloured to dark ink, selected
//! at runtime. Icon selection therefore has two independent dimensions —
//! [`IconState`] (which glyph) and [`Appearance`] (which ink) — and eight
//! embedded assets. Both sets come from `design/generate-icons.py`; the red
//! error dot is exempt from the recolour and is byte-identical in both, because
//! red reads on either background and is the one state whose colour carries
//! meaning.
//!
//! The appearance is read from `NSApp.effectiveAppearance` rather than from
//! Tauri, and it is *polled* rather than observed. Both are forced: Tauri 2.11
//! exposes appearance only on a `Window` (`theme()`, and `ThemeChanged` inside
//! `RunEvent::WindowEvent`), and this app is `LSUIElement` with no window at
//! startup, so there is nothing to ask and no event to subscribe to. AppKit's
//! own signal, `effectiveAppearanceDidChange`, is KVO — observing it from here
//! would mean registering a custom `NSObject` subclass purely to forward a
//! notification the existing once-a-second loop can just as well notice. See
//! [`appearance`] for the read and [`update_icon`] for the change guard, which
//! is over the (state, appearance) pair so a flip repaints once and records
//! once rather than once per tick.
//!
//! `effectiveAppearance` is main-thread-only and the poll loop is not on the
//! main thread, so the read is marshalled — see the threading note on
//! [`appearance`]. Getting that wrong does not fail loudly: the off-thread read
//! reports `Light` unconditionally, which on a dark menu bar installs the
//! near-invisible dark-ink asset from the second tick onward, while startup
//! looks perfectly correct.
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
//! — the settings window writes the config through the command layer, and the
//! core has no way to announce it. Each tick compares the ids the current menu
//! was built from against the snapshot's, and rebuilds the menu only when they
//! differ. Rebuilding unconditionally would flicker the menu once a second for
//! the sake of something that happens when a user adds a profile; rebuilding on
//! change costs nothing in the common case and is the only way an added profile
//! ever appears or a deleted one ever goes away.

use std::time::Duration;

use tauri::menu::{CheckMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::MessageDialogKind;

use fh_cloud_sql_proxy_gui::core::audit::Category;
use fh_cloud_sql_proxy_gui::core::proxy::ProxyStatus;

use crate::app_state::SharedState;
use crate::commands;
use crate::dialogs::{confirm, report_error};
use crate::window::{open_settings, Section};

/// How often the menu re-reads live status. See the module docs.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Menu item ids. Profile toggles get `profile:{id}`, so a profile named
/// `logs` cannot collide with the `Logs…` item.
const ID_STATUS: &str = "status";
const ID_PROFILES: &str = "open-profiles";
const ID_LOGS: &str = "open-logs";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const PROFILE_PREFIX: &str = "profile:";

/// The tray's own id, used to look the menu back up when a click has to
/// correct a checkbox the OS already flipped.
const TRAY_ID: &str = "main";

// The window itself -- its label, its size, and the sidecar that remembers
// where it was -- lives in `crate::window`. The tray only names the section to
// open; see `Section` there.

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
    /// Which of the four menu bar icons the aggregate state calls for.
    ///
    /// One glyph, many profiles, so this is a precedence and not a tally. In
    /// descending order:
    ///
    /// 1. **any `Failed` → `Error`.** A failure the user has not seen yet is
    ///    the only state here that needs them to *do* something, and the menu
    ///    bar is the only place it can be raised unprompted. If dev is running
    ///    and prd failed, reporting "connected" would hide the one fact worth
    ///    surfacing; the menu one click away still shows dev up and prd
    ///    failed, so nothing is lost by ranking the failure first.
    /// 2. **else any `Starting` → `Connecting`.** Carried over from the old
    ///    two-state icon: `Starting` must move the glyph the instant you
    ///    click, not a second later when the ready line lands. It ranks below
    ///    `Failed` because a start in flight is not news.
    /// 3. **else any `Running` → `Connected`.**
    /// 4. **else (all `Stopped`) → `Disconnected`.** Also the empty-profile
    ///    case: nothing configured is nothing connected.
    ///
    /// Note that `Failed` never reads as connected, and `Starting` never reads
    /// as disconnected — the two invariants the two-state icon encoded.
    fn icon_state(&self) -> IconState {
        let mut starting = false;
        let mut running = false;
        for row in &self.rows {
            match row.status {
                // Highest precedence: return as soon as one is found, so no
                // later row can talk us out of reporting the failure.
                ProxyStatus::Failed(_) => return IconState::Error,
                ProxyStatus::Starting => starting = true,
                ProxyStatus::Running => running = true,
                ProxyStatus::Stopped => {}
            }
        }
        if starting {
            IconState::Connecting
        } else if running {
            IconState::Connected
        } else {
            IconState::Disconnected
        }
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
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit", true, Some("Cmd+Q"))?;

    let mut builder = MenuBuilder::new(app).item(&status_item).separator();
    for item in &profile_items {
        builder = builder.item(item);
    }
    let menu = builder
        .separator()
        .item(&open_profiles)
        .item(&open_logs)
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

/// The aggregate state the menu bar glyph shows. One variant per embedded
/// asset; see [`Snapshot::icon_state`] for how a set of profiles maps onto it.
///
/// The designer's set also includes a `paused` state. It is deliberately not
/// represented here and its asset is not in the repo: the app has no pause
/// concept — a profile is `Stopped`, `Starting`, `Running`, or `Failed`, with
/// no way to reach anything a user would call paused — so a `Paused` variant
/// would be unreachable code and an embedded asset would be dead weight. If a
/// pause feature ever lands, `paused-18px.png` is in the designer's delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IconState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl IconState {
    /// For the audit log: the state name as the designer's assets name it.
    fn as_str(self) -> &'static str {
        match self {
            IconState::Disconnected => "disconnected",
            IconState::Connecting => "connecting",
            IconState::Connected => "connected",
            IconState::Error => "error",
        }
    }
}

/// Which menu bar the icon is being drawn on, and therefore which ink it needs.
///
/// This is a second, independent dimension of icon selection: the state says
/// *which* glyph, the appearance says *which ink*. Four states times two
/// appearances is the eight embedded assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Appearance {
    /// A dark menu bar. The designer's artwork as delivered — white ink.
    Dark,
    /// A light menu bar. The derived variant — dark ink.
    Light,
}

impl Appearance {
    /// For the audit log.
    fn as_str(self) -> &'static str {
        match self {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }
}

/// The system appearance right now.
///
/// # Why this reads AppKit directly
///
/// Tauri 2.11 has no app-level appearance API. `theme()` and the
/// `ThemeChanged` event both hang off a `Window`/`WebviewWindow`, and
/// `RunEvent` has no appearance variant at all — the only `ThemeChanged` is
/// nested inside `RunEvent::WindowEvent`. This app is `LSUIElement` with
/// `ActivationPolicy::Accessory` and opens no window until the user asks for
/// one, so at startup, and for most of its life, there is no window to ask.
/// Routing the tray's ink through a hidden window created solely to be
/// interrogated would be a heavier and more fragile thing than reading the
/// value AppKit already holds.
///
/// So this is the same read tao performs for its own `Theme`:
/// `NSApp.effectiveAppearance`, resolved with
/// `bestMatchFromAppearancesWithNames:` against the two system appearance
/// names. Matching that way rather than comparing the raw name is what makes
/// the accessibility appearances work — `NSAppearanceNameAccessibilityHighContrastDarkAqua`
/// is not equal to `NSAppearanceNameDarkAqua` but does best-match it, so a user
/// on increased contrast gets the dark-menu-bar ink rather than silently
/// falling through to the light branch.
///
/// # Threading
///
/// `NSApp.effectiveAppearance` is main-thread-only, and the poll loop is not on
/// the main thread — it is a `tauri::async_runtime` task on a tokio worker. So
/// the read is marshalled with [`AppHandle::run_on_main_thread`] and the result
/// returned over a channel. Reading it directly from the calling thread is not
/// a viable shortcut: `MainThreadMarker::new()` correctly refuses off-thread,
/// and the resulting fallback silently reports `Light` on a dark menu bar,
/// installing the near-invisible wrong-ink asset. That is a worse bug than the
/// one this mechanism fixes, and it does not reproduce at startup — only from
/// the second tick onward — so it survives any check that only looks at launch.
///
/// If the dispatch fails or the main thread does not answer, the previous known
/// appearance is kept (see the `fallback` argument) rather than guessing:
/// leaving a correct icon alone beats flipping it to a default.
#[cfg(target_os = "macos")]
fn appearance<R: Runtime>(app: &AppHandle<R>, fallback: Appearance) -> Appearance {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel();
    if app
        .run_on_main_thread(move || {
            let _ = tx.send(read_appearance());
        })
        .is_err()
    {
        return fallback;
    }
    // Bounded: a hung main thread must not stall the poll loop, which also
    // drives the status line and the profile rows. One second is far longer
    // than a property read and still under the poll interval.
    rx.recv_timeout(Duration::from_secs(1)).unwrap_or(fallback)
}

/// The actual AppKit read. Must be called on the main thread; see
/// [`appearance`], which is what arranges that.
#[cfg(target_os = "macos")]
fn read_appearance() -> Appearance {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{ns_string, MainThreadMarker, NSArray, NSString};

    // `None` off the main thread. Callers get here only via
    // `run_on_main_thread`, so this is a belt-and-braces guard rather than an
    // expected path; `Light` matches the unknown-appearance default.
    let Some(marker) = MainThreadMarker::new() else {
        return Appearance::Light;
    };

    let names = NSArray::from_slice(&[
        ns_string!("NSAppearanceNameAqua"),
        ns_string!("NSAppearanceNameDarkAqua"),
    ]);
    let appearance = NSApplication::sharedApplication(marker).effectiveAppearance();
    let matched: Option<objc2::rc::Retained<NSString>> =
        appearance.bestMatchFromAppearancesWithNames(&names);

    match matched.as_deref().map(NSString::to_string).as_deref() {
        Some("NSAppearanceNameDarkAqua") => Appearance::Dark,
        _ => Appearance::Light,
    }
}

/// Non-macOS builds have no menu bar appearance to track. The tray code is
/// cross-platform enough to compile, and `Dark` keeps the designer's artwork as
/// delivered rather than substituting a derived variant on a platform nobody has
/// looked at the icon on.
#[cfg(not(target_os = "macos"))]
fn appearance<R: Runtime>(_app: &AppHandle<R>, _fallback: Appearance) -> Appearance {
    Appearance::Dark
}

/// The menu bar icons, embedded rather than read from disk: a dev run and an
/// installed bundle resolve resource paths differently, and an icon that
/// silently fails to load leaves an invisible tray.
///
/// These are colour assets, not template images — see the module docs on why
/// `icon_as_template(false)` is load-bearing for this artwork, and why that in
/// turn is what forces two sets rather than one.
///
/// Eight assets: four states times two menu bar appearances. Both sets are
/// generated by `design/generate-icons.py` from the committed sources in
/// `design/tray-source/`; the dark-menu-bar set is the designer's delivery
/// copied verbatim, the light-menu-bar set is its mechanical recolour.
///
/// **These are the 36px `@2x` assets**, which is the designer's README
/// recommendation and — unlike the 18px set that used to be here — the size that
/// actually renders sharply.
///
/// # Pixels are backing store, not points
///
/// An earlier version of this comment claimed the opposite: that the menu bar
/// takes a PNG's declared pixel size at face value, so a 36px asset would fill
/// the ~22pt slot edge to edge and read as a solid block. That was inherited from
/// a different, earlier icon set and never re-checked against this artwork. It is
/// false, and the reason is one hardcoded constant.
///
/// `Image::from_bytes` only carries the RGBA buffer and its dimensions. The path
/// on to the screen is `tauri::image::Image` -> `tray_icon::Icon` ->
/// `NSStatusItem.button.image`, and `tray-icon` 0.24.2's
/// `set_icon_for_ns_status_item_button` does this to every image it is handed,
/// on both the builder path and `set_icon`:
///
/// ```text
/// let icon_height: f64 = 18.0;
/// let icon_width: f64 = (width as f64) / (height as f64 / icon_height);
/// nsimage.setSize(NSSize::new(icon_width, icon_height));
/// ```
///
/// The point size is therefore **always 18pt high**; the PNG's pixel dimensions
/// set only the aspect ratio and, because `NSImage` keeps the pixels it was
/// decoded from as its representation, the backing-store density. Probing the
/// live `NSImage` off `NSStatusBarButton` confirmed it directly:
///
/// | embedded PNG | `NSStatusBar.thickness` | `NSImage.size` | representation |
/// |--------------|-------------------------|----------------|----------------|
/// | 18x18        | 22                      | 18x18 pt       | 18x18 px (1x)  |
/// | 36x36        | 22                      | 18x18 pt       | 36x36 px (2x)  |
/// | 44x44        | 22                      | 18x18 pt       | 44x44 px       |
///
/// Nothing overflows at any size. So the 18px set was not "correctly sized" — it
/// was a 1x asset on a Retina display, which is exactly the softness a user
/// comparing it against a neighbouring menu bar icon reports as "small and
/// blurry".
///
/// # What this does and does not fix
///
/// It fixes the blur: 36px is a pixel-exact 2x for an 18pt image. It does **not**
/// change how much of the slot the glyph occupies, because 18 of the 22pt is
/// `tray-icon`'s hardcoded choice and is not reachable from here. Embedding a
/// larger asset cannot raise it; only patching or replacing that crate's macOS
/// backend, or setting the size on the `NSImage` ourselves afterwards, could. If
/// the glyph still reads as small next to its neighbours, that constant — not
/// this asset size — is the thing to change.
///
/// The 24px and 48px variants are omitted: nothing here can use them, and an
/// unused asset in the tree is an invitation to wire up the wrong one.
///
/// The dark-menu-bar set: the designer's artwork as delivered, white ink.
const TRAY_ICON_DISCONNECTED: &[u8] = include_bytes!("../icons/tray-disconnected.png");
const TRAY_ICON_CONNECTING: &[u8] = include_bytes!("../icons/tray-connecting.png");
const TRAY_ICON_CONNECTED: &[u8] = include_bytes!("../icons/tray-connected.png");
const TRAY_ICON_ERROR: &[u8] = include_bytes!("../icons/tray-error.png");

/// The light-menu-bar set: the same artwork recoloured to dark ink by
/// `design/generate-icons.py`. Same geometry, same red error dot — only the ink
/// and its alpha differ. See that script for how the ink was chosen.
const TRAY_ICON_DISCONNECTED_LIGHT: &[u8] = include_bytes!("../icons/tray-disconnected-light.png");
const TRAY_ICON_CONNECTING_LIGHT: &[u8] = include_bytes!("../icons/tray-connecting-light.png");
const TRAY_ICON_CONNECTED_LIGHT: &[u8] = include_bytes!("../icons/tray-connected-light.png");
const TRAY_ICON_ERROR_LIGHT: &[u8] = include_bytes!("../icons/tray-error-light.png");

/// The embedded bytes for one (state, appearance) pair.
///
/// Split out from [`tray_icon`] so the mapping can be tested without a Tauri
/// runtime: the tests walk all eight pairs and assert both that each decodes to
/// exactly `TRAY_ICON_PX` square and that no two pairs share an asset. A
/// copy-paste slip that pointed two states at one image would otherwise be
/// invisible — the tray still shows *an* icon.
fn tray_icon_bytes(state: IconState, appearance: Appearance) -> &'static [u8] {
    match (state, appearance) {
        (IconState::Disconnected, Appearance::Dark) => TRAY_ICON_DISCONNECTED,
        (IconState::Connecting, Appearance::Dark) => TRAY_ICON_CONNECTING,
        (IconState::Connected, Appearance::Dark) => TRAY_ICON_CONNECTED,
        (IconState::Error, Appearance::Dark) => TRAY_ICON_ERROR,
        (IconState::Disconnected, Appearance::Light) => TRAY_ICON_DISCONNECTED_LIGHT,
        (IconState::Connecting, Appearance::Light) => TRAY_ICON_CONNECTING_LIGHT,
        (IconState::Connected, Appearance::Light) => TRAY_ICON_CONNECTED_LIGHT,
        (IconState::Error, Appearance::Light) => TRAY_ICON_ERROR_LIGHT,
    }
}

/// Decode the icon for an aggregate state on a given menu bar appearance.
///
/// Returns `None` only if the embedded PNG fails to decode, which would mean a
/// corrupt build; callers fall back to a text title so the tray stays clickable.
fn tray_icon(state: IconState, appearance: Appearance) -> Option<tauri::image::Image<'static>> {
    tauri::image::Image::from_bytes(tray_icon_bytes(state, appearance)).ok()
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
/// does not flicker; a profile added or deleted in the settings window is rare
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
    // Read the appearance once here and hand the same value to the poll loop as
    // its seed, so the first tick does not rewrite an already-correct icon.
    //
    // `read_appearance` directly rather than `appearance`: this runs inside
    // `setup`, which is already the main thread *and* is before `app.run` starts
    // the event loop. `run_on_main_thread` only queues onto that loop, so going
    // through it here would wait for a runner that has not started yet and fall
    // back on the timeout.
    #[cfg(target_os = "macos")]
    let initial_appearance = read_appearance();
    #[cfg(not(target_os = "macos"))]
    let initial_appearance = Appearance::Dark;
    match tray_icon(initial.icon_state(), initial_appearance) {
        Some(icon) => tray = tray.icon(icon),
        None => tray = tray.title("SQL"),
    }
    tray
        // NOT a template image. These are colour assets: a translucent glyph
        // with a status dot whose colour is the signal. As a template macOS
        // would flatten every pixel to a black silhouette, discarding both the
        // translucency and the red error dot — so `true` here would silently
        // destroy the thing the four-state set exists to show.
        .icon_as_template(false)
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
        (initial.icon_state(), initial_appearance),
    );

    Ok(())
}

/// Point the tray icon at the snapshot's aggregate state and the current menu
/// bar appearance, if that is not already where it points.
///
/// The icon is the only signal visible without opening the menu, so it tracks
/// the aggregate state — but only on change. `set_icon` goes through to the OS,
/// and rewriting the same image once a second is churn for no benefit; the same
/// reason applies to the audit record, which is why it is written here (on a
/// real transition) rather than per poll tick. `last` only advances when the
/// write succeeds, so a transient failure is retried next tick instead of being
/// recorded as done.
///
/// The guard is over the *pair*, not the state alone. Appearance is the second
/// input to icon selection, and a user flipping Light/Dark in System Settings
/// changes which asset is correct without changing the state — so keying the
/// early return on state only would leave the wrong-ink icon installed until
/// the next unrelated status change, which is the original invisible-icon bug
/// with extra steps. Reading the appearance every tick is a cheap AppKit
/// property read; it is the *write* and the audit record that are gated, so a
/// flip costs exactly one record rather than one per tick.
fn update_icon<R: Runtime>(
    app: &AppHandle<R>,
    state: &SharedState,
    snapshot: &Snapshot,
    last: &mut Option<(IconState, Appearance)>,
) {
    // Keep the current appearance if it cannot be read, rather than letting a
    // failed read masquerade as a flip and repaint the icon wrongly.
    let current_appearance = last.map_or(Appearance::Dark, |(_, was)| was);
    let next = (snapshot.icon_state(), appearance(app, current_appearance));
    if *last == Some(next) {
        return;
    }
    let Some((tray, icon)) = app.tray_by_id(TRAY_ID).zip(tray_icon(next.0, next.1)) else {
        return;
    };
    if tray.set_icon(Some(icon)).is_ok() {
        // Report whichever dimension actually moved. A status change and an
        // appearance flip are different events to anyone reading the log, and
        // collapsing both into one "state/appearance" line would make the
        // common case (a status change) noisier for no gain.
        let message = match *last {
            Some((from_state, from_appearance)) if from_state == next.0 => format!(
                "tray icon appearance: {} -> {} ({})",
                from_appearance.as_str(),
                next.1.as_str(),
                next.0.as_str()
            ),
            Some((from_state, from_appearance)) if from_appearance == next.1 => format!(
                "tray icon: {} -> {} ({} menu bar)",
                from_state.as_str(),
                next.0.as_str(),
                next.1.as_str()
            ),
            Some((from_state, from_appearance)) => format!(
                "tray icon: {} ({}) -> {} ({})",
                from_state.as_str(),
                from_appearance.as_str(),
                next.0.as_str(),
                next.1.as_str()
            ),
            None => format!(
                "tray icon: unknown -> {} ({} menu bar)",
                next.0.as_str(),
                next.1.as_str()
            ),
        };
        state.audit.info(Category::Event, None, message);
        *last = Some(next);
    }
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
///
/// Either way the icon is refreshed through [`update_icon`], including on the
/// rebuild path — which returns early and would otherwise leave it stale.
fn spawn_poll_loop<R: Runtime>(
    app: AppHandle<R>,
    state: SharedState,
    autostart: CheckMenuItem<R>,
    handles: MenuHandles<R>,
    initial_ids: Vec<String>,
    initial_icon: (IconState, Appearance),
) {
    tauri::async_runtime::spawn(async move {
        let mut handles = handles;
        let mut known_ids = initial_ids;
        let mut last_status = String::new();
        // Seeded from the state *and appearance* the tray was built with, so the
        // first tick does not redundantly rewrite an already-correct icon.
        let mut last_icon: Option<(IconState, Appearance)> = Some(initial_icon);
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

                match &rebuilt {
                    Some(_) => state.audit.info(
                        Category::Event,
                        None,
                        format!(
                            "tray menu rebuilt: profiles [{}] (was [{}])",
                            ids.join(", "),
                            known_ids.join(", ")
                        ),
                    ),
                    // Worth a warning: the menu is now stale relative to the
                    // config, and the loop is retrying rather than recovering.
                    None => state.audit.warn(
                        Category::Event,
                        None,
                        "tray menu rebuild failed; keeping the previous menu and retrying",
                    ),
                }

                if let Some(new_handles) = rebuilt {
                    last_status = snapshot.status_line();
                    last_labels = snapshot.rows.iter().map(|row| row.label()).collect();
                    last_checked = snapshot
                        .rows
                        .iter()
                        .map(|row| Some(row.checked()))
                        .collect();
                    handles = new_handles;
                    known_ids = ids;

                    // A set change can also change the aggregate state —
                    // deleting the only running profile is exactly that — and
                    // the `continue` below skips the icon update further down,
                    // so it has to happen here or the icon stays stale until
                    // some later status change happens to move it.
                    update_icon(&app, &state, &snapshot, &mut last_icon);

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

            update_icon(&app, &state, &snapshot, &mut last_icon);

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
            state
                .audit
                .info(Category::Action, None, "chose Quit from the tray menu");
            // `RunEvent::Exit` in `main` stops every child, so this does not
            // leak a proxy holding 15432.
            app.exit(0);
        }
        ID_PROFILES => {
            state.audit.info(
                Category::Action,
                None,
                "opened the settings window on Profiles",
            );
            open_settings(&app, Section::Profiles);
        }
        ID_LOGS => {
            state
                .audit
                .info(Category::Action, None, "opened the settings window on Logs");
            open_settings(&app, Section::Logs);
        }
        ID_AUTOSTART => toggle_autostart(&app, &autostart_item, &state),
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

    state.audit.info(
        Category::Action,
        Some(profile_id),
        format!(
            "clicked the tray row for '{profile_id}' while it was {}",
            if running { "running" } else { "not running" }
        ),
    );

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
        // A profile whose connection names have not been typed in yet is
        // blocked by preflight, which says so and points at the Profiles
        // window. Surfacing it as a modal is the point: a click that silently
        // did nothing would read as a broken app.
        report_error(app, &format!("Could not start {profile_id}"), &message);
    }
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

    state.audit.warn(
        Category::Action,
        Some(profile_id),
        format!("production-start confirmation shown for '{name}' on {ports}"),
    );

    let confirmed = confirm(
        app,
        format!("Start {name}?"),
        message,
        "Start production",
        MessageDialogKind::Warning,
    )
    .await;

    // Both answers are recorded. A cancelled production start is exactly the
    // kind of thing someone later wants to prove happened -- "did I actually
    // start prd at 14:02, or did I back out?" -- and only logging the
    // confirmation would make the two indistinguishable.
    if confirmed {
        state.audit.warn(
            Category::Action,
            Some(profile_id),
            format!("CONFIRMED the production start of '{name}'"),
        );
    } else {
        state.audit.info(
            Category::Action,
            Some(profile_id),
            format!("cancelled the production start of '{name}'"),
        );
    }

    confirmed
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
fn toggle_autostart<R: Runtime>(app: &AppHandle<R>, item: &CheckMenuItem<R>, state: &SharedState) {
    use tauri_plugin_autostart::ManagerExt;

    let autolaunch = app.autolaunch();
    let currently = autolaunch.is_enabled().unwrap_or(false);

    let result = if currently {
        autolaunch.disable()
    } else {
        autolaunch.enable()
    };

    let now = autolaunch.is_enabled().unwrap_or(currently);
    let _ = item.set_checked(now);

    if let Err(error) = result {
        state.audit.error(
            Category::Action,
            None,
            format!("Launch at Login toggle failed: {error}"),
        );
        report_error(app, "Could not change Launch at Login", &error.to_string());
        return;
    }
    state.audit.info(
        Category::Action,
        None,
        format!("Launch at Login {currently} -> {now}"),
    );
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
    fn icon_is_disconnected_when_everything_is_stopped() {
        assert_eq!(
            snapshot_of(&["dev", "stg"]).icon_state(),
            IconState::Disconnected
        );
        // No profiles configured is also nothing connected.
        assert_eq!(
            Snapshot { rows: vec![] }.icon_state(),
            IconState::Disconnected
        );
    }

    #[test]
    fn one_running_profile_makes_the_icon_connected() {
        let mut snapshot = snapshot_of(&["dev", "stg", "prd"]);
        snapshot.rows[1].status = ProxyStatus::Running;
        assert_eq!(snapshot.icon_state(), IconState::Connected);
    }

    #[test]
    fn starting_shows_connecting_not_disconnected() {
        // Carried over from the two-state icon: the port is bound, and
        // feedback the instant you click beats waiting a second for the ready
        // line. What must never happen is `Starting` reading as disconnected.
        let snapshot = Snapshot {
            rows: vec![row("dev", false, ProxyStatus::Starting)],
        };
        assert_eq!(snapshot.icon_state(), IconState::Connecting);
        assert_ne!(snapshot.icon_state(), IconState::Disconnected);
    }

    #[test]
    fn failed_profile_never_shows_as_connected() {
        // Nothing is listening, so a connected glyph would be a lie — the menu
        // reports the failure, the icon must not claim a connection.
        let snapshot = Snapshot {
            rows: vec![row("dev", false, ProxyStatus::Failed(diagnosis()))],
        };
        assert_eq!(snapshot.icon_state(), IconState::Error);
        assert_ne!(snapshot.icon_state(), IconState::Connected);
    }

    #[test]
    fn failure_outranks_a_healthy_profile() {
        // The precedence case: dev is up, prd failed. A failure the user has
        // not seen is the thing worth surfacing in the menu bar; the menu
        // itself still carries "dev running, prd failed".
        let mut snapshot = snapshot_of(&["dev", "prd"]);
        snapshot.rows[0].status = ProxyStatus::Running;
        snapshot.rows[1].status = ProxyStatus::Failed(diagnosis());
        assert_eq!(snapshot.icon_state(), IconState::Error);

        // Order within the row list must not change the answer.
        snapshot.rows.reverse();
        assert_eq!(snapshot.icon_state(), IconState::Error);
    }

    #[test]
    fn failure_outranks_a_start_in_flight() {
        let mut snapshot = snapshot_of(&["dev", "prd"]);
        snapshot.rows[0].status = ProxyStatus::Starting;
        snapshot.rows[1].status = ProxyStatus::Failed(diagnosis());
        assert_eq!(snapshot.icon_state(), IconState::Error);
    }

    #[test]
    fn starting_outranks_running() {
        // A start in flight is the more recent user action and the one whose
        // outcome is still unknown, so it wins over an already-settled
        // connection.
        let mut snapshot = snapshot_of(&["dev", "prd"]);
        snapshot.rows[0].status = ProxyStatus::Running;
        snapshot.rows[1].status = ProxyStatus::Starting;
        assert_eq!(snapshot.icon_state(), IconState::Connecting);
    }

    // The per-asset checks (decodes, exact 2x size, all eight distinct) live at the
    // bottom of this module with `ALL_PAIRS`, so they cover the appearance
    // dimension as well as the state one.

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
        for reserved in [ID_QUIT, ID_LOGS, ID_PROFILES, ID_AUTOSTART, ID_STATUS] {
            assert_ne!(format!("{PROFILE_PREFIX}{reserved}"), reserved);
        }
    }

    /// The pixel size every embedded tray asset must be.
    ///
    /// Kept as an exact number rather than a lower bound because that is what
    /// would have caught the 18px regression: an asset silently at 1x is not a
    /// decode failure and not visible in a diff, it is just soft. `TRAY_SIZE` in
    /// `design/generate-icons.py` is the same number on the generating side.
    const TRAY_ICON_PX: u32 = 36;

    /// Every (state, appearance) pair the tray can ask for.
    const ALL_PAIRS: [(IconState, Appearance); 8] = [
        (IconState::Disconnected, Appearance::Dark),
        (IconState::Connecting, Appearance::Dark),
        (IconState::Connected, Appearance::Dark),
        (IconState::Error, Appearance::Dark),
        (IconState::Disconnected, Appearance::Light),
        (IconState::Connecting, Appearance::Light),
        (IconState::Connected, Appearance::Light),
        (IconState::Error, Appearance::Light),
    ];

    /// Read a PNG's declared dimensions straight out of its IHDR chunk.
    ///
    /// Deliberately not via `Image::from_bytes`: that needs no runtime here, but
    /// going to the bytes checks the number the menu bar will actually act on
    /// rather than whatever a decoder chose to hand back.
    fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(&bytes[1..4], b"PNG", "not a PNG");
        let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        (width, height)
    }

    #[test]
    fn every_tray_asset_is_exactly_the_2x_size() {
        // Exact, not a minimum, and that is the point: `tray-icon` draws whatever
        // it is handed at a hardcoded 18pt, so an asset at the wrong size is never
        // a decode failure and never visibly broken in review — it is silently
        // soft. The 18px set that used to be here was a 1x asset on a Retina
        // display, which is what "small and blurry" was. See the comment on the
        // `include_bytes!` block for the measurements.
        for (state, appearance) in ALL_PAIRS {
            let bytes = tray_icon_bytes(state, appearance);
            assert_eq!(
                png_dimensions(bytes),
                (TRAY_ICON_PX, TRAY_ICON_PX),
                "{} / {} is not {TRAY_ICON_PX}x{TRAY_ICON_PX}",
                state.as_str(),
                appearance.as_str()
            );
        }
    }

    #[test]
    fn every_tray_asset_decodes_at_the_2x_size() {
        // A corrupt embed would leave the tray falling back to a text title,
        // which is a silent degradation nobody would notice in review. The
        // dimensions are re-checked here on the *decoded* image rather than the
        // header, because `Image::from_bytes` is what actually feeds the menu
        // bar and it is its notion of the size that governs.
        for (state, appearance) in ALL_PAIRS {
            let icon = tray_icon(state, appearance).unwrap_or_else(|| {
                panic!(
                    "{} / {} failed to decode",
                    state.as_str(),
                    appearance.as_str()
                )
            });
            assert_eq!(
                (icon.width(), icon.height()),
                (TRAY_ICON_PX, TRAY_ICON_PX),
                "decoded {} / {} is not {TRAY_ICON_PX}x{TRAY_ICON_PX}",
                state.as_str(),
                appearance.as_str()
            );
        }
    }

    #[test]
    fn no_two_state_appearance_pairs_share_an_asset() {
        // A copy-paste slip in `tray_icon_bytes` that pointed two pairs at one
        // image is otherwise invisible: the tray still shows *an* icon, just the
        // wrong one, and only for whichever state nobody was watching.
        for (i, &(state, appearance)) in ALL_PAIRS.iter().enumerate() {
            for &(other_state, other_appearance) in &ALL_PAIRS[i + 1..] {
                assert_ne!(
                    tray_icon_bytes(state, appearance),
                    tray_icon_bytes(other_state, other_appearance),
                    "{} / {} and {} / {} are the same asset",
                    state.as_str(),
                    appearance.as_str(),
                    other_state.as_str(),
                    other_appearance.as_str()
                );
            }
        }
    }

    #[test]
    fn the_two_appearances_of_a_state_differ_from_each_other() {
        // Subsumed by the pairwise check above, but stated on its own because it
        // is the specific failure this feature exists to prevent: if the light
        // variant were accidentally the dark bytes, the icon would be invisible
        // on a light menu bar exactly as before, and every other test would
        // still pass.
        for state in [
            IconState::Disconnected,
            IconState::Connecting,
            IconState::Connected,
            IconState::Error,
        ] {
            assert_ne!(
                tray_icon_bytes(state, Appearance::Dark),
                tray_icon_bytes(state, Appearance::Light),
                "{} has the same asset for both appearances",
                state.as_str()
            );
        }
    }

    #[test]
    fn state_and_appearance_names_are_distinct_for_the_audit_log() {
        // The audit log distinguishes a status change from an appearance flip by
        // these names, so a collision would make the log ambiguous.
        assert_ne!(Appearance::Dark.as_str(), Appearance::Light.as_str());
        let states = [
            IconState::Disconnected.as_str(),
            IconState::Connecting.as_str(),
            IconState::Connected.as_str(),
            IconState::Error.as_str(),
        ];
        for (i, name) in states.iter().enumerate() {
            assert!(
                !states[i + 1..].contains(name),
                "duplicate state name {name}"
            );
        }
    }
}
