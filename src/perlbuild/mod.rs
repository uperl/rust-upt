//! `upt perlbuild` — a drop-in replacement for the `perl-build` command from
//! the [Perl-Build](https://metacpan.org/dist/Perl-Build) CPAN distribution,
//! doing the heavy lifting with the
//! [`perl-build`](https://github.com/uperl/rust-perl-build) crate.
//!
//! Invoke it as `upt perlbuild ...`, or as `perl-build ...` when the `upt`
//! binary is symlinked or copied to that name. Option parsing follows the
//! original `perl-build` script as closely as possible (`Getopt::Long` with
//! `pass_through`, `bundling`, `no_ignore_case`); errors follow upt
//! conventions — a usage error prints the short usage and exits 2, and a build
//! failure propagates to upt's top-level handler.

mod args;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use perl_build::{PatchPerl, PerlBuild, PerlReleases, symlink_devel_executables};

use args::{BuildArgs, Outcome};

/// Where `perl-build blead` fetches the development tip from.
const BLEAD_URL: &str = "https://github.com/Perl/perl5/archive/blead.tar.gz";

/// Entry point for the `perlbuild` drop-in replacement.
pub fn run(cx: &crate::Cx, argv: &[String]) -> Result<i32> {
    // Surface the `perl-build` crate's `log` progress output; `RUST_LOG`
    // overrides the default. `try_init` so a second call in-process is a no-op.
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .format_target(false)
        .try_init();

    let outcome = match args::parse(argv) {
        Ok(outcome) => outcome,
        Err(args::UsageError(message)) => {
            if let Some(message) = message {
                eprintln!("{}: {message}", prog());
            }
            eprint!("{}", usage_short());
            return Ok(2);
        }
    };

    let build_args = match outcome {
        Outcome::Help => {
            print!("{}", usage_full());
            return Ok(0);
        }
        Outcome::Version => {
            print_version(cx.patch_perl);
            return Ok(0);
        }
        Outcome::Definitions => return block_on(definitions()),
        Outcome::Build(build_args) => build_args,
    };

    if let Some(plugin) = &build_args.patches {
        // SAFETY: single-threaded here — this runs before the async runtime is
        // built and before any thread is spawned, so nothing can be reading the
        // environment concurrently. `perl-build` passes this straight down to
        // the `patchperl` child through the inherited environment.
        unsafe { std::env::set_var("PERL5_PATCHPERL_PLUGIN", plugin) };
    }

    block_on(build(build_args, cx.patch_perl))
}

/// Run `future` on a fresh current-thread Tokio runtime: `Ok(0)` on success,
/// otherwise the error propagates to upt's top-level handler.
fn block_on<F>(future: F) -> Result<i32>
where
    F: std::future::Future<Output = Result<()>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;
    runtime.block_on(future)?;
    Ok(0)
}

/// Map `perlbuild.patch-perl` to the `perl-build` crate's [`PatchPerl`] setting.
/// The crate applies the fix-ups itself — with the external `patchperl` or the
/// in-process `patch-perl` port — so `upt` just forwards the choice.
fn patch_perl_setting(mode: crate::config::PatchPerlMode) -> PatchPerl {
    use crate::config::PatchPerlMode;
    match mode {
        PatchPerlMode::Auto => PatchPerl::Auto,
        PatchPerlMode::External => PatchPerl::External,
        PatchPerlMode::Internal => PatchPerl::Internal,
        PatchPerlMode::Off => PatchPerl::Disabled,
    }
}

async fn build(args: BuildArgs, mode: crate::config::PatchPerlMode) -> Result<()> {
    let BuildArgs {
        stuff,
        dest,
        mut configure_options,
        test,
        build_dir,
        tarball_dir,
        jobs,
        patches: _,
        symlink_devel_executables: want_symlinks,
    } = args;

    let dest = absolute(&dest).context("could not resolve the destination path")?;

    let is_blead = stuff == "blead";
    if is_blead {
        // `perl-build blead` prepends -Dusedevel to whatever else was requested.
        configure_options.insert(0, "-Dusedevel".to_string());
    }

    let jobs = jobs.unwrap_or_else(default_jobs);

    let mut perl_build = PerlBuild::new(&dest)
        .configure_options(configure_options)
        .jobs(jobs)
        .patchperl(patch_perl_setting(mode));
    if let Some(test) = test {
        perl_build = perl_build.test(test);
    }
    if let Some(build_dir) = &build_dir {
        perl_build = perl_build.build_dir(build_dir);
    }
    if let Some(tarball_dir) = &tarball_dir {
        perl_build = perl_build.tarball_dir(tarball_dir);
    }

    // `perl-build` drives the whole build — download / unpack / patch / build —
    // applying the Devel::PatchPerl fix-ups per the `patchperl` setting above.
    let built = if is_blead {
        perl_build
            .install_from_url(BLEAD_URL)
            .await
            .context("build from blead failed")?
    } else if is_url(&stuff) {
        perl_build
            .install_from_url(&stuff)
            .await
            .with_context(|| format!("build from {stuff} failed"))?
    } else if is_tarball(&stuff) {
        perl_build
            .install_from_tarball(&stuff)
            .with_context(|| format!("build from tarball {stuff} failed"))?
    } else {
        perl_build
            .install_from_cpan(&stuff)
            .await
            .with_context(|| format!("build of perl {stuff} failed"))?
    };

    if want_symlinks {
        symlink_devel_executables(&built.bin_dir())
            .context("could not symlink development executables")?;
    }

    println!("perl installed in {}", built.prefix().display());
    Ok(())
}

