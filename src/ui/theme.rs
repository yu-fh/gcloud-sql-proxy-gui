//! Colours and the few element shapes every view shares.
//!
//! Colours are light/dark pairs ([`AdaptiveColor`]), so the window follows the
//! system appearance with no per-view code.

pub use nsh_core::element::AdaptiveColor;
use nsh_core::prelude::*;

pub const WINDOW: AdaptiveColor = AdaptiveColor::rgb(0xffffff, 0x1e1e1e);
pub const SIDEBAR: AdaptiveColor = AdaptiveColor::rgb(0xf0f0f2, 0x2a2a2c);
pub const GROUP: AdaptiveColor = AdaptiveColor::rgb(0xf5f5f7, 0x2c2c2e);
pub const FIELD: AdaptiveColor = AdaptiveColor::rgb(0xffffff, 0x3a3a3c);
pub const SELECTED: AdaptiveColor = AdaptiveColor::rgb(0xd6e6ff, 0x0a4a9c);
pub const BORDER: AdaptiveColor = AdaptiveColor::rgb(0xd2d2d7, 0x48484a);
pub const TEXT: AdaptiveColor = AdaptiveColor::rgb(0x1d1d1f, 0xf5f5f7);
pub const MUTED: AdaptiveColor = AdaptiveColor::rgb(0x6e6e73, 0x98989d);
pub const ACCENT: AdaptiveColor = AdaptiveColor::rgb(0x0a60ff, 0x409cff);
pub const ERROR: AdaptiveColor = AdaptiveColor::rgb(0xd70015, 0xff6961);
pub const WARN: AdaptiveColor = AdaptiveColor::rgb(0x9a6700, 0xffd60a);
pub const ERROR_BG: AdaptiveColor = AdaptiveColor::rgb(0xffe5e8, 0x4a1d22);

/// A small bordered push button. Disabled buttons are dimmed and ignore presses.
pub fn button(
    label: impl IntoReactive<SharedString> + 'static,
    enabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let live = enabled.clone();
    view()
        .px_3()
        .py_1()
        .rounded(px(6.0))
        .border_1()
        .border_color(BORDER)
        .bg(FIELD)
        .opacity(move || if enabled() { 1.0_f32 } else { 0.4_f32 })
        .on_press(move || {
            if live() {
                on_press();
            }
        })
        .child(text(label).text_size(px(13.0)).text_color(TEXT))
}

/// A grouped list: the rounded panel System Settings rows sit in.
pub fn group() -> View {
    view()
        .rounded(px(10.0))
        .bg(GROUP)
        .border_1()
        .border_color(BORDER)
        .mb(px(12.0))
}

/// One row of a group: label left, control right.
pub fn row() -> View {
    view()
        .flex_row()
        .items_center()
        .gap_3()
        .px(px(14.0))
        .min_h(px(40.0))
}
