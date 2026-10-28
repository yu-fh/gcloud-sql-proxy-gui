//! The Logs section: the audit trail, filtered, plus where its file lives.
//!
//! A snapshot, not a stream: it reloads on entry, on a filter change and on
//! Refresh. A log pane that silently stops updating would be worse than one you
//! refresh.

use nsh_core::prelude::*;

use super::bridge;
use super::theme::{self, button};
use crate::service::{LogRecordView, LogsView, ProfileView};

/// How many records to render. The backend keeps up to 2000; a view of 2000
/// rows is slow to build, so the newest slice is shown and the footer says
/// where the whole trail is.
const RENDER_LIMIT: usize = 500;

/// Local wall-clock `HH:MM:SS` for epoch milliseconds. The audit file carries
/// UTC; this is the timezone the user is actually in. The UTC stamp stays in
/// the file for matching a row to a line.
pub fn local_time(at_ms: u64) -> String {
    let secs = (at_ms / 1000) as libc::time_t;
    // SAFETY: `localtime_r` writes only to the `tm` we hand it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if !ok {
        return String::new();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

fn category_label(category: &str) -> &str {
    match category {
        "system" => "sys",
        "action" => "act",
        "event" => "evt",
        other => other,
    }
}

fn severity_color(severity: &str) -> theme::AdaptiveColor {
    match severity {
        "error" => theme::ERROR,
        "warn" => theme::WARN,
        _ => theme::MUTED,
    }
}

/// The summary line: count, plus a note when the file is not being written.
pub fn summary(view: &LogsView) -> String {
    let n = view.records.len();
    let mut s = format!("{n} record{}", if n == 1 { "" } else { "s" });
    if view.write_failures > 0 {
        s.push_str(&format!(
            " — {} write failure{}; the file is not being updated",
            view.write_failures,
            if view.write_failures == 1 { "" } else { "s" }
        ));
    }
    s
}

/// The newest `RENDER_LIMIT` records, oldest first.
pub fn visible(records: &[LogRecordView]) -> Vec<LogRecordView> {
    let start = records.len().saturating_sub(RENDER_LIMIT);
    records[start..].to_vec()
}

fn chip(
    label: impl IntoReactive<SharedString> + 'static,
    selected: impl Fn() -> bool + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    let selected = std::rc::Rc::new(selected);
    let for_bg = selected.clone();
    view()
        .px_3()
        .py_1()
        .rounded(px(12.0))
        .bg(move || {
            if for_bg() {
                theme::SELECTED
            } else {
                theme::GROUP
            }
        })
        .on_press(on_press)
        .child(text(label).text_size(px(12.0)).text_color(theme::TEXT))
}

fn log_row(record: LogRecordView) -> impl IntoElement {
    let source = record
        .profile_id
        .clone()
        .unwrap_or_else(|| category_label(&record.category).to_string());
    view()
        .flex_row()
        .gap_2()
        .px(px(10.0))
        .py(px(2.0))
        .child(
            text(local_time(record.at))
                .text_size(px(11.0))
                .text_color(theme::MUTED)
                .w(px(62.0)),
        )
        // The severity is text, not only a colour: colour alone fails for
        // anyone who cannot tell the two reds apart.
        .child(
            text(record.severity.clone())
                .text_size(px(11.0))
                .text_color(severity_color(&record.severity))
                .w(px(38.0)),
        )
        .child(
            text(source)
                .text_size(px(11.0))
                .text_color(theme::MUTED)
                .w(px(56.0)),
        )
        .child(
            text(record.message.clone())
                .text_size(px(11.0))
                .text_color(theme::TEXT)
                .flex_1(),
        )
}

pub fn logs_section(
    profiles: Signal<Vec<ProfileView>>,
    logs: Signal<Option<LogsView>>,
) -> impl IntoElement {
    let profile = signal(String::new());
    let severity = signal(String::new());

    let reload = move || {
        let p = profile.get_untracked();
        let s = severity.get_untracked();
        bridge::load_logs((!p.is_empty()).then_some(p), (!s.is_empty()).then_some(s));
    };
    reload();

    let severity_chip = move |label: &'static str, value: &'static str| {
        chip(
            label,
            move || severity.get() == value,
            move || {
                severity.set(value.to_string());
                reload();
            },
        )
    };

    let profile_chips = each(
        move || {
            let mut chips = vec![(String::new(), "All Profiles".to_string())];
            chips.extend(
                profiles
                    .get()
                    .iter()
                    .map(|p| (p.profile.id.clone(), p.profile.name.clone())),
            );
            chips
        },
        |c: &(String, String)| c.0.clone(),
        move |item, _| {
            let id = item.with_untracked(|c| c.0.clone());
            let id2 = id.clone();
            chip(
                move || item.with(|c| c.1.clone()),
                move || profile.get() == id,
                move || {
                    profile.set(id2.clone());
                    reload();
                },
            )
        },
    );

    let rows = each(
        move || {
            logs.get()
                .map(|l| visible(&l.records))
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .collect::<Vec<_>>()
        },
        |item: &(usize, LogRecordView)| item.0,
        |item, _| log_row(item.with_untracked(|i| i.1.clone())),
    );

    view()
        .flex_1()
        .p(px(16.0))
        .gap_2()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(text("Profile").text_size(px(12.0)).text_color(theme::MUTED))
                .child(view().flex_row().gap_1().child(profile_chips))
                .child(text("Level").text_size(px(12.0)).text_color(theme::MUTED))
                .child(severity_chip("All", ""))
                .child(severity_chip("Warnings & Errors", "warn"))
                .child(severity_chip("Errors Only", "error"))
                .child(view().flex_1())
                .child(button("Refresh", || true, reload)),
        )
        .child(
            scroll().flex_1().rounded(px(8.0)).bg(theme::GROUP).child(
                view()
                    .py(px(6.0))
                    .child(when(
                        move || logs.get().is_none_or(|l| l.records.is_empty()),
                        || {
                            text("No log records yet.")
                                .text_size(px(12.0))
                                .text_color(theme::MUTED)
                                .p(px(10.0))
                        },
                    ))
                    .child(rows),
            ),
        )
        .child(
            view()
                .gap_1()
                .child(
                    text(move || logs.get().map(|l| summary(&l)).unwrap_or_default())
                        .text_size(px(11.0))
                        .text_color(theme::MUTED),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            text("Log file")
                                .text_size(px(11.0))
                                .text_color(theme::MUTED),
                        )
                        .child(
                            text(move || {
                                logs.get()
                                    .and_then(|l| l.file_path)
                                    .unwrap_or_else(|| "Not being written to disk".to_string())
                            })
                            .text_size(px(11.0))
                            .text_color(theme::TEXT)
                            .flex_1(),
                        )
                        .child(button(
                            "Show in Finder",
                            move || logs.get().is_some_and(|l| l.file_path.is_some()),
                            bridge::reveal_log,
                        )),
                ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(n: u64) -> LogRecordView {
        LogRecordView {
            at: n,
            at_display: String::new(),
            severity: "info".into(),
            category: "event".into(),
            profile_id: None,
            message: format!("m{n}"),
        }
    }

    #[test]
    fn only_the_newest_slice_is_rendered() {
        let all: Vec<_> = (0..(RENDER_LIMIT as u64 + 20)).map(record).collect();
        let shown = visible(&all);
        assert_eq!(shown.len(), RENDER_LIMIT);
        assert_eq!(shown.last().unwrap().message, all.last().unwrap().message);
        assert_eq!(shown[0].message, "m20");
    }

    #[test]
    fn a_short_trail_is_shown_whole() {
        assert_eq!(visible(&[record(1), record(2)]).len(), 2);
        assert!(visible(&[]).is_empty());
    }

    #[test]
    fn the_summary_says_when_the_file_is_not_being_written() {
        let mut view = LogsView {
            records: vec![record(1)],
            file_path: None,
            write_failures: 0,
        };
        assert_eq!(summary(&view), "1 record");
        view.write_failures = 2;
        assert!(summary(&view).contains("2 write failures"));
    }

    #[test]
    fn local_time_is_hh_mm_ss() {
        let t = local_time(1_700_000_000_000);
        assert_eq!(t.len(), 8);
        assert_eq!(t.matches(':').count(), 2);
    }
}
