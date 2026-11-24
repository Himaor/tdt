use async_channel::{Receiver, Sender};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
pub use mac::take_permission_granted;
#[cfg(target_os = "macos")]
use mac::{key_down, modifiers_down, tick_count};
#[cfg(windows)]
use win::{key_down, modifiers_down, tick_count};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    PressStarted(u64),
    /// A hold came up. The `Instant` is the key-up: a hold asks the take to
    /// stop when the user lets go.
    HoldReleased(Instant),
    /// A short tap. The `Instant` is the key-down: a tap that stops a take
    /// asks to stop when the user presses it, not when the key comes up.
    Toggled(Instant),
    Captured(HotkeyBinding),
    CaptureCancelled,
    /// No key: a tray click or a click on the pill asks the UI loop to run now.
    Wake,
}

/// The keycaps of Ctrl, Alt, Shift, and Win. On a Mac keyboard the Win key
/// of the binding is Command and Alt is Option. `parse` reads both names.
#[cfg(not(target_os = "macos"))]
const MODIFIER_LABELS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Win"];
#[cfg(target_os = "macos")]
const MODIFIER_LABELS: [&str; 4] = ["Ctrl", "Option", "Shift", "Cmd"];

/// A press longer than this records until release instead of toggling.
const HOLD_THRESHOLD: Duration = Duration::from_millis(350);

/// A rebind request that is left unanswered stops listening for keys after
/// this long, so a forgotten capture cannot swallow the keyboard.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(6);

/// Virtual-key codes of the side mouse buttons (XBUTTON1 and XBUTTON2).
const VK_MOUSE4: u16 = 0x05;
const VK_MOUSE5: u16 = 0x06;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u16,
}

impl HotkeyBinding {
    pub const DEFAULT: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
        win: false,
        vk: 0xBA,
    };

    pub fn parse(raw: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut win = false;
        let mut vk = None;
        for part in raw.split('+') {
            let token = part.trim();
            if token.is_empty() {
                continue;
            }
            match token.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "option" => alt = true,
                "shift" => shift = true,
                "win" | "windows" | "super" | "meta" | "cmd" | "command" => win = true,
                other => vk = parse_vk(other),
            }
        }
        let binding = Self {
            ctrl,
            alt,
            shift,
            win,
            vk: vk?,
        };
        binding.is_valid().then_some(binding)
    }

    /// The text of the shortcut in the settings file and on the screen, such as `Ctrl+;`.
    pub fn display(self) -> String {
        self.key_labels().join("+")
    }

    /// The modifiers, then the key, each as the keycap shows it.
    pub fn key_labels(self) -> Vec<String> {
        let mut parts = Vec::new();
        let [ctrl, alt, shift, win] = MODIFIER_LABELS;
        for (down, label) in [
            (self.ctrl, ctrl),
            (self.alt, alt),
            (self.shift, shift),
            (self.win, win),
        ] {
            if down {
                parts.push(label.to_string());
            }
        }
        parts.push(vk_label(self.vk));
        parts
    }

    pub fn is_valid(self) -> bool {
        if self.vk == 0 || self.vk == 0x1B || crate::keys::is_modifier(self.vk) {
            return false;
        }
        let has_mod = self.ctrl || self.alt || self.shift || self.win;
        let is_function = (0x70..=0x87).contains(&self.vk);
        has_mod || is_function || self.is_mouse()
    }

    /// Mouse 4 and Mouse 5, the side buttons. Other mouse buttons stay free.
    pub fn is_mouse(self) -> bool {
        matches!(self.vk, VK_MOUSE4 | VK_MOUSE5)
    }
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self::DEFAULT
    }
}

static BINDING: Mutex<HotkeyBinding> = Mutex::new(HotkeyBinding::DEFAULT);
static CAPTURE: AtomicBool = AtomicBool::new(false);
static CAPTURE_START: Mutex<Option<Instant>> = Mutex::new(None);
static PRESS_START: Mutex<Option<Instant>> = Mutex::new(None);
static SENDER: Mutex<Option<Sender<HotkeyAction>>> = Mutex::new(None);

pub fn set_binding(binding: HotkeyBinding) {
    if binding.is_valid() {
        *BINDING.lock() = binding;
        // The mark is for the key of the binding before.
        SHORTCUT_DOWN.store(false, Ordering::SeqCst);
    }
}

