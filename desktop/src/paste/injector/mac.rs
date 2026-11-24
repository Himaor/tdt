//! The macOS form of auto-paste: bring the app of the dictation back to the
//! front, then type the text as Unicode key events with CGEventPost.
//!
//! The target is the process ID of the app that was in front when the take
//! started (`ui::window_util::dictation_target_window`).

use super::NotTyped;
use crate::macos::{self, ffi::*};
use std::thread;
use std::time::Duration;

/// UTF-16 units in one key event. macOS reads at most 20 from each event.
const UNITS_PER_EVENT: usize = 20;
/// UTF-16 units between two checks of the target and of the keyboard, with
/// a short pause so the app can read its input before more comes.
const TYPED_AT_ONCE: usize = 200;
const PAUSE_BETWEEN_BATCHES: Duration = Duration::from_millis(1);

/// The process ID of the app that gets the text.
pub(super) type Prepared = i32;

/// Brings the app of the dictation back to the front.
pub(super) fn prepare_target(target: Option<isize>) -> Result<Prepared, NotTyped> {
    let Some(pid) = target.and_then(|raw| i32::try_from(raw).ok()) else {
        return Err(NotTyped::NoFocus("no target app"));
    };
    // Without the permission macOS drops each key event and reports nothing.
    if !macos::accessibility_trusted() {
        return Err(NotTyped::NotAllowed);
    }
    // Secure input drops the events too, and also reports nothing.
    if macos::secure_input_on() {
        return Err(NotTyped::Elevated);
    }
    if !macos::app_is_running(pid) {
        return Err(NotTyped::NoFocus("the app is gone"));
    }
    if macos::frontmost_pid() == Some(pid) {
        return Ok(pid);
    }
    let allowed = macos::activate_app(pid);
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(15));
        if macos::frontmost_pid() == Some(pid) {
            return Ok(pid);
        }
    }
    Err(NotTyped::NoFocus(if allowed {
        "the app did not come to the front"
    } else {
        "macOS refused to bring the app to the front"
    }))
}

fn still_ready(pid: Prepared, user_typed: &dyn Fn() -> bool, sent: usize) -> Result<(), NotTyped> {
    if user_typed() {
        return Err(if sent == 0 {
            NotTyped::UserTyped
        } else {
            NotTyped::Partial("stopped typing because a key was pressed".to_string())
        });
    }
    if macos::frontmost_pid() != Some(pid) {
        return Err(if sent == 0 {
            NotTyped::NoFocus("the app lost focus before typing")
        } else {
            NotTyped::Partial("stopped typing because the app lost focus".to_string())
        });
    }
    if macos::secure_input_on() {
        return Err(if sent == 0 {
            NotTyped::Elevated
        } else {
            NotTyped::Partial("stopped typing because secure input came on".to_string())
        });
    }
    Ok(())
}

/// One key press of the text: a Unicode run, or a named key for a line
/// break or a tab, which apps read as the key and not as a character.
#[derive(Debug, PartialEq)]
enum Stroke {
    Units(Vec<u16>),
    Key(u16),
}

/// Splits `text` into strokes. A run never splits a surrogate pair.
fn strokes(text: &str) -> Result<Vec<Stroke>, NotTyped> {
    let mut strokes = Vec::new();
    let mut run: Vec<u16> = Vec::new();
    for character in text.chars() {
        let key = match character {
            '\n' => Some(crate::keys::mac::RETURN),
            '\t' => Some(crate::keys::mac::TAB),
            '\r' => continue,
            '\0' => {
                return Err(NotTyped::Partial(
                    "Cannot type text containing a null character".to_string(),
                ))
            }
            _ => None,
        };
        if let Some(key) = key {
            if !run.is_empty() {
                strokes.push(Stroke::Units(std::mem::take(&mut run)));
            }
            strokes.push(Stroke::Key(key));
            continue;
        }
        let mut encoded = [0u16; 2];
        let units = character.encode_utf16(&mut encoded);
        if run.len() + units.len() > UNITS_PER_EVENT {
            strokes.push(Stroke::Units(std::mem::take(&mut run)));
        }
        run.extend_from_slice(units);
    }
    if !run.is_empty() {
        strokes.push(Stroke::Units(run));
    }
    Ok(strokes)
}

impl Stroke {
    fn units(&self) -> usize {
        match self {
            Self::Units(units) => units.len(),
            Self::Key(_) => 1,
        }
    }
}

/// An event source of TDT's own: its key state is private, so a modifier
/// that the user still holds does not turn the text into shortcuts.
struct Source(CGEventSourceRef);

impl Drop for Source {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the source was created by `CGEventSourceCreate` and is released once.
            unsafe { CFRelease(self.0) };
        }
    }
}

fn post(source: &Source, stroke: &Stroke) -> Result<(), NotTyped> {
    for down in [true, false] {
        let key = match stroke {
            Stroke::Units(_) => 0,
            Stroke::Key(key) => *key,
        };
        // SAFETY: the event is created, filled from the live slice, posted,
        // and released once. A null event (out of memory) posts nothing.
        unsafe {
            let event = CGEventCreateKeyboardEvent(source.0, key, down);
            if event.is_null() {
                return Err(NotTyped::Partial(
                    "macOS could not create a key event".to_string(),
                ));
            }
            if let Stroke::Units(units) = stroke {
                CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
            }
            // No modifiers: a held Ctrl or Cmd must not change the text.
            CGEventSetFlags(event, 0);
            // The key tap passes on events with this mark.
            CGEventSetIntegerValueField(
                event,
                kCGEventSourceUserData,
                crate::keys::OWN_INPUT as i64,
            );
            CGEventPost(kCGHIDEventTap, event);
            CFRelease(event);
        }
    }
    Ok(())
}

pub(super) fn type_text(
    text: &str,
    pid: Prepared,
    user_typed: &dyn Fn() -> bool,
) -> Result<(), NotTyped> {
    let strokes = strokes(text)?;
    still_ready(pid, user_typed, 0)?;
    // SAFETY: creates a new source with a private state; `Source` releases it.
    let source = Source(unsafe { CGEventSourceCreate(kCGEventSourceStatePrivate) });
    let mut in_batch = 0;
    for (index, stroke) in strokes.iter().enumerate() {
        if in_batch + stroke.units() > TYPED_AT_ONCE {
            thread::sleep(PAUSE_BETWEEN_BATCHES);
            still_ready(pid, user_typed, index)?;
            in_batch = 0;
        }
        post(&source, stroke)?;
        in_batch += stroke.units();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_keep_surrogate_pairs_together() {
        let text = format!("{}\u{1f642}", "a".repeat(UNITS_PER_EVENT - 1));
        let strokes = strokes(&text).expect("splits");
        assert_eq!(strokes.len(), 2);
        assert_eq!(strokes[0].units(), UNITS_PER_EVENT - 1);
        assert_eq!(strokes[1].units(), 2);
    }

    #[test]
    fn line_breaks_and_tabs_are_keys() {
        let strokes = strokes("a\r\nb\tc").expect("splits");
        assert_eq!(
            strokes,
            [
                Stroke::Units(vec![u16::from(b'a')]),
                Stroke::Key(crate::keys::mac::RETURN),
                Stroke::Units(vec![u16::from(b'b')]),
                Stroke::Key(crate::keys::mac::TAB),
                Stroke::Units(vec![u16::from(b'c')]),
            ]
        );
    }

    #[test]
    fn null_character_is_refused() {
        assert!(strokes("a\0b").is_err());
    }
}
