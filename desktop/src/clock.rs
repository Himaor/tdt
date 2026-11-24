//! The local time, for History and for the log.

/// `YYYY-MM-DD HH:MM:SS` in the local time zone.
#[cfg(windows)]
pub fn local_hms() -> String {
    // SAFETY: GetLocalTime takes no pointer and returns the time by value.
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// `YYYY-MM-DD HH:MM:SS` in the local time zone.
#[cfg(unix)]
pub fn local_hms() -> String {
    // SAFETY: `time` and `tm` are valid for the calls; localtime_r is the
    // thread-safe form and writes only into `tm`.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}