fn is_url(stuff: &str) -> bool {
    stuff.starts_with("http://") || stuff.starts_with("https://")
}

fn is_tarball(stuff: &str) -> bool {
    stuff.ends_with(".gz") || stuff.ends_with(".bz2") || stuff.ends_with(".xz")
}

async fn definitions() -> Result<()> {
    let releases = PerlReleases::new()
        .list_with_dev()
        .await
        .context("could not list perl releases from MetaCPAN")?;
    for release in releases {
        println!("{}", release.version);
    }
    Ok(())
}

/// The `-j` default: the number of processor threads visible to this process,
/// falling back to 1 when that cannot be determined.
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Absolute form of `path`, resolved against the current directory, matching
/// Perl's `File::Spec->rel2abs`. No filesystem access, no symlink resolution.
fn absolute(path: &str) -> std::io::Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// How this command was invoked, for diagnostics: `perl-build` when running
/// under that name (a symlink or copy of `upt`), otherwise `upt perlbuild`.
fn prog() -> String {
    let invoked_as = std::env::args_os()
        .next()
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_stem)
        .and_then(|stem| stem.to_str())
        .map(str::to_owned);
    match invoked_as.as_deref() {
        Some("perl-build") => "perl-build".to_string(),
        _ => "upt perlbuild".to_string(),
    }
}

fn print_version(mode: crate::config::PatchPerlMode) {
    let exe = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "?".to_string());
    println!("{} {} ({exe})", prog(), env!("CARGO_PKG_VERSION"));
    println!("backend: perl-build <https://github.com/uperl/rust-perl-build>");
    println!("         patch-perl <https://github.com/uperl/rust-patch-perl>");
    println!(
        "patch-perl mode: {} (config: perlbuild.patch-perl)",
        mode_label(mode)
    );
    match which("patchperl") {
        Some(path) => println!("external patchperl: {}", path.display()),
        None => println!("external patchperl: not found on PATH"),
    }
}

fn mode_label(mode: crate::config::PatchPerlMode) -> &'static str {
    use crate::config::PatchPerlMode;
    match mode {
        PatchPerlMode::Auto => "auto",
        PatchPerlMode::Off => "off",
        PatchPerlMode::Internal => "internal",
        PatchPerlMode::External => "external",
    }
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}

fn usage_short() -> String {
    format!(
        "usage: {p} [options] <version|url|tarball|blead> <destination> \
         [-- <configure options>...]\ntry `{p} --help` for details\n",
        p = prog()
    )
}

fn usage_full() -> String {
    format!(
        "{p} - build and install a perl from source

USAGE:
    {p} [options] <stuff> <destination> [-- <configure options>...]

    <stuff> is one of:
      a version      e.g. 5.40.2          (resolved on CPAN through MetaCPAN)
      a URL          http(s)://.../perl-5.40.2.tar.gz
      a tarball      path/to/perl-5.40.2.tar.{{gz,bz2,xz}}
      blead          the development tip from github.com/Perl/perl5

    <destination> is the install prefix (-Dprefix); a relative path is made
    absolute against the current directory.

    Arguments after <destination> (or after a `--`) are passed straight to
    ./Configure. With none given, `-de` is used.

OPTIONS:
    -D <define>          pass -D<define> to ./Configure   (repeatable)
    -A <append>          pass -A<append> to ./Configure   (repeatable)
    -U <undef>           pass -U<undef>  to ./Configure   (repeatable)
    --test               run the test suite after building
    --no-test            do not run the test suite         (default)
    -j, --jobs <n>       build (and test) with <n> parallel jobs
                         (default: the number of detected processor threads)
    --build-dir <dir>    unpack and build here             (default: a temp dir)
    --tarball-dir <dir>  download source tarballs here     (default: a temp dir)
    --patches <plugin>   set PERL5_PATCHPERL_PLUGIN for patchperl
    --symlink-devel-executables
                         symlink versioned dev executables (perl5.41.0 -> perl)
    --noman             skip manpages (-Dman1dir=none -Dman3dir=none)
    --definitions       list the perl versions available on CPAN, then exit
    --version           print version information, then exit
    -h, --help          print this help, then exit

The `perlbuild.patch-perl` config item selects the Devel::PatchPerl
implementation: `auto` (default) uses the external `patchperl` when on PATH and
otherwise the bundled `patch-perl` crate in-process; `external` only ever runs
`patchperl` (warns and skips if missing); `internal` only ever uses the crate;
`off` applies no fix-ups.
",
        p = prog()
    )
}

#[cfg(test)]
mod tests {
    use super::patch_perl_setting;
    use crate::config::PatchPerlMode;
    use perl_build::PatchPerl;

    #[test]
    fn modes_forward_to_the_perl_build_patchperl_setting() {
        assert!(matches!(
            patch_perl_setting(PatchPerlMode::Auto),
            PatchPerl::Auto
        ));
        assert!(matches!(
            patch_perl_setting(PatchPerlMode::External),
            PatchPerl::External
        ));
        assert!(matches!(
            patch_perl_setting(PatchPerlMode::Internal),
            PatchPerl::Internal
        ));
        assert!(matches!(
            patch_perl_setting(PatchPerlMode::Off),
            PatchPerl::Disabled
        ));
    }
}
