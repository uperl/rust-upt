//! The user config file: `config.toml` with a `[global]` section.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// The parsed contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub global: Global,
}

/// The `[global]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    #[serde(default)]
    pub color: ColorChoice,
}

/// Value of `global.color`: whether to emit ANSI color escapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorChoice {
    /// Always colorize.
    On,
    /// Never colorize.
    Off,
    /// Colorize only when writing to a terminal.
    #[default]
    Auto,
}

impl Config {
    /// Load the configuration from `path`.
    ///
    /// A missing file is not an error and yields the default configuration; a
    /// file that exists but does not parse is.
    pub fn load(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("failed to parse config file {}", path.display())),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(anyhow::Error::new(err)
                .context(format!("failed to read config file {}", path.display()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_auto() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.global.color, ColorChoice::Auto);
    }

    #[test]
    fn reads_global_color() {
        let cfg: Config = toml::from_str("[global]\ncolor = \"off\"\n").unwrap();
        assert_eq!(cfg.global.color, ColorChoice::Off);
    }

    #[test]
    fn rejects_unknown_color() {
        assert!(toml::from_str::<Config>("[global]\ncolor = \"maybe\"\n").is_err());
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(toml::from_str::<Config>("[global]\ncolour = \"on\"\n").is_err());
    }
}
