//! The macOS side of the system calls: AppKit through `objc2`, and the C
//! frameworks (CoreGraphics, CoreFoundation, Accessibility) through `ffi`.
//! Each Windows module that has a macOS form calls into here.

pub mod ffi;

use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicPtr, Ordering};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// An `NSRect`: AppKit screen points, origin at the bottom left of the main screen.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.origin.x
            && point.x < self.origin.x + self.size.width
            && point.y >= self.origin.y
            && point.y < self.origin.y + self.size.height
    }
}

// SAFETY: the layouts are `#[repr(C)]` with the fields of CGPoint, CGSize,
// and CGRect in the same order, and the encodings name those structs.
unsafe impl Encode for Point {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: see `Point`.
unsafe impl Encode for Size {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
// SAFETY: see `Point`.
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Size::ENCODING]);
}

type Id = *mut AnyObject;

/// Text of an `NSString`, or `None` for nil.
///
/// # Safety
/// `string` must be nil or a live `NSString`.
unsafe fn string_of(string: Id) -> Option<String> {
    if string.is_null() {
        return None;
    }
    // SAFETY: the caller gives a live NSString. UTF8String returns a pointer
    // that lives as long as the string, and it is copied at once.
    unsafe {
        let utf8: *const c_char = msg_send![string, UTF8String];
        (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
    }
}

fn workspace() -> Id {
    // SAFETY: NSWorkspace's shared instance can be used from any thread.
    unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] }
}

/// The process ID of the app in front: the app that gets the typed text.
pub fn frontmost_pid() -> Option<i32> {
    // SAFETY: frontmostApplication returns nil or an NSRunningApplication.
    unsafe {
        let app: Id = msg_send![workspace(), frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        (pid > 0).then_some(pid)
    }
}

fn running_app(pid: i32) -> Id {
    // SAFETY: the class method returns nil for a process that is not an app.
    unsafe {
        msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ]
    }
}

/// The name and the bundle ID of the app `pid`.
pub fn app_identity(pid: i32) -> Option<(String, String)> {
    let app = running_app(pid);
    if app.is_null() {
        return None;
    }
    // SAFETY: `app` is a live NSRunningApplication; both properties are nil or NSString.
    unsafe {
        let name = string_of(msg_send![app, localizedName]).unwrap_or_else(|| "?".into());
        let bundle = string_of(msg_send![app, bundleIdentifier]).unwrap_or_default();
        Some((name, bundle))
    }
}

/// True while the app `pid` runs.
pub fn app_is_running(pid: i32) -> bool {
    let app = running_app(pid);
    // SAFETY: `app` is a live NSRunningApplication.
    !app.is_null() && unsafe { !msg_send![app, isTerminated] }
}

/// Asks macOS to bring the app `pid` to the front. Returns false when it refuses.
pub fn activate_app(pid: i32) -> bool {
    let app = running_app(pid);
    if app.is_null() {
        return false;
    }
    /// `NSApplicationActivateIgnoringOtherApps`. macOS 14 ignores the flag
    /// and activates the app as a cooperative request.
    const IGNORING_OTHER_APPS: usize = 1 << 1;
    // SAFETY: `app` is a live NSRunningApplication.
    unsafe {
        let activated: Bool = msg_send![app, activateWithOptions: IGNORING_OTHER_APPS];
        activated.as_bool()
    }
}

/// The user asked macOS to reduce motion (System Settings > Accessibility > Display).
pub fn reduce_motion() -> bool {
    // SAFETY: a plain property of the shared workspace.
    unsafe {
        let reduce: Bool = msg_send![workspace(), accessibilityDisplayShouldReduceMotion];
        reduce.as_bool()
    }
}

/// Makes TDT a menu bar app: no Dock icon and no app menu. GPUI makes every
/// app a regular app when it finishes launching, so this runs after that.
/// Must run on the main thread.
pub fn become_accessory_app() {
    /// `NSApplicationActivationPolicyAccessory`.
    const ACCESSORY: isize = 1;
    // SAFETY: called on the main thread, after NSApplication exists.
    unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: ACCESSORY];
    }
}

/// The pointer, in AppKit screen points.
pub fn mouse_location() -> Point {
    // SAFETY: a class property; Apple documents it as safe off the main thread.
    unsafe { msg_send![class!(NSEvent), mouseLocation] }
}

