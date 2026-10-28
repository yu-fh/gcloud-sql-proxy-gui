//! What the menu bar tray shows, as plain data: no AppKit in here, so all of it
//! is unit-testable.
//!
//! # Four glyphs, by aggregate state
//!
//! The menu bar glyph is the only part of the UI visible without opening the
//! menu, so it carries the aggregate state: `disconnected`, `connecting`,
//! `connected`, `error`. One glyph stands for many profiles, so the four states
//! are a strict precedence rather than a tally; see [`Snapshot::icon_state`].
//!
//! # Two ink sets, by system appearance
//!
//! The assets are colour images, not template images (a template would flatten
//! the status dot to a black silhouette). Opting out of the template inversion
//! means a light menu bar needs its own dark-ink set. Icon selection therefore
//! has two independent dimensions, [`IconState`] and [`Appearance`], and eight
//! embedded assets. The error dot is red in both sets.
//!
//! The assets are 72px square: a pixel-exact 4x of the 18pt status item image.
//! `icons/generate-tray-icons.py` is the source of record for them, not the
//! PNGs; change the geometry there and re-run it.
//!
//! # Blink
//!
//! `Connecting` blinks. The dark half of the blink is the bare glyph, which is
//! the `Disconnected` artwork, so it reuses that asset.

use std::time::Duration;

use crate::app_state::SharedState;
use crate::core::proxy::ProxyStatus;

/// How often live status is re-read. The core exposes no change notification,
/// so the tray polls.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long each half of the `Connecting` blink is held. A divisor of
/// [`POLL_INTERVAL`], so the blink subdivides a status tick evenly.
pub const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// The pixel size of every embedded tray asset.
pub const TRAY_ICON_PX: u32 = 72;

/// One profile's rendering inputs, cloned out from behind the locks.
#[derive(Debug, Clone)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub ports: Vec<u16>,
    pub danger: bool,
    pub status: ProxyStatus,
}

impl ProfileRow {
    /// The menu item label, e.g. `"⚠ prd  (15432/15433)"` or
    /// `"dev  (15432/15433) — starting…"`.
    ///
    /// The ports are always present because the environments share 15432/15433
    /// by convention: a bare "prd" next to a bare "dev" would make the user
    /// guess which one owns the port their client points at. The `⚠` marks
    /// danger profiles; menu items carry no colour, so a glyph is the channel.
    pub fn label(&self) -> String {
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

    /// Checked while `Running` or `Starting`: the check mark means "you asked
    /// for this to be on", so it appears the moment a start is underway.
    pub fn checked(&self) -> bool {
        matches!(self.status, ProxyStatus::Running | ProxyStatus::Starting)
    }
}

pub fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// The whole menu's rendering inputs, read under the locks in one pass.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub rows: Vec<ProfileRow>,
}

