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
pub const HOVER: AdaptiveColor = AdaptiveColor::rgb(0xf2f2f4, 0x454548);
pub const PRESSED: AdaptiveColor = AdaptiveColor::rgb(0xe2e2e7, 0x58585c);
pub const ROW_HOVER: AdaptiveColor = AdaptiveColor::rgb(0xe6e6ea, 0x363638);
pub const ACCENT_HOVER: AdaptiveColor = AdaptiveColor::rgb(0x2a73ff, 0x5aa8ff);
pub const ACCENT_PRESSED: AdaptiveColor = AdaptiveColor::rgb(0x0849c4, 0x2f86e6);
pub const ON_ACCENT: AdaptiveColor = AdaptiveColor::rgb(0xffffff, 0x0b0b0c);
pub const OK: AdaptiveColor = AdaptiveColor::rgb(0x1a7f37, 0x30d158);
pub const ERROR_BG: AdaptiveColor = AdaptiveColor::rgb(0xffe5e8, 0x4a1d22);

/// A small bordered push button. Hover, press and keyboard focus each show;
/// a disabled one is dimmed, loses the pointer hand and ignores presses.
pub fn button(
    label: impl IntoReactive<SharedString> + 'static,
    enabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let live = enabled.clone();
    let off = enabled.clone();
    view()
        .px_3()
        .py_1()
        .rounded(px(6.0))
        .border_1()
        .border_color(BORDER)
        .bg(FIELD)
        .cursor_pointer()
        .transition()
        .focusable(true)
        .hover(|s| s.bg(HOVER))
        .active(|s| s.bg(PRESSED))
        .focus(|s| s.border_color(ACCENT))
        .disabled(move || !off())
        .opacity(move || if enabled() { 1.0_f32 } else { 0.4_f32 })
        .on_press(move || {
            if live() {
                on_press();
            }
        })
        .child(text(label).text_size(px(13.0)).text_color(TEXT))
}

/// The window's one default action: filled with the accent colour.
pub fn primary_button(
    label: impl IntoReactive<SharedString> + 'static,
    enabled: impl Fn() -> bool + Clone + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let live = enabled.clone();
    let off = enabled.clone();
    view()
        .px_3()
        .py_1()
        .rounded(px(6.0))
        .border_1()
        .border_color(ACCENT)
        .bg(ACCENT)
        .cursor_pointer()
        .transition()
        .focusable(true)
        .hover(|s| s.bg(ACCENT_HOVER).border_color(ACCENT_HOVER))
        .active(|s| s.bg(ACCENT_PRESSED).border_color(ACCENT_PRESSED))
        .disabled(move || !off())
        .opacity(move || if enabled() { 1.0_f32 } else { 0.4_f32 })
        .on_press(move || {
            if live() {
                on_press();
            }
        })
        .child(text(label).text_size(px(13.0)).text_color(ON_ACCENT))
}

/// A pressable list row or chip: `base` at rest, a wash under the pointer,
/// `SELECTED` while `selected()`, and a darker wash while pressed.
pub fn selectable(base: AdaptiveColor, selected: impl Fn() -> bool + 'static) -> View {
    let hovered = signal(false);
    view()
        .cursor_pointer()
        .transition()
        .on_hover_in(move || hovered.set(true))
        .on_hover_out(move || hovered.set(false))
        .bg(move || {
            if selected() {
                SELECTED
            } else if hovered.get() {
                ROW_HOVER
            } else {
                base
            }
        })
        .active(|s| s.bg(PRESSED))
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
