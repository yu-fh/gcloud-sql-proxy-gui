//! The Profiles section: the environment list on the left, the selected
//! environment's settings on the right, and the add / import / delete / save
//! actions.
//!
//! # Editing state
//!
//! The editor holds its own copy of the profiles (`Model::edit`). Field handlers
//! mutate it in place, and Save sends the whole copy to the service. The detail
//! pane builds its inputs once from that copy and is keyed by (id, epoch), so a
//! keystroke never rebuilds the field being typed in, and a reload (which bumps
//! the epoch) does rebuild it from fresh values.

use nsh_core::element::input;
use nsh_core::prelude::*;

use super::bridge;
use super::root::Model;
use super::theme::{self, button, group, row};
use crate::core::profile::{InstanceRole, Profile};
use crate::host::runtime;
use crate::service::ProfileView;

pub fn status_label(status: &str) -> &'static str {
    match status {
        "stopped" => "Stopped",
        "starting" => "Starting…",
        "running" => "Running",
        "failed" => "Failed",
        _ => "—",
    }
}

pub fn role_label(role: &InstanceRole) -> &'static str {
    match role {
        InstanceRole::Primary => "Primary",
        InstanceRole::Replica => "Read Replica",
    }
}

/// `gcloud` command that lists a project's instances with their connection
/// names, for a user who has to type them in.
pub fn lookup_command(project: &str) -> String {
    let project = if project.is_empty() {
        "<project>"
    } else {
        project
    };
    format!("gcloud sql instances list --project={project} --format='value(name,connectionName)'")
}

