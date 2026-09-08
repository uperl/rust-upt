//! `upt patchperl` — a drop-in replacement for the `patchperl` command from the
//! [Devel-PatchPerl](https://metacpan.org/dist/Devel-PatchPerl) CPAN
//! distribution, doing the work with the
//! [`patch-perl`](https://github.com/uperl/rust-patch-perl) crate.
//!
//! Invoke it as `upt patchperl ...`, or as `patchperl ...` when the `upt`
//! binary is symlinked or copied to that name. The CLI mirrors the original
//! `patchperl` script: two boolean switches (`--version`, `--patchlevel`) and
//! up to two positionals — the source tree (default `.`) and, optionally, the
//! Perl version to patch as (otherwise read from `patchlevel.h`). Errors follow
//! upt conventions: a usage error prints the short usage and exits 2, and a
//! patch failure propagates to upt's top-level handler.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Entry point for the `patchperl` drop-in replacement.
pub fn run(_cx: &crate::Cx, argv: &[String]) -> Result<i32> {
    // Surface the `patch-perl` crate's `log` output (it warns about an
    // auto-guessed version, reports each patch, ...); `RUST_LOG` overrides.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .try_init();

    let parsed = match parse(argv) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{}: {message}", prog());
            eprint!("{}", usage_short());
            return Ok(2);
        }
    };

    match parsed {
        Parsed::Help => {
            print!("{}", usage_full());
            Ok(0)
        }
        Parsed::Version => {
            print_version();
            Ok(0)
        }
        Parsed::Run {
            source,
            version,
            patchlevel,
        } => {
            if patchlevel {
                // The `patch-perl` crate skips updating `patchlevel.h` inside a
                // git checkout unless this is set — matching the original
                // script's `local $ENV{PERL5_PATCHPERL_PATCHLEVEL} = 1`.
                //
                // SAFETY: single-threaded here; set before any patch work and
                // before any thread is spawned.
                unsafe { std::env::set_var("PERL5_PATCHPERL_PATCHLEVEL", "1") };
            }

            let mut builder = patch_perl::PatchPerl::new().source(&source);
            if let Some(version) = &version {
                builder = builder.version(version.clone());
            }
            builder.run().with_context(|| {
                format!("patching the Perl source tree in {}", source.display())
            })?;
            Ok(0)
        }
    }
}

/// The outcome of parsing the command line.
enum Parsed {
    Help,
    Version,
    Run {
        source: PathBuf,
        version: Option<String>,
        patchlevel: bool,
    },
}

/// Parse `argv`, emulating the original script's `Getopt::Long` handling of its
/// two boolean switches: `-opt` and `--opt` both work, unambiguous
/// abbreviations are accepted (`--ver`, `--pat`), a `no` prefix negates, and
/// `--` ends option processing. `Err` carries a usage message.
fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut want_version = false;
    let mut want_help = false;
    let mut patchlevel = false;
    let mut positionals: Vec<&str> = Vec::new();
    let mut opts_done = false;

    for arg in argv {
        if opts_done {
            positionals.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => opts_done = true,
            "-h" | "--help" => want_help = true,
            option if option.starts_with('-') && option != "-" => {
                let body = option.trim_start_matches('-');
                let (negated, name) =
                    match body.strip_prefix("no-").or_else(|| body.strip_prefix("no")) {
                        Some(rest) if !rest.is_empty() => (true, rest),
                        _ => (false, body),
                    };
                match match_flag(name) {
                    Some(Flag::Version) => want_version = !negated,
                    Some(Flag::Patchlevel) => patchlevel = !negated,
                    Some(Flag::Help) => want_help = !negated,
                    None => return Err(format!("unknown option `{option}`")),
                }
            }
            _ => positionals.push(arg),
        }
    }

    if want_help {
        return Ok(Parsed::Help);
    }
    if want_version {
        return Ok(Parsed::Version);
    }

    if positionals.len() > 2 {
        return Err("too many arguments (expected at most <source-dir> <version>)".to_string());
    }
    Ok(Parsed::Run {
        source: positionals
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from),
        version: positionals.get(1).map(|value| value.to_string()),
        patchlevel,
    })
}

#[derive(Clone, Copy)]
enum Flag {
    Version,
    Patchlevel,
    Help,
}

/// Resolve an option name (already stripped of dashes and any `no` prefix) to a
/// flag: an exact match, or an unambiguous prefix.
fn match_flag(name: &str) -> Option<Flag> {
    const FLAGS: &[(&str, Flag)] = &[
        ("version", Flag::Version),
        ("patchlevel", Flag::Patchlevel),
        ("help", Flag::Help),
    ];
    if name.is_empty() {
        return None;
    }
    if let Some((_, flag)) = FLAGS.iter().find(|(candidate, _)| *candidate == name) {
        return Some(*flag);
    }
    let mut prefixed = FLAGS
        .iter()
        .filter(|(candidate, _)| candidate.starts_with(name));
    match (prefixed.next(), prefixed.next()) {
        (Some((_, flag)), None) => Some(*flag),
        _ => None,
    }
}

