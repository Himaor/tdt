//! The overlay window: the pill and the panel in one borderless window.
//! The Win32 side is `win`, the AppKit side is `mac`; both give the same calls.

/// The overlay window always has the panel's size. The pill is drawn at its
/// bottom center, and the window region (Windows) or the click-through
/// switch (macOS) limits input to the pill while the panel is closed, so the
/// rest never blocks clicks. The window never resizes on screen, so the
/// system never shows a stretched frame.
pub const PANEL_WIDTH: i32 = 400;
pub const PANEL_HEIGHT: i32 = 540;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
pub use mac::*;
#[cfg(windows)]
pub use win::*;
