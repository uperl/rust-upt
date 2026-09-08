//! `upt perl` — run `perl` through a configured
//! [`perl-wrapper`](https://github.com/uperl/rust-perl-wrapper).
//!
//! Each `[perl.<name>]` section of the config file describes one `perl-wrapper`
//! object (which `perl` and `make` to use, the install prefix, the extra
//! `PERL5LIB` directories). `upt perl exec` builds that wrapper and executes
//! `perl` with it:
//!
//! ```text
//! upt perl exec [--perl <name>] [-- <perl options>...]
//! ```
//!
//! `--perl <name>` selects the `[perl.<name>]` section; without it, `perl.default`
//! from the config is used. Everything after `--` is passed straight to `perl`;
//! the command exits with `perl`'s own status.

use std::ffi::OsString;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use perl_wrapper::Perl;

use crate::config::PerlConfig;

/// Entry point for the `perl` built-in: parse `args` with clap, then dispatch.
pub fn run(cx: &crate::Cx, args: &[String]) -> Result<i32> {
    let argv = std::iter::once(OsString::from("upt perl")).chain(args.iter().map(OsString::from));
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        // clap prints `--help` / `--version` and usage errors itself; mirror its
        // own exit codes (0 for help/version, 2 for a usage error).
        Err(err) => {
            err.print().ok();
            return Ok(err.exit_code());
        }
    };

    match cli.command {
        Command::Exec { perl, perl_args } => exec(cx, perl.as_deref(), &perl_args),
    }
}

/// Run `perl` through a configured perl-wrapper.
#[derive(Debug, Parser)]
#[command(
    name = "upt perl",
    version,
    about = "Run perl through a configured perl-wrapper",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Execute `perl` using the `perl-wrapper` built from a `[perl.<name>]`
    /// config section.
    ///
    /// With no `--perl`, the `perl.default` config entry is used. Put `perl`'s
    /// own options after a `--`, e.g. `upt perl exec --perl dev -- -E 'say 42'`.
    Exec {
        /// Name of the `[perl.<name>]` config section to run (default:
        /// `perl.default`).
        #[arg(long, value_name = "NAME")]
        perl: Option<String>,

        /// Options and arguments passed straight to `perl`, after a `--`.
        #[arg(last = true, value_name = "PERL_OPTIONS")]
        perl_args: Vec<String>,
    },
}

/// Resolve the perl name, build its wrapper, and exec `perl` with `perl_args`.
/// Returns `perl`'s exit status as this process's exit code.
fn exec(cx: &crate::Cx, name: Option<&str>, perl_args: &[String]) -> Result<i32> {
    let name = match name {
        Some(name) => name,
        None => cx.perl.default.as_deref().ok_or_else(|| {
            anyhow!(
                "no perl selected: pass `--perl <name>` or set `perl.default` in {}",
                cx.config_path.display()
            )
        })?,
    };

    let config = cx.perl.perls.get(name).ok_or_else(|| {
        anyhow!(
            "no `[perl.{name}]` section in {}{}",
            cx.config_path.display(),
            available(cx)
        )
    })?;

    let perl = build_wrapper(config)
        .with_context(|| format!("building the perl-wrapper for `[perl.{name}]`"))?;

    let result = perl
        .execute_perl(perl_args)
        .with_context(|| format!("running perl for `[perl.{name}]`"))?;

    Ok(exit_code(result.is_success, result.code))
}

/// Build a [`Perl`] wrapper from a `[perl.<name>]` config section. A missing
/// `perl` key falls back to the first `perl` on `PATH`.
fn build_wrapper(config: &PerlConfig) -> Result<Perl> {
    let mut perl = match &config.perl {
        Some(path) => Perl::with_perl(path),
        None => Perl::new().context("could not locate a `perl` interpreter on PATH")?,
    };

    if let Some(make) = &config.make {
        perl = perl.with_make(make);
    }
    if let Some(base) = &config.install_base {
        perl = perl.with_install_base(base);
    }
    if !config.lib.is_empty() {
        perl = perl.with_lib(config.lib.clone());
    }

    Ok(perl)
}

/// A `" (available: a, b, c)"` / `" (no [perl.*] sections are defined)"` hint for
/// the "unknown perl name" error.
fn available(cx: &crate::Cx) -> String {
    let names: Vec<&str> = cx.perl.perls.keys().map(String::as_str).collect();
    if names.is_empty() {
        " (no [perl.*] sections are defined)".to_string()
    } else {
        format!(" (available: {})", names.join(", "))
    }
}

/// The exit code to exit `upt` with for a `perl` run: `0` on success, the
/// child's `code` (coerced into `1..=255`) on a non-zero exit, and `1` when it
/// was killed by a signal (`code` is `None`).
fn exit_code(is_success: bool, code: Option<i32>) -> i32 {
    if is_success {
        return 0;
    }
    match code {
        Some(code) => {
            let byte = u8::try_from(code).unwrap_or(1);
            i32::from(if byte == 0 { 1 } else { byte })
        }
        None => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cx_with(perl: crate::config::PerlSection) -> crate::Cx {
        crate::Cx {
            style: crate::style::Style::new(false),
            color: crate::config::ColorChoice::Never,
            config_path: PathBuf::from("/tmp/upt/config.toml"),
            cache_dir: None,
            database_path: None,
            patch_perl: crate::config::PatchPerlMode::Auto,
            perl,
        }
    }

    #[test]
    fn missing_default_is_an_error_that_names_the_config() {
        let cx = cx_with(crate::config::PerlSection::default());
        let err = exec(&cx, None, &[]).unwrap_err().to_string();
        assert!(err.contains("--perl"));
        assert!(err.contains("perl.default"));
        assert!(err.contains("/tmp/upt/config.toml"));
    }

    #[test]
    fn unknown_name_lists_the_available_ones() {
        let mut section = crate::config::PerlSection::default();
        section
            .perls
            .insert("alpha".to_string(), PerlConfig::default());
        section
            .perls
            .insert("beta".to_string(), PerlConfig::default());
        let cx = cx_with(section);

        let err = exec(&cx, Some("gamma"), &[]).unwrap_err().to_string();
        assert!(err.contains("[perl.gamma]"));
        assert!(err.contains("available: alpha, beta"));
    }

    #[test]
    fn exit_code_maps_success_failure_and_signal() {
        assert_eq!(exit_code(true, Some(0)), 0);
        assert_eq!(exit_code(false, Some(3)), 3);
        // A non-zero exit whose byte truncates to 0 is reported as 1.
        assert_eq!(exit_code(false, Some(256)), 1);
        // Killed by a signal.
        assert_eq!(exit_code(false, None), 1);
    }
}
