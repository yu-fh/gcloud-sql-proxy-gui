//! The settings window: Profiles and Logs.
//!
//! Not rebuilt yet. Until the nsh views land, opening it says so rather than
//! doing nothing, because a menu item that silently does nothing reads as a
//! broken app.

use crate::dialogs::report_error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Profiles,
    Logs,
}

pub fn open(section: Section) {
    let name = match section {
        Section::Profiles => "Profiles",
        Section::Logs => "Logs",
    };
    report_error(
        &format!("{name} is not available yet"),
        "The settings window is being rebuilt. Edit profiles.json directly for now.",
    );
}
