//! The environment list, written as an nsh component. It runs on nsh's engine
//! thread; `set_interval` re-reads the backend snapshot while it is mounted.

use std::time::Duration;

use nsh_core::prelude::*;

use crate::backend::{self, Phase, Row};

fn label(row: &Row) -> String {
    match &row.phase {
        Phase::Stopped => "Disconnected".into(),
        Phase::Starting => "Connecting…".into(),
        Phase::Running => "Connected".into(),
        Phase::Failed(why) => format!("Failed: {why}"),
    }
}

#[component]
pub fn Environments() -> impl IntoElement {
    let rows = signal(backend::snapshot());
    set_interval(Duration::from_millis(250), move || rows.set(backend::snapshot()));

    view().flex_1().p_4().gap_2().child(each(
        move || rows.get(),
        |row: &Row| row.id.clone(),
        |row, _| {
            let id = row.with_untracked(|r| r.id.clone());
            view()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    view()
                        .flex_1()
                        .child(text(move || row.with(|r| r.name.clone())).text_size(px(15.0)))
                        .child(
                            text(move || row.with(|r| format!("{} · ports {}", label(r), r.ports)))
                                .text_size(px(12.0)),
                        ),
                )
                .child(
                    view()
                        .px_3()
                        .py_1()
                        .rounded(px(6.0))
                        .border_1()
                        .on_press(move || backend::toggle(id.clone()))
                        .child(text(move || {
                            row.with(|r| match r.phase {
                                Phase::Running | Phase::Starting => "Disconnect",
                                _ => "Connect",
                            })
                        })),
                )
        },
    ))
}
