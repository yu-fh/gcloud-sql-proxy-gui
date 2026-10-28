//! Library target exposing the pure-Rust core modules so integration tests
//! (and the AppKit host) can import them.

pub mod app_state;
pub mod autostart;
pub mod core;
pub mod dialogs;
pub mod host;
pub mod service;
pub mod tray;
pub mod tray_model;
pub mod window;
