//! The Windows form of the shortcut listener: low-level keyboard and mouse
//! hooks on a thread of their own, with a watch that installs them again
//! when Windows removes them.

use super::*;

impl HotkeyListener {
    pub fn start() -> Result<(Self, Receiver<HotkeyAction>), String> {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, KillTimer, SetTimer, MSG, WM_TIMER,
        };

        let (sender, receiver) = async_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let running = Arc::new(AtomicBool::new(true));
        let running_thread = Arc::clone(&running);
        {
            let mut guard = SENDER.lock();
            if guard.is_some() {
                return Err("Hotkey listener is already running.".to_string());
            }
            *guard = Some(sender);
        }

        // SAFETY: this thread installs the hooks, runs their message loop, and removes them, as
        // WH_KEYBOARD_LL and WH_MOUSE_LL require. `msg` outlives the loop. The timer has no
        // window, so its messages come to this loop.
        std::thread::spawn(move || unsafe {
            // Each key event of every app waits for this thread, and so does
            // each character TDT types.
            crate::priority::raise_thread();
            HOOK_THREAD_ID.store(
                windows::Win32::System::Threading::GetCurrentThreadId(),
                Ordering::SeqCst,
            );
            match install_hooks() {
                Err(error) => {
                    let _ = ready_tx.send(Err(format!(
                        "Could not install the keyboard hook. Restart TDT. {error}"
                    )));
                    return;
                }
                Ok(Some(mouse_error)) => voice_stt_desktop::log::note(&format!(
                    "Could not install the mouse hook, so a mouse button cannot be the shortcut. {mouse_error}"
                )),
                Ok(None) => {}
            }
            let _ = ready_tx.send(Ok(()));
            let timer = SetTimer(HWND::default(), 0, HOOK_WATCH_MS, None);
            let mut watch = HookWatch::default();
            let mut msg = MSG::default();
            while running_thread.load(Ordering::Relaxed)
                && GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool()
            {
                if msg.message == WM_TIMER && msg.hwnd == HWND::default() {
                    watch.check();
                } else {
                    let _ = DispatchMessageW(&msg);
                }
            }
            let _ = KillTimer(HWND::default(), timer);
            remove_hooks();
            if running_thread.load(Ordering::Relaxed) {
                voice_stt_desktop::log::note("The message loop of the hooks ended. The shortcut does not work until TDT starts again.");
            }
        });

        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok((
                Self {
                    running: running.clone(),
                },
                receiver,
            )),
            failed => {
                // A hook that installs after the timeout must not keep
                // swallowing the shortcut for a listener that is gone, and a
                // later start must not find the old sender.
                drop(Self { running });
                *SENDER.lock() = None;
                Err(match failed {
                    Ok(Err(error)) => error,
                    _ => "Could not install the keyboard hook. Restart TDT.".to_string(),
                })
            }
        }
    }

    pub(super) fn shutdown_thread(&self) {
        self.running.store(false, Ordering::SeqCst);
        let thread_id = HOOK_THREAD_ID.load(Ordering::SeqCst);
        if thread_id != 0 {
            // SAFETY: PostThreadMessageW only queues WM_QUIT; a dead thread id makes it fail.
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
                let _ = PostThreadMessageW(
                    thread_id,
                    WM_QUIT,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        }
    }
}

static HOOK_THREAD_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static HOOK_PTR: std::sync::atomic::AtomicPtr<core::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

static MOUSE_HOOK_PTR: std::sync::atomic::AtomicPtr<core::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// When a hook last got an event, as a Windows tick count.
static HOOK_SEEN_TICK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The time between two checks of the hooks.
const HOOK_WATCH_MS: u32 = 2_000;
/// Input this much newer than the last hook event did not reach the hooks.
const UNSEEN_INPUT_MS: i32 = 1_000;

/// Key events and mouse events that the hooks got since the last check of the
/// watch, for its log line.
static KEY_EVENTS: AtomicU32 = AtomicU32::new(0);
static MOUSE_EVENTS: AtomicU32 = AtomicU32::new(0);

