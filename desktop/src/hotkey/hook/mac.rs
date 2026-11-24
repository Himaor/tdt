//! The macOS form of the shortcut listener: a CGEventTap on a thread of its
//! own. The tap sees each key, modifier, and side-button event of the
//! session, and it can keep the shortcut from the apps, as the Windows hooks do.
//!
//! macOS creates the tap only after the user allows TDT in Privacy &
//! Security > Accessibility. Until then the thread tries again every two
//! seconds, so the shortcut starts to work without a restart.

// The CoreGraphics names of the event kinds, matched as patterns.
#![allow(non_upper_case_globals)]

use super::*;
use crate::macos::ffi::*;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::AtomicPtr;
use std::sync::OnceLock;

/// The tap and the run loop of its thread, for the timeout and for shutdown.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static RUN_LOOP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Set when the tap starts after TDT waited for the permission, so the UI
/// loop removes the notice that asked for it.
static PERMISSION_GRANTED: AtomicBool = AtomicBool::new(false);

/// True once after the user gave TDT the Accessibility permission.
pub fn take_permission_granted() -> bool {
    PERMISSION_GRANTED.swap(false, Ordering::SeqCst)
}

/// The time between two tries to create the tap while TDT waits for the permission.
const PERMISSION_RETRY: Duration = Duration::from_secs(2);

thread_local! {
    /// The modifier flags of the event that the tap reads now. The shortcut
    /// must match the modifiers of that event, not the state a moment later.
    static EVENT_FLAGS: Cell<Option<CGEventFlags>> = const { Cell::new(None) };
}

/// Device-dependent modifier bits (`NX_DEVICE*KEYMASK`): they tell the left
/// key from the right key, so each modifier key has its own up and down.
const DEVICE_MODIFIER_BITS: [(u16, CGEventFlags); 8] = [
    (0xA2, 0x0000_0001), // Left Control
    (0xA0, 0x0000_0002), // Left Shift
    (0xA1, 0x0000_0004), // Right Shift
    (0x5B, 0x0000_0008), // Left Command
    (0x5C, 0x0000_0010), // Right Command
    (0xA4, 0x0000_0020), // Left Option
    (0xA5, 0x0000_0040), // Right Option
    (0xA3, 0x0000_2000), // Right Control
];

/// Milliseconds since the first call. It wraps like a Windows tick count,
/// and the callers compare tick counts through their difference.
pub(super) fn tick_count() -> u32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u32
}

/// True while the key or the mouse button `vk` is down on the hardware.
pub(super) fn key_down(vk: u16) -> bool {
    let button = match vk {
        0x01 => Some(0),
        0x02 => Some(1),
        0x04 => Some(2),
        VK_MOUSE4 => Some(3),
        VK_MOUSE5 => Some(4),
        _ => None,
    };
    // SAFETY: both calls only read the state of the HID system.
    unsafe {
        match button {
            Some(button) => CGEventSourceButtonState(kCGEventSourceStateHIDSystemState, button),
            None => crate::keys::mac::keycode_of(vk)
                .is_some_and(|key| CGEventSourceKeyState(kCGEventSourceStateHIDSystemState, key)),
        }
    }
}

/// Control, Option, Shift, and Command. Command is the Win key of the binding.
pub(super) fn modifiers_down() -> (bool, bool, bool, bool) {
    let flags = EVENT_FLAGS.get().unwrap_or_else(|| {
        // SAFETY: reads the modifier state of the HID system.
        unsafe { CGEventSourceFlagsState(kCGEventSourceStateHIDSystemState) }
    });
    (
        flags & kCGEventFlagMaskControl != 0,
        flags & kCGEventFlagMaskAlternate != 0,
        flags & kCGEventFlagMaskShift != 0,
        flags & kCGEventFlagMaskCommand != 0,
    )
}

