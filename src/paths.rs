//! Platform-appropriate locations for the config file, cache directory, and
//! user database.

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

/// Path to the user SQLite database. The file is created lazily the first time
/// a subcommand asks for it.
///
/// * Linux:   `~/.local/share/upt/upt.sqlite` (or `$XDG_DATA_HOME/upt/upt.sqlite`)
/// * macOS:   `~/Library/Application Support/upt/upt.sqlite`
/// * Windows: `%APPDATA%\upt\upt.sqlite`
pub fn data_file() -> Result<PathBuf> {
    let dir = dirs::data_dir().context("could not determine the user data directory")?;
    Ok(dir.join("upt").join("upt.sqlite"))
}
