use crate::macos;
use std::fmt;

/// The app of a process ID. `raw` is the target that
/// `ui::window_util::dictation_target_window` gave.
pub struct Target {
    name: String,
    bundle: String,
}

impl Target {
    pub fn of(raw: isize) -> Self {
        let identity = i32::try_from(raw).ok().and_then(macos::app_identity);
        let (name, bundle) = identity.unwrap_or_else(|| ("?".into(), String::new()));
        Self { name, bundle }
    }

    /// True when macOS drops TDT's keys: secure input is on. The app does
    /// not matter, because secure input holds for the whole session.
    #[allow(dead_code)]
    pub fn blocks_input(&self) -> bool {
        macos::secure_input_on()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.bundle)?;
        if macos::secure_input_on() {
            f.write_str(", secure input")?;
        }
        Ok(())
    }
}
