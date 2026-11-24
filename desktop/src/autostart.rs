//! Start TDT when the user signs in: a Task Scheduler task on Windows
//! (`win`), a LaunchAgent on macOS (`mac`).

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
pub use mac::{is_enabled, refresh_if_enabled, set_enabled};
#[cfg(windows)]
pub use win::{is_enabled, refresh_if_enabled, set_enabled};