/// The shortcut key is down from a press that TDT kept from the apps. TDT
/// keeps the repeats of the key and its key-up from the apps too. A hold that
/// ends with the modifier up first gave the repeats of the key to the app:
/// the app got characters that the user did not type, and TDT then did not
/// type the text, because a key went down.
static SHORTCUT_DOWN: AtomicBool = AtomicBool::new(false);
/// When the shortcut key last went down or repeated, as a Windows tick count.
static SHORTCUT_DOWN_TICK: AtomicU32 = AtomicU32::new(0);
/// A held key repeats after 1 s at most, the longest repeat delay of
/// Windows. A key-down after a longer time is a new press: the key-up of the
/// press before did not reach the hook.
const REPEAT_WITHIN_MS: u32 = 1_500;

/// Marks the shortcut key as down from a press that TDT kept.
fn shortcut_went_down() {
    SHORTCUT_DOWN_TICK.store(tick_count(), Ordering::SeqCst);
    SHORTCUT_DOWN.store(true, Ordering::SeqCst);
}

/// True for a key-down of the shortcut key that is a repeat of a press that
/// TDT kept.
fn shortcut_repeats() -> bool {
    if !SHORTCUT_DOWN.load(Ordering::SeqCst) {
        return false;
    }
    let now = tick_count();
    // Tick counts wrap, so compare them through their difference.
    if now.wrapping_sub(SHORTCUT_DOWN_TICK.load(Ordering::SeqCst)) > REPEAT_WITHIN_MS {
        SHORTCUT_DOWN.store(false, Ordering::SeqCst);
        return false;
    }
    SHORTCUT_DOWN_TICK.store(now, Ordering::SeqCst);
    true
}

pub fn current_binding() -> HotkeyBinding {
    *BINDING.lock()
}

pub fn begin_capture() {
    // A press was possibly still active; never leave the app thinking it is.
    if PRESS_START.lock().take().is_some() {
        send_action(HotkeyAction::HoldReleased(Instant::now()));
    }
    *CAPTURE_START.lock() = Some(Instant::now());
    CAPTURE.store(true, Ordering::SeqCst);
}

pub fn end_capture() {
    *CAPTURE_START.lock() = None;
    CAPTURE.store(false, Ordering::SeqCst);
}

/// Cancel an abandoned capture from the application poll loop. Keyboard hooks
/// only run when a key changes, so relying on the hook alone would leave the
/// settings UI in capture mode forever when the user walks away.
pub fn poll_capture_timeout() {
    if CAPTURE.load(Ordering::SeqCst) && capture_expired() {
        end_capture();
        send_action(HotkeyAction::CaptureCancelled);
    }
}

fn capture_expired() -> bool {
    CAPTURE_START
        .lock()
        .is_some_and(|start| start.elapsed() >= CAPTURE_TIMEOUT)
}

/// Ends the wait of the UI loop. Its idle tick is slow, and a click must
/// not wait for it.
pub fn wake_loop() {
    send_action(HotkeyAction::Wake);
}

/// Ends a press whose key-up did not reach the hook: the lock screen or an
/// administrator window took it. Windows still knows that a modifier of the
/// shortcut is up. A shortcut with no modifier has no such sign: Windows
/// does not keep the state of a key that the hook holds back.
pub fn poll_missed_release() {
    if PRESS_START.lock().is_none() {
        return;
    }
    let binding = current_binding();
    let (ctrl, alt, shift, win) = modifiers_down();
    let released = (binding.ctrl && !ctrl)
        || (binding.alt && !alt)
        || (binding.shift && !shift)
        || (binding.win && !win);
    if released {
        finish_press();
    }
}

fn send_action(action: HotkeyAction) {
    if let Some(ref sender) = *SENDER.lock() {
        // Unbounded, so this never waits inside the hook. The receiving
        // loop wakes at once instead of at its next poll.
        let _ = sender.try_send(action);
    }
}

// The upper bits count key events. The lower 16 bits hold the latest key.
static TYPED_EVENT: AtomicU64 = AtomicU64::new(0);

pub fn typed_generation() -> u64 {
    TYPED_EVENT.load(Ordering::SeqCst) >> 16
}

pub fn note_user_typed(vk: u16) {
    let _ = TYPED_EVENT.try_update(Ordering::SeqCst, Ordering::SeqCst, |event| {
        Some(((event & !0xffff).wrapping_add(1 << 16)) | u64::from(vk))
    });
}

