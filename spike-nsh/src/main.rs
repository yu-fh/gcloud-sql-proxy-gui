//! Spike: the menu bar app on AppKit, with the environment window drawn by nsh.
//!
//! Host-owned (not nsh's `run_app`), because this is an `LSUIElement`-style
//! tray app: no window at launch, an `NSStatusItem` whose menu is rebuilt from
//! the backend snapshot each time it opens, and a window that exists only
//! while it is open — closing it drops the `AppView` and with it nsh's engine,
//! so an idle app holds no UI at all.

// The app's own core, unchanged and not copied. It needs no Tauri.
#[path = "../../src-tauri/src/core/mod.rs"]
mod core;

mod backend;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use nsh_platform_macos as platform;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSControlStateValueOn, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use backend::Phase;

const APP_ID: &str = "ai.firsthand.cloud-sql-proxy.spike";

struct Host {
    delegate: Retained<Delegate>,
    status_item: Retained<NSStatusItem>,
    window: Option<Retained<NSWindow>>,
    view: Option<Rc<platform::AppView>>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SpikeDelegate"]
    #[ivars = ()]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            false
        }
    }

    unsafe impl NSMenuDelegate for Delegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            rebuild_menu(self, menu);
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            // Stop the AppView (nsh's engine and its thread) with the window.
            HOST.with(|h| {
                if let Some(host) = h.borrow_mut().as_mut() {
                    if let Some(window) = host.window.take() {
                        window.setContentView(None);
                        window.setDelegate(None);
                    }
                    // Dropping an AppView does not stop it: stop() joins the
                    // engine thread and frees the mounted views.
                    if let Some(view) = host.view.take() {
                        view.stop();
                    }
                }
            });
        }
    }

    impl Delegate {
        #[unsafe(method(toggle:))]
        fn toggle(&self, sender: &NSMenuItem) {
            if let Some(row) = backend::snapshot().get(sender.tag() as usize) {
                backend::toggle(row.id.clone());
            }
        }

        #[unsafe(method(openWindow:))]
        fn open_window(&self, _sender: Option<&AnyObject>) {
            show_window(self.mtm());
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            backend::stop_all_blocking();
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

fn menu_item(
    mtm: MainThreadMarker,
    delegate: &Delegate,
    title: &str,
    action: Option<objc2::runtime::Sel>,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            action,
            ns_string!(""),
        )
    };
    if action.is_some() {
        unsafe { item.setTarget(Some(delegate)) };
    }
    item
}

fn rebuild_menu(delegate: &Delegate, menu: &NSMenu) {
    let mtm = delegate.mtm();
    menu.removeAllItems();
    let rows = backend::snapshot();
    if rows.is_empty() {
        menu.addItem(&menu_item(mtm, delegate, "No environments", None));
    }
    for (i, row) in rows.iter().enumerate() {
        let state = match &row.phase {
            Phase::Stopped => "disconnected".to_string(),
            Phase::Starting => "connecting…".to_string(),
            Phase::Running => "connected".to_string(),
            Phase::Failed(why) => format!("failed: {why}"),
        };
        let item = menu_item(
            mtm,
            delegate,
            &format!("{} — {state}", row.name),
            Some(sel!(toggle:)),
        );
        item.setTag(i as isize);
        if row.phase == Phase::Running {
            item.setState(NSControlStateValueOn);
        }
        menu.addItem(&item);
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, delegate, "Environments…", Some(sel!(openWindow:))));
    menu.addItem(&menu_item(mtm, delegate, "Quit", Some(sel!(quit:))));
}

/// The status item's title says how many are connected, so the tray shows
/// state without opening the menu.
fn update_status_title() {
    let connected = backend::snapshot().iter().filter(|r| r.phase == Phase::Running).count();
    let title = if connected == 0 { "SQL".to_string() } else { format!("SQL ●{connected}") };
    HOST.with(|h| {
        if let Some(host) = h.borrow().as_ref() {
            if let Some(button) = host.status_item.button(host.delegate.mtm()) {
                button.setTitle(&NSString::from_str(&title));
            }
        }
    });
}

fn show_window(mtm: MainThreadMarker) {
    HOST.with(|h| {
        let mut h = h.borrow_mut();
        let host = h.as_mut().expect("host");
        if let Some(window) = &host.window {
            window.makeKeyAndOrderFront(None);
        } else {
            let (w, hgt) = (460, 320);
            let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, hgt as f64));
            let style = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable;
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    mtm.alloc(),
                    rect,
                    style,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setTitle(ns_string!("Environments"));
            window.center();
            window.setDelegate(Some(ProtocolObject::from_ref(&*host.delegate)));

            let view = platform::AppView::new(
                mtm,
                platform::AppOptions {
                    title: "Environments".to_owned(),
                    app_id: APP_ID.to_owned(),
                    size: (w, hgt),
                    ..Default::default()
                },
            );
            view.load(platform::App::Rust(
                platform::Root::of::<ui::Environments>().app_id(APP_ID),
            ));
            window.setContentView(Some(&view.view()));
            window.makeKeyAndOrderFront(None);
            host.window = Some(window);
            host.view = Some(view);
        }
    });
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

fn main() {
    let mtm = MainThreadMarker::new().expect("main thread");
    backend::init();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    let menu = NSMenu::new(mtm);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    status_item.setMenu(Some(&menu));

    HOST.with(|h| {
        *h.borrow_mut() = Some(Host { delegate, status_item, window: None, view: None });
    });
    update_status_title();
    backend::on_change(|| {
        dispatch2::DispatchQueue::main().exec_async(update_status_title);
    });

    // Verification hook: drive the same `toggle` the tray and window call, and
    // print what the snapshot says, so the wiring is checked without a click.
    if std::env::var_os("SPIKE_SELFTEST").is_some() {
        std::thread::spawn(|| {
            let show = |step: &str| {
                let rows: Vec<String> = backend::snapshot()
                    .iter()
                    .map(|r| format!("{}={:?}", r.id, r.phase))
                    .collect();
                println!("{step:<22} {}", rows.join("  "));
            };
            std::thread::sleep(std::time::Duration::from_millis(500));
            show("initial");
            backend::toggle("dev".into());
            std::thread::sleep(std::time::Duration::from_millis(1200));
            show("connect dev");
            backend::toggle("stg".into()); // shares ports with dev: dev must stop
            std::thread::sleep(std::time::Duration::from_millis(1200));
            show("connect stg");
            backend::toggle("prd".into()); // own ports: runs alongside stg
            std::thread::sleep(std::time::Duration::from_millis(1200));
            show("connect prd");
            backend::toggle("stg".into());
            std::thread::sleep(std::time::Duration::from_millis(1200));
            show("disconnect stg");
            backend::stop_all_blocking();
            std::process::exit(0);
        });
    }
    if std::env::var_os("SPIKE_OPEN_WINDOW").is_some() {
        show_window(mtm);
    }
    // Measurement hook: close the window as a click on its close button would,
    // so the memory after a close can be read without driving the UI.
    if let Some(ms) = std::env::var("SPIKE_CLOSE_AFTER_MS").ok().and_then(|v| v.parse().ok()) {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            dispatch2::DispatchQueue::main().exec_async(|| {
                // Clone out first: performClose re-enters `HOST` through
                // windowWillClose, so no borrow may be held across it.
                let window = HOST.with(|h| h.borrow().as_ref().and_then(|h| h.window.clone()));
                if let Some(window) = window {
                    window.performClose(None);
                }
            });
        });
    }
    app.run();
}