/// An empty optional field is `None`, not `Some("")`.
pub fn optional(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

/// Edit the profile `id` in the editor's copy and mark the editor dirty.
fn edit(m: Model, id: &str, f: impl FnOnce(&mut Profile)) {
    m.edit.update(|profiles| {
        if let Some(p) = profiles.iter_mut().find(|p| p.id == id) {
            f(p);
        }
    });
    m.dirty.set(true);
}

/// A port as typed: a number from 1 to 65535, else nothing.
pub fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

fn field_row(
    label: &'static str,
    value: String,
    placeholder: &'static str,
    on_change: impl Fn(String) + 'static,
) -> impl IntoElement {
    field_row_checked(label, value, placeholder, on_change, || false)
}

/// A field row that outlines itself in red while `invalid` holds.
fn field_row_checked(
    label: &'static str,
    value: String,
    placeholder: &'static str,
    on_change: impl Fn(String) + 'static,
    invalid: impl Fn() -> bool + 'static,
) -> impl IntoElement {
    row()
        .child(
            text(label)
                .text_size(px(13.0))
                .text_color(theme::TEXT)
                .w(px(130.0)),
        )
        .child(
            input()
                .default_value(value)
                .placeholder(placeholder)
                .on_change(move |s: &str| on_change(s.to_string()))
                .flex_1()
                .px(px(8.0))
                .h(px(28.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(move || {
                    if invalid() {
                        theme::ERROR
                    } else {
                        theme::FIELD
                    }
                })
                .bg(theme::FIELD)
                .text_size(px(13.0))
                .text_color(theme::TEXT),
        )
}

fn static_row(
    label: &'static str,
    value: impl IntoReactive<SharedString> + 'static,
) -> impl IntoElement {
    row()
        .child(
            text(label)
                .text_size(px(13.0))
                .text_color(theme::TEXT)
                .w(px(130.0)),
        )
        .child(
            text(value)
                .text_size(px(13.0))
                .text_color(theme::MUTED)
                .flex_1(),
        )
}

fn switch_row(
    label: &'static str,
    description: &'static str,
    initial: bool,
    on_change: impl Fn(bool) + 'static,
) -> impl IntoElement {
    let on = signal(initial);
    row()
        .child(
            view()
                .flex_1()
                .child(text(label).text_size(px(13.0)).text_color(theme::TEXT))
                .child(
                    text(description)
                        .text_size(px(11.0))
                        .text_color(theme::MUTED),
                ),
        )
        .child(
            view()
                .w(px(38.0))
                .h(px(22.0))
                .rounded(px(11.0))
                .cursor_pointer()
                .transition()
                .bg(move || {
                    if on.get() {
                        theme::ACCENT
                    } else {
                        theme::BORDER
                    }
                })
                .on_press(move || {
                    let next = !on.get_untracked();
                    on.set(next);
                    on_change(next);
                })
                .child(
                    view()
                        .w(px(18.0))
                        .h(px(18.0))
                        .rounded(px(9.0))
                        .bg(theme::FIELD)
                        .mt(px(2.0))
                        .ml(move || px(if on.get() { 18.0 } else { 2.0 })),
                ),
        )
}

fn command_row(command: String) -> impl IntoElement {
    let for_copy = command.clone();
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            text(command)
                .text_size(px(11.0))
                .text_color(theme::TEXT)
                .flex_1(),
        )
        .child(button(
            "Copy",
            || true,
            move || bridge::copy(for_copy.clone()),
        ))
}

fn notice(
    title: impl IntoReactive<SharedString> + 'static,
    body: &'static str,
    error: bool,
) -> View {
    view()
        .rounded(px(8.0))
        .p(px(12.0))
        .gap_1()
        .mb(px(12.0))
        .bg(if error { theme::ERROR_BG } else { theme::GROUP })
        .child(text(title).text_size(px(13.0)).text_color(if error {
            theme::ERROR
        } else {
            theme::TEXT
        }))
        .child(text(body).text_size(px(12.0)).text_color(theme::MUTED))
}

/// The detail pane for one profile, built from the editor's current copy.
fn detail(m: Model, id: String) -> impl IntoElement {
    let Some(profile) = m.edit.get_untracked().into_iter().find(|p| p.id == id) else {
        return view().child(text("Select an environment.").text_color(theme::MUTED));
    };

    let id_for = |id: &str| id.to_string();
    let live = {
        let id = id.clone();
        move || m.live.get().into_iter().find(|v| v.profile.id == id)
    };

    let mut fields = group()
        .child(field_row("Name", profile.name.clone(), "", {
            let id = id_for(&id);
            move |v| edit(m, &id, |p| p.name = v)
        }))
        .child(field_row("Project", profile.project.clone(), "", {
            let id = id_for(&id);
            move |v| edit(m, &id, |p| p.project = v)
        }));
    for (index, instance) in profile.instances.iter().enumerate() {
        fields = fields
            .child(field_row(
                role_label(&instance.role),
                instance.connection_name.clone(),
                "project:region:instance",
                {
                    let id = id_for(&id);
                    move |v| edit(m, &id, |p| p.instances[index].connection_name = v)
                },
            ))
            .child(field_row_checked(
                "Port",
                instance.port.to_string(),
                "",
                {
                    let id = id_for(&id);
                    let key = format!("{id}:{index}");
                    // Only a valid port reaches the editor's copy. Anything else
                    // is flagged instead, which turns the field red and holds
                    // Done back: applying the last keystroke that happened to
                    // parse would save a port the user never typed (`1543x`
                    // would save 1543).
                    move |v| match parse_port(&v) {
                        Some(port) => {
                            m.bad.update(|bad| bad.retain(|k| k != &key));
                            edit(m, &id, |p| p.instances[index].port = port);
                        }
                        None => {
                            m.bad.update(|bad| {
                                if !bad.contains(&key) {
                                    bad.push(key.clone());
                                }
                            });
                            m.dirty.set(true);
                        }
                    }
                },
                {
                    let key = format!("{id}:{index}");
                    move || m.bad.get().contains(&key)
                },
            ));
    }

    let options = group()
        .child(switch_row(
            "Production",
            "Confirm before starting this environment.",
            profile.danger,
            {
                let id = id_for(&id);
                move |on| edit(m, &id, |p| p.danger = on)
            },
        ))
        .child(field_row(
            "VPN Probe Host",
            profile.vpn_probe_host.clone().unwrap_or_default(),
            "Optional, e.g. pg.dev.private.example",
            {
                let id = id_for(&id);
                move |v| edit(m, &id, |p| p.vpn_probe_host = optional(&v))
            },
        ));

    let info = group()
        .child(static_row("Identifier", profile.id.clone()))
        .child(static_row("Region", profile.region.clone()))
        .child(static_row("Status", {
            let live = live.clone();
            move || {
                live()
                    .map(|v| status_label(&v.status).to_string())
                    .unwrap_or_default()
            }
        }));

    let missing_names = {
        let id = id.clone();
        move || {
            m.edit
                .get()
                .iter()
                .find(|p| p.id == id)
                .is_some_and(|p| p.instances.iter().any(|i| i.connection_name.is_empty()))
        }
    };
    let project_for_hint = {
        let id = id.clone();
        move || {
            m.edit
                .get()
                .iter()
                .find(|p| p.id == id)
                .map(|p| lookup_command(&p.project))
                .unwrap_or_default()
        }
    };

    view()
        .gap_1()
        .child(when(
            move || !m.ignored.get().is_empty(),
            move || {
                notice(
                    "Imported, but some flags were not carried across.",
                    "This app has no setting for these, so the profile below does not do what they did:",
                    false,
                )
                .child(text(move || m.ignored.get().join("\n")).text_size(px(11.0)).text_color(theme::TEXT))
            },
        ))
        .child(when(missing_names, move || {
            notice(
                "This environment has no connection names yet, so it cannot start.",
                "Type each one into the fields below, in the form project:region:instance. To look them up:",
                false,
            )
            .child(command_row(project_for_hint()))
        }))
        .child(when(
            {
                let live = live.clone();
                move || live().is_some_and(|v| v.detail.is_some())
            },
            {
                let live = live.clone();
                move || {
                    let v = live();
                    let detail = v.as_ref().and_then(|v| v.detail.clone()).unwrap_or_default();
                    let fix = v.and_then(|v| v.fix_command);
                    let mut n = view()
                        .rounded(px(8.0))
                        .p(px(12.0))
                        .gap_1()
                        .mb(px(12.0))
                        .bg(theme::ERROR_BG)
                        .child(text("Last start failed").text_size(px(13.0)).text_color(theme::ERROR))
                        .child(text(detail).text_size(px(12.0)).text_color(theme::TEXT));
                    if let Some(fix) = fix {
                        n = n.child(command_row(fix));
                    }
                    n
                }
            },
        ))
        .child(fields)
        .child(options)
        .child(info)
}

fn import_panel(m: Model) -> impl IntoElement {
    let command = signal(String::new());
    group()
        .p(px(14.0))
        .gap_2()
        .child(text("Paste a cloud-sql-proxy command.").text_size(px(13.0)).text_color(theme::TEXT))
        .child(
            text(
                "The instances, ports and flags are read from the command; you supply the name. \
                 Anything this app cannot represent is listed after the import rather than silently dropped.",
            )
            .text_size(px(12.0))
            .text_color(theme::MUTED),
        )
        .child(
            input()
                .multiline(true)
                .placeholder("cloud-sql-proxy --auto-iam-authn --private-ip \\\n  \"my-project:us-central1:my-instance?port=15432\"")
                .on_change(move |s: &str| command.set(s.to_string()))
                .h(px(120.0))
                .p(px(8.0))
                .rounded(px(6.0))
                .bg(theme::FIELD)
                .text_size(px(12.0))
                .text_color(theme::TEXT),
        )
        .child(
            view()
                .flex_row()
                .gap_2()
                .child(view().flex_1())
                .child(button("Cancel", || true, move || m.importing.set(false)))
                .child(button("Import", || true, move || {
                    let c = command.get_untracked();
                    if c.trim().is_empty() {
                        bridge::fail_input("Could not import", "Paste a cloud-sql-proxy command first.");
                        return;
                    }
                    // The panel stays open until the import lands: a failed one
                    // leaves the pasted text there to fix. A successful one
                    // closes it when the new profile loads.
                    let command = c.trim().to_string();
                    bridge::guarded(m.dirty.get_untracked(), "Importing", move || {
                        bridge::import(command)
                    });
                })),
        )
}

fn list_row(m: Model, id: String) -> impl IntoElement {
    let for_press = id.clone();
    let id_sel = id.clone();
    let name = {
        let id = id.clone();
        move || {
            m.edit
                .get()
                .iter()
                .find(|p| p.id == id)
                .map(|p| {
                    let name = if p.name.is_empty() {
                        p.id.clone()
                    } else {
                        p.name.clone()
                    };
                    if p.danger {
                        format!("⚠ {name}")
                    } else {
                        name
                    }
                })
                .unwrap_or_default()
        }
    };
    let status = {
        let id = id.clone();
        move || {
            m.live
                .get()
                .iter()
                .find(|v| v.profile.id == id)
                .map(|v| status_label(&v.status).to_string())
                .unwrap_or_default()
        }
    };
    theme::selectable(theme::WINDOW, move || {
        m.selected.get().as_deref() == Some(id_sel.as_str())
    })
    .px(px(10.0))
    .py(px(6.0))
    .rounded(px(6.0))
    .on_press(move || {
        m.importing.set(false);
        m.selected.set(Some(for_press.clone()));
    })
    .child(text(name).text_size(px(13.0)).text_color(theme::TEXT))
    .child(text(status).text_size(px(11.0)).text_color(theme::MUTED))
}

pub fn profiles_section(m: Model) -> impl IntoElement {
    let ids = move || m.edit.get().into_iter().map(|p| p.id).collect::<Vec<_>>();

    let add = move || {
        let dirty = m.dirty.get_untracked();
        runtime().spawn(async move {
            if !dirty || bridge::confirm_discard("Adding an environment").await {
                bridge::add();
            }
        });
    };
    let delete = move || {
        let Some(id) = m.selected.get_untracked() else {
            return;
        };
        let dirty = m.dirty.get_untracked();
        if let Some(view) = m
            .live
            .get_untracked()
            .into_iter()
            .find(|v| v.profile.id == id)
        {
            bridge::guarded(dirty, "Deleting an environment", move || {
                bridge::delete(view)
            });
        }
    };

    let sidebar = view()
        .w(px(240.0))
        .p(px(10.0))
        .gap_1()
        .child(
            text("Environments")
                .text_size(px(11.0))
                .text_color(theme::MUTED),
        )
        .child(each(
            ids,
            |id: &String| id.clone(),
            move |id, _| list_row(m, id.with_untracked(|i| i.clone())),
        ))
        .child(view().flex_1())
        .child(
            view()
                .flex_row()
                .gap_2()
                .child(button("+", move || !m.importing.get(), add))
                .child(button(
                    "−",
                    move || !m.importing.get() && m.selected.get().is_some(),
                    delete,
                ))
                .child(button(
                    "Import…",
                    move || !m.importing.get(),
                    move || {
                        m.ignored.set(Vec::new());
                        bridge::clear_ignored_flags();
                        m.importing.set(true);
                    },
                )),
        );

    let pane = scroll().flex_1().child(view().p(px(16.0)).child(
        when(move || m.importing.get(), move || import_panel(m)).otherwise(move || {
            each(
                move || {
                    m.selected
                        .get()
                        .map(|id| (id, m.epoch.get()))
                        .into_iter()
                        .collect::<Vec<_>>()
                },
                |k: &(String, u64)| k.clone(),
                move |k, _| detail(m, k.with_untracked(|k| k.0.clone())),
            )
        }),
    ));

    view()
        .flex_1()
        .child(view().flex_1().flex_row().child(sidebar).child(pane))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .px(px(16.0))
                .py(px(10.0))
                .child(
                    text(move || {
                        if !m.bad.get().is_empty() {
                            "A port must be a number from 1 to 65535.".to_string()
                        } else if m.dirty.get() {
                            "Unsaved changes".to_string()
                        } else {
                            m.note.get()
                        }
                    })
                    .text_size(px(12.0))
                    .text_color(theme::MUTED),
                )
                .child(view().flex_1())
                .child(button(
                    "Reload",
                    || true,
                    move || {
                        bridge::guarded(m.dirty.get_untracked(), "Reloading", || {
                            bridge::load_profiles(true, None)
                        })
                    },
                ))
                .child(theme::primary_button(
                    "Save",
                    move || m.dirty.get() && m.bad.get().is_empty(),
                    move || bridge::save(m.edit.get_untracked()),
                )),
        )
}

#[allow(dead_code)]
fn _assert_view(_: ProfileView) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_labels_cover_every_status() {
        for s in ["stopped", "starting", "running", "failed"] {
            assert_ne!(status_label(s), "—", "{s}");
        }
        assert_eq!(status_label("nonsense"), "—");
    }

    #[test]
    fn the_lookup_command_names_the_project_or_a_placeholder() {
        assert!(lookup_command("fh-dev").contains("--project=fh-dev "));
        assert!(lookup_command("").contains("--project=<project> "));
    }

    #[test]
    fn a_blank_optional_field_is_none() {
        assert_eq!(optional(""), None);
        assert_eq!(optional("   "), None);
        assert_eq!(optional("pg.dev"), Some("pg.dev".to_string()));
    }

    #[test]
    fn only_a_whole_port_number_is_accepted() {
        assert_eq!(parse_port("15432"), Some(15432));
        assert_eq!(parse_port(" 5432 "), Some(5432));
        assert_eq!(parse_port("65535"), Some(65535));
        // The cases a keystroke-by-keystroke parse used to turn into another port.
        assert_eq!(parse_port("70000"), None);
        assert_eq!(parse_port("1543x"), None);
        assert_eq!(parse_port(""), None);
        assert_eq!(parse_port("0"), None);
        assert_eq!(parse_port("-1"), None);
    }

    #[test]
    fn roles_have_display_names() {
        assert_eq!(role_label(&InstanceRole::Primary), "Primary");
        assert_eq!(role_label(&InstanceRole::Replica), "Read Replica");
    }
}
