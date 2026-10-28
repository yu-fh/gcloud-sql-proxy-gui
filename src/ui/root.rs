//! The window's root: the sidebar that picks a section, the error banner, and
//! the polling that carries service results into the signals.
//!
//! One window, two sections, chosen from a source-list sidebar, as a macOS
//! settings window is. A tab strip inside a settings window is the most
//! web-looking thing a native panel can have.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use nsh_core::prelude::*;

use super::bridge::{self, Banner};
use super::logs::logs_section;
use super::profiles::profiles_section;
use super::theme::{self, button};
use crate::core::profile::Profile;
use crate::service::{LogsView, ProfileView};
use crate::window::Section;

/// Which section the window should show. The tray writes it (including while
/// the window is already open); the root reads it on its poll tick.
static REQUESTED: AtomicU8 = AtomicU8::new(0);

pub fn request_section(section: Section) {
    REQUESTED.store(section as u8, Ordering::Release);
}

fn requested() -> Section {
    if REQUESTED.load(Ordering::Acquire) == Section::Logs as u8 {
        Section::Logs
    } else {
        Section::Profiles
    }
}

/// How often the root looks for new results and a section request.
const POLL: Duration = Duration::from_millis(150);
/// How often live status is refreshed while the window is open.
const STATUS_POLL: Duration = Duration::from_secs(1);

/// The window's signals, shared by its views.
#[derive(Clone, Copy)]
pub struct Model {
    /// Profiles with live status, as the service last reported them.
    pub live: Signal<Vec<ProfileView>>,
    /// The editor's own copy of the profiles.
    pub edit: Signal<Vec<Profile>>,
    pub selected: Signal<Option<String>>,
    pub importing: Signal<bool>,
    pub dirty: Signal<bool>,
    /// Bumped when the editor's copy is replaced, so the detail pane rebuilds.
    pub epoch: Signal<u64>,
    pub ignored: Signal<Vec<String>>,
    pub note: Signal<String>,
    pub banner: Signal<Option<Banner>>,
    pub logs: Signal<Option<LogsView>>,
}

impl Model {
    fn new() -> Self {
        Self {
            live: signal(Vec::new()),
            edit: signal(Vec::new()),
            selected: signal(None),
            importing: signal(false),
            dirty: signal(false),
            epoch: signal(0),
            ignored: signal(Vec::new()),
            note: signal(String::new()),
            banner: signal(None),
            logs: signal(None),
        }
    }

    /// Take what the bridge has and put it into the signals.
    fn apply(&self, data: bridge::Data) {
        self.live.set(data.profiles.clone());
        if data.reset_edits {
            self.edit
                .set(data.profiles.iter().map(|v| v.profile.clone()).collect());
            self.dirty.set(false);
            self.epoch.update(|e| *e += 1);
        }
        if let Some(id) = data.select {
            self.selected.set(Some(id));
        }
        // The selection must name a profile that exists; after a delete or a
        // reload it may not, and an empty detail pane for a ghost is wrong.
        let current = self.selected.get_untracked();
        let valid = current
            .as_ref()
            .is_some_and(|id| data.profiles.iter().any(|v| &v.profile.id == id));
        if !valid {
            self.selected
                .set(data.profiles.first().map(|v| v.profile.id.clone()));
        }
        self.ignored.set(data.ignored_flags);
        self.banner.set(data.banner);
        self.note.set(data.note);
        self.logs.set(data.logs);
    }
}

fn nav_row(label: &'static str, section: Section, current: Signal<Section>) -> impl IntoElement {
    view()
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(6.0))
        .bg(move || {
            if current.get() == section {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            }
        })
        .on_press(move || {
            request_section(section);
            current.set(section);
        })
        .child(text(label).text_size(px(13.0)).text_color(theme::TEXT))
}

#[component]
pub fn Settings() -> impl IntoElement {
    let m = Model::new();
    let section = signal(requested());
    let seen = signal(0u64);

    bridge::load_profiles(true, None);

    set_interval(POLL, move || {
        let want = requested();
        if section.get_untracked() != want {
            section.set(want);
        }
        let version = bridge::version();
        if seen.get_untracked() != version {
            seen.set(version);
            m.apply(bridge::take());
        }
    });
    set_interval(STATUS_POLL, bridge::refresh_status);

    let banner = when(
        move || m.banner.get().is_some(),
        move || {
            let b = m.banner.get_untracked().unwrap_or_default();
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .px(px(16.0))
                .py(px(8.0))
                .bg(theme::ERROR_BG)
                .child(
                    view()
                        .flex_1()
                        .child(text(b.title).text_size(px(13.0)).text_color(theme::ERROR))
                        .child(text(b.body).text_size(px(12.0)).text_color(theme::TEXT)),
                )
                .child(button("Dismiss", || true, bridge::clear_banner))
        },
    );

    view()
        .flex_1()
        .flex_row()
        .bg(theme::WINDOW)
        .child(
            view()
                .w(px(170.0))
                .p(px(10.0))
                .gap_1()
                .bg(theme::SIDEBAR)
                .child(nav_row("Profiles", Section::Profiles, section))
                .child(nav_row("Logs", Section::Logs, section)),
        )
        .child(
            view().flex_1().child(banner).child(
                when(
                    move || section.get() == Section::Profiles,
                    move || profiles_section(m),
                )
                .otherwise(move || logs_section(m.live, m.logs)),
            ),
        )
}