pub(super) fn tick_count() -> u32 {
    // SAFETY: GetTickCount takes no pointers.
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

fn note_hook_event(events: &AtomicU32) {
    HOOK_SEEN_TICK.store(tick_count(), Ordering::Relaxed);
    events.fetch_add(1, Ordering::Relaxed);
}

/// Installs the keyboard hook and the mouse hook on the calling thread.
/// Without the mouse hook only a mouse shortcut fails, so that is no error:
/// the result then holds the cause.
unsafe fn install_hooks() -> Result<Option<String>, String> {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_KEYBOARD_LL, WH_MOUSE_LL};
    let module = GetModuleHandleW(None).unwrap_or_default();
    let keyboard = SetWindowsHookExW(
        WH_KEYBOARD_LL,
        Some(hotkey_hook_proc),
        HINSTANCE(module.0),
        0,
    )
    .map_err(|error| error.to_string())?;
    HOOK_PTR.store(keyboard.0, Ordering::SeqCst);
    let mouse_error =
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), HINSTANCE(module.0), 0) {
            Ok(mouse) => {
                MOUSE_HOOK_PTR.store(mouse.0, Ordering::SeqCst);
                None
            }
            Err(error) => Some(error.to_string()),
        };
    HOOK_SEEN_TICK.store(tick_count(), Ordering::Relaxed);
    Ok(mouse_error)
}

/// What Windows said when TDT removed one hook.
enum HookEnd {
    /// TDT had no hook of this kind.
    NotInstalled,
    /// Windows still had the hook.
    Removed,
    /// Windows did not know the hook: it removed the hook before.
    Gone(String),
}

impl std::fmt::Display for HookEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("was not installed"),
            Self::Removed => f.write_str("was installed"),
            Self::Gone(error) => write!(f, "was gone ({error})"),
        }
    }
}

/// Removes the hooks on the thread that installed them. Returns what Windows
/// said for the mouse hook and for the keyboard hook.
unsafe fn remove_hooks() -> [HookEnd; 2] {
    use windows::Win32::UI::WindowsAndMessaging::{UnhookWindowsHookEx, HHOOK};
    [&MOUSE_HOOK_PTR, &HOOK_PTR].map(|slot| {
        let hook = slot.swap(std::ptr::null_mut(), Ordering::SeqCst);
        if hook.is_null() {
            return HookEnd::NotInstalled;
        }
        match UnhookWindowsHookEx(HHOOK(hook)) {
            Ok(()) => HookEnd::Removed,
            Err(error) => HookEnd::Gone(error.to_string()),
        }
    })
}

/// Windows removes a low-level hook that answers too slowly, and gives no
/// notice: the shortcut then does nothing until TDT starts again. Windows
/// also reports the time of the last input. Input that no hook saw means the
/// hooks are gone, so this installs them again.
///
/// The hooks also see nothing while an administrator window has the input:
/// that case is no reason for an install. For other input that the hooks
/// cannot see (the lock screen), the installs become less frequent, down to
/// about one each minute, until a hook sees an event again.
#[derive(Default)]
struct HookWatch {
    /// Installs in sequence that no hook event followed.
    failed: u32,
    /// Checks to skip before the next install.
    skip: u32,
    /// The last line in the log. Touch input can cause an install each
    /// minute, and the log needs only one line for that.
    logged: Option<Instant>,
    /// Installs since that line that got no line.
    unlogged: u32,
}

impl HookWatch {
    /// Runs on the hook thread.
    unsafe fn check(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        let mut last = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // The events of the time since the last check, for the log line.
        let keys = KEY_EVENTS.swap(0, Ordering::Relaxed);
        let mouse = MOUSE_EVENTS.swap(0, Ordering::Relaxed);
        if !GetLastInputInfo(&mut last).as_bool() {
            return;
        }
        // Tick counts wrap, so compare them through their difference.
        let seen = HOOK_SEEN_TICK.load(Ordering::Relaxed);
        let unseen_ms = last.dwTime.wrapping_sub(seen) as i32;
        if unseen_ms < UNSEEN_INPUT_MS {
            self.failed = 0;
            self.skip = 0;
            return;
        }
        // An administrator window in front takes the input: Windows gives it
        // to no hook of a normal program. The hooks are still there.
        let front = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        let front_app = (!front.is_invalid()).then(|| crate::target::Target::of(front.0 as isize));
        if front_app
            .as_ref()
            .is_some_and(crate::target::Target::blocks_input)
        {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        let [mouse_end, keyboard_end] = remove_hooks();
        let installed = install_hooks();
        // A hook that Windows removed is the case that this watch is for. A
        // hook that was still installed means input that no hook can get.
        // The line holds the data that tells the two apart.
        let gone =
            matches!(mouse_end, HookEnd::Gone(_)) || matches!(keyboard_end, HookEnd::Gone(_));
        let line_due = |limit: u64| {
            self.logged
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(limit))
        };
        if (gone && line_due(600)) || (self.failed == 0 && line_due(3600)) {
            self.logged = Some(Instant::now());
            let app = front_app.map_or_else(|| "no window".to_string(), |app| app.to_string());
            let result = match installed {
                Ok(None) => "TDT installed the hooks again.".to_string(),
                Ok(Some(error)) => {
                    format!(
                        "TDT installed the keyboard hook again, but not the mouse hook: {error}"
                    )
                }
                Err(error) => format!("TDT could not install the hooks again: {error}"),
            };
            voice_stt_desktop::log::note(&format!(
                "The hooks got no event for input that Windows reported {unseen_ms} ms after the last hook event. \
                 Since the check before: {keys} key events, {mouse} mouse events. Keyboard hook: {keyboard_end}. \
                 Mouse hook: {mouse_end}. App in front: {app}. {result} Installs with no line before this one: {}.",
                self.unlogged
            ));
            self.unlogged = 0;
        } else {
            self.unlogged += 1;
        }
        self.failed = (self.failed + 1).min(5);
        self.skip = (1 << self.failed) - 1;
    }
}