impl Snapshot {
    /// Which of the four icons the aggregate state calls for, in descending
    /// precedence:
    ///
    /// 1. any `Failed` → `Error`. A failure the user has not seen is the only
    ///    state that needs them to act, and the menu bar is the only place it
    ///    can be raised unprompted.
    /// 2. else any `Starting` → `Connecting`. A start must move the glyph the
    ///    instant you click, not when the ready line lands.
    /// 3. else any `Running` → `Connected`.
    /// 4. else → `Disconnected`, which is also the no-profiles case.
    pub fn icon_state(&self) -> IconState {
        let mut starting = false;
        let mut running = false;
        for row in &self.rows {
            match row.status {
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

    /// The disabled line at the top of the menu: what is active, and where.
    pub fn status_line(&self) -> String {
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
/// guards drop before anything touches AppKit.
///
/// Lock order: `config`, then `manager` while holding `config`.
pub async fn snapshot(state: &SharedState) -> Snapshot {
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

/// The aggregate state the glyph shows. One variant per embedded asset.
///
/// The designer's set also has a `paused` state. It is deliberately absent: the
/// app has no pause concept, so the variant would be unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

impl IconState {
    /// For the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            IconState::Disconnected => "disconnected",
            IconState::Connecting => "connecting",
            IconState::Connected => "connected",
            IconState::Error => "error",
        }
    }
}

/// Which menu bar the icon is drawn on, and therefore which ink it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// A dark menu bar: the artwork as delivered, white ink.
    Dark,
    /// A light menu bar: the derived dark-ink variant.
    Light,
}

impl Appearance {
    /// For the audit log.
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }
}

/// Which half of the blink cycle a `Connecting` icon is drawn in. For every
/// other state both phases select the same asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blink {
    /// The dot is drawn. The only phase a non-blinking state is ever in.
    On,
    /// The dot is omitted. Reached only by `Connecting`.
    Off,
}

impl Blink {
    pub fn flipped(self) -> Self {
        match self {
            Blink::On => Blink::Off,
            Blink::Off => Blink::On,
        }
    }
}

const DISCONNECTED: &[u8] = include_bytes!("../icons/tray-disconnected.png");
const CONNECTING: &[u8] = include_bytes!("../icons/tray-connecting.png");
const CONNECTED: &[u8] = include_bytes!("../icons/tray-connected.png");
const ERROR: &[u8] = include_bytes!("../icons/tray-error.png");
const DISCONNECTED_LIGHT: &[u8] = include_bytes!("../icons/tray-disconnected-light.png");
const CONNECTING_LIGHT: &[u8] = include_bytes!("../icons/tray-connecting-light.png");
const CONNECTED_LIGHT: &[u8] = include_bytes!("../icons/tray-connected-light.png");
const ERROR_LIGHT: &[u8] = include_bytes!("../icons/tray-error-light.png");

/// The embedded PNG for one (state, appearance, blink phase) triple.
pub fn icon_bytes(state: IconState, appearance: Appearance, blink: Blink) -> &'static [u8] {
    match (state, appearance, blink) {
        // The only cells where the phase changes the answer.
        (IconState::Connecting, Appearance::Dark, Blink::Off) => DISCONNECTED,
        (IconState::Connecting, Appearance::Light, Blink::Off) => DISCONNECTED_LIGHT,

        (IconState::Disconnected, Appearance::Dark, _) => DISCONNECTED,
        (IconState::Connecting, Appearance::Dark, _) => CONNECTING,
        (IconState::Connected, Appearance::Dark, _) => CONNECTED,
        (IconState::Error, Appearance::Dark, _) => ERROR,
        (IconState::Disconnected, Appearance::Light, _) => DISCONNECTED_LIGHT,
        (IconState::Connecting, Appearance::Light, _) => CONNECTING_LIGHT,
        (IconState::Connected, Appearance::Light, _) => CONNECTED_LIGHT,
        (IconState::Error, Appearance::Light, _) => ERROR_LIGHT,
    }
}

/// What is currently drawn on the status item.
pub type Drawn = (IconState, Appearance, Blink);