impl HotkeyListener {
    pub fn start() -> Result<(Self, Receiver<HotkeyAction>), String> {
        let (sender, receiver) = async_channel::unbounded();
        {
            let mut guard = SENDER.lock();
            if guard.is_some() {
                return Err("Hotkey listener is already running.".to_string());
            }
            *guard = Some(sender);
        }
        let running = Arc::new(AtomicBool::new(true));
        let running_thread = Arc::clone(&running);
        let spawned = std::thread::Builder::new()
            .name("tdt-key-tap".into())
            .spawn(move || run_tap(&running_thread));
        if let Err(error) = spawned {
            *SENDER.lock() = None;
            return Err(format!("Could not start the shortcut listener: {error}"));
        }
        Ok((Self { running }, receiver))
    }

    pub(super) fn shutdown_thread(&self) {
        self.running.store(false, Ordering::SeqCst);
        let run_loop = RUN_LOOP.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !run_loop.is_null() {
            // SAFETY: the slot held a retain of the run loop, which the swap
            // made ours, so the run loop is alive here. CFRunLoopStop may be
            // called from any thread. A stop before a turn of the loop starts
            // is lost, but the next check of `running` ends the loop.
            unsafe {
                CFRunLoopStop(run_loop);
                CFRelease(run_loop);
            }
        }
    }
}

fn run_tap(running: &AtomicBool) {
    // Each key event of the session waits for this thread.
    crate::priority::raise_thread();
    let mut waiting_logged = false;
    let mut asked_input_monitoring = false;
    let tap = loop {
        if !running.load(Ordering::SeqCst) {
            return;
        }
        let mask: CGEventMask = (1 << kCGEventKeyDown)
            | (1 << kCGEventKeyUp)
            | (1 << kCGEventFlagsChanged)
            | (1 << kCGEventOtherMouseDown)
            | (1 << kCGEventOtherMouseUp);
        // SAFETY: the callback is a plain function and takes no user data.
        let tap = unsafe {
            CGEventTapCreate(
                kCGSessionEventTap,
                kCGHeadInsertEventTap,
                kCGEventTapOptionDefault,
                mask,
                tap_callback,
                std::ptr::null_mut(),
            )
        };
        if !tap.is_null() {
            break tap;
        }
        if !waiting_logged {
            voice_stt_desktop::log::note(&format!(
                "macOS did not let TDT read the shortcut (Accessibility {}, Input Monitoring {}). \
                 TDT waits for the permission.",
                allowed(crate::macos::accessibility_trusted()),
                allowed(crate::macos::input_monitoring_allowed()),
            ));
            waiting_logged = true;
        }
        // Some macOS versions also want Input Monitoring for a key tap.
        // Ask once, after Accessibility, so the user sees one prompt at a time.
        if !asked_input_monitoring
            && crate::macos::accessibility_trusted()
            && !crate::macos::input_monitoring_allowed()
        {
            crate::macos::request_input_monitoring();
            asked_input_monitoring = true;
        }
        std::thread::sleep(PERMISSION_RETRY);
    };
    if waiting_logged {
        voice_stt_desktop::log::note("TDT has the Accessibility permission. The shortcut works.");
        PERMISSION_GRANTED.store(true, Ordering::SeqCst);
        wake_loop();
    }
    // SAFETY: the tap and its source belong to this thread; they are added to
    // this thread's run loop, released once after it stops, and the slots are
    // cleared before the release.
    unsafe {
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        TAP.store(tap, Ordering::SeqCst);
        // The slot holds its own retain, so a shutdown on another thread
        // never stops a run loop that is gone.
        RUN_LOOP.store(CFRetain(run_loop).cast_mut(), Ordering::SeqCst);
        CGEventTapEnable(tap, true);
        // A shutdown between the check and the start of the loop would be
        // lost, so the loop runs in short turns and checks again.
        while running.load(Ordering::SeqCst) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, false);
        }
        let slot = RUN_LOOP.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if !slot.is_null() {
            CFRelease(slot);
        }
        TAP.store(std::ptr::null_mut(), Ordering::SeqCst);
        CFMachPortInvalidate(tap);
        if !source.is_null() {
            CFRelease(source);
        }
        CFRelease(tap);
    }
    if running.load(Ordering::SeqCst) {
        voice_stt_desktop::log::note(
            "The run loop of the key tap ended. The shortcut does not work until TDT starts again.",
        );
    }
}

fn allowed(yes: bool) -> &'static str {
    if yes {
        "allowed"
    } else {
        "not allowed"
    }
}