pub(super) fn key_down(vk: u16) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    // SAFETY: GetAsyncKeyState takes no pointers.
    unsafe { GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000 != 0 }
}

pub(super) fn modifiers_down() -> (bool, bool, bool, bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL, VK_RMENU,
        VK_RSHIFT, VK_RWIN, VK_SHIFT,
    };
    let ctrl = key_down(VK_CONTROL.0) || key_down(VK_LCONTROL.0) || key_down(VK_RCONTROL.0);
    let alt = key_down(VK_MENU.0) || key_down(VK_LMENU.0) || key_down(VK_RMENU.0);
    let shift = key_down(VK_SHIFT.0) || key_down(VK_LSHIFT.0) || key_down(VK_RSHIFT.0);
    let win = key_down(VK_LWIN.0) || key_down(VK_RWIN.0);
    (ctrl, alt, shift, win)
}

unsafe extern "system" fn hotkey_hook_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    if code >= 0 {
        note_hook_event(&KEY_EVENTS);
        let kbd = *(lparam.0 as *const KBDLLHOOKSTRUCT);
        let msg = wparam.0 as u32;
        let vk = kbd.vkCode as u16;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

        // Pass on the keys that TDT sends: auto-paste types text with
        // SendInput, and that text must never start a recording or be
        // mistaken for a new shortcut. Keys that other programs send (a remap
        // tool, a remote desktop) can be the shortcut.
        // With no listener left (a failed start), every key passes on.
        if kbd.dwExtraInfo == crate::keys::OWN_INPUT || !listening() {
            return CallNextHookEx(
                HHOOK(HOOK_PTR.load(Ordering::Relaxed)),
                code,
                wparam,
                lparam,
            );
        }
        let physical = kbd.flags & LLKHF_INJECTED != LLKHF_INJECTED;
        if on_key(vk, is_down, is_up, physical) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(
        HHOOK(HOOK_PTR.load(Ordering::Relaxed)),
        code,
        wparam,
        lparam,
    )
}

/// Only Mouse 4 and Mouse 5 events are read. All other mouse events pass on
/// at once, because this hook runs for each mouse move on the system.
unsafe extern "system" fn mouse_hook_proc(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, HHOOK, MSLLHOOKSTRUCT, WM_XBUTTONDOWN, WM_XBUTTONUP,
    };

    if code >= 0 {
        note_hook_event(&MOUSE_EVENTS);
    }
    let msg = wparam.0 as u32;
    if code >= 0 && (msg == WM_XBUTTONDOWN || msg == WM_XBUTTONUP) {
        let info = *(lparam.0 as *const MSLLHOOKSTRUCT);
        let vk = match info.mouseData >> 16 {
            1 => Some(VK_MOUSE4),
            2 => Some(VK_MOUSE5),
            _ => None,
        };
        // A button that another program sends can be the shortcut. TDT sends none.
        if let Some(vk) = vk.filter(|_| listening()) {
            if handle_mouse_button(vk, msg == WM_XBUTTONDOWN) {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(
        HHOOK(MOUSE_HOOK_PTR.load(Ordering::Relaxed)),
        code,
        wparam,
        lparam,
    )
}
