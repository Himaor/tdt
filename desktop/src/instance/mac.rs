//! The macOS single-instance claim: an exclusive `flock` on a file in the
//! data folder, and a Unix socket next to it. A second start connects to the
//! socket, and the running TDT opens its panel. macOS releases the lock when
//! the process ends, also after a crash.

use super::InstanceClaim;
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

const LOCK_FILE: &str = "tdt.lock";
const SOCKET_FILE: &str = "tdt.sock";

/// The socket of the running TDT. `take_show_request` reads it.
static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

pub struct InstanceGuard {
    _lock: File,
    socket: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        *LISTENER.lock() = None;
        let _ = std::fs::remove_file(&self.socket);
    }
}

pub fn claim_instance() -> InstanceClaim {
    match try_claim() {
        Ok(Some(guard)) => InstanceClaim::Owned(guard),
        Ok(None) => InstanceClaim::AlreadyRunning,
        Err(error) => InstanceClaim::Failed(format!("Could not start TDT: {error}")),
    }
}

fn try_claim() -> Result<Option<InstanceGuard>, String> {
    let dir = voice_stt_desktop::data_dir().ok_or("the home folder is not known")?;
    create_private_dir(&dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .map_err(|e| format!("could not open the lock file: {e}"))?;
    // SAFETY: flock on a descriptor that `lock` owns; it takes no pointer.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            signal_running_instance(&dir);
            return Ok(None);
        }
        return Err(format!("could not lock the lock file: {error}"));
    }
    // The lock is ours, so a socket file that is still there is from a TDT
    // that ended without removing it.
    let socket = dir.join(SOCKET_FILE);
    let _ = std::fs::remove_file(&socket);
    match UnixListener::bind(&socket).and_then(|listener| {
        listener.set_nonblocking(true)?;
        Ok(listener)
    }) {
        Ok(listener) => *LISTENER.lock() = Some(listener),
        // TDT runs without it: only a second start cannot open the panel.
        Err(error) => voice_stt_desktop::log::note(&format!(
            "Could not open the socket for a second start: {error}"
        )),
    }
    Ok(Some(InstanceGuard {
        _lock: lock,
        socket,
    }))
}

/// The data folder, readable only by the user, as the rest of the home folder.
fn create_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("could not create {}: {error}", dir.display())),
    }
}

fn signal_running_instance(dir: &Path) {
    if let Ok(mut stream) = UnixStream::connect(dir.join(SOCKET_FILE)) {
        let _ = stream.write_all(b"show");
    }
}

/// True when a second start asked for the panel since the last call.
pub fn take_show_request() -> bool {
    let guard = LISTENER.lock();
    let Some(listener) = guard.as_ref() else {
        return false;
    };
    let mut asked = false;
    while let Ok((stream, _)) = listener.accept() {
        drop(stream);
        asked = true;
    }
    asked
}
