//! One TDT for each user: a second start asks the running TDT to open its
//! panel, then ends.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;
#[cfg(target_os = "macos")]
pub use mac::{claim_instance, take_show_request, InstanceGuard};
#[cfg(windows)]
pub use win::{claim_instance, take_show_request, InstanceGuard};

pub enum InstanceClaim {
    Owned(InstanceGuard),
    AlreadyRunning,
    Failed(String),
}
