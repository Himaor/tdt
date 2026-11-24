use super::hud::HudStatus;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Ordinal of the icon in `desktop/assets/tdt.rc`. Same image as the exe icon.
#[cfg(windows)]
const APP_ICON_ID: u16 = 1;

/// A click on the icon opens the menu on macOS, as each menu bar app does.
/// On Windows a left click opens TDT, and a right click opens the menu.
const MENU_ON_LEFT_CLICK: bool = cfg!(target_os = "macos");

pub struct SystemTray {
    pub tray_icon: TrayIcon,
    pub auto_paste_item: CheckMenuItem,
    pub settings_item: MenuItem,
    pub show_item: MenuItem,
    pub updates_item: MenuItem,
    pub quit_item: MenuItem,
    pub stop_item: MenuItem,
    last_tooltip: String,
}

impl SystemTray {
    pub fn new(auto_paste_enabled: bool, hotkey_label: &str) -> Result<Self, String> {
        let tray_menu = Menu::new();

        let show_item = MenuItem::new("Open TDT", true, None);
        let stop_item = MenuItem::new("Stop recording", false, None);
        let settings_item = MenuItem::new("Open settings", true, None);
        let auto_paste_item = CheckMenuItem::new(
            "Auto-paste into the focused app",
            true,
            auto_paste_enabled,
            None,
        );
        let updates_item = MenuItem::new("Check for updates", true, None);
        let quit_item = MenuItem::new("Quit TDT", true, None);

        tray_menu
            .append_items(&[
                &show_item,
                &settings_item,
                &stop_item,
                &PredefinedMenuItem::separator(),
                &auto_paste_item,
                &updates_item,
                &PredefinedMenuItem::separator(),
                &quit_item,
            ])
            .map_err(|e| format!("Menu error: {e}"))?;

        let icon = app_icon()?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_menu_on_left_click(MENU_ON_LEFT_CLICK)
            .with_tooltip(tooltip_text(hotkey_label));
        // macOS tints a template image for light and dark menu bars.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(icon);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(icon);
        let tray_icon = builder
            .build()
            .map_err(|e| format!("Failed to build tray icon: {e}"))?;

        Ok(Self {
            tray_icon,
            auto_paste_item,
            settings_item,
            show_item,
            updates_item,
            quit_item,
            stop_item,
            last_tooltip: String::new(),
        })
    }

    pub fn update(&mut self, status: &HudStatus, hotkey: &str) {
        let recording = matches!(status, HudStatus::Listening { .. });
        if self.stop_item.is_enabled() != recording {
            self.stop_item.set_enabled(recording);
        }
        // A tooltip change is a call to Explorer, and a slow Explorer holds
        // the UI loop. During a take the loop must see the release, so the
        // tooltip changes only when no take runs.
        if matches!(
            status,
            HudStatus::Listening { .. } | HudStatus::Transcribing { .. }
        ) {
            return;
        }
        let tooltip = status_tooltip(status, hotkey);
        if tooltip != self.last_tooltip && self.tray_icon.set_tooltip(Some(&tooltip)).is_ok() {
            self.last_tooltip = tooltip;
        }
    }
}

#[cfg(windows)]
fn app_icon() -> Result<Icon, String> {
    let side = tray_icon_px();
    Icon::from_resource(APP_ICON_ID, Some((side, side)))
        .map_err(|error| format!("Failed to load the app icon: {error}"))
}

/// The menu bar icon on macOS: a template image (black with alpha, which
/// macOS tints for light and dark menu bars) of the orb, a ring around a
/// dot, at 22 points for a 2x screen.
#[cfg(target_os = "macos")]
fn app_icon() -> Result<Icon, String> {
    const SIDE: u32 = 44;
    let center = (SIDE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let distance = (x as f32 - center).hypot(y as f32 - center);
            // Coverage of the ring (radius 15 to 19) and the dot (radius 7),
            // with one pixel of smoothing at each edge.
            let edge = |inside: f32| inside.clamp(0.0, 1.0);
            let ring = edge(19.0 - distance).min(edge(distance - 14.0));
            let dot = edge(7.5 - distance);
            let alpha = (ring.max(dot) * 255.0).round() as u8;
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    Icon::from_rgba(rgba, SIDE, SIDE)
        .map_err(|error| format!("Failed to make the menu bar icon: {error}"))
}

#[cfg(windows)]
fn tray_icon_px() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
    // SAFETY: GetSystemMetrics takes no pointers.
    let side = unsafe { GetSystemMetrics(SM_CXSMICON) };
    side.max(16) as u32
}

pub fn tooltip_text(hotkey_label: &str) -> String {
    format!("TDT. Shortcut {hotkey_label}. Click to open.")
}

fn status_tooltip(status: &HudStatus, hotkey: &str) -> String {
    let text = match status {
        HudStatus::Idle => format!("TDT. Ready. Shortcut {hotkey}. Click to open."),
        HudStatus::Listening { .. } => format!("TDT. Listening. Press {hotkey} to stop."),
        HudStatus::Transcribing { .. } => "TDT. Transcribing.".into(),
        HudStatus::Success {
            text,
            auto_pasted,
            latency_ms,
            ..
        } => format!(
            "TDT. {} {}: {text}",
            if *auto_pasted { "Inserted" } else { "Copied" },
            crate::ui::text::format_latency_ms(*latency_ms)
        ),
        HudStatus::NoSpeech { .. } => "TDT. No speech detected. Try again or open settings.".into(),
        HudStatus::Error { message, .. } => format!("TDT. {message}"),
    };
    // NOTIFYICONDATAW reserves 128 UTF-16 units including its terminator.
    let mut units = 0;
    text.chars()
        .take_while(|ch| {
            let len = ch.len_utf16();
            if units + len > 127 {
                return false;
            }
            units += len;
            true
        })
        .collect()
}