/// The work area (the frame without the menu bar and the Dock) of each
/// screen with its full frame. The first screen holds the menu bar.
///
/// Must run on the main thread.
pub fn screens() -> Vec<(Rect, Rect)> {
    // SAFETY: NSScreen calls on the main thread. `screens` is an NSArray of NSScreen.
    unsafe {
        let screens: Id = msg_send![class!(NSScreen), screens];
        if screens.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![screens, count];
        (0..count)
            .map(|index| {
                let screen: Id = msg_send![screens, objectAtIndex: index];
                let frame: Rect = msg_send![screen, frame];
                let visible: Rect = msg_send![screen, visibleFrame];
                (frame, visible)
            })
            .collect()
    }
}

/// The overlay's `NSWindow`, set once when GPUI opens it.
static OVERLAY: AtomicPtr<AnyObject> = AtomicPtr::new(std::ptr::null_mut());

/// Remembers the overlay window from the `NSView` that GPUI draws into.
///
/// # Safety
/// `ns_view` must be the live content view of the overlay window, and the
/// window must stay open for the life of the process (TDT never closes it).
pub unsafe fn set_overlay_view(ns_view: *mut c_void) {
    // SAFETY: the caller gives a live NSView; `window` returns its NSWindow.
    let window: Id = unsafe { msg_send![ns_view.cast::<AnyObject>(), window] };
    OVERLAY.store(window, Ordering::SeqCst);
}

fn overlay() -> Option<Id> {
    let window = OVERLAY.load(Ordering::SeqCst);
    (!window.is_null()).then_some(window)
}

/// The overlay's frame. Must run on the main thread.
pub fn overlay_frame() -> Option<Rect> {
    let window = overlay()?;
    // SAFETY: the overlay lives for the process; called on the main thread.
    Some(unsafe { msg_send![window, frame] })
}

/// Moves the overlay so that its bottom left corner is at `origin`. Must run
/// on the main thread.
pub fn set_overlay_origin(origin: Point) {
    if let Some(window) = overlay() {
        // SAFETY: the overlay lives for the process; called on the main thread.
        unsafe {
            let _: () = msg_send![window, setFrameOrigin: origin];
        }
    }
}

/// Removes the frame that AppKit draws on the transparent overlay: the
/// shadow (a thin outline around the whole canvas) and the titled style,
/// whose rim is a line across the top of the canvas. GPUI gives a window
/// without a title bar the titled style; the panel keeps its other styles
/// (non-activating, full-size content). Must run on the main thread.
pub fn remove_overlay_chrome() {
    /// `NSWindowStyleMaskTitled`. macOS draws a rim on each titled window,
    /// and on a transparent window the top of the rim is a line in the air.
    const TITLED: usize = 1 << 0;
    if let Some(window) = overlay() {
        // SAFETY: the overlay lives for the process; called on the main thread.
        unsafe {
            let _: () = msg_send![window, setHasShadow: Bool::NO];
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask & !TITLED];
        }
    }
}

/// Clicks pass through the overlay to the app below while `ignore` is true.
/// Must run on the main thread.
pub fn set_overlay_ignores_mouse(ignore: bool) {
    if let Some(window) = overlay() {
        // SAFETY: the overlay lives for the process; called on the main thread.
        unsafe {
            let _: () = msg_send![window, setIgnoresMouseEvents: Bool::new(ignore)];
        }
    }
}

/// Gives the open panel the keyboard without activating TDT, so a click in
/// another app takes the key status away and closes the panel. Must run on
/// the main thread.
pub fn make_overlay_key() {
    if let Some(window) = overlay() {
        // SAFETY: the overlay lives for the process; called on the main thread.
        unsafe {
            let _: () = msg_send![window, makeKeyWindow];
        }
    }
}

/// Puts the overlay back on top without activating TDT. Must run on the main thread.
pub fn order_overlay_front() {
    if let Some(window) = overlay() {
        // SAFETY: the overlay lives for the process; called on the main thread.
        unsafe {
            let _: () = msg_send![window, orderFrontRegardless];
        }
    }
}

/// Runs `work` on the main thread, after the current event.
pub fn on_main_thread(work: impl FnOnce() + Send + 'static) {
    extern "C" fn trampoline(context: *mut c_void) {
        // SAFETY: `context` is the box that `on_main_thread` leaked, used once.
        let work = unsafe { Box::from_raw(context.cast::<Box<dyn FnOnce() + Send>>()) };
        work();
    }
    let work: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(work));
    // SAFETY: the main queue lives for the process, and the trampoline takes
    // the box back exactly once.
    unsafe {
        ffi::dispatch_async_f(
            std::ptr::addr_of!(ffi::_dispatch_main_q).cast_mut().cast(),
            Box::into_raw(work).cast(),
            trampoline,
        );
    }
}

