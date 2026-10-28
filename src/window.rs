//! The settings window: an `NSWindow` owned here, with an nsh `AppView` as its
//! content (see [`crate::ui`]).
//!
//! The window exists only while it is open. Closing it stops the `AppView`
//! (nsh's engine and its thread) and drops it, so an idle app holds no UI at
//! all. Dropping an `AppView` does not stop it; `stop()` does.
//!
//! An accessory app has no main menu, so nothing answers Cmd+W or the editing
//! shortcuts; [`install_main_menu`] supplies the minimum a text-editing window
//! needs.

use std::cell::RefCell;
use std::rc::Rc;

use nsh_platform_macos as platform;
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSMenu, NSMenuItem, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use crate::ui;

const APP_ID: &str = "ai.firsthand.fh-cloud-sql-proxy-gui";
const WINDOW_TITLE: &str = "Cloud SQL Proxy";
const WINDOW_SIZE: (u32, u32) = (900, 600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Section {
    Profiles = 0,
    Logs = 1,
}

struct Open {
    window: Retained<NSWindow>,
    view: Rc<platform::AppView>,
}

thread_local! {
    static OPEN: RefCell<Option<Open>> = const { RefCell::new(None) };
    static DELEGATE: RefCell<Option<Retained<WindowDelegate>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SettingsWindowDelegate"]
    #[ivars = ()]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        /// Closing drops the editor's copy, so unsaved edits are asked about
        /// first. This runs from AppKit's event handling, not a GCD block, so
        /// the modal does not starve the main queue.
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &NSWindow) -> Bool {
            if !ui::is_dirty() {
                return Bool::YES;
            }
            Bool::new(crate::dialogs::run_modal(
                self.mtm(),
                "Discard your unsaved changes?",
                "Closing the window will lose the changes you have not saved.",
                crate::dialogs::Kind::Warning,
                "Discard",
                Some("Keep Editing"),
            ))
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            // Take the state out first: closing re-enters this delegate.
            let open = OPEN.with(|o| o.borrow_mut().take());
            if let Some(open) = open {
                open.window.setContentView(None);
                open.window.setDelegate(None);
                open.view.stop();
            }
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// Open the settings window on `section`, or bring the open one forward and
/// switch it. Callable from the main thread only.
pub fn open(section: Section) {
    let Some(mtm) = MainThreadMarker::new() else {
        crate::host::on_main(move || open(section));
        return;
    };
    ui::request_section(section);

    let already = OPEN.with(|o| o.borrow().as_ref().map(|o| o.window.clone()));
    if let Some(window) = already {
        window.makeKeyAndOrderFront(None);
    } else {
        // A new window starts clean: no banner or note from the last one.
        ui::reset_session();
        let (w, h) = WINDOW_SIZE;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
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
        window.setTitle(&NSString::from_str(WINDOW_TITLE));
        window.setMinSize(NSSize::new(640.0, 420.0));
        // AppKit remembers the size and position across launches.
        window.center();
        window.setFrameAutosaveName(ns_string!("SettingsWindow"));

        let delegate = DELEGATE.with(|d| {
            d.borrow_mut()
                .get_or_insert_with(|| WindowDelegate::new(mtm))
                .clone()
        });
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        let view = platform::AppView::new(
            mtm,
            platform::AppOptions {
                title: WINDOW_TITLE.to_owned(),
                app_id: APP_ID.to_owned(),
                size: (w as i32, h as i32),
                ..Default::default()
            },
        );
        view.load(platform::App::Rust(
            platform::Root::of::<ui::Settings>().app_id(APP_ID),
        ));
        window.setContentView(Some(&view.view()));
        window.makeKeyAndOrderFront(None);
        OPEN.with(|o| *o.borrow_mut() = Some(Open { window, view }));
    }
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    key: &NSString,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(title),
            Some(action),
            key,
        )
    }
}

fn submenu(
    mtm: MainThreadMarker,
    title: &str,
    items: &[Retained<NSMenuItem>],
) -> Retained<NSMenuItem> {
    let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title));
    for i in items {
        menu.addItem(i);
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&menu));
    holder
}

/// Edit and Window menus, so Cmd+C/V/X/A/Z and Cmd+W/M work in the window. The
/// menu bar is never shown (the app is an accessory) but its key equivalents
/// still route.
pub fn install_main_menu(mtm: MainThreadMarker) {
    let bar = NSMenu::new(mtm);
    // The first menu is the application menu by convention. Cmd+Q lives here, so
    // it quits from the settings window too, not only while the tray menu is open.
    bar.addItem(&submenu(
        mtm,
        "",
        &[item(
            mtm,
            "Quit Cloud SQL Proxy",
            sel!(terminate:),
            ns_string!("q"),
        )],
    ));
    bar.addItem(&submenu(
        mtm,
        "Edit",
        &[
            item(mtm, "Undo", sel!(undo:), ns_string!("z")),
            item(mtm, "Redo", sel!(redo:), ns_string!("Z")),
            item(mtm, "Cut", sel!(cut:), ns_string!("x")),
            item(mtm, "Copy", sel!(copy:), ns_string!("c")),
            item(mtm, "Paste", sel!(paste:), ns_string!("v")),
            item(mtm, "Select All", sel!(selectAll:), ns_string!("a")),
        ],
    ));
    bar.addItem(&submenu(
        mtm,
        "Window",
        &[
            item(mtm, "Minimize", sel!(performMiniaturize:), ns_string!("m")),
            item(mtm, "Close", sel!(performClose:), ns_string!("w")),
        ],
    ));
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&bar));
}
