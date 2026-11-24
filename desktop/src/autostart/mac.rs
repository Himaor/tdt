//! Per-user "Open at login" on macOS: a LaunchAgent in
//! `~/Library/LaunchAgents` that starts TDT when this user logs in. The file
//! names the TDT program of the app bundle that runs now, so a moved app
//! gets a new file at its next start (`refresh_if_enabled`).
//!
//! launchd reads the file at the next login. TDT does not load it now: TDT
//! already runs, and the single-instance lock would stop a second start.

use std::path::{Path, PathBuf};

/// The label of the job, and the name of its file.
const LABEL: &str = "io.github.hi9841.tdt";

fn agent_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(
        home.join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
    )
}

pub fn is_enabled() -> bool {
    agent_path().is_some_and(|path| path.is_file())
}

/// Points an enabled agent at this TDT, for a TDT that moved.
pub fn refresh_if_enabled() {
    let (Some(path), Ok(exe)) = (agent_path(), std::env::current_exe()) else {
        return;
    };
    if is_translocated(&exe) {
        return;
    }
    let Ok(current) = std::fs::read_to_string(&path) else {
        return;
    };
    let wanted = agent_plist(&exe);
    if current != wanted {
        if let Err(error) = write_agent(&path, &wanted) {
            voice_stt_desktop::log::note(&format!("Could not update the login item: {error}"));
        }
    }
}

/// macOS runs an app that the user opened from the disk image or from
/// Downloads from a temporary copy (App Translocation). That path is gone
/// after a restart, so it never goes into the login item.
fn is_translocated(exe: &Path) -> bool {
    exe.to_string_lossy().contains("/AppTranslocation/")
}

pub fn set_enabled(enable: bool) -> Result<(), String> {
    let path = agent_path().ok_or("The home folder is not known")?;
    if enable {
        let exe = std::env::current_exe().map_err(|e| format!("Could not find TDT: {e}"))?;
        if is_translocated(&exe) {
            return Err("Move TDT to the Applications folder first".into());
        }
        write_agent(&path, &agent_plist(&exe))
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("Could not remove the login item: {error}")),
        }
    }
}

fn write_agent(path: &Path, plist: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    // A new file and a rename, so launchd never reads half a file.
    let partial = path.with_extension("plist.partial");
    std::fs::write(&partial, plist).map_err(|e| format!("Could not write the login item: {e}"))?;
    std::fs::rename(&partial, path).map_err(|e| format!("Could not write the login item: {e}"))
}

/// The LaunchAgent. `ProgramArguments` holds the path as one argument, so
/// no shell reads it; the path is escaped for XML.
fn agent_plist(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        xml_text(&exe.to_string_lossy())
    )
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_the_path() {
        let plist = agent_plist(Path::new("/Users/a&b/<TDT>.app/Contents/MacOS/TDT"));
        assert!(
            plist.contains("<string>/Users/a&amp;b/&lt;TDT&gt;.app/Contents/MacOS/TDT</string>")
        );
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    }
}