pub fn user_typed_since(generation: u64) -> Option<u16> {
    let event = TYPED_EVENT.load(Ordering::SeqCst);
    (event >> 16 != generation).then_some(event as u16)
}

/// Mouse buttons are 0x01 through 0x06. The shortcut key and modifiers are not typing.
fn note_physical_key(vk: u16, binding_vk: u16) {
    if vk > 0x06 && !crate::keys::is_modifier(vk) && vk != binding_vk {
        note_user_typed(vk);
    }
}

/// Keys the hook saw go down and not up. Keys that a program sent are not here.
static PHYSICAL_DOWN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

fn is_mouse_vk(vk: u16) -> bool {
    matches!(vk, 0x01 | 0x02 | 0x04 | 0x05 | 0x06)
}

/// The shortcut went down. Key repeat sends more downs; only the first counts.
fn start_press() {
    let mut start_guard = PRESS_START.lock();
    if start_guard.is_none() {
        *start_guard = Some(Instant::now());
        send_action(HotkeyAction::PressStarted(typed_generation()));
    }
}

/// A valid capture replaces the shortcut. An invalid one, such as a lone
/// letter, ends capture instead of swallowing input until the timeout.
fn finish_capture(captured: HotkeyBinding) {
    end_capture();
    if captured.is_valid() {
        set_binding(captured);
        send_action(HotkeyAction::Captured(captured));
    } else {
        send_action(HotkeyAction::CaptureCancelled);
    }
}

fn finish_press() {
    let Some(start) = PRESS_START.lock().take() else {
        return;
    };
    let action = if start.elapsed() > HOLD_THRESHOLD {
        HotkeyAction::HoldReleased(Instant::now())
    } else {
        HotkeyAction::Toggled(start)
    };
    send_action(action);
}

pub struct HotkeyListener {
    running: Arc<AtomicBool>,
}

impl Drop for HotkeyListener {
    fn drop(&mut self) {
        self.shutdown_thread();
    }
}

/// A key or mouse button, other than the shortcut, that is down now.
///
/// A keyboard key counts only when the hook saw it go down. Some laptop
/// drivers report a key as down that never goes up, and that key would stop
/// every paste.
pub fn input_held(binding_vk: u16) -> Option<u16> {
    (1..=255u16).find(|&vk| {
        let real = is_mouse_vk(vk) || PHYSICAL_DOWN[usize::from(vk)].load(Ordering::SeqCst);
        real && !crate::keys::is_modifier(vk) && vk != binding_vk && key_down(vk)
    })
}

fn binding_with_modifiers(vk: u16) -> HotkeyBinding {
    let (ctrl, alt, shift, win) = modifiers_down();
    HotkeyBinding {
        ctrl,
        alt,
        shift,
        win,
        vk,
    }
}

fn modifiers_match(binding: HotkeyBinding) -> bool {
    let (ctrl, alt, shift, win) = modifiers_down();
    ctrl == binding.ctrl && alt == binding.alt && shift == binding.shift && win == binding.win
}

fn is_required_modifier(vk: u16, binding: HotkeyBinding) -> bool {
    (binding.ctrl && matches!(vk, 0x11 | 0xA2 | 0xA3))
        || (binding.alt && matches!(vk, 0x12 | 0xA4 | 0xA5))
        || (binding.shift && matches!(vk, 0x10 | 0xA0 | 0xA1))
        || (binding.win && matches!(vk, 0x5B | 0x5C))
}

