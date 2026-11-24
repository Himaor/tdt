//! The Windows single-instance claim: a named mutex, and a named event that
//! a second start sets so the running TDT opens its panel.

use super::InstanceClaim;
use parking_lot::Mutex;

/// The single-instance mutex. Setup (`packaging/tdt-setup`, `INSTANCE_MUTEX`)
/// waits on this name for TDT to quit: change both together.
const INSTANCE_MUTEX: windows::core::PCWSTR = windows::core::w!("Local\\TDT-TalkDontType");
/// A second start sets this event, and the running TDT opens its panel.
const SHOW_EVENT: windows::core::PCWSTR = windows::core::w!("Local\\TDT-ShowOverlay");

pub struct InstanceGuard {
    mutex: windows::Win32::Foundation::HANDLE,
    show_event: windows::Win32::Foundation::HANDLE,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: both handles come from `claim_instance` and are closed once, here. The slot is cleared first,
        // so `take_show_request` cannot use the event after it closes.
        unsafe {
            *show_event_slot().lock() = None;
            let _ = windows::Win32::Foundation::CloseHandle(self.show_event);
            let _ = windows::Win32::Foundation::CloseHandle(self.mutex);
        }
    }
}

fn show_event_slot() -> &'static Mutex<Option<isize>> {
    static SLOT: Mutex<Option<isize>> = Mutex::new(None);
    &SLOT
}

pub fn take_show_request() -> bool {
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::WaitForSingleObject;
    // The lock stays held, so `InstanceGuard::drop` cannot close the event mid-call.
    let slot = show_event_slot().lock();
    let Some(raw) = *slot else {
        return false;
    };
    // SAFETY: the slot holds the event until `InstanceGuard::drop` clears it under this
    // lock, so the handle is open. A zero timeout only reads the event state.
    unsafe { WaitForSingleObject(HANDLE(raw as _), 0) == WAIT_OBJECT_0 }
}

fn signal_running_instance() {
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    // SAFETY: the name is a static null-terminated string, and the opened handle is closed after use.
    unsafe {
        if let Ok(handle) = OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT) {
            let _ = SetEvent(handle);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
    }
}

pub fn claim_instance() -> InstanceClaim {
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::{CreateEventW, CreateMutexW};

    // SAFETY: Win32 object calls with static names. Each handle is closed on every failure path or owned
    // by the returned guard.
    unsafe {
        // CreateMutexW's wrapper does not surface ERROR_ALREADY_EXISTS.
        // Open the existing object first so a second launch cannot start
        // another speech worker.
        if let Ok(existing) = windows::Win32::System::Threading::OpenMutexW(
            windows::Win32::System::Threading::SYNCHRONIZATION_ACCESS_RIGHTS(0x00100000),
            false,
            INSTANCE_MUTEX,
        ) {
            let _ = windows::Win32::Foundation::CloseHandle(existing);
            signal_running_instance();
            return InstanceClaim::AlreadyRunning;
        }
        match CreateMutexW(None, true, INSTANCE_MUTEX) {
            Ok(mutex) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let _ = windows::Win32::Foundation::CloseHandle(mutex);
                    signal_running_instance();
                    InstanceClaim::AlreadyRunning
                } else {
                    match CreateEventW(None, false, false, SHOW_EVENT) {
                        Ok(show_event) => {
                            *show_event_slot().lock() = Some(show_event.0 as isize);
                            InstanceClaim::Owned(InstanceGuard { mutex, show_event })
                        }
                        Err(error) => {
                            let _ = windows::Win32::Foundation::CloseHandle(mutex);
                            InstanceClaim::Failed(format!("Could not start TDT: {error}"))
                        }
                    }
                }
            }
            Err(error) => InstanceClaim::Failed(format!("Could not start TDT: {error}")),
        }
    }
}
