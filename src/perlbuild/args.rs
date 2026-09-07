//! Command-line parsing for `upt perlbuild` / `perl-build`.
//!
//! This is a deliberate re-implementation of the argument handling in the
//! `perl-build` script from the [`Perl-Build`] CPAN distribution, which uses
//! `Getopt::Long` configured with `pass_through`, `no_ignore_case` and
//! `bundling`. The notable consequences we reproduce:
//!
//! * `-D`, `-A` and `-U` always take a value (`-Dfoo`, `-D foo`, `-D=foo`) and
//!   are turned into `-Dfoo` / `-Afoo` / `-Ufoo` Configure options, appended
//!   after any explicit trailing options.
//! * Anything not recognised as an option -- bare words *and* unknown
//!   dash-options alike -- is left in argument order; the first two such items
//!   are `<stuff>` and `<destination>`, the rest are `./Configure` options.
//! * With no trailing Configure options, `-de` is used.
//! * A leading `--` ends option processing (a second one is also skipped).
//!
//! [`Perl-Build`]: https://metacpan.org/dist/Perl-Build

/// The result of a successful parse.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `-h` / `--help`: print full usage and exit 0.
    Help,
    /// `--version`: print version information and exit 0.
    Version,
    /// `--definitions`: list available perl versions and exit 0.
    Definitions,
    /// Build a perl with these settings.
    Build(BuildArgs),
}

/// A parsed build request.
#[derive(Debug, PartialEq, Eq)]
pub struct BuildArgs {
    /// Version, URL, tarball path, or `blead`.
    pub stuff: String,
    /// Install prefix, as given (not yet made absolute).
    pub dest: String,
    /// Fully assembled `./Configure` options (base, then `-D`/`-A`/`-U`, then
    /// `--noman`). Does not include the `-Dusedevel` that `blead` adds.
    pub configure_options: Vec<String>,
    /// `Some(true)` for `--test`, `Some(false)` for `--no-test`, `None` if
    /// neither was given.
    pub test: Option<bool>,
    /// `--build-dir`.
    pub build_dir: Option<String>,
    /// `--tarball-dir`.
    pub tarball_dir: Option<String>,
    /// `-j` / `--jobs`.
    pub jobs: Option<usize>,
    /// `--patches` (exported as `PERL5_PATCHPERL_PLUGIN`).
    pub patches: Option<String>,
    /// `--symlink-devel-executables`.
    pub symlink_devel_executables: bool,
}

/// A usage error. `Some(message)` is printed before the usage summary; `None`
/// mirrors bare `pod2usage()` (missing `<stuff>` / `<destination>`).
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub Option<String>);

