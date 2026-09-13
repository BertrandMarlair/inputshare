//! Starting with the computer (requirement 4).
//!
//! Registered per user, not per machine. It needs no elevation, it follows the
//! person rather than the box, and — the part that matters for this product —
//! it starts in a desktop session, which is where input capture has to live.
//!
//! Every platform here writes the same thing in its own idiom: "run this
//! executable at login". Nothing is written unless the user asked for it.

use std::path::{Path, PathBuf};

/// The name the entry is filed under. Stable, so toggling the setting updates
/// the existing entry instead of leaving a trail of orphans.
pub const ENTRY_NAME: &str = "InputShare";

pub type Result<T> = std::result::Result<T, std::io::Error>;

/// Path of the currently running executable, which is what gets registered.
pub fn current_executable() -> Result<PathBuf> {
    std::env::current_exe()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn run_key(access: u32) -> Result<RegKey> {
        RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, access)
    }

    pub fn is_enabled() -> bool {
        run_key(KEY_READ)
            .and_then(|key| key.get_value::<String, _>(ENTRY_NAME))
            .is_ok()
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let key = run_key(KEY_WRITE)?;
        // Quoted: the path routinely contains spaces, and an unquoted one makes
        // Windows try to run the first word as the program.
        key.set_value(ENTRY_NAME, &format!("\"{}\"", executable.display()))
    }

    pub fn disable() -> Result<()> {
        match run_key(KEY_WRITE)?.delete_value(ENTRY_NAME) {
            Ok(()) => Ok(()),
            // Already gone is the state we wanted.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn plist_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
        Ok(PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join("dev.inputshare.agent.plist"))
    }

    pub fn is_enabled() -> bool {
        plist_path().map(|path| path.exists()).unwrap_or(false)
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let path = plist_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // KeepAlive rather than RunAtLoad alone: the agent is meant to be up
        // whenever the user is logged in, not merely started once.
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.inputshare.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
            executable.display()
        );
        std::fs::write(path, plist)
    }

    pub fn disable() -> Result<()> {
        let path = plist_path()?;
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;

    fn desktop_path() -> Result<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory")
            })?;
        Ok(base.join("autostart").join("inputshare.desktop"))
    }

    pub fn is_enabled() -> bool {
        desktop_path().map(|path| path.exists()).unwrap_or(false)
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let path = desktop_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            format!(
                "[Desktop Entry]\nType=Application\nName={ENTRY_NAME}\nExec={}\nX-GNOME-Autostart-enabled=true\n",
                executable.display()
            ),
        )
    }

    pub fn disable() -> Result<()> {
        match std::fs::remove_file(desktop_path()?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Whether this machine is set to start InputShare at login.
pub fn is_enabled() -> bool {
    platform::is_enabled()
}

/// Turns the login entry on or off, pointing at `executable`.
pub fn set(enabled: bool, executable: &Path) -> Result<()> {
    if enabled {
        platform::enable(executable)
    } else {
        platform::disable()
    }
}
