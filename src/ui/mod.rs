//! The settings window's views, written as nsh components.
//!
//! nsh runs a component on its own engine thread and its signals are not
//! shareable, while the service layer is async on the tokio runtime. [`bridge`]
//! is the seam: a task on the runtime writes its result into a mutex-guarded
//! struct and bumps a counter; a component polls the counter with
//! `set_interval` and copies what changed into its signals.

mod bridge;
mod logs;
mod profiles;
mod root;
mod theme;

pub use bridge::{is_dirty, reset_session};
pub use root::{request_section, Settings};