/// Parse `argv` (without the program name).
pub fn parse(argv: &[String]) -> Result<Outcome, UsageError> {
    let mut test: Option<bool> = None;
    let mut d: Vec<String> = Vec::new();
    let mut a: Vec<String> = Vec::new();
    let mut u: Vec<String> = Vec::new();
    let mut definitions = false;
    let mut patches: Option<String> = None;
    let mut build_dir: Option<String> = None;
    let mut tarball_dir: Option<String> = None;
    let mut jobs: Option<usize> = None;
    let mut show_version = false;
    let mut help = false;
    let mut symlink = false;
    let mut noman = false;

    // Non-options and unrecognised options, in the order they appeared.
    let mut rest: Vec<String> = Vec::new();

    let mut i = 0;
    let mut opts_done = false;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;

        if opts_done {
            rest.push(arg.to_string());
            continue;
        }

        if arg == "--" {
            opts_done = true;
            // Getopt::Long removes the first `--`; the script shifts a second.
            if i < argv.len() && argv[i] == "--" {
                i += 1;
            }
            continue;
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match name {
                "help" => help = true,
                "version" => show_version = true,
                "definitions" => definitions = true,
                "noman" => noman = true,
                "test" => test = Some(true),
                "no-test" | "notest" => test = Some(false),
                "symlink-devel-executables" => symlink = true,
                "no-symlink-devel-executables" | "nosymlink-devel-executables" => symlink = false,
                "patches" => patches = Some(take_long(argv, &mut i, name, &inline)?),
                "build-dir" => build_dir = Some(take_long(argv, &mut i, name, &inline)?),
                "tarball-dir" => tarball_dir = Some(take_long(argv, &mut i, name, &inline)?),
                "jobs" => jobs = Some(parse_jobs(&take_long(argv, &mut i, name, &inline)?)?),
                // Unknown long option: pass through, exactly as Getopt::Long does.
                _ => rest.push(arg.to_string()),
            }
            continue;
        }

        if let Some(body) = arg.strip_prefix('-')
            && !body.is_empty()
        {
            let mut chars = body.chars();
            let first = chars.next().unwrap();
            let tail: String = chars.collect();
            match first {
                'h' => help = true,
                'D' | 'A' | 'U' | 'j' => {
                    let mut val = if tail.is_empty() {
                        match take_short(argv, &mut i) {
                            Some(v) => v,
                            None => {
                                return Err(UsageError(Some(format!(
                                    "option -{first} requires an argument"
                                ))));
                            }
                        }
                    } else {
                        tail
                    };
                    if let Some(stripped) = val.strip_prefix('=') {
                        val = stripped.to_string();
                    }
                    match first {
                        'D' => d.push(val),
                        'A' => a.push(val),
                        'U' => u.push(val),
                        'j' => jobs = Some(parse_jobs(&val)?),
                        _ => unreachable!(),
                    }
                }
                // Unknown short option: pass through.
                _ => rest.push(arg.to_string()),
            }
            continue;
        }

        // Bare word (including a lone "-").
        rest.push(arg.to_string());
    }

    if show_version {
        return Ok(Outcome::Version);
    }
    if help {
        return Ok(Outcome::Help);
    }
    if definitions {
        return Ok(Outcome::Definitions);
    }

    let mut rest = rest.into_iter();
    let stuff = rest.next().ok_or(UsageError(None))?;
    let dest = rest.next().ok_or(UsageError(None))?;
    let trailing: Vec<String> = rest.collect();

    let mut configure_options = if trailing.is_empty() {
        vec!["-de".to_string()]
    } else {
        trailing
    };
    configure_options.extend(d.iter().map(|s| format!("-D{s}")));
    configure_options.extend(a.iter().map(|s| format!("-A{s}")));
    configure_options.extend(u.iter().map(|s| format!("-U{s}")));
    if noman {
        configure_options.push("-Dman1dir=none".to_string());
        configure_options.push("-Dman3dir=none".to_string());
    }

    Ok(Outcome::Build(BuildArgs {
        stuff,
        dest,
        configure_options,
        test,
        build_dir,
        tarball_dir,
        jobs,
        patches,
        symlink_devel_executables: symlink,
    }))
}

fn take_long(
    argv: &[String],
    i: &mut usize,
    name: &str,
    inline: &Option<String>,
) -> Result<String, UsageError> {
    if let Some(v) = inline {
        return Ok(v.clone());
    }
    if *i < argv.len() {
        let v = argv[*i].clone();
        *i += 1;
        Ok(v)
    } else {
        Err(UsageError(Some(format!(
            "option --{name} requires an argument"
        ))))
    }
}

fn take_short(argv: &[String], i: &mut usize) -> Option<String> {
    if *i < argv.len() {
        let v = argv[*i].clone();
        *i += 1;
        Some(v)
    } else {
        None
    }
}