/// The key that the log names for typing on a key outside the table.
const UNMAPPED_KEY: u16 = 0xFF;

/// Logs the source state of the first key event: TDT counts a key as typed
/// by the user when the state is the HID system state.
fn log_source_state_once(state: i64) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        voice_stt_desktop::log::note(&format!(
            "The first key event has source state {state} (the keyboard is {kCGEventSourceStateHIDSystemState})."
        ));
    }
}

/// macOS turns a tap off when its callback is slow, and when the user types
/// in a secure field; the callback then gets a notice instead of an event.
unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if kind == kCGEventTapDisabledByTimeout || kind == kCGEventTapDisabledByUserInput {
        let tap = TAP.load(Ordering::SeqCst);
        if !tap.is_null() {
            // SAFETY: the tap stays valid while its run loop runs this callback.
            unsafe { CGEventTapEnable(tap, true) };
        }
        if kind == kCGEventTapDisabledByTimeout {
            voice_stt_desktop::log::note(
                "macOS turned the key tap off after a slow answer. TDT turned it on again.",
            );
        }
        return event;
    }
    // SAFETY: macOS gives the callback a live event for the other kinds.
    let (own, physical, flags) = unsafe {
        (
            CGEventGetIntegerValueField(event, kCGEventSourceUserData)
                == crate::keys::OWN_INPUT as i64,
            CGEventGetIntegerValueField(event, kCGEventSourceStateID)
                == i64::from(kCGEventSourceStateHIDSystemState),
            CGEventGetFlags(event),
        )
    };
    // The text that TDT types must never start a recording or become a new
    // shortcut. With no listener left, every event passes on.
    if own || !listening() {
        return event;
    }
    EVENT_FLAGS.set(Some(flags));
    // SAFETY: the event is live for the length of the callback.
    let keep = unsafe { decide(kind, event, flags, physical) };
    EVENT_FLAGS.set(None);
    if keep {
        std::ptr::null_mut()
    } else {
        event
    }
}