/// The decision for one key event, on every system. `physical` is true for a
/// key from the keyboard, not from a program. Returns true when TDT keeps the
/// event from the apps.
fn on_key(vk: u16, is_down: bool, is_up: bool, physical: bool) -> bool {
    // Only keys from the keyboard count as typing by the user.
    if physical && (is_down || is_up) {
        PHYSICAL_DOWN[usize::from(vk & 0xFF)].store(is_down, Ordering::SeqCst);
    }

    if CAPTURE.load(Ordering::SeqCst) {
        if capture_expired() {
            end_capture();
            send_action(HotkeyAction::CaptureCancelled);
        } else if is_down && vk == 0x1B {
            end_capture();
            send_action(HotkeyAction::CaptureCancelled);
            return true;
        } else if !crate::keys::is_modifier(vk) && (is_down || is_up) {
            if is_down {
                finish_capture(binding_with_modifiers(vk));
            }
            return true;
        }
        return false;
    }
    let binding = current_binding();
    if is_down && vk == binding.vk {
        if modifiers_match(binding) {
            start_press();
            shortcut_went_down();
            return true;
        }
        // A repeat of a press that TDT kept, after a modifier came up.
        if shortcut_repeats() {
            return true;
        }
    }
    if is_down && physical {
        note_physical_key(vk, binding.vk);
    }
    if is_up && vk == binding.vk {
        // The apps did not get the key-down of a press that TDT
        // kept, so they do not get its key-up.
        let kept = SHORTCUT_DOWN.swap(false, Ordering::SeqCst);
        if PRESS_START.lock().is_some() {
            finish_press();
            return true;
        }
        if kept {
            return true;
        }
    } else if is_up && is_required_modifier(vk, binding) {
        // Releasing a required modifier ends the press even if the
        // main key is still held; the later keyup of the main key is
        // then ignored on purpose (no repeat-press handling).
        finish_press();
    }
    false
}

/// False after a failed start: with no listener, every key passes on.
fn listening() -> bool {
    SENDER.lock().is_some()
}

/// Side buttons whose down event TDT swallowed. Their up event is swallowed
/// too, so an app never gets a lone up and goes Back or Forward.
static MOUSE_SWALLOWED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Returns true when TDT keeps the event from other apps.
fn handle_mouse_button(vk: u16, is_down: bool) -> bool {
    let bit = 1u8 << (vk - VK_MOUSE4);
    if !is_down {
        if current_binding().vk == vk {
            finish_press();
        }
        return MOUSE_SWALLOWED.fetch_and(!bit, Ordering::SeqCst) & bit != 0;
    }
    if CAPTURE.load(Ordering::SeqCst) {
        if capture_expired() {
            end_capture();
            send_action(HotkeyAction::CaptureCancelled);
            return false;
        }
        finish_capture(binding_with_modifiers(vk));
    } else {
        let binding = current_binding();
        if vk != binding.vk || !modifiers_match(binding) {
            return false;
        }
        start_press();
    }
    MOUSE_SWALLOWED.fetch_or(bit, Ordering::SeqCst);
    true
}

/// The keys whose name is not their character: the label that the keycap
/// shows, then each name that the settings file can use for the key.
const KEY_NAMES: &[(u16, &str, &[&str])] = &[
    (VK_MOUSE4, "Mouse 4", &["mouse4", "xbutton1"]),
    (VK_MOUSE5, "Mouse 5", &["mouse5", "xbutton2"]),
    (0x08, "Backspace", &["backspace"]),
    (0x09, "Tab", &["tab"]),
    (0x0D, "Enter", &["enter", "return"]),
    (0x20, "Space", &["space"]),
    (0xBA, ";", &[";", "semicolon"]),
    (0xBB, "=", &["=", "plus", "equal"]),
    (0xBC, ",", &[",", "comma"]),
    (0xBD, "-", &["-", "minus"]),
    (0xBE, ".", &[".", "period", "dot"]),
    (0xBF, "/", &["/", "slash"]),
    (0xC0, "`", &["`", "grave"]),
    (0xDB, "[", &["["]),
    (0xDC, "\\", &["\\", "backslash"]),
    (0xDD, "]", &["]"]),
    (0xDE, "'", &["'", "quote"]),
];

fn parse_vk(token: &str) -> Option<u16> {
    let lower = token.trim().to_ascii_lowercase().replace(' ', "");
    if let Some((vk, ..)) = KEY_NAMES
        .iter()
        .find(|(_, _, names)| names.contains(&lower.as_str()))
    {
        return Some(*vk);
    }
    if let Some(num) = lower.strip_prefix('f') {
        let n: u16 = num.parse().ok()?;
        if (1..=24).contains(&n) {
            return Some(0x6F + n);
        }
    }
    let mut chars = lower.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphanumeric() => Some(ch.to_ascii_uppercase() as u16),
        _ => None,
    }
}

fn vk_label(vk: u16) -> String {
    if let Some((_, label, _)) = KEY_NAMES.iter().find(|(key, ..)| *key == vk) {
        return (*label).to_string();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("Vk{vk:02X}"),
    }
}
