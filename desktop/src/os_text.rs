//! User-facing words that name the operating system or its keys.

#[cfg(windows)]
mod imp {
    /// The keys that paste the clipboard.
    pub const PASTE_KEYS: &str = "Ctrl+V";
    /// Where the user allows the microphone.
    pub const MICROPHONE_ACCESS: &str = "Windows microphone access";
    /// The label of the startup switch in Settings.
    pub const START_AT_SIGN_IN: &str = "Start with Windows";
    /// The settings that control startup apps.
    pub const STARTUP_PERMISSIONS: &str = "Windows startup permissions";
}

#[cfg(target_os = "macos")]
mod imp {
    pub const PASTE_KEYS: &str = "Cmd+V";
    pub const MICROPHONE_ACCESS: &str =
        "microphone access in System Settings > Privacy & Security > Microphone";
    pub const START_AT_SIGN_IN: &str = "Open at login";
    pub const STARTUP_PERMISSIONS: &str = "Login Items in System Settings > General";
}

pub use imp::*;