/// Returns true when TDT keeps the event from the apps.
///
/// # Safety
/// `event` must be the live event of the callback.
unsafe fn decide(
    kind: CGEventType,
    event: CGEventRef,
    flags: CGEventFlags,
    physical: bool,
) -> bool {
    // SAFETY: the caller gives a live event.
    let field = |name| unsafe { CGEventGetIntegerValueField(event, name) };
    match kind {
        kCGEventKeyDown | kCGEventKeyUp => {
            let keycode = field(kCGKeyboardEventKeycode) as u16;
            log_source_state_once(field(kCGEventSourceStateID));
            match crate::keys::mac::vk_of(keycode) {
                Some(vk) => on_key(vk, kind == kCGEventKeyDown, kind == kCGEventKeyUp, physical),
                // A key that is not in the table (an arrow, Home, a keypad
                // key) is typing, but it can be neither the shortcut nor a
                // captured shortcut, and it always reaches the app.
                None => {
                    if physical && kind == kCGEventKeyDown && !CAPTURE.load(Ordering::SeqCst) {
                        note_user_typed(UNMAPPED_KEY);
                    }
                    false
                }
            }
        }
        kCGEventFlagsChanged => {
            let keycode = field(kCGKeyboardEventKeycode) as u16;
            let Some(vk) = crate::keys::mac::vk_of(keycode) else {
                return false;
            };
            let Some(&(_, bit)) = DEVICE_MODIFIER_BITS.iter().find(|(key, _)| *key == vk) else {
                return false;
            };
            let down = flags & bit != 0;
            on_key(vk, down, !down, physical);
            // A modifier change always reaches the apps.
            false
        }
        kCGEventOtherMouseDown | kCGEventOtherMouseUp => {
            let vk = match field(kCGMouseEventButtonNumber) {
                3 => VK_MOUSE4,
                4 => VK_MOUSE5,
                _ => return false,
            };
            handle_mouse_button(vk, kind == kCGEventOtherMouseDown)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real key event through the real tap callback. Returns true when
    /// TDT keeps it from the apps.
    fn send(kind: CGEventType, keycode: u16, flags: CGEventFlags) -> bool {
        // SAFETY: the event is created, read by the callback, and released once.
        unsafe {
            let event =
                CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, kind == kCGEventKeyDown);
            assert!(!event.is_null());
            if kind == kCGEventFlagsChanged {
                // A keyboard event becomes a modifier change with this type.
                CGEventSetType(event, kCGEventFlagsChanged);
            }
            CGEventSetFlags(event, flags);
            let kept =
                tap_callback(std::ptr::null_mut(), kind, event, std::ptr::null_mut()).is_null();
            CFRelease(event);
            kept
        }
    }

    const SEMICOLON: u16 = 0x29;
    const LEFT_CONTROL: u16 = 0x3B;
    /// Control, with its device bit for the left key.
    const CTRL: CGEventFlags = kCGEventFlagMaskControl | 0x0000_0001;

    fn actions(rx: &Receiver<HotkeyAction>) -> Vec<HotkeyAction> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    /// One test: the listener state is global.
    #[test]
    fn tap_and_hold_of_ctrl_semicolon() {
        let (tx, rx) = async_channel::unbounded();
        *SENDER.lock() = Some(tx);
        set_binding(HotkeyBinding::DEFAULT);

        // A tap toggles. The key and its key-up never reach the app.
        assert!(send(kCGEventKeyDown, SEMICOLON, CTRL));
        assert!(send(kCGEventKeyUp, SEMICOLON, CTRL));
        let got = actions(&rx);
        assert!(
            matches!(
                got[..],
                [HotkeyAction::PressStarted(_), HotkeyAction::Toggled(_)]
            ),
            "{got:?}"
        );

        // A hold records until release. Key repeats start no second press.
        assert!(send(kCGEventKeyDown, SEMICOLON, CTRL));
        std::thread::sleep(HOLD_THRESHOLD + Duration::from_millis(50));
        assert!(send(kCGEventKeyDown, SEMICOLON, CTRL));
        assert!(send(kCGEventKeyUp, SEMICOLON, CTRL));
        let got = actions(&rx);
        assert!(
            matches!(
                got[..],
                [HotkeyAction::PressStarted(_), HotkeyAction::HoldReleased(_)]
            ),
            "{got:?}"
        );

        // A hold that ends with Control up first ends at that moment.
        assert!(send(kCGEventKeyDown, SEMICOLON, CTRL));
        std::thread::sleep(HOLD_THRESHOLD + Duration::from_millis(50));
        assert!(
            !send(kCGEventFlagsChanged, LEFT_CONTROL, 0),
            "a modifier change reaches the apps"
        );
        assert!(
            send(kCGEventKeyUp, SEMICOLON, 0),
            "the key-up of a kept press stays kept"
        );
        let got = actions(&rx);
        assert!(
            matches!(
                got[..],
                [HotkeyAction::PressStarted(_), HotkeyAction::HoldReleased(_)]
            ),
            "{got:?}"
        );

        // Without Control the key is typing and goes to the app.
        assert!(!send(kCGEventKeyDown, SEMICOLON, 0));
        assert!(!send(kCGEventKeyUp, SEMICOLON, 0));
        assert!(actions(&rx).is_empty());

        // The text that TDT types never starts a recording.
        // SAFETY: as in `send`.
        unsafe {
            let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), SEMICOLON, true);
            CGEventSetFlags(event, CTRL);
            CGEventSetIntegerValueField(
                event,
                kCGEventSourceUserData,
                crate::keys::OWN_INPUT as i64,
            );
            assert!(!tap_callback(
                std::ptr::null_mut(),
                kCGEventKeyDown,
                event,
                std::ptr::null_mut()
            )
            .is_null());
            CFRelease(event);
        }
        assert!(actions(&rx).is_empty());

        // A key outside the table (Left Arrow) can never become the shortcut.
        begin_capture();
        assert!(!send(kCGEventKeyDown, 0x7B, CTRL));
        assert!(!send(kCGEventKeyUp, 0x7B, CTRL));
        assert!(actions(&rx).is_empty());
        end_capture();
        assert_eq!(current_binding(), HotkeyBinding::DEFAULT);

        *SENDER.lock() = None;
    }
}