fn parse_jobs(s: &str) -> Result<usize, UsageError> {
    s.trim()
        .parse::<usize>()
        .map_err(|_| UsageError(Some(format!("invalid job count: {s:?}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn build(args: &[&str]) -> BuildArgs {
        match parse(&v(args)) {
            Ok(Outcome::Build(b)) => b,
            other => panic!("expected a build, got {other:?}"),
        }
    }

    #[test]
    fn minimal() {
        let b = build(&["5.40.2", "/opt/perl-5.40.2"]);
        assert_eq!(b.stuff, "5.40.2");
        assert_eq!(b.dest, "/opt/perl-5.40.2");
        assert_eq!(b.configure_options, ["-de"]);
        assert_eq!(b.test, None);
        assert_eq!(b.jobs, None);
    }

    #[test]
    fn dash_d_before_positionals_is_consumed() {
        let b = build(&["-Dusethreads", "5.40.2", "/opt/perl"]);
        assert_eq!(b.stuff, "5.40.2");
        assert_eq!(b.dest, "/opt/perl");
        assert_eq!(b.configure_options, ["-de", "-Dusethreads"]);
    }

    #[test]
    fn dash_u_after_positionals_is_consumed() {
        let b = build(&["5.40.2", "/opt/perl", "-Uversiononly"]);
        assert_eq!(b.configure_options, ["-de", "-Uversiononly"]);
    }

    #[test]
    fn dash_d_forms() {
        assert_eq!(
            build(&["-D", "usethreads", "5", "/o"]).configure_options,
            ["-de", "-Dusethreads"]
        );
        assert_eq!(
            build(&["-D=usethreads", "5", "/o"]).configure_options,
            ["-de", "-Dusethreads"]
        );
        assert_eq!(
            build(&["-Dcc=clang", "5", "/o"]).configure_options,
            ["-de", "-Dcc=clang"]
        );
    }

    #[test]
    fn explicit_trailing_options_replace_the_de_default() {
        let b = build(&["5.40.2", "/opt", "-de", "-Dcc=clang"]);
        assert_eq!(b.configure_options, ["-de", "-Dcc=clang"]);
    }

    #[test]
    fn explicit_trailing_then_appended_dash_d() {
        // -Dfoo is captured by -D=s@, -Dusedevel-ish literal passes through.
        let b = build(&["5.40.2", "/opt", "--", "-Accflags=-DFOO"]);
        assert_eq!(b.configure_options, ["-Accflags=-DFOO"]);
    }

    #[test]
    fn noman_appends_configure_options() {
        let b = build(&["--noman", "5.40.2", "/opt"]);
        assert_eq!(
            b.configure_options,
            ["-de", "-Dman1dir=none", "-Dman3dir=none"]
        );
    }

    #[test]
    fn jobs_forms() {
        assert_eq!(build(&["-j4", "5", "/o"]).jobs, Some(4));
        assert_eq!(build(&["-j", "4", "5", "/o"]).jobs, Some(4));
        assert_eq!(build(&["--jobs", "8", "5", "/o"]).jobs, Some(8));
        assert_eq!(build(&["--jobs=8", "5", "/o"]).jobs, Some(8));
    }

    #[test]
    fn test_flag() {
        assert_eq!(build(&["--test", "5", "/o"]).test, Some(true));
        assert_eq!(build(&["--no-test", "5", "/o"]).test, Some(false));
        assert_eq!(build(&["--notest", "5", "/o"]).test, Some(false));
    }

    #[test]
    fn dirs_and_patches() {
        let b = build(&[
            "--build-dir",
            "/tmp/b",
            "--tarball-dir=/tmp/t",
            "--patches",
            "Asan",
            "5",
            "/o",
        ]);
        assert_eq!(b.build_dir.as_deref(), Some("/tmp/b"));
        assert_eq!(b.tarball_dir.as_deref(), Some("/tmp/t"));
        assert_eq!(b.patches.as_deref(), Some("Asan"));
    }

    #[test]
    fn symlink_devel_executables_flag() {
        assert!(build(&["--symlink-devel-executables", "blead", "/o"]).symlink_devel_executables);
        assert!(!build(&["blead", "/o"]).symlink_devel_executables);
    }

    #[test]
    fn double_dash_lets_a_dashed_stuff_through() {
        let b = build(&["--", "-weird-version", "/o"]);
        assert_eq!(b.stuff, "-weird-version");
        assert_eq!(b.dest, "/o");
    }

    #[test]
    fn second_double_dash_is_skipped() {
        let b = build(&["--", "--", "5.40.2", "/o"]);
        assert_eq!(b.stuff, "5.40.2");
        assert_eq!(b.dest, "/o");
    }

    #[test]
    fn unknown_long_option_passes_through_as_a_positional() {
        // Faithful to Getopt::Long pass_through: this really does become <stuff>.
        let b = build(&["--frobnicate", "5.40.2", "/opt"]);
        assert_eq!(b.stuff, "--frobnicate");
        assert_eq!(b.dest, "5.40.2");
        assert_eq!(b.configure_options, ["/opt"]);
    }

    #[test]
    fn missing_operands() {
        assert_eq!(parse(&v(&[])), Err(UsageError(None)));
        assert_eq!(parse(&v(&["5.40.2"])), Err(UsageError(None)));
    }

    #[test]
    fn modes() {
        assert_eq!(parse(&v(&["--version"])), Ok(Outcome::Version));
        assert_eq!(parse(&v(&["-h"])), Ok(Outcome::Help));
        assert_eq!(parse(&v(&["--help"])), Ok(Outcome::Help));
        assert_eq!(parse(&v(&["--definitions"])), Ok(Outcome::Definitions));
        // A mode wins even with operands present, matching the script's order.
        assert_eq!(
            parse(&v(&["--version", "5.40.2", "/opt"])),
            Ok(Outcome::Version)
        );
    }

    #[test]
    fn bad_job_count_is_a_usage_error() {
        assert!(matches!(
            parse(&v(&["-j", "lots", "5", "/o"])),
            Err(UsageError(Some(_)))
        ));
    }
}
