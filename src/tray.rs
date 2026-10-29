//! The menu bar tray: the app's primary UI, modelled on Tailscale.
//!
//! A native `NSMenu` hanging off an `NSStatusItem` lists every profile with its
//! ports and live status; clicking one toggles it. `Profiles…` and `Logs…` open
//! the settings window ([`crate::window`]); the alerts live in
//! [`crate::dialogs`].
//!
//! The rendering inputs and the icon rules are in [`crate::tray_model`]. This
//! module is the AppKit half: everything here runs on the main thread.
//!
//! # No duplicated policy
//!
//! Every action calls the matching function in [`crate::service`], the same one
//! the settings window uses, so the two cannot drift apart on the start policy,
//! the preflight gate, or the stop-then-start conflict resolution.
//!
//! # Polling
//!
//! A profile reaches `Running` a second or so after spawn, when the log watcher
//! sees the ready line, and the core exposes no change notification. So a task
//! on the runtime re-reads the state every [`BLINK_INTERVAL`] and hands each
//! snapshot to the main thread. The menu is rebuilt from the latest snapshot
//! each time it opens (`menuNeedsUpdate:`), which is how an added or deleted
//! profile ever appears, with no diffing of the profile set.
//!
//! # Appearance
//!
//! The icon ink follows the system appearance, read from
//! `NSApp.effectiveAppearance` on every tick. That read is main-thread-only and
//! the tick is delivered there, so it is correct by construction. The icon is
//! only rewritten when the (state, appearance, blink) triple changes.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSControlStateValueOn,
    NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{
    ns_string, NSArray, NSData, NSNotification, NSObject, NSObjectProtocol, NSSize, NSString,
};

use crate::core::audit::Category;
use crate::dialogs::{self, Kind};
use crate::host::{on_main, runtime, state};
use crate::tray_model::{
    icon_bytes, join_ports, snapshot, Appearance, Blink, Drawn, IconState, Snapshot, BLINK_INTERVAL,
};
use crate::window::{self, Section};
use crate::{autostart, service};

/// The status item image height in points. The assets are 72px, a pixel-exact
/// 4x of this.
const ICON_POINTS: f64 = 18.0;

