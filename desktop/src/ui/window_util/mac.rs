//! The AppKit side of the overlay window.
//!
//! GPUI makes a `WindowKind::PopUp` window a non-activating panel at the
//! pop-up level that joins every Space and full-screen app, so the pill
//! already floats over the user's work and never takes focus. This module
//! adds what GPUI leaves to the app: the place on the screen, clicks that
//! pass through the window outside the pill, and the keyboard for the open
//! panel.

use crate::macos::{self, Point, Rect};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The gap between the bottom of the work area and the window, in points.
const BOTTOM_GAP: f64 = 70.0;
/// How often the pointer is checked against the pill. A click on the pill
/// within this time after the pointer arrives can miss it once.
const POINTER_CHECK: Duration = Duration::from_millis(40);

static EXPANDED: AtomicBool = AtomicBool::new(false);

/// When the panel last opened, and how long after that a loss of the key
/// status does not close it.
static OPENED_AT: Mutex<Option<Instant>> = Mutex::new(None);
const OPEN_GRACE: Duration = Duration::from_millis(600);

/// Logical (x, y, w, h) inside the canvas that takes input, from its top
/// left corner. `None` is the whole canvas.
static PILL_HIT_RECT: Mutex<Option<(f32, f32, f32, f32)>> = Mutex::new(None);

/// The overlay's frame, kept here so the pointer thread does not ask AppKit
/// off the main thread. Only this module moves the window.
static FRAME: Mutex<Option<Rect>> = Mutex::new(None);

/// Takes the overlay window from the `NSView` that GPUI draws into, puts it
/// at the bottom center of the main screen, and starts the click-through
/// check. Must run on the main thread, once.
///
/// # Safety
/// `ns_view` must be the live content view of the overlay window, which
/// stays open for the life of the process.
pub unsafe fn install_overlay(ns_view: *mut std::ffi::c_void) {
    // SAFETY: the caller's guarantee.
    unsafe { macos::set_overlay_view(ns_view) };
    macos::remove_overlay_chrome();
    macos::set_overlay_ignores_mouse(true);
    let screens = macos::screens();
    if let Some(&(_, work)) = screens.first() {
        place_at_bottom_center(work);
    }
    std::thread::Builder::new()
        .name("tdt-pointer".into())
        .spawn(follow_pointer)
        .ok();
}

/// Limit input on the collapsed overlay to `rect`.
pub fn set_pill_hit_rect(rect: (f32, f32, f32, f32)) {
    *PILL_HIT_RECT.lock() = Some(rect);
}

/// Switch between the pill and the open panel. The open panel takes the
/// keyboard, so a click in another app closes it (`observe_window_activation`
/// in `main`). Runs on the main thread.
pub fn set_window_mode(is_expanded: bool) {
    EXPANDED.store(is_expanded, Ordering::SeqCst);
    if is_expanded {
        *OPENED_AT.lock() = Some(Instant::now());
        macos::set_overlay_ignores_mouse(false);
        macos::order_overlay_front();
        macos::make_overlay_key();
        // A second start of TDT (the panel request) is an app that macOS
        // activates for a moment, and it takes the key status. After the
        // grace time the panel takes it back, so a later click elsewhere
        // closes it.
        std::thread::spawn(|| {
            std::thread::sleep(OPEN_GRACE);
            macos::on_main_thread(|| {
                if EXPANDED.load(Ordering::SeqCst) {
                    macos::make_overlay_key();
                }
            });
        });
    }
}

/// False just after the panel opens: a loss of the key status then comes
/// from the start of another app, not from the user.
pub fn closes_on_deactivation() -> bool {
    OPENED_AT.lock().is_none_or(|at| at.elapsed() >= OPEN_GRACE)
}

/// Nothing to do on macOS: GPUI keeps the panel at the pop-up level.
pub fn ensure_overlay_topmost() {}

/// Nothing to do on macOS: the window has no frame and never resizes.
pub fn lock_overlay_chrome() {}

/// Opens a file with its default program. A text file opens in the user's
/// text editor.
pub fn open_file(path: &std::path::Path) -> Result<(), String> {
    let mut open = std::process::Command::new("/usr/bin/open");
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
    {
        open.arg("-t");
    }
    let status = open
        .arg(path)
        .status()
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("macOS could not open {}", path.display()))
    }
}

