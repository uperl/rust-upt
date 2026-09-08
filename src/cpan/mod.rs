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
//! The `[cpan]` config section (`source`, `metacpan-base-url`,
//! `mirror-base-url`) supplies the defaults; `--source`, `--metacpan-base-url`
//! and `--mirror-base-url` override them for a single invocation and may be
//! given before or after the subcommand name.
//!
//! Only the command-line surface is implemented so far; running it reports that
//! the installer is not built yet.

use std::ffi::OsString;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::config::CpanSource;

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

    let Cli { common, command } = cli;
    match command {
        Command::Install(args) => install(cx, &common, args),
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
    #[command(flatten)]
    common: CommonArgs,

    #[command(subcommand)]
    command: Command,
}

/// Options that override the `[cpan]` config section. They are `global`, so
/// they may appear before or after the subcommand name.
#[derive(Debug, Args)]
struct CommonArgs {
    /// Where to fetch releases from, overriding `cpan.source`.
    #[arg(long, global = true, value_name = "SOURCE")]
    source: Option<SourceArg>,

    /// Base URL of the MetaCPAN API, overriding `cpan.metacpan-base-url`.
    #[arg(long, global = true, value_name = "URL")]
    metacpan_base_url: Option<String>,

    /// Base URL of the CPAN mirror, overriding `cpan.mirror-base-url`.
    #[arg(long, global = true, value_name = "URL")]
    mirror_base_url: Option<String>,
}

/// `--source` value: the CLI spelling of [`CpanSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// Resolve and download through the MetaCPAN API.
    Metacpan,
    /// Fetch from a configured CPAN mirror.
    Mirror,
}

impl From<SourceArg> for CpanSource {
    fn from(arg: SourceArg) -> Self {
        match arg {
            SourceArg::Metacpan => CpanSource::Metacpan,
            SourceArg::Mirror => CpanSource::Mirror,
        }
    }
}

/// The effective `[cpan]` settings for one invocation: the config values with
/// each `--flag` applied on top.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCpan {
    source: CpanSource,
    metacpan_base_url: String,
    mirror_base_url: String,
}

impl CommonArgs {
    /// Fold these overrides onto the `[cpan]` config section from `cx`.
    fn resolve(&self, cx: &crate::Cx) -> ResolvedCpan {
        ResolvedCpan {
            source: self.source.map_or(cx.cpan.source, CpanSource::from),
            metacpan_base_url: self
                .metacpan_base_url
                .clone()
                .unwrap_or_else(|| cx.cpan.metacpan_base_url.clone()),
            mirror_base_url: self
                .mirror_base_url
                .clone()
                .unwrap_or_else(|| cx.cpan.mirror_base_url.clone()),
        }
    }
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

/// `upt cpan install`: not implemented yet — only the argument parser and the
/// config/flag resolution exist.
fn install(cx: &crate::Cx, common: &CommonArgs, _args: InstallArgs) -> Result<i32> {
    let _cpan = common.resolve(cx);
    bail!("`upt cpan install` is not implemented yet");
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command, InstallArgs, ResolvedCpan, SourceArg};
    use crate::config::{Cpan, CpanSource};
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        let argv: Vec<&str> = std::iter::once("upt cpan")
            .chain(args.iter().copied())
            .collect();
        Cli::try_parse_from(argv).unwrap()
    }

    fn install_args(args: &[&str]) -> InstallArgs {
        match parse(args).command {
            Command::Install(args) => args,
        }
    }

    fn cx_with(cpan: Cpan) -> crate::Cx {
        crate::Cx {
            style: crate::style::Style::new(false),
            color: crate::config::ColorChoice::Never,
            config_path: std::path::PathBuf::from("/tmp/upt/config.toml"),
            cache_dir: None,
            database_path: None,
            patch_perl: crate::config::PatchPerlMode::Auto,
            dist_prefer: crate::config::DistPrefer::Auto,
            cpan,
            perl: crate::config::PerlSection::default(),
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
    fn source_and_url_overrides_parse_before_or_after_the_subcommand() {
        let before = parse(&[
            "--source",
            "mirror",
            "--metacpan-base-url",
            "https://api.example/",
            "--mirror-base-url",
            "https://cpan.example/",
            "install",
            "JSON::PP",
        ]);
        assert_eq!(before.common.source, Some(SourceArg::Mirror));
        assert_eq!(
            before.common.metacpan_base_url.as_deref(),
            Some("https://api.example/")
        );
        assert_eq!(
            before.common.mirror_base_url.as_deref(),
            Some("https://cpan.example/")
        );

        let after = parse(&["install", "JSON::PP", "--source", "metacpan"]);
        assert_eq!(after.common.source, Some(SourceArg::Metacpan));
    }

    #[test]
    fn resolve_defaults_to_the_config_when_no_flags_are_given() {
        let cx = cx_with(Cpan {
            source: CpanSource::Mirror,
            metacpan_base_url: "https://cfg.metacpan/".to_string(),
            mirror_base_url: "https://cfg.mirror/".to_string(),
        });
        let common = parse(&["install", "JSON::PP"]).common;
        assert_eq!(
            common.resolve(&cx),
            ResolvedCpan {
                source: CpanSource::Mirror,
                metacpan_base_url: "https://cfg.metacpan/".to_string(),
                mirror_base_url: "https://cfg.mirror/".to_string(),
            }
        );
    }

    #[test]
    fn resolve_lets_each_flag_override_its_config_value() {
        let cx = cx_with(Cpan {
            source: CpanSource::Metacpan,
            metacpan_base_url: "https://cfg.metacpan/".to_string(),
            mirror_base_url: "https://cfg.mirror/".to_string(),
        });
        let common = parse(&[
            "install",
            "JSON::PP",
            "--source",
            "mirror",
            "--mirror-base-url",
            "https://flag.mirror/",
        ])
        .common;
        assert_eq!(
            common.resolve(&cx),
            ResolvedCpan {
                source: CpanSource::Mirror,
                // untouched by any flag -> from the config
                metacpan_base_url: "https://cfg.metacpan/".to_string(),
                mirror_base_url: "https://flag.mirror/".to_string(),
            }
        );
    }

    #[test]
    fn rejects_an_unknown_source_value() {
        assert!(
            Cli::try_parse_from(["upt cpan", "install", "JSON::PP", "--source", "cpanm"]).is_err()
        );
    }

    #[test]
    fn install_is_a_stub_for_now() {
        let cx = cx_with(Cpan::default());
        let err = super::install(
            &cx,
            &parse(&["install", "JSON::PP"]).common,
            install_args(&["install", "JSON::PP"]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("not implemented"), "{err}");
    }
}