struct Tray {
    delegate: Retained<Delegate>,
    status_item: Retained<NSStatusItem>,
    /// What the status item currently shows, so an unchanged tick writes nothing.
    drawn: Option<Drawn>,
    /// The newest snapshot, which the menu is built from when it opens.
    latest: Snapshot,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TrayDelegate"]
    #[ivars = ()]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            false
        }

        /// The one place children are stopped, so every way of quitting (the
        /// menu, Cmd+Q, logout) goes through it. A leaked child keeps holding
        /// 15432/15433 and breaks the next launch.
        /// Launching the app again (Finder, Spotlight, `open`) while it runs
        /// only reaches here; with no Dock icon the menu-bar item may be
        /// hidden, so show the window rather than do nothing.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: Bool) -> Bool {
            window::open(Section::Profiles);
            Bool::YES
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            shut_down();
        }
    }

    unsafe impl NSMenuDelegate for Delegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            rebuild_menu(self, menu);
        }
    }

    impl Delegate {
        #[unsafe(method(toggleProfile:))]
        fn toggle_profile_action(&self, sender: &NSMenuItem) {
            let id = sender
                .representedObject()
                .and_then(|o| o.downcast::<NSString>().ok())
                .map(|s| s.to_string());
            if let Some(id) = id {
                runtime().spawn(toggle_profile(id));
            }
        }

        #[unsafe(method(openProfiles:))]
        fn open_profiles(&self, _sender: Option<&AnyObject>) {
            state()
                .audit
                .info(Category::Action, None, "opened the settings window on Profiles");
            window::open(Section::Profiles);
        }

        #[unsafe(method(openLogs:))]
        fn open_logs(&self, _sender: Option<&AnyObject>) {
            state()
                .audit
                .info(Category::Action, None, "opened the settings window on Logs");
            window::open(Section::Logs);
        }

        #[unsafe(method(toggleAutostart:))]
        fn toggle_autostart_action(&self, _sender: Option<&AnyObject>) {
            toggle_autostart();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            state()
                .audit
                .info(Category::Action, None, "chose Quit from the tray menu");
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Stop every child, record the exit, and give the audit writer a bounded
/// moment to land the last records. The bound matters more than the flush: an
/// app that will not quit is worse than a log missing its final line.
fn shut_down() {
    let state = state();
    state
        .audit
        .info(Category::System, None, "--- app exiting ---");
    runtime().block_on(async {
        state.manager.lock().await.stop_all().await;
    });
    state
        .audit
        .flush_blocking(std::time::Duration::from_millis(500));
}

/// Build the status item and start the poll loop. Main thread, before
/// `NSApplication::run`.
pub fn install(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    // Menu-bar-only: no Dock icon, no app switcher entry. `LSUIElement` in
    // Info.plist keeps the Dock icon from ever being drawn; this covers a run
    // outside a bundle.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    status_item.setMenu(Some(&menu));

    // Read the initial state synchronously so the first icon is right. Safe
    // because nothing else can be holding either lock yet.
    let initial = runtime().block_on(snapshot(state()));

    TRAY.with(|t| {
        *t.borrow_mut() = Some(Tray {
            delegate,
            status_item,
            drawn: None,
            latest: Snapshot::default(),
        });
    });
    apply(initial, Blink::On);

    runtime().spawn(async {
        let mut ticker = tokio::time::interval(BLINK_INTERVAL);
        let mut phase = Blink::On;
        loop {
            ticker.tick().await;
            phase = phase.flipped();
            let snap = snapshot(state()).await;
            on_main(move || apply(snap, phase));
        }
    });
}

/// Take a snapshot and blink phase from the poll loop and bring the status item
/// in line with them. Main thread.
fn apply(snap: Snapshot, phase: Blink) {
    let icon_state = snap.icon_state();
    let appearance = read_appearance();
    // Only `Connecting` blinks; every other state holds the `On` frame.
    let blink = if icon_state == IconState::Connecting {
        phase
    } else {
        Blink::On
    };

    TRAY.with(|t| {
        let mut t = t.borrow_mut();
        let Some(tray) = t.as_mut() else { return };
        tray.latest = snap;

        let wanted = (icon_state, appearance, blink);
        if tray.drawn == Some(wanted) {
            return;
        }
        let mtm = tray.delegate.mtm();
        if draw_icon(tray, mtm, icon_state, appearance, blink) {
            let changed = tray
                .drawn
                .map(|(s, a, _)| (s, a) != (icon_state, appearance))
                .unwrap_or(true);
            tray.drawn = Some(wanted);
            // The blink repaints twice a second; only a change of what the icon
            // says is worth a record.
            if changed {
                state().audit.info(
                    Category::Event,
                    None,
                    format!(
                        "tray icon -> {} ({} menu bar)",
                        icon_state.as_str(),
                        appearance.as_str()
                    ),
                );
            }
        }
    });
}

/// Install the image for one icon triple. False when it could not be decoded;
/// the item then falls back to a text title so the tray stays clickable.
fn draw_icon(
    tray: &Tray,
    mtm: MainThreadMarker,
    icon_state: IconState,
    appearance: Appearance,
    blink: Blink,
) -> bool {
    let Some(button) = tray.status_item.button(mtm) else {
        return false;
    };
    let data = NSData::with_bytes(icon_bytes(icon_state, appearance, blink));
    match NSImage::initWithData(mtm.alloc::<NSImage>(), &data) {
        Some(image) => {
            image.setSize(NSSize::new(ICON_POINTS, ICON_POINTS));
            // Colour assets, not template images: a template would flatten the
            // whole thing to a black silhouette and lose the status dot.
            image.setTemplate(false);
            button.setImage(Some(&image));
            button.setTitle(ns_string!(""));
            true
        }
        None => {
            button.setImage(None);
            button.setTitle(ns_string!("SQL"));
            false
        }
    }
}

fn read_appearance() -> Appearance {
    let Some(mtm) = MainThreadMarker::new() else {
        return Appearance::Light;
    };
    let names = NSArray::from_slice(&[
        ns_string!("NSAppearanceNameAqua"),
        ns_string!("NSAppearanceNameDarkAqua"),
    ]);
    let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
    let matched: Option<Retained<NSString>> = appearance.bestMatchFromAppearancesWithNames(&names);
    match matched.as_deref().map(NSString::to_string).as_deref() {
        Some("NSAppearanceNameDarkAqua") => Appearance::Dark,
        _ => Appearance::Light,
    }
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<objc2::runtime::Sel>,
    key: &NSString,
    target: Option<&Delegate>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            action,
            key,
        )
    };
    if let Some(target) = target {
        unsafe { item.setTarget(Some(target)) };
    }
    item
}

