use arboard::Clipboard;
use parking_lot::Mutex;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
use mac::{prepare_target, type_text};
#[cfg(windows)]
use win::{prepare_target, type_text};

/// Where the delivery time went, for the log.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeliverySteps {
    pub copy: Duration,
    pub focus: Duration,
    pub typing: Duration,
    /// When the text was in the app. The wait of the user ends here.
    pub typed_at: Option<Instant>,
}

impl std::fmt::Display for DeliverySteps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "copy {}, focus {}, typing {}",
            self.copy.as_millis(),
            self.focus.as_millis(),
            self.typing.as_millis()
        )
    }
}

#[derive(Debug, PartialEq)]
pub enum DeliveryOutcome {
    Copied,
    Pasted,
    /// The text is in the app, but the clipboard did not take it.
    PastedNotCopied(String),
    CopiedFallback(NotTyped),
}

/// Why the text is on the clipboard but not in the app.
#[derive(Debug, PartialEq)]
pub enum NotTyped {
    /// A key or mouse button went down while TDT was finishing.
    UserTyped,
    /// The app runs as administrator. Windows drops TDT's keys and reports no error.
    Elevated,
    /// The app is gone, or Windows did not give it the focus.
    NoFocus(&'static str),
    /// Windows refused some of the key events.
    Partial(String),
    /// macOS drops TDT's key events until the user allows TDT in Privacy &
    /// Security > Accessibility.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    NotAllowed,
}

impl NotTyped {
    /// What the pill tells the user.
    #[cfg(not(target_os = "macos"))]
    pub fn notice(&self) -> &'static str {
        match self {
            Self::UserTyped => {
                "You typed while TDT was finishing. The final text is on the clipboard."
            }
            Self::Elevated => {
                "This app runs as administrator, so Windows does not let TDT type into it. \
                 Press Ctrl+V to paste the text."
            }
            Self::NoFocus(_) => {
                "TDT could not go back to the app. Click in the app and press Ctrl+V to paste the text."
            }
            Self::Partial(_) => {
                "Copied. Could not insert all text. Check the destination before pasting with Ctrl+V."
            }
            Self::NotAllowed => "TDT is not allowed to type. Press Ctrl+V to paste the text.",
        }
    }

    /// What the pill tells the user.
    #[cfg(target_os = "macos")]
    pub fn notice(&self) -> &'static str {
        match self {
            Self::UserTyped => {
                "You typed while TDT was finishing. The final text is on the clipboard."
            }
            Self::Elevated => {
                "Secure input is on (a password field, or a terminal with Secure Keyboard Entry), \
                 so macOS does not let TDT type. Press Cmd+V to paste the text."
            }
            Self::NoFocus(_) => {
                "TDT could not go back to the app. Click in the app and press Cmd+V to paste the text."
            }
            Self::Partial(_) => {
                "Copied. Could not insert all text. Check the destination before pasting with Cmd+V."
            }
            Self::NotAllowed => {
                "Allow TDT in System Settings > Privacy & Security > Accessibility so it can type. \
                 The text is on the clipboard: press Cmd+V."
            }
        }
    }
}

impl std::fmt::Display for NotTyped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserTyped => f.write_str("the user typed"),
            Self::Elevated => f.write_str("the app runs as administrator"),
            Self::NoFocus(detail) => f.write_str(detail),
            Self::Partial(detail) => f.write_str(detail),
            Self::NotAllowed => f.write_str("no Accessibility permission"),
        }
    }
}

#[derive(Clone)]
pub struct PasteInjector {
    clipboard: Arc<Mutex<Option<Clipboard>>>,
}

impl PasteInjector {
    pub fn new() -> Self {
        let clipboard = Clipboard::new().ok();
        Self {
            clipboard: Arc::new(Mutex::new(clipboard)),
        }
    }

    pub fn copy_to_clipboard(&self, text: &str) -> Result<(), String> {
        // The transcript intentionally replaces the clipboard: copy-to-
        // clipboard is a documented feature, not a side effect, so the
        // previous contents are never restored.
        let mut last_err = String::new();
        for _ in 0..4 {
            let mut guard = self.clipboard.lock();
            if guard.is_none() {
                *guard = Clipboard::new().ok();
            }

            if let Some(ref mut cb) = *guard {
                match cb.set_text(text) {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        last_err = format!("Clipboard error: {e}");
                        // Another app can hold the clipboard for a moment.
                        *guard = None;
                    }
                }
            } else {
                last_err = "Failed to open system clipboard".to_string();
            }
            drop(guard);
            thread::sleep(Duration::from_millis(25));
        }
        Err(last_err)
    }

    pub fn copy_timed(&self, text: &str, steps: &mut DeliverySteps) -> Result<(), String> {
        let started = Instant::now();
        let copied = self.copy_to_clipboard(text);
        steps.copy = started.elapsed();
        copied
    }

    /// Focuses `target_hwnd` and types `text`, re-checking `user_typed` and
    /// target focus before each `SendInput` batch so text cannot land in a
    /// switched window or overwrite newly typed keys.
    pub fn try_type(
        &self,
        text: &str,
        target_hwnd: Option<isize>,
        user_typed: &dyn Fn() -> bool,
        steps: &mut DeliverySteps,
    ) -> Result<(), NotTyped> {
        let started = Instant::now();
        let target = prepare_target(target_hwnd);
        steps.focus = started.elapsed();
        let target = target?;
        let started = Instant::now();
        // Unicode key events, not Ctrl+V: some apps paste a synthetic Ctrl+V
        // as an image, even when the clipboard holds text.
        let typed = type_text(text, target, user_typed);
        steps.typing = started.elapsed();
        if typed.is_ok() {
            steps.typed_at = Some(Instant::now());
        }
        typed
    }
}
