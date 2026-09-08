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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use perl_build::{PatchPerl, PerlBuild, PerlReleases, extract_tarball, symlink_devel_executables};

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

/// How the Devel::PatchPerl fix-ups will be applied for this build.
enum PatchStrategy {
    /// Let `perl-build` drive the build, with this `patchperl` setting.
    /// `PatchPerl::Auto` runs the external `patchperl` and warns+skips if it is
    /// missing; `PatchPerl::Disabled` applies nothing.
    PerlBuild(PatchPerl),
    /// Obtain and unpack the source here, patch it in-process with the
    /// `patch-perl` crate, then build the patched tree.
    Internal,
}

/// Resolve `perlbuild.patch-perl` to a concrete strategy.
fn patch_strategy(mode: crate::config::PatchPerlMode) -> PatchStrategy {
    use crate::config::PatchPerlMode;
    match mode {
        PatchPerlMode::Off => PatchStrategy::PerlBuild(PatchPerl::Disabled),
        PatchPerlMode::External => PatchStrategy::PerlBuild(PatchPerl::Auto),
        PatchPerlMode::Internal => PatchStrategy::Internal,
        PatchPerlMode::Auto => {
            if which("patchperl").is_some() {
                PatchStrategy::PerlBuild(PatchPerl::Auto)
            } else {
                log::warn!(
                    "`patchperl` not found on PATH; applying Devel::PatchPerl fix-ups with the \
                     bundled patch-perl crate"
                );
                PatchStrategy::Internal
            }
        }
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
        .jobs(jobs);
    if let Some(test) = test {
        perl_build = perl_build.test(test);
    }
    if let Some(build_dir) = &build_dir {
        perl_build = perl_build.build_dir(build_dir);
    }
    if let Some(tarball_dir) = &tarball_dir {
        perl_build = perl_build.tarball_dir(tarball_dir);
    }

    let built = match patch_strategy(mode) {
        // `perl-build` drives the whole build; it applies (or skips) the
        // Devel::PatchPerl fix-ups according to `setting`.
        PatchStrategy::PerlBuild(setting) => {
            perl_build = perl_build.patchperl(setting);
            if is_blead {
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
            }
        }
        // Obtain and unpack the source ourselves, apply the fix-ups in-process
        // with the `patch-perl` crate, then build from the patched tree — no
        // shelling out.
        PatchStrategy::Internal => {
            perl_build = perl_build.patchperl(PatchPerl::Disabled);

            let tarball = obtain_tarball(&stuff, is_blead, tarball_dir.as_deref())
                .await
                .with_context(|| format!("obtaining the source for {stuff}"))?;

            let build_root = match &build_dir {
                Some(dir) => {
                    std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
                    PathBuf::from(dir)
                }
                None => temp_dir("src").context("creating a build directory")?,
            };
            let src = extract_tarball(&tarball, &build_root)
                .with_context(|| format!("unpacking {}", tarball.display()))?;

            patch_perl::PatchPerl::new()
                .source(src.as_path())
                .run()
                .context("applying Devel::PatchPerl fix-ups")?;

            perl_build
                .install_from_source(&src)
                .with_context(|| format!("build of {stuff} failed"))?
        }
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

/// Produce a local source tarball for `stuff`: a local tarball path is returned
/// as-is; `blead`, a URL, or a CPAN version is downloaded (the version resolved
/// through MetaCPAN) into `tarball_dir` (or a temp dir).
async fn obtain_tarball(stuff: &str, is_blead: bool, tarball_dir: Option<&str>) -> Result<PathBuf> {
    if !is_blead && !is_url(stuff) && is_tarball(stuff) {
        return Ok(PathBuf::from(stuff));
    }

    let url = if is_blead {
        BLEAD_URL.to_string()
    } else if is_url(stuff) {
        stuff.to_string()
    } else {
        let release = PerlReleases::new()
            .find(stuff)
            .await
            .with_context(|| format!("resolving perl {stuff} on CPAN"))?;
        log::info!(
            "resolved perl {stuff} to {} ({})",
            release.name,
            release.download_url
        );
        release.download_url
    };

    let dir = match tarball_dir {
        Some(dir) => {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {dir}"))?;
            PathBuf::from(dir)
        }
        None => temp_dir("tarball").context("creating a download directory")?,
    };
    let dest = dir.join(url_filename(&url));
    download(&url, &dest)
        .await
        .with_context(|| format!("downloading {url}"))?;
    Ok(dest)
}

/// The file name to save a downloaded URL under.
fn url_filename(url: &str) -> String {
    url.split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("perl-source.tar.gz")
        .to_string()
}

/// GET `url` and write the body to `dest`.
async fn download(url: &str, dest: &Path) -> Result<()> {
    let client = metacpan_api_modern::Client::builder()
        .build()
        .context("building the HTTP client")?;
    let bytes = client
        .http()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    std::fs::write(dest, &bytes).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

/// A fresh directory under the system temp directory, e.g.
/// `.../upt-perlbuild-<kind>-<pid>-<nanos>`.
fn temp_dir(kind: &str) -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "upt-perlbuild-{kind}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
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
    use super::{PatchStrategy, patch_strategy};
    use crate::config::PatchPerlMode;
    use perl_build::PatchPerl;

    #[test]
    fn explicit_modes_map_to_a_fixed_strategy() {
        assert!(matches!(
            patch_strategy(PatchPerlMode::Off),
            PatchStrategy::PerlBuild(PatchPerl::Disabled)
        ));
        assert!(matches!(
            patch_strategy(PatchPerlMode::External),
            PatchStrategy::PerlBuild(PatchPerl::Auto)
        ));
        assert!(matches!(
            patch_strategy(PatchPerlMode::Internal),
            PatchStrategy::Internal
        ));
    }
}