/// Rebuild the whole menu from the latest snapshot. Runs when the menu opens.
fn rebuild_menu(delegate: &Delegate, menu: &NSMenu) {
    let mtm = delegate.mtm();
    let snap = TRAY.with(|t| {
        t.borrow()
            .as_ref()
            .map(|t| t.latest.clone())
            .unwrap_or_default()
    });
    let none = ns_string!("");

    menu.removeAllItems();

    // The disabled status line: no action means AppKit draws it greyed out.
    menu.addItem(&menu_item(mtm, &snap.status_line(), None, none, None));
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    for row in &snap.rows {
        let item = menu_item(
            mtm,
            &row.label(),
            Some(sel!(toggleProfile:)),
            none,
            Some(delegate),
        );
        let id = NSString::from_str(&row.id);
        unsafe { item.setRepresentedObject(Some(&id)) };
        if row.checked() {
            item.setState(NSControlStateValueOn);
        }
        menu.addItem(&item);
    }

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(
        mtm,
        "Profiles…",
        Some(sel!(openProfiles:)),
        none,
        Some(delegate),
    ));
    menu.addItem(&menu_item(
        mtm,
        "Logs…",
        Some(sel!(openLogs:)),
        none,
        Some(delegate),
    ));

    let launch = menu_item(
        mtm,
        "Launch at Login",
        Some(sel!(toggleAutostart:)),
        none,
        Some(delegate),
    );
    if autostart::is_enabled() {
        launch.setState(NSControlStateValueOn);
    }
    menu.addItem(&launch);

    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(
        mtm,
        "Quit",
        Some(sel!(quit:)),
        ns_string!("q"),
        Some(delegate),
    ));
}

/// Start or stop a profile.
///
/// Which way round it goes is decided from the live status rather than from the
/// check mark: the menu is at most half a second stale, and acting on a stale
/// check mark could stop something the user just started.
async fn toggle_profile(profile_id: String) {
    let state = state();
    let running = state.manager.lock().await.is_running(&profile_id);

    state.audit.info(
        Category::Action,
        Some(&profile_id),
        format!(
            "clicked the tray row for '{profile_id}' while it was {}",
            if running { "running" } else { "not running" }
        ),
    );

    if running {
        if let Err(message) = service::stop_profile(state, profile_id.clone()).await {
            dialogs::report_error(&format!("Could not stop {profile_id}"), &message);
        }
        return;
    }

    // Danger profiles get their own confirmation on their own merits: prd
    // should never be one unguarded click from live traffic.
    let plan = match service::plan_for(state, profile_id.clone()).await {
        Ok(plan) => plan,
        Err(message) => {
            dialogs::report_error(&format!("Could not start {profile_id}"), &message);
            return;
        }
    };

    if plan.requires_confirmation && !confirm_danger(&profile_id, &plan.stop_first).await {
        return;
    }

    if let Err(message) = service::start_profile(state, profile_id.clone()).await {
        // A profile whose connection names are not typed in yet is blocked by
        // preflight, which says so. Surfacing it as a modal is the point: a
        // click that silently did nothing would read as a broken app.
        dialogs::report_error(&format!("Could not start {profile_id}"), &message);
    }
}

/// Ask before starting a `danger` profile. True to proceed.
async fn confirm_danger(profile_id: &str, stop_first: &[String]) -> bool {
    let state = state();
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

    let confirmed = dialogs::confirm(
        format!("Start {name}?"),
        message,
        "Start production",
        Kind::Warning,
    )
    .await;

    // Both answers are recorded: "did I actually start prd at 14:02, or did I
    // back out?" is unanswerable if only the confirmation is logged.
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

/// Flip the launch-at-login registration. The menu re-reads the state each time
/// it opens, so the check mark always reflects what is actually registered.
fn toggle_autostart() {
    let currently = autostart::is_enabled();
    let result = if currently {
        autostart::disable()
    } else {
        autostart::enable()
    };
    let now = autostart::is_enabled();

    if let Err(error) = result {
        state().audit.error(
            Category::Action,
            None,
            format!("Launch at Login toggle failed: {error}"),
        );
        dialogs::report_error("Could not change Launch at Login", &error);
        return;
    }
    state().audit.info(
        Category::Action,
        None,
        format!("Launch at Login {currently} -> {now}"),
    );
}