/// The identity of the profile set a menu was built from: the ids, in order.
pub fn profile_ids(snapshot: &Snapshot) -> Vec<String> {
    snapshot.rows.iter().map(|row| row.id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::log_watcher::{Diagnosis, FailureKind};

    fn row(name: &str, danger: bool, status: ProxyStatus) -> ProfileRow {
        ProfileRow {
            id: name.to_string(),
            name: name.to_string(),
            ports: vec![15432, 15433],
            danger,
            status,
        }
    }

    fn failed() -> ProxyStatus {
        ProxyStatus::Failed(Diagnosis {
            kind: FailureKind::PortInUse,
            message: "port 15432 is already in use".to_string(),
            fix_command: None,
        })
    }

    fn snapshot_of(statuses: Vec<ProxyStatus>) -> Snapshot {
        Snapshot {
            rows: statuses
                .into_iter()
                .enumerate()
                .map(|(i, s)| row(&format!("p{i}"), false, s))
                .collect(),
        }
    }

    const STATES: [IconState; 4] = [
        IconState::Disconnected,
        IconState::Connecting,
        IconState::Connected,
        IconState::Error,
    ];
    const APPEARANCES: [Appearance; 2] = [Appearance::Dark, Appearance::Light];

    #[test]
    fn nothing_configured_or_all_stopped_is_disconnected() {
        assert_eq!(Snapshot::default().icon_state(), IconState::Disconnected);
        let all_stopped = snapshot_of(vec![ProxyStatus::Stopped, ProxyStatus::Stopped]);
        assert_eq!(all_stopped.icon_state(), IconState::Disconnected);
    }

    #[test]
    fn one_running_profile_makes_the_icon_connected() {
        let s = snapshot_of(vec![
            ProxyStatus::Stopped,
            ProxyStatus::Running,
            ProxyStatus::Stopped,
        ]);
        assert_eq!(s.icon_state(), IconState::Connected);
    }

    #[test]
    fn starting_shows_connecting_not_disconnected() {
        let s = snapshot_of(vec![ProxyStatus::Starting]);
        assert_eq!(s.icon_state(), IconState::Connecting);
    }

    #[test]
    fn failed_never_reads_as_connected() {
        assert_eq!(snapshot_of(vec![failed()]).icon_state(), IconState::Error);
    }

    #[test]
    fn precedence_is_failed_then_starting_then_running() {
        let healthy_and_failed = snapshot_of(vec![ProxyStatus::Running, failed()]);
        assert_eq!(healthy_and_failed.icon_state(), IconState::Error);
        let failed_and_starting = snapshot_of(vec![ProxyStatus::Starting, failed()]);
        assert_eq!(failed_and_starting.icon_state(), IconState::Error);
        let starting_and_running = snapshot_of(vec![ProxyStatus::Running, ProxyStatus::Starting]);
        assert_eq!(starting_and_running.icon_state(), IconState::Connecting);
    }

    #[test]
    fn the_label_always_shows_ports_and_marks_danger() {
        let dev = row("dev", false, ProxyStatus::Stopped);
        assert_eq!(dev.label(), "dev  (15432/15433)");
        let prd = row("prd", true, ProxyStatus::Stopped);
        assert_eq!(prd.label(), "⚠ prd  (15432/15433)");
    }

    #[test]
    fn transient_states_are_suffixed_and_settled_ones_are_not() {
        assert!(row("dev", false, ProxyStatus::Starting)
            .label()
            .ends_with(" — starting…"));
        assert!(row("dev", false, failed()).label().ends_with(" — failed"));
        assert!(!row("dev", false, ProxyStatus::Running)
            .label()
            .contains('—'));
    }

    #[test]
    fn a_single_port_renders_without_a_slash() {
        let mut r = row("dev", false, ProxyStatus::Stopped);
        r.ports = vec![15432];
        assert_eq!(r.label(), "dev  (15432)");
    }

    #[test]
    fn checked_covers_starting_as_well_as_running() {
        assert!(row("a", false, ProxyStatus::Running).checked());
        assert!(row("a", false, ProxyStatus::Starting).checked());
        assert!(!row("a", false, ProxyStatus::Stopped).checked());
        assert!(!row("a", false, failed()).checked());
    }

    #[test]
    fn the_status_line_says_nothing_running_when_idle() {
        let s = snapshot_of(vec![ProxyStatus::Stopped]);
        assert_eq!(s.status_line(), "Nothing running");
    }

    #[test]
    fn the_status_line_names_active_profiles_and_ports() {
        let s = snapshot_of(vec![ProxyStatus::Running, ProxyStatus::Stopped]);
        assert_eq!(s.status_line(), "p0 — running on 15432/15433");
        let s = snapshot_of(vec![ProxyStatus::Starting, failed()]);
        assert_eq!(
            s.status_line(),
            "p0 — starting… on 15432/15433, p1 — failed on 15432/15433"
        );
    }

    /// Width and height from a PNG's IHDR, without a decoder.
    fn png_dimensions(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
        let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
        (w, h)
    }

    #[test]
    fn every_asset_is_exactly_the_tray_size() {
        for state in STATES {
            for appearance in APPEARANCES {
                for blink in [Blink::On, Blink::Off] {
                    let bytes = icon_bytes(state, appearance, blink);
                    assert_eq!(
                        png_dimensions(bytes),
                        (TRAY_ICON_PX, TRAY_ICON_PX),
                        "{} / {} / {blink:?}",
                        state.as_str(),
                        appearance.as_str()
                    );
                }
            }
        }
    }

    #[test]
    fn no_two_state_appearance_pairs_share_an_asset() {
        // A copy-paste slip pointing two states at one image would be invisible:
        // the tray still shows *an* icon.
        let mut seen: Vec<(&[u8], IconState, Appearance)> = Vec::new();
        for state in STATES {
            for appearance in APPEARANCES {
                let bytes = icon_bytes(state, appearance, Blink::On);
                for (other, os, oa) in &seen {
                    assert!(
                        bytes != *other,
                        "{}/{} shares an asset with {}/{}",
                        state.as_str(),
                        appearance.as_str(),
                        os.as_str(),
                        oa.as_str()
                    );
                }
                seen.push((bytes, state, appearance));
            }
        }
    }

    #[test]
    fn the_two_appearances_of_a_state_differ() {
        for state in STATES {
            assert_ne!(
                icon_bytes(state, Appearance::Dark, Blink::On),
                icon_bytes(state, Appearance::Light, Blink::On),
                "{} has one asset for both appearances",
                state.as_str()
            );
        }
    }

    #[test]
    fn only_connecting_changes_asset_between_blink_phases() {
        for state in STATES {
            for appearance in APPEARANCES {
                let on = icon_bytes(state, appearance, Blink::On);
                let off = icon_bytes(state, appearance, Blink::Off);
                if state == IconState::Connecting {
                    assert_ne!(on, off);
                } else {
                    assert_eq!(on, off, "{} must not blink", state.as_str());
                }
            }
        }
    }

    #[test]
    fn the_dark_half_of_the_blink_is_the_disconnected_glyph() {
        for appearance in APPEARANCES {
            assert_eq!(
                icon_bytes(IconState::Connecting, appearance, Blink::Off),
                icon_bytes(IconState::Disconnected, appearance, Blink::On),
            );
        }
    }

    #[test]
    fn blink_flips_between_exactly_two_phases() {
        assert_eq!(Blink::On.flipped(), Blink::Off);
        assert_eq!(Blink::Off.flipped(), Blink::On);
        assert_eq!(Blink::On.flipped().flipped(), Blink::On);
    }

    #[test]
    fn the_blink_interval_evenly_subdivides_the_poll_interval() {
        assert_eq!(POLL_INTERVAL.as_millis() % BLINK_INTERVAL.as_millis(), 0);
    }

    #[test]
    fn profile_ids_are_ordered_and_ignore_everything_else() {
        let a = snapshot_of(vec![ProxyStatus::Stopped, ProxyStatus::Running]);
        let mut b = a.clone();
        b.rows[0].name = "renamed".into();
        b.rows[1].status = ProxyStatus::Stopped;
        assert_eq!(profile_ids(&a), profile_ids(&b));
        b.rows.reverse();
        assert_ne!(profile_ids(&a), profile_ids(&b));
    }

    #[test]
    fn state_and_appearance_names_are_distinct_for_the_audit_log() {
        let names: Vec<_> = STATES.iter().map(|s| s.as_str()).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), names.len());
        assert_ne!(Appearance::Dark.as_str(), Appearance::Light.as_str());
    }
}
