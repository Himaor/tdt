//! Thread priority for the path from the key to the typed text.

/// For a thread between the key and the typed text. These threads sleep
/// nearly all the time, so the priority costs other programs nothing, and a
/// CPU load at normal priority no longer stands in front of each step.
#[cfg(windows)]
pub fn raise_thread() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    // SAFETY: the pseudo handle names the calling thread; the call takes no
    // pointer. A failed call only leaves the thread at normal priority.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL) };
}

/// The macOS form: the user-interactive QoS class, which also keeps the
/// thread on the performance cores of Apple silicon.
#[cfg(target_os = "macos")]
pub fn raise_thread() {
    // SAFETY: the call changes only the QoS of the calling thread. A failed
    // call leaves the thread at its default class.
    let _ = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
}