/// How this command was invoked, for diagnostics: `patchperl` when running
/// under that name (a symlink or copy of `upt`), otherwise `upt patchperl`.
fn prog() -> String {
    match std::env::args_os()
        .next()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_stem)
        .and_then(|stem| stem.to_str())
    {
        Some("patchperl") => "patchperl".to_string(),
        _ => "upt patchperl".to_string(),
    }
}

fn print_version() {
    let exe = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("{} {} ({exe})", prog(), env!("CARGO_PKG_VERSION"));
    println!("backend: patch-perl <https://github.com/uperl/rust-patch-perl>");
}

fn usage_short() -> String {
    format!(
        "usage: {p} [--patchlevel] [<source-dir> [<version>]]\ntry `{p} --help` for details\n",
        p = prog()
    )
}

fn usage_full() -> String {
    format!(
        "{p} - patch a Perl source tree so it builds on a modern toolchain

USAGE:
    {p} [--patchlevel] [<source-dir> [<version>]]
    {p} --version

    <source-dir> is the root of an unpacked Perl source tree (default: the
    current directory). <version> is the Perl version to patch as, e.g.
    5.10.1 or 5.005_03; when omitted it is read from the tree's patchlevel.h.

OPTIONS:
    --patchlevel   Update patchlevel.h even inside a git checkout
                   (sets PERL5_PATCHPERL_PATCHLEVEL).
    --version      Print version information, then exit.
    -h, --help     Print this help, then exit.

Set PERL5_PATCHPERL_PLUGIN to load a compiled Devel::PatchPerl plugin.
",
        p = prog()
    )
}

#[cfg(test)]
mod tests {
    use super::{Flag, Parsed, match_flag, parse};

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn run(args: &[&str]) -> (String, Option<String>, bool) {
        match parse(&v(args)) {
            Ok(Parsed::Run {
                source,
                version,
                patchlevel,
            }) => (source.display().to_string(), version, patchlevel),
            other => panic!("expected a run, got {}", label(other)),
        }
    }

    fn label(p: Result<Parsed, String>) -> &'static str {
        match p {
            Ok(Parsed::Help) => "help",
            Ok(Parsed::Version) => "version",
            Ok(Parsed::Run { .. }) => "run",
            Err(_) => "error",
        }
    }

    #[test]
    fn defaults_to_the_current_directory() {
        assert_eq!(run(&[]), (".".to_string(), None, false));
    }

    #[test]
    fn positionals_are_source_then_version() {
        assert_eq!(
            run(&["/src/perl-5.40.0"]),
            ("/src/perl-5.40.0".to_string(), None, false)
        );
        assert_eq!(
            run(&["/src/perl", "5.10.1"]),
            ("/src/perl".to_string(), Some("5.10.1".to_string()), false)
        );
    }

    #[test]
    fn patchlevel_switch_and_abbreviations() {
        assert!(run(&["--patchlevel", "/s"]).2);
        assert!(run(&["--pat", "/s"]).2);
        assert!(run(&["-p", "/s"]).2);
        assert!(!run(&["--no-patchlevel", "/s"]).2);
    }

    #[test]
    fn version_switch_wins_over_positionals() {
        assert_eq!(label(parse(&v(&["--version", "/s", "5.10.1"]))), "version");
        assert_eq!(label(parse(&v(&["--ver"]))), "version");
        assert_eq!(label(parse(&v(&["-v"]))), "version");
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            run(&["--", "--weird-dir", "5.8.9"]),
            ("--weird-dir".to_string(), Some("5.8.9".to_string()), false)
        );
    }

    #[test]
    fn help_switch() {
        assert_eq!(label(parse(&v(&["--help"]))), "help");
        assert_eq!(label(parse(&v(&["-h"]))), "help");
    }

    #[test]
    fn unknown_option_and_extra_args_are_usage_errors() {
        assert_eq!(label(parse(&v(&["--frob"]))), "error");
        assert_eq!(label(parse(&v(&["-x"]))), "error");
        assert_eq!(label(parse(&v(&["a", "b", "c"]))), "error");
    }

    #[test]
    fn match_flag_needs_an_unambiguous_prefix() {
        assert!(matches!(match_flag("version"), Some(Flag::Version)));
        assert!(matches!(match_flag("pat"), Some(Flag::Patchlevel)));
        assert!(match_flag("").is_none());
        assert!(match_flag("x").is_none());
    }
}