/// False when the user asked macOS to reduce motion.
pub fn client_animations_enabled() -> bool {
    // Cached: this runs on every render, and the setting changes rarely.
    static CACHE: AtomicBool = AtomicBool::new(true);
    static LAST_CHECK: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST_CHECK.lock();
    if last.is_some_and(|at| at.elapsed() < Duration::from_millis(500)) {
        return CACHE.load(Ordering::Relaxed);
    }
    let enabled = !macos::reduce_motion();
    CACHE.store(enabled, Ordering::Relaxed);
    *last = Some(Instant::now());
    enabled
}

/// The process ID of the app that gets the text: the app in front. Never TDT.
pub fn dictation_target_window() -> Option<isize> {
    let pid = macos::frontmost_pid()?;
    (pid as u32 != std::process::id()).then_some(pid as isize)
}

/// Moves the pill to the screen with the pointer when it is on another
/// screen, so a pill on a second screen does not listen out of view. The
/// open panel does not move. The target does not matter: on macOS the
/// pointer is a better sign of the screen the user looks at than the
/// frontmost app, which can have windows on each screen.
pub fn move_pill_to_screen_of(_target: Option<isize>) {
    if EXPANDED.load(Ordering::SeqCst) {
        return;
    }
    macos::on_main_thread(|| {
        let pointer = macos::mouse_location();
        let Some(frame) = macos::overlay_frame() else {
            return;
        };
        let center = Point {
            x: frame.origin.x + frame.size.width / 2.0,
            y: frame.origin.y + frame.size.height / 2.0,
        };
        let screens = macos::screens();
        let screen_of = |point: Point| screens.iter().find(|(full, _)| full.contains(point));
        if let Some(&(full, work)) = screen_of(pointer) {
            if screen_of(center).map(|(other, _)| *other) != Some(full) {
                place_at_bottom_center(work);
            }
        }
    });
}

/// Main thread only.
fn place_at_bottom_center(work: Rect) {
    let Some(frame) = macos::overlay_frame() else {
        return;
    };
    let origin = Point {
        x: (work.origin.x + (work.size.width - frame.size.width) / 2.0).round(),
        y: (work.origin.y + BOTTOM_GAP).round(),
    };
    macos::set_overlay_origin(origin);
    *FRAME.lock() = Some(Rect {
        origin,
        size: frame.size,
    });
}

/// Turns click-through off while the pointer is over the pill or the panel
/// is open, and on again when it leaves. The window has no input region on
/// macOS, so this check takes its place.
fn follow_pointer() {
    let mut ignoring = true;
    loop {
        std::thread::sleep(POINTER_CHECK);
        let frame = *FRAME.lock();
        let want_ignore = !EXPANDED.load(Ordering::SeqCst)
            && frame.is_some_and(|frame| !over_pill(frame, macos::mouse_location()));
        if want_ignore != ignoring {
            ignoring = want_ignore;
            macos::on_main_thread(move || {
                // The panel can open between the check and this call.
                if !want_ignore || !EXPANDED.load(Ordering::SeqCst) {
                    macos::set_overlay_ignores_mouse(want_ignore);
                }
            });
        }
    }
}

/// True when `pointer` (screen points, origin at the bottom left) is over
/// the pill of the overlay at `frame`.
fn over_pill(frame: Rect, pointer: Point) -> bool {
    let Some((x, y, w, h)) = *PILL_HIT_RECT.lock() else {
        return frame.contains(pointer);
    };
    let top = frame.origin.y + frame.size.height;
    let pill = Rect {
        origin: Point {
            x: frame.origin.x + f64::from(x),
            y: top - f64::from(y + h),
        },
        size: macos::Size {
            width: f64::from(w),
            height: f64::from(h),
        },
    };
    pill.contains(pointer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_rect_flips_to_screen_points() {
        *PILL_HIT_RECT.lock() = Some((150.0, 500.0, 100.0, 30.0));
        let frame = Rect {
            origin: Point { x: 100.0, y: 70.0 },
            size: macos::Size {
                width: 400.0,
                height: 540.0,
            },
        };
        // The pill spans x 250..350 and, from the bottom, y 80..110.
        assert!(over_pill(frame, Point { x: 300.0, y: 95.0 }));
        assert!(!over_pill(frame, Point { x: 300.0, y: 400.0 }));
        assert!(!over_pill(frame, Point { x: 120.0, y: 95.0 }));
    }
}
