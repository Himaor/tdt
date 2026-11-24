//! The Windows form of auto-paste: bring the window back with
//! SetForegroundWindow, then type the text as Unicode key events with SendInput.

use super::NotTyped;
use std::thread;
use std::time::{Duration, Instant};

/// The longest wait for a restored window to accept text. An app that never
/// reports an input focus waits this long.
const INPUT_READY_BUDGET: Duration = Duration::from_millis(60);
/// Granularity of the readiness poll. Small enough that a late-ready window is
/// still caught early, large enough not to spin.
const INPUT_READY_STEP: Duration = Duration::from_millis(4);

/// UTF-16 units in one `SendInput` call. A short pause follows each call but
/// the last, so the app can read its input before more comes.
const TYPED_AT_ONCE: usize = 256;
const PAUSE_BETWEEN_CALLS: Duration = Duration::from_millis(1);

/// The window that gets the text, and its focused control when the app reports one.
pub(super) type Prepared = (
    windows::Win32::Foundation::HWND,
    Option<windows::Win32::Foundation::HWND>,
);

/// Returns the focused control HWND of `hwnd`'s GUI thread, if the app reports one.
fn focused_control(
    hwnd: windows::Win32::Foundation::HWND,
) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    };
    // SAFETY: `info` has `cbSize` set and outlives the call. a stale handle makes a call fail, not misbehave.
    unsafe {
        let thread = GetWindowThreadProcessId(hwnd, None);
        if thread == 0 {
            return None;
        }
        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        // hwndFocus is the window itself when it owns focus directly, so this
        // also covers apps with no child edit control.
        (GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.is_invalid())
            .then_some(info.hwndFocus)
    }
}

fn wait_for_input_focus(
    hwnd: windows::Win32::Foundation::HWND,
) -> Option<windows::Win32::Foundation::HWND> {
    let started = Instant::now();
    loop {
        if let Some(focus) = focused_control(hwnd) {
            return Some(focus);
        }
        if started.elapsed() >= INPUT_READY_BUDGET {
            return None;
        }
        thread::sleep(INPUT_READY_STEP);
    }
}

/// Brings the window of the dictation back to the front.
pub(super) fn prepare_target(target_hwnd: Option<isize>) -> Result<Prepared, NotTyped> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, SetForegroundWindow,
    };
    let Some(raw) = target_hwnd else {
        return Err(NotTyped::NoFocus("no target window"));
    };
    // Windows would drop the keys and report success, so do not
    // claim a paste that cannot happen.
    if crate::target::Target::of(raw).blocks_input() {
        return Err(NotTyped::Elevated);
    }
    let hwnd = HWND(raw as _);
    // SAFETY: foreground calls on the caller's target handle; a stale handle makes a call fail, not misbehave.
    let foreground_allowed = unsafe {
        // SetForegroundWindow is frequently denied to background
        // apps; the loop below is the real check.
        let allowed = SetForegroundWindow(hwnd).as_bool();
        let _ = BringWindowToTop(hwnd);
        allowed
    };
    for _ in 0..8 {
        // SAFETY: GetForegroundWindow takes no arguments.
        if unsafe { GetForegroundWindow() } == hwnd {
            // The `focus` time of the delivery line in the log holds this wait.
            let focus_control = wait_for_input_focus(hwnd);
            return Ok((hwnd, focus_control));
        }
        thread::sleep(Duration::from_millis(15));
    }
    Err(NotTyped::NoFocus(if foreground_allowed {
        "the window did not come to the front"
    } else {
        "Windows refused the focus"
    }))
}

struct TypingGuard<'a> {
    hwnd: windows::Win32::Foundation::HWND,
    /// Focused control seen right after `prepare_target`, when the app reports
    /// one. Apps that never report `hwndFocus` still check `GetForegroundWindow`.
    focus_control: Option<windows::Win32::Foundation::HWND>,
    user_typed: &'a dyn Fn() -> bool,
}

fn target_still_ready(guard: &TypingGuard<'_>, batches_sent: usize) -> Result<(), NotTyped> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    if (guard.user_typed)() {
        return Err(if batches_sent == 0 {
            NotTyped::UserTyped
        } else {
            NotTyped::Partial("stopped typing because a key was pressed".to_string())
        });
    }
    // SAFETY: GetForegroundWindow takes no arguments.
    if unsafe { GetForegroundWindow() } != guard.hwnd {
        return Err(if batches_sent == 0 {
            NotTyped::NoFocus("the window lost focus before typing")
        } else {
            NotTyped::Partial("stopped typing because the window lost focus".to_string())
        });
    }
    if let Some(expected_focus) = guard.focus_control {
        if focused_control(guard.hwnd) != Some(expected_focus) {
            return Err(if batches_sent == 0 {
                NotTyped::NoFocus("the focused field changed before typing")
            } else {
                NotTyped::Partial("stopped typing because the focused field changed".to_string())
            });
        }
    }
    Ok(())
}

