//! Where the machine keeps its configuration.

use std::path::PathBuf;

use crate::{Error, Result};

/// Overrides the platform config location.
///
/// Tests use it, and so does running two agents side by side on one box during
/// development.
pub const CONFIG_DIR_ENV: &str = "INPUTSHARE_CONFIG_DIR";

/// Directory holding this machine's identity, workspace document and hints.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    directories::ProjectDirs::from("dev", "InputShare", "InputShare")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or(Error::NoConfigDir)
}
