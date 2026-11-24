//! The app that gets the dictation: its name for the log, and whether the
//! system lets TDT type into it.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
pub use mac::Target;
#[cfg(windows)]
pub use win::Target;