fn release_physical_modifiers() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, KEYEVENTF_KEYUP};

    let down: Vec<u16> = crate::keys::MODIFIERS
        .into_iter()
        // SAFETY: GetAsyncKeyState takes no pointers.
        .filter(|vk| unsafe { GetAsyncKeyState(i32::from(*vk)) as u16 & 0x8000 != 0 })
        .collect();
    if down.is_empty() {
        return;
    }
    let inputs: Vec<_> = down
        .into_iter()
        .map(|vk| {
            keyboard_input(
                windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                0,
                KEYEVENTF_KEYUP,
            )
        })
        .collect();
    let _ = send_input_batch(&inputs);
}

pub(super) fn type_text(
    text: &str,
    (hwnd, focus_control): Prepared,
    user_typed: &dyn Fn() -> bool,
) -> Result<(), NotTyped> {
    let guard = TypingGuard {
        hwnd,
        focus_control,
        user_typed,
    };
    target_still_ready(&guard, 0)?;
    release_physical_modifiers();
    let (inputs, batch_ends) = encode_text_inputs(text)?;
    let mut start = 0;
    for (index, end) in batch_ends.into_iter().enumerate() {
        if index > 0 {
            thread::sleep(PAUSE_BETWEEN_CALLS);
        }
        target_still_ready(&guard, index)?;
        send_input_batch(&inputs[start..end])?;
        start = end;
    }
    Ok(())
}

fn encode_text_inputs(
    text: &str,
) -> Result<
    (
        Vec<windows::Win32::UI::Input::KeyboardAndMouse::INPUT>,
        Vec<usize>,
    ),
    NotTyped,
> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_RETURN, VK_TAB,
    };
    let mut inputs = Vec::with_capacity(text.encode_utf16().count() * 2);
    let mut batch_ends = Vec::new();
    let mut batch_start = 0;
    for character in text.chars() {
        if inputs.len() - batch_start + 2 * character.len_utf16() > 2 * TYPED_AT_ONCE {
            batch_start = inputs.len();
            batch_ends.push(batch_start);
        }
        match character {
            '\n' => push_key(&mut inputs, VK_RETURN),
            '\t' => push_key(&mut inputs, VK_TAB),
            '\r' => continue,
            '\0' => {
                return Err(NotTyped::Partial(
                    "Cannot type text containing a null character".to_string(),
                ));
            }
            _ => {
                let mut encoded = [0_u16; 2];
                for &unit in character.encode_utf16(&mut encoded).iter() {
                    inputs.push(keyboard_input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
                    inputs.push(keyboard_input(
                        VIRTUAL_KEY(0),
                        unit,
                        KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    ));
                }
            }
        }
    }
    if inputs.len() > batch_start {
        batch_ends.push(inputs.len());
    }
    Ok((inputs, batch_ends))
}

fn keyboard_input(
    virtual_key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    scan_code: u16,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: scan_code,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: crate::keys::OWN_INPUT,
            },
        },
    }
}

fn push_key(
    inputs: &mut Vec<windows::Win32::UI::Input::KeyboardAndMouse::INPUT>,
    virtual_key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
    inputs.push(keyboard_input(virtual_key, 0, KEYBD_EVENT_FLAGS::default()));
    inputs.push(keyboard_input(virtual_key, 0, KEYEVENTF_KEYUP));
}

fn send_input_batch(
    inputs: &[windows::Win32::UI::Input::KeyboardAndMouse::INPUT],
) -> Result<(), NotTyped> {
    use windows::Win32::UI::Input::KeyboardAndMouse::SendInput;
    // SAFETY: `inputs` is a valid slice of INPUT and the size is for one INPUT.
    let sent = unsafe { SendInput(inputs, std::mem::size_of_val(&inputs[0]) as i32) };
    if sent != inputs.len() as u32 {
        return Err(NotTyped::Partial(format!(
            "Windows accepted {sent} of {} text input events: {}",
            inputs.len(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_batch_keeps_surrogate_pair_together() {
        let text = format!("{}\u{1f642}", "a".repeat(TYPED_AT_ONCE - 1));
        let (_, ends) = encode_text_inputs(&text).expect("encodes");
        assert_eq!(ends, [2 * (TYPED_AT_ONCE - 1), 2 * (TYPED_AT_ONCE + 1)]);
    }
}
