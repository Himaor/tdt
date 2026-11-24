//! C declarations of the macOS frameworks that TDT calls. Only the calls
//! that TDT makes are here.
#![allow(non_upper_case_globals, non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

pub type CFTypeRef = *const c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CGEventTapProxy = *mut c_void;
pub type CGEventType = u32;
pub type CGEventMask = u64;
pub type CGEventFlags = u64;
pub type CGKeyCode = u16;
pub type CGEventField = u32;
pub type CGEventSourceStateID = i32;
pub type Boolean = u8;

pub type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    kind: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

// Event types.
pub const kCGEventKeyDown: CGEventType = 10;
pub const kCGEventKeyUp: CGEventType = 11;
pub const kCGEventFlagsChanged: CGEventType = 12;
pub const kCGEventOtherMouseDown: CGEventType = 25;
pub const kCGEventOtherMouseUp: CGEventType = 26;
pub const kCGEventTapDisabledByTimeout: CGEventType = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: CGEventType = 0xFFFF_FFFF;

// Event flags: the modifier keys.
pub const kCGEventFlagMaskShift: CGEventFlags = 0x0002_0000;
pub const kCGEventFlagMaskControl: CGEventFlags = 0x0004_0000;
pub const kCGEventFlagMaskAlternate: CGEventFlags = 0x0008_0000;
pub const kCGEventFlagMaskCommand: CGEventFlags = 0x0010_0000;

// Event fields.
pub const kCGMouseEventButtonNumber: CGEventField = 3;
pub const kCGKeyboardEventKeycode: CGEventField = 9;
/// The field that a program can set on the events that it posts.
pub const kCGEventSourceUserData: CGEventField = 42;
/// The source state of an event: `kCGEventSourceStateHIDSystemState` for a real key.
pub const kCGEventSourceStateID: CGEventField = 45;

// Event tap places and options.
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;
/// Post at the point where the hardware events enter the system.
pub const kCGHIDEventTap: u32 = 0;

// Event source states.
pub const kCGEventSourceStatePrivate: CGEventSourceStateID = -1;
pub const kCGEventSourceStateHIDSystemState: CGEventSourceStateID = 1;

#[repr(C)]
pub struct CFDictionaryKeyCallBacks {
    _private: [u8; 0],
}

#[repr(C)]
pub struct CFDictionaryValueCallBacks {
    _private: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub static kCFRunLoopCommonModes: CFStringRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFTypeDictionaryKeyCallBacks: CFDictionaryKeyCallBacks;
    pub static kCFTypeDictionaryValueCallBacks: CFDictionaryValueCallBacks;

    pub fn CFRelease(object: CFTypeRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub static kCFRunLoopDefaultMode: CFStringRef;
    pub fn CFRetain(object: CFTypeRef) -> CFTypeRef;
    pub fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: bool) -> i32;
    pub fn CFRunLoopStop(run_loop: CFRunLoopRef);
    pub fn CFRunLoopAddSource(
        run_loop: CFRunLoopRef,
        source: CFRunLoopSourceRef,
        mode: CFStringRef,
    );
    pub fn CFMachPortCreateRunLoopSource(
        allocator: CFTypeRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const CFDictionaryKeyCallBacks,
        value_callbacks: *const CFDictionaryValueCallBacks,
    ) -> CFDictionaryRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);

    pub fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    pub fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
    #[cfg(test)]
    pub fn CGEventSetType(event: CGEventRef, kind: CGEventType);
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: CGEventField) -> i64;
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: CGEventField, value: i64);
    pub fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        key: CGKeyCode,
        key_down: bool,
    ) -> CGEventRef;
    pub fn CGEventKeyboardSetUnicodeString(event: CGEventRef, length: usize, string: *const u16);
    pub fn CGEventPost(tap: u32, event: CGEventRef);

    pub fn CGEventSourceCreate(state: CGEventSourceStateID) -> CGEventSourceRef;
    /// Input Monitoring (macOS 10.15 and later).
    pub fn CGPreflightListenEventAccess() -> bool;
    pub fn CGRequestListenEventAccess() -> bool;
    pub fn CGEventSourceFlagsState(state: CGEventSourceStateID) -> CGEventFlags;
    pub fn CGEventSourceKeyState(state: CGEventSourceStateID, key: CGKeyCode) -> bool;
    pub fn CGEventSourceButtonState(state: CGEventSourceStateID, button: u32) -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    pub static kAXTrustedCheckOptionPrompt: CFStringRef;
    pub fn AXIsProcessTrusted() -> Boolean;
    pub fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> Boolean;
}

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {
    /// An `NSString`: the media type of audio capture.
    pub static AVMediaTypeAudio: *mut objc2::runtime::AnyObject;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    pub fn IsSecureEventInputEnabled() -> Boolean;
}

// libdispatch is part of libSystem, which every program links.
extern "C" {
    /// The queue of the main thread. `dispatch_get_main_queue()` is a C macro for its address.
    pub static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}
