//! `upt cpan` — install distributions from CPAN by name.
//!
//! Where [`upt dist`](crate::dist) drives the build lifecycle of an *already
//! unpacked* distribution, `upt cpan` starts from a module or distribution
//! name: resolve it through MetaCPAN, download and unpack the release, then run
//! the `dist` pipeline (`configure` … `install`) on it.
//!
//! * `upt cpan install <SPEC>...` installs one or more modules / distributions.
//!   `--perl <name>` selects the `[perl.<name>]` config section to build with
//!   (without it, `perl.default`), the same resolution as
//!   [`upt perl exec`](crate::perl) and `upt dist`. `--no-test` installs
//!   without running the test suite first.
//!
//! Only the command-line surface is implemented so far; running it reports that
//! the installer is not built yet.

use std::ffi::OsString;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};

/// Entry point for the `cpan` built-in: parse `args` with clap, then dispatch.
pub fn run(cx: &crate::Cx, args: &[String]) -> Result<i32> {
    let argv = std::iter::once(OsString::from("upt cpan")).chain(args.iter().map(OsString::from));
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        // clap prints `--help` / `--version` and usage errors itself; mirror
        // its own exit codes (0 for help/version, 2 for a usage error).
        Err(err) => {
            err.print().ok();
            return Ok(err.exit_code());
        }
    };

    match cli.command {
        Command::Install(args) => install(cx, args),
    }
}

/// Install distributions from CPAN by name.
#[derive(Debug, Parser)]
#[command(
    name = "upt cpan",
    version,
    about = "Install distributions from CPAN by name",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Resolve each SPEC through MetaCPAN, download and unpack the release, and
    /// run the `dist` pipeline through `install` on it.
    Install(InstallArgs),
}

/// Arguments for `upt cpan install`.
#[derive(Debug, Args)]
struct InstallArgs {
    /// Modules or distributions to install (e.g. `JSON::PP`, `JSON-PP`).
    #[arg(value_name = "SPEC", required = true)]
    packages: Vec<String>,

    /// Name of the `[perl.<name>]` config section to build with. Without it,
    /// `perl.default` is used.
    #[arg(long, value_name = "NAME")]
    perl: Option<String>,

    /// Install without running the test suite first.
    #[arg(long = "no-test", short = 'n')]
    no_test: bool,
}

/// `upt cpan install`: not implemented yet — only the argument parser exists.
fn install(_cx: &crate::Cx, _args: InstallArgs) -> Result<i32> {
    bail!("`upt cpan install` is not implemented yet");
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, InstallArgs};
    use clap::Parser;

    fn install_args(args: &[&str]) -> InstallArgs {
        let argv: Vec<&str> = std::iter::once("upt cpan").chain(args.iter().copied()).collect();
        match Cli::try_parse_from(argv).unwrap().command {
            Command::Install(args) => args,
        }
    }

    #[test]
    fn install_takes_one_or_more_specs() {
        let args = install_args(&["install", "JSON::PP", "Moo"]);
        assert_eq!(args.packages, ["JSON::PP", "Moo"]);
        assert_eq!(args.perl, None);
        assert!(!args.no_test);
    }

    #[test]
    fn install_accepts_perl_and_no_test_flags() {
        let args = install_args(&["install", "--perl", "dev", "-n", "JSON::PP"]);
        assert_eq!(args.perl.as_deref(), Some("dev"));
        assert!(args.no_test);
        assert_eq!(args.packages, ["JSON::PP"]);
    }

    #[test]
    fn install_requires_at_least_one_spec() {
        assert!(Cli::try_parse_from(["upt cpan", "install"]).is_err());
    }

    #[test]
    fn install_is_a_stub_for_now() {
        let cx = crate::Cx {
            style: crate::style::Style::new(false),
            color: crate::config::ColorChoice::Never,
            config_path: std::path::PathBuf::from("/tmp/upt/config.toml"),
            cache_dir: None,
            database_path: None,
            patch_perl: crate::config::PatchPerlMode::Auto,
            dist_prefer: crate::config::DistPrefer::Auto,
            cpan_source: crate::config::CpanSource::Metacpan,
            perl: crate::config::PerlSection::default(),
        };
        let err = super::install(&cx, install_args(&["install", "JSON::PP"]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not implemented"), "{err}");
    }
}
