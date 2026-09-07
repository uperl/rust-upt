//! `upt` — a cargo/git-style command multiplexer.
//!
//! Built-in subcommands are handled directly; anything else is dispatched to an
//! executable named `upt-<name>` found on `PATH`, so `upt foo` runs `upt-foo`.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow, bail};

mod commands;
mod config;
mod dist;
mod external;
mod json;
mod metacpan;
mod paths;
mod pathsearch;
mod style;

use config::{ColorChoice, Config};
use style::Style;

/// Prefix for external subcommand executables: `upt foo` -> `upt-foo`.
pub const EXTERNAL_PREFIX: &str = "upt-";

/// Context shared with every subcommand.
pub struct Cx {
    pub style: Style,
    /// The effective color choice (`--color` over `global.color`), before it is
    /// resolved against a specific stream. Subcommands with their own output
    /// styling (e.g. `metacpan`) fall back to this when not given `--color`.
    pub color: ColorChoice,
    pub config_path: PathBuf,
    pub cache_dir: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(err) => {
            eprintln!("upt: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<i32> {
    let cli = Cli::parse(std::env::args().skip(1))?;

    if cli.show_version {
        println!("upt {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }

    let (config_path, default_path) = match cli.config {
        Some(path) => (path, false),
        None => (paths::config_file()?, true),
    };

    // First run: drop a starter config file at the default location so the
    // user has something to edit. Best effort — a failure here is not fatal,
    // `Config::load` just falls back to the defaults. An explicit `--config`
    // path is never created behind the user's back.
    if default_path && !config_path.exists() {
        if let Some(parent) = config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&config_path, config::DEFAULT_FILE);
    }

    let config = Config::load(&config_path)?;

    // Best effort: make sure the cache directory exists so plugins and the
    // user can rely on it.
    let cache_dir = paths::cache_dir().ok();
    if let Some(dir) = &cache_dir {
        let _ = std::fs::create_dir_all(dir);
    }

    let choice = cli.color.unwrap_or(config.global.color);
    let cx = Cx {
        style: Style::new(style::resolve(choice, std::io::stdout().is_terminal())),
        color: choice,
        config_path,
        cache_dir,
    };

    let Some(name) = cli.subcommand else {
        // No command given: usage to stderr, non-zero exit.
        eprint!("{}", commands::help::general(&cx));
        return Ok(2);
    };

    if let Some(builtin) = commands::find(&name) {
        return (builtin.run)(&cx, &cli.args);
    }

    match pathsearch::find_external(&name) {
        Some(path) => external::exec(&cx, &path, &cli.args),
        None => bail!("'{name}' is not a upt command; see 'upt help'"),
    }
}

/// The parsed top-level command line: global options up to the first bare word,
/// then that word as the subcommand and everything after it as its arguments.
struct Cli {
    color: Option<ColorChoice>,
    config: Option<PathBuf>,
    subcommand: Option<String>,
    args: Vec<String>,
    show_version: bool,
}

impl Cli {
    fn parse(raw: impl Iterator<Item = String>) -> Result<Cli> {
        let raw: Vec<String> = raw.collect();
        let mut cli = Cli {
            color: None,
            config: None,
            subcommand: None,
            args: Vec::new(),
            show_version: false,
        };
        let mut want_help = false;
        let mut i = 0;

        while i < raw.len() {
            let arg = &raw[i];
            i += 1;
            match arg.as_str() {
                "--" => {
                    if i < raw.len() {
                        cli.subcommand = Some(raw[i].clone());
                        cli.args.extend_from_slice(&raw[i + 1..]);
                    }
                    break;
                }
                "-h" | "--help" => want_help = true,
                "-V" | "--version" => cli.show_version = true,
                "--color" => {
                    let value = raw.get(i).ok_or_else(|| {
                        anyhow!("--color requires a value: always, never, or auto")
                    })?;
                    cli.color = Some(parse_color(value)?);
                    i += 1;
                }
                _ if arg.starts_with("--color=") => {
                    cli.color = Some(parse_color(&arg["--color=".len()..])?);
                }
                "--config" => {
                    let value = raw
                        .get(i)
                        .ok_or_else(|| anyhow!("--config requires a file path"))?;
                    cli.config = Some(PathBuf::from(value));
                    i += 1;
                }
                _ if arg.starts_with("--config=") => {
                    cli.config = Some(PathBuf::from(&arg["--config=".len()..]));
                }
                _ if arg.starts_with('-') && arg != "-" => {
                    bail!("unrecognized option '{arg}'; try 'upt help'");
                }
                _ => {
                    cli.subcommand = Some(arg.clone());
                    cli.args.extend_from_slice(&raw[i..]);
                    break;
                }
            }
        }

        if want_help && !cli.show_version {
            // `upt -h [topic]` is an alias for `upt help [topic]`.
            let mut help_args = Vec::new();
            if let Some(name) = cli.subcommand.take() {
                help_args.push(name);
            }
            help_args.append(&mut cli.args);
            cli.subcommand = Some("help".to_string());
            cli.args = help_args;
        }

        Ok(cli)
    }
}

fn parse_color(value: &str) -> Result<ColorChoice> {
    match value {
        "always" => Ok(ColorChoice::Always),
        "never" => Ok(ColorChoice::Never),
        "auto" => Ok(ColorChoice::Auto),
        other => bail!("invalid --color value '{other}'; expected always, never, or auto"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn bare_invocation_has_no_subcommand() {
        let cli = parse(&[]);
        assert!(cli.subcommand.is_none());
    }

    #[test]
    fn splits_subcommand_and_forwards_rest_verbatim() {
        let cli = parse(&["which", "--weird", "foo"]);
        assert_eq!(cli.subcommand.as_deref(), Some("which"));
        assert_eq!(cli.args, vec!["--weird", "foo"]);
    }

    #[test]
    fn global_options_before_subcommand() {
        let cli = parse(&["--color", "never", "--config=/tmp/c.toml", "help", "which"]);
        assert_eq!(cli.color, Some(ColorChoice::Never));
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/c.toml")));
        assert_eq!(cli.subcommand.as_deref(), Some("help"));
        assert_eq!(cli.args, vec!["which"]);
    }

    #[test]
    fn dash_h_is_help_alias() {
        let cli = parse(&["-h", "which"]);
        assert_eq!(cli.subcommand.as_deref(), Some("help"));
        assert_eq!(cli.args, vec!["which"]);
    }

    #[test]
    fn double_dash_forces_subcommand() {
        let cli = parse(&["--", "--color", "x"]);
        assert_eq!(cli.subcommand.as_deref(), Some("--color"));
        assert_eq!(cli.args, vec!["x"]);
    }

    #[test]
    fn unknown_global_option_errors() {
        assert!(Cli::parse(["--nope".to_string()].into_iter()).is_err());
    }

    #[test]
    fn bad_color_value_errors() {
        assert!(Cli::parse(["--color=purple".to_string()].into_iter()).is_err());
    }
}