/// True when the user allowed TDT in Privacy & Security > Accessibility.
/// macOS drops TDT's key events, and gives TDT no key events, without it.
pub fn accessibility_trusted() -> bool {
    // SAFETY: takes no arguments.
    unsafe { ffi::AXIsProcessTrusted() != 0 }
}

/// Shows the system prompt that opens Privacy & Security > Accessibility,
/// once for each launch that is not yet trusted.
pub fn prompt_for_accessibility() -> bool {
    // SAFETY: the dictionary is built from the CFString constant and the CF
    // boolean, with the CF type callbacks, and released after the call.
    unsafe {
        let keys = [ffi::kAXTrustedCheckOptionPrompt.cast::<c_void>()];
        let values = [ffi::kCFBooleanTrue.cast::<c_void>()];
        let options = ffi::CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &ffi::kCFTypeDictionaryKeyCallBacks,
            &ffi::kCFTypeDictionaryValueCallBacks,
        );
        let trusted = ffi::AXIsProcessTrustedWithOptions(options) != 0;
        if !options.is_null() {
            ffi::CFRelease(options);
        }
        trusted
    }
}

/// The bundle ID of the running app, or `None` outside an app bundle
/// (`cargo run`).
pub fn bundle_id() -> Option<String> {
    // SAFETY: the main bundle always exists; bundleIdentifier is nil or an NSString.
    unsafe {
        let bundle: Id = msg_send![class!(NSBundle), mainBundle];
        string_of(msg_send![bundle, bundleIdentifier])
    }
}

/// Removes TDT's Accessibility and Input Monitoring entries from Privacy &
/// Security. macOS ties the entry of an app without a Developer ID to the
/// exact build, so after an update the old entry is still on in the list but
/// counts for nothing, and the user cannot tell why the shortcut is dead.
/// Without the entry, the prompt that follows adds one for this build.
/// Call it only when TDT is not trusted: it can remove no working grant then.
pub fn reset_stale_permissions() {
    let Some(bundle) = bundle_id() else {
        return;
    };
    for service in ["Accessibility", "ListenEvent"] {
        let reset = std::process::Command::new("/usr/bin/tccutil")
            .args(["reset", service, &bundle])
            .output();
        if let Err(error) = reset {
            voice_stt_desktop::log::note(&format!("Could not reset the {service} entry: {error}"));
        }
    }
}

/// True when the user allowed TDT in Privacy & Security > Input Monitoring.
pub fn input_monitoring_allowed() -> bool {
    // SAFETY: takes no arguments.
    unsafe { ffi::CGPreflightListenEventAccess() }
}

/// Shows the Input Monitoring prompt, once for each start of TDT.
pub fn request_input_monitoring() {
    // SAFETY: takes no arguments.
    unsafe {
        let _ = ffi::CGRequestListenEventAccess();
    }
}

/// True while a password field (or an app such as a terminal with Secure
/// Keyboard Entry) holds secure input. macOS then gives no app typed keys
/// from TDT, and the key tap sees no keys.
pub fn secure_input_on() -> bool {
    // SAFETY: takes no arguments.
    unsafe { ffi::IsSecureEventInputEnabled() != 0 }
}

/// What the user allowed for the microphone (Privacy & Security > Microphone).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicrophoneAccess {
    Allowed,
    /// The user has not answered yet.
    NotAsked,
    /// Denied by the user or by a profile. macOS then gives TDT silence, not an error.
    Denied,
}

pub fn microphone_access() -> MicrophoneAccess {
    // SAFETY: a class method of AVCaptureDevice with the AVFoundation constant.
    let status: isize = unsafe {
        msg_send![
            class!(AVCaptureDevice),
            authorizationStatusForMediaType: ffi::AVMediaTypeAudio
        ]
    };
    // AVAuthorizationStatus: 0 not determined, 1 restricted, 2 denied, 3 authorized.
    match status {
        3 => MicrophoneAccess::Allowed,
        0 => MicrophoneAccess::NotAsked,
        _ => MicrophoneAccess::Denied,
    }
}

/// Shows the macOS microphone prompt and returns at once. `answered` runs on
/// a queue of AVFoundation with the user's answer. Opening the microphone
/// before the answer would block the calling thread until the user answers.
pub fn request_microphone_access(answered: impl Fn(bool) + Send + 'static) {
    let handler = block2::RcBlock::new(move |granted: Bool| answered(granted.as_bool()));
    // SAFETY: AVCaptureDevice copies the block and calls it once.
    unsafe {
        let _: () = msg_send![
            class!(AVCaptureDevice),
            requestAccessForMediaType: ffi::AVMediaTypeAudio,
            completionHandler: &*handler
        ];
    }
}
