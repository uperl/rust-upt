//! Platform-appropriate locations for the config file and cache directory.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Path to the user configuration file.
///
/// * Linux:   `~/.config/upt/config.toml` (or `$XDG_CONFIG_HOME/upt/config.toml`)
/// * macOS:   `~/Library/Application Support/upt/config.toml`
/// * Windows: `%APPDATA%\upt\config.toml`
pub fn config_file() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine the user config directory")?;
    Ok(dir.join("upt").join("config.toml"))
}

/// Path to the cache directory.
///
/// * Linux:   `~/.cache/upt` (or `$XDG_CACHE_HOME/upt`)
/// * macOS:   `~/Library/Caches/upt`
/// * Windows: `%LOCALAPPDATA%\upt`
pub fn cache_dir() -> Result<PathBuf> {
    let dir = dirs::cache_dir().context("could not determine the user cache directory")?;
    Ok(dir.join("upt"))
}
