//! `upt cpan` — install distributions from CPAN by name.
//!
//! Where [`upt dist`](crate::dist) drives the build lifecycle of an *already
//! unpacked* distribution, `upt cpan` starts from a module or distribution
//! name: resolve it, download and unpack the release, then run the `dist`
//! pipeline (`pre-configure` … `install`) on it, recursively installing any
//! missing hard (`requires`) prerequisites discovered at the `pre-configure`
//! and `configure` steps.
//!
//! * `upt cpan install <SPEC>...` installs one or more modules / distributions.
//!   `--perl <name>` selects the `[perl.<name>]` config section to build with
//!   (without it, `perl.default`), the same resolution as
//!   [`upt perl exec`](crate::perl) and `upt dist`. `--no-test` installs
//!   without running the test suite first, and skips `test`-phase prerequisites.
//!
//! # Resolution
//!
//! With `cpan.source = "metacpan"` (the default) each SPEC is resolved through
//! the MetaCPAN `download_url` API and the tarball is fetched from the URL it
//! hands back.
//!
//! With `cpan.source = "mirror"` the mirror's own package index
//! (`<mirror-base-url>/modules/02packages.details.txt.gz`) is downloaded once
//! and every SPEC — including recursively-discovered prerequisites — is looked
//! up in it; the tarball is fetched from `<mirror-base-url>/authors/id/<path>`.
//! MetaCPAN is not contacted. The index carries no checksums, so downloads are
//! not verified in this mode. `mirror-base-url` may be an `http(s)://` URL or a
//! `file:///absolute/path` for a mirror on local disk or an NFS mount.
//!
//! The `[cpan]` config section (`source`, `metacpan-base-url`,
//! `mirror-base-url`) supplies the defaults; `--source`, `--metacpan-base-url`
//! and `--mirror-base-url` override them for a single invocation and may be
//! given before or after the subcommand name.
//!
//! # Cache layout
//!
//! Everything for one `upt cpan install` run lands under a fresh directory
//! `<cache>/upt/cpan/<run-id>/`, where `<run-id>` is a UTC timestamp
//! (`YYYYMMDDTHHMMSSZ`, with a `-NN` suffix if two runs collide on the same
//! second) so `ls` lists runs oldest-first. On platforms with symlinks a
//! `latest` symlink in `<cache>/upt/cpan/` is pointed at the newest run (best
//! effort). Inside the run directory:
//!
//! * `install.log` — the merged raw output of every build step, for every dist.
//! * `<dist>-<version>/` — the unpacked tarball (its `Makefile.PL` / `Build.PL`
//!   sits directly in here).
//! * `<dist>-<version>.<step>.json` — written as each step completes, with the
//!   same JSON body `upt dist <step> --json` produces.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use cpan_distribution_build::{
    BuildTool, Dependencies, Dependency, Distribution, ExecuteResult, Perl,
};
use cpan_packagedetails::PackageDetails;
use metacpan_api_modern::Client;
use metacpan_api_modern::reqwest::Url;
use metacpan_api_modern::types::{DownloadUrl, Release};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::CpanSource;

/// `User-Agent` sent with every MetaCPAN request and tarball download.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

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

// `next_display_order = None`: list subcommands alphabetically in `--help`,
// matching `upt help`.
#[derive(Debug, Subcommand)]
#[command(next_display_order = None)]
enum Command {
    /// Resolve each SPEC (through MetaCPAN, or the mirror's `02packages` index
    /// with `--source mirror`), download and unpack the release, and run the
    /// `dist` pipeline through `install` on it, recursively installing missing
    /// `requires` prerequisites.
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

    /// Install without running the test suite first, and without installing
    /// `test`-phase prerequisites.
    #[arg(long = "no-test", visible_alias = "no-tests", short = 'n')]
    no_test: bool,
}

/// `upt cpan install`: set up the run directory, then walk each SPEC through the
/// build pipeline on a Tokio runtime.
fn install(cx: &crate::Cx, common: &CommonArgs, args: InstallArgs) -> Result<i32> {
    let resolved = common.resolve(cx);

    let (_name, perl_config) = crate::perl::resolve_perl(cx, args.perl.as_deref())?;
    let perl = crate::perl::build_wrapper(perl_config)?.with_capture_output(true);

    let cpan_cache = cx
        .cache_dir
        .clone()
        .context("could not determine the user cache directory")?
        .join("cpan");
    let run_dir = unique_run_dir(&cpan_cache, &utc_timestamp())
        .with_context(|| format!("creating a run directory under {}", cpan_cache.display()))?;
    link_latest(&cpan_cache, &run_dir);
    println!("run: {}", run_dir.display());

    let prefer = match cx.dist_prefer {
        crate::config::DistPrefer::Auto => None,
        crate::config::DistPrefer::Mb => Some(BuildTool::ModuleBuild),
        crate::config::DistPrefer::Eumm => Some(BuildTool::Eumm),
    };

    let log_path = run_dir.join("install.log");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(async move {
        let mut client = Client::builder()
            .user_agent(USER_AGENT)
            .base_url(resolved.metacpan_base_url.clone());
        if let Some(dir) = cx.cache_dir.clone() {
            client = client.cache_dir(dir.join("metacpan"));
        }
        let client = client.build().context("building the MetaCPAN client")?;

        // In mirror mode, pull the mirror's package index once and resolve
        // everything against it.
        let packages = match resolved.source {
            CpanSource::Metacpan => None,
            CpanSource::Mirror => Some(
                load_mirror_index(&client, &resolved.mirror_base_url)
                    .await
                    .context("loading the mirror package index")?,
            ),
        };

        let installer = Installer {
            log_path,
            run_dir,
            perl,
            prefer,
            no_test: args.no_test,
            source: resolved.source,
            mirror_base_url: resolved.mirror_base_url,
            packages,
            started: Mutex::new(HashSet::new()),
        };

        for spec in &args.packages {
            installer.install_spec(&client, spec.clone()).await?;
        }
        println!("logs: {}", installer.log_path.display());
        anyhow::Ok(())
    })?;

    Ok(0)
}

/// Fetch and parse `<mirror>/modules/02packages.details.txt.gz`.
async fn load_mirror_index(client: &Client, mirror_base_url: &str) -> Result<PackageDetails> {
    let url = format!(
        "{}/modules/02packages.details.txt.gz",
        mirror_base_url.trim_end_matches('/')
    );
    let bytes = fetch(client, &url).await?;
    PackageDetails::load_bytes(&bytes).with_context(|| format!("parsing {url}"))
}

/// Fetch `url`'s bytes: over HTTP(S) through `client`, or straight off the local
/// filesystem for a `file://` URL (so a mirror can live on disk or an NFS
/// mount).
async fn fetch(client: &Client, url: &str) -> Result<Vec<u8>> {
    if let Some(path) = file_url_path(url)? {
        return std::fs::read(&path).with_context(|| format!("reading {}", path.display()));
    }
    let response = client
        .http()
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("{url}: HTTP {}", status.as_u16());
    }
    Ok(response
        .bytes()
        .await
        .with_context(|| format!("reading the response body of {url}"))?
        .to_vec())
}

/// The local path a `file:` URL points at, or `None` for any other scheme. A
/// `file://` URL with a remote host (or one that is otherwise not a local path)
/// is an error.
fn file_url_path(url: &str) -> Result<Option<PathBuf>> {
    if !url.starts_with("file:") {
        return Ok(None);
    }
    let parsed = Url::parse(url).with_context(|| format!("invalid file URL {url}"))?;
    parsed.to_file_path().map(Some).map_err(|()| {
        anyhow!("{url}: not a local path (use `file:///absolute/path`, not a remote host)")
    })
}

/// One `upt cpan install` run: the immutable configuration plus the set of
/// distributions already handled (so a prerequisite shared by several targets,
/// or a dependency cycle, is built at most once).
struct Installer {
    run_dir: PathBuf,
    log_path: PathBuf,
    /// The build interpreter, with output capture on so each step's log can be
    /// written to disk while stdout stays a concise summary.
    perl: Perl,
    /// Build-tool preference for a dual-config distribution (`dist.prefer`);
    /// `None` means "the build library's own choice".
    prefer: Option<BuildTool>,
    no_test: bool,
    source: CpanSource,
    mirror_base_url: String,
    /// The mirror's `02packages.details.txt` index, loaded once when
    /// `source = "mirror"`; `None` in MetaCPAN mode.
    packages: Option<PackageDetails>,
    started: Mutex<HashSet<String>>,
}

/// A resolved release, ready to download and build.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    distribution: String,
    version: String,
    /// The URL to fetch the tarball from — MetaCPAN's download URL, or
    /// `<mirror>/authors/id/<path>` in mirror mode.
    url: String,
    /// SHA-256 of the archive when it is known (MetaCPAN mode only; the mirror
    /// index carries no checksums).
    checksum: Option<String>,
    /// File name to save the tarball under, e.g. `JSON-PP-4.16.tar.gz`.
    archive_name: String,
}

impl Installer {
    /// Install one top-level SPEC (a module *or* distribution name).
    fn install_spec<'a>(
        &'a self,
        client: &'a Client,
        spec: String,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            let resolved = self.resolve_spec(client, &spec).await?;
            self.install_resolved(client, resolved).await
        })
    }

    /// Install the distribution that provides `dep.module`, unless it is already
    /// satisfied on the build interpreter's search path or has already been
    /// built in this run.
    fn install_dep<'a>(
        &'a self,
        client: &'a Client,
        dep: Dependency,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            if dep.module == "perl" || satisfied(&self.perl, &dep) {
                return Ok(());
            }
            let resolved = self
                .resolve_module(client, &dep.module)
                .await
                .with_context(|| format!("resolving prerequisite {}", dep.module))?;
            self.install_resolved(client, resolved).await
        })
    }

    /// Download, unpack, and run the pipeline for an already-resolved release.
    async fn install_resolved(&self, client: &Client, resolved: Resolved) -> Result<()> {
        let label = format!("{}-{}", resolved.distribution, resolved.version);
        if !self.mark_started(&resolved.distribution) {
            return Ok(());
        }

        let bytes = fetch(client, &resolved.url)
            .await
            .with_context(|| format!("fetching {}", resolved.url))?;
        verify_sha256(&bytes, resolved.checksum.as_deref(), &label)?;

        let archive_path = self.run_dir.join(&resolved.archive_name);
        fs::write(&archive_path, &bytes)
            .with_context(|| format!("writing {}", archive_path.display()))?;

        let extracted = perl_build::extract_tarball(&archive_path, &self.run_dir)
            .with_context(|| format!("unpacking {}", archive_path.display()))?;
        let target = self.run_dir.join(&label);
        if extracted != target {
            if target.exists() {
                let _ = fs::remove_dir_all(&target);
            }
            fs::rename(&extracted, &target).with_context(|| {
                format!("renaming {} to {}", extracted.display(), target.display())
            })?;
        }
        println!("{label}  fetch  ok");

        let mut dist = self.open_distribution(&target)?;

        // --- pre-configure: compute configure prerequisites, install missing.
        let pre = dist.execute_pre_configure();
        self.write_step_json(
            &label,
            "pre-configure",
            envelope(
                Some(crate::dist::pre_configure_prereqs_json(&pre)),
                "",
                0,
                true,
            ),
        )?;
        let missing = self.missing(&dist.perl, pre.iter());
        self.report(&label, "pre-configure", true, 0, &missing);
        for dep in missing {
            self.install_dep(client, dep).await?;
        }

        // --- configure.
        let (result, tree) = dist
            .execute_configure()
            .with_context(|| format!("starting the configure step for {label}"))?;
        let code = exit_code(&result);
        self.log(&label, "configure", &captured(&result))?;
        self.write_step_json(
            &label,
            "configure",
            envelope(
                Some(crate::dist::resolved_prereqs_json(&tree)),
                &captured(&result),
                code,
                result.is_success,
            ),
        )?;
        if !result.is_success {
            self.report(&label, "configure", false, code, &[]);
            bail!(
                "configure failed for {label} (exit {code}); see {}",
                self.log_path.display()
            );
        }
        let missing = self.missing(&dist.perl, self.resolved_requires(&tree).into_iter());
        self.report(&label, "configure", true, 0, &missing);
        for dep in missing {
            self.install_dep(client, dep).await?;
        }

        // --- build / test / install.
        self.run_step(&dist, &label, "build")?;
        if !self.no_test {
            self.run_step(&dist, &label, "test")?;
        }
        self.run_step(&dist, &label, "install")?;

        Ok(())
    }

    /// Run one of `build` / `test` / `install`: execute it, append its output to
    /// the shared log, write `<label>.<step>.json`, and stop the whole run on a
    /// non-zero exit.
    fn run_step(&self, dist: &Distribution, label: &str, step: &str) -> Result<()> {
        let result = match step {
            "build" => dist.execute_build(),
            "test" => dist.execute_test(),
            "install" => dist.execute_install(),
            other => unreachable!("unknown build step {other}"),
        }
        .with_context(|| format!("starting the {step} step for {label}"))?;

        let code = exit_code(&result);
        self.log(label, step, &captured(&result))?;
        self.write_step_json(
            label,
            step,
            envelope(None, &captured(&result), code, result.is_success),
        )?;

        if !result.is_success {
            self.report(label, step, false, code, &[]);
            bail!(
                "{step} failed for {label} (exit {code}); see {}",
                self.log_path.display()
            );
        }
        self.report(label, step, true, 0, &[]);
        Ok(())
    }

    /// Record that `distribution` is being handled this run. Returns `false`
    /// when it was already recorded, so the caller should skip it.
    fn mark_started(&self, distribution: &str) -> bool {
        self.started
            .lock()
            .unwrap()
            .insert(distribution.to_string())
    }

    /// The `requires` prerequisites from a resolved dependency tree that
    /// `upt cpan` will try to install: `configure`, `build` and `runtime`
    /// always, plus `test` unless `--no-test`.
    fn resolved_requires<'a>(&self, tree: &'a Dependencies) -> Vec<&'a Dependency> {
        let mut out: Vec<&Dependency> = tree
            .configure
            .requires
            .iter()
            .chain(&tree.build.requires)
            .chain(&tree.runtime.requires)
            .collect();
        if !self.no_test {
            out.extend(&tree.test.requires);
        }
        out
    }

    /// The subset of `deps` that is not already satisfied on `perl`'s search
    /// path, de-duplicated by module name and with the `perl` marker dropped.
    fn missing<'a>(
        &self,
        perl: &Perl,
        deps: impl Iterator<Item = &'a Dependency>,
    ) -> Vec<Dependency> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for dep in deps {
            if dep.module == "perl" || !seen.insert(dep.module.clone()) {
                continue;
            }
            if !satisfied(perl, dep) {
                out.push(dep.clone());
            }
        }
        out
    }

    /// Open the unpacked distribution in `dir`, honouring `dist.prefer`.
    fn open_distribution(&self, dir: &Path) -> Result<Distribution> {
        let perl = self.perl.clone();
        let opened = match self.prefer {
            None => Distribution::new(dir, perl),
            Some(tool) => Distribution::with_preference(dir, perl, tool),
        };
        opened.with_context(|| format!("opening the distribution in {}", dir.display()))
    }

    /// Resolve a top-level SPEC (a module *or* distribution name).
    async fn resolve_spec(&self, client: &Client, spec: &str) -> Result<Resolved> {
        match self.source {
            CpanSource::Mirror => self.resolve_from_index(spec),
            CpanSource::Metacpan => match client.download_url(spec).await {
                Ok(d) => self.resolved_from_download_url(spec, &d),
                Err(err) if err.is_not_found() => {
                    let release = client.release(spec).await.with_context(|| {
                        format!("`{spec}` is not a known module or distribution")
                    })?;
                    self.resolved_from_release(spec, &release)
                }
                Err(err) => Err(anyhow::Error::new(err).context(format!("resolving `{spec}`"))),
            },
        }
    }

    /// Resolve a module name to its providing release.
    async fn resolve_module(&self, client: &Client, module: &str) -> Result<Resolved> {
        match self.source {
            CpanSource::Mirror => self.resolve_from_index(module),
            CpanSource::Metacpan => {
                let d = client
                    .download_url(module)
                    .await
                    .map_err(anyhow::Error::new)?;
                self.resolved_from_download_url(module, &d)
            }
        }
    }

    /// Resolve `query` against the mirror's `02packages` index.
    fn resolve_from_index(&self, query: &str) -> Result<Resolved> {
        let index = self
            .packages
            .as_ref()
            .expect("the package index is loaded in mirror mode");
        resolve_index_entry(index, &self.mirror_base_url, query)
    }

    fn resolved_from_download_url(&self, query: &str, d: &DownloadUrl) -> Result<Resolved> {
        let cdn = d
            .download_url
            .as_deref()
            .with_context(|| format!("MetaCPAN returned no download URL for `{query}`"))?;
        let (distribution, version) = name_and_version(
            d.distribution.as_deref(),
            d.version.as_deref(),
            d.release.as_deref(),
            query,
        )?;
        Ok(Resolved {
            archive_name: archive_name(cdn, &distribution, &version),
            url: cdn.to_string(),
            checksum: d.checksum_sha256.clone(),
            distribution,
            version,
        })
    }

    fn resolved_from_release(&self, query: &str, r: &Release) -> Result<Resolved> {
        let cdn = r
            .download_url
            .as_deref()
            .with_context(|| format!("MetaCPAN returned no download URL for `{query}`"))?;
        let (distribution, version) = name_and_version(
            r.distribution.as_deref(),
            r.version.as_deref(),
            r.name.as_deref(),
            query,
        )?;
        Ok(Resolved {
            archive_name: archive_name(cdn, &distribution, &version),
            url: cdn.to_string(),
            checksum: r.checksum_sha256.clone(),
            distribution,
            version,
        })
    }

    /// Append one step's captured output to the shared `install.log`, under a
    /// header line so the steps stay separable.
    fn log(&self, label: &str, step: &str, output: &str) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("opening {}", self.log_path.display()))?;
        writeln!(file, "===== {label}  {step} =====")?;
        file.write_all(output.as_bytes())?;
        if !output.ends_with('\n') {
            writeln!(file)?;
        }
        Ok(())
    }

    /// Write `<label>.<step>.json` with the same body `upt dist <step> --json`
    /// produces.
    fn write_step_json(&self, label: &str, step: &str, body: Value) -> Result<()> {
        let path = self.run_dir.join(format!("{label}.{step}.json"));
        fs::write(&path, crate::json::to_string(&body, false))
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Print the concise stdout line(s) for a finished step: one `ok` / `FAILED`
    /// line, plus a `missing:` line naming any prerequisites still to install.
    fn report(&self, label: &str, step: &str, ok: bool, code: u8, missing: &[Dependency]) {
        if ok {
            println!("{label}  {step}  ok");
        } else {
            println!("{label}  {step}  FAILED (exit {code})");
        }
        if !missing.is_empty() {
            println!("{label}  {step}  missing: {}", format_missing(missing));
        }
    }
}

/// Create a unique run directory `base/<stamp>`, adding a `-NN` suffix if that
/// name is taken (another `upt cpan install` in the same second). `create_dir`
/// is atomic, so racing processes each get their own.
fn unique_run_dir(base: &Path, stamp: &str) -> Result<PathBuf> {
    fs::create_dir_all(base)?;
    for n in 0..=99 {
        let name = if n == 0 {
            stamp.to_string()
        } else {
            format!("{stamp}-{n:02}")
        };
        let dir = base.join(&name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    bail!("{stamp}: 100 run directories already exist for this second")
}

/// Point `base/latest` at `run_dir` (by its final component, so the link stays
/// valid if the cache moves). Best effort: any failure, including another
/// process winning the race, is ignored.
fn link_latest(base: &Path, run_dir: &Path) {
    #[cfg(unix)]
    {
        let Some(name) = run_dir.file_name() else {
            return;
        };
        let tmp = base.join(format!(".latest.{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        if std::os::unix::fs::symlink(name, &tmp).is_ok() {
            // rename over the existing link is atomic on unix.
            let _ = fs::rename(&tmp, base.join("latest"));
        }
        let _ = fs::remove_file(&tmp);
    }
    #[cfg(not(unix))]
    {
        let _ = (base, run_dir);
    }
}

/// The current UTC time as `YYYYMMDDTHHMMSSZ`.
fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_utc(secs as i64)
}

/// Format a Unix timestamp (seconds) as `YYYYMMDDTHHMMSSZ`.
fn format_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    format!("{y:04}{m:02}{d:02}T{hh:02}{mm:02}{ss:02}Z")
}

/// Convert a day count since 1970-01-01 to `(year, month, day)`, using Howard
/// Hinnant's `civil_from_days` algorithm (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

/// Resolve a `(distribution, version)` pair from the explicit fields MetaCPAN
/// returned, falling back to splitting a `Foo-Bar-1.23` release name.
fn name_and_version(
    distribution: Option<&str>,
    version: Option<&str>,
    release: Option<&str>,
    query: &str,
) -> Result<(String, String)> {
    let split = release.and_then(split_release_name);
    let distribution = distribution
        .map(str::to_string)
        .or_else(|| split.as_ref().map(|(d, _)| d.clone()))
        .with_context(|| format!("could not determine the distribution name for `{query}`"))?;
    let version = version
        .map(str::to_string)
        .or_else(|| split.as_ref().map(|(_, v)| v.clone()))
        .with_context(|| format!("could not determine the version for `{query}`"))?;
    Ok((distribution, version))
}

/// Split a release name like `JSON-PP-4.16` into `("JSON-PP", "4.16")`. The last
/// `-`-separated segment is only taken as the version when it starts with a
/// digit; otherwise `None`.
fn split_release_name(release: &str) -> Option<(String, String)> {
    let (name, version) = release.rsplit_once('-')?;
    if version.starts_with(|c: char| c.is_ascii_digit()) && !name.is_empty() {
        Some((name.to_string(), version.to_string()))
    } else {
        None
    }
}

/// The file name to save a tarball under: the last path segment of the download
/// URL (query string stripped), or `<distribution>-<version>.tar.gz` if that
/// cannot be derived.
fn archive_name(url: &str, distribution: &str, version: &str) -> String {
    url.split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .find(|s| !s.is_empty())
        .filter(|s| s.contains('.'))
        .map_or_else(
            || format!("{distribution}-{version}.tar.gz"),
            str::to_string,
        )
}

/// Strip a CPAN archive's compression/container extension:
/// `Foo-Bar-1.23.tar.gz` -> `Foo-Bar-1.23`.
fn strip_archive_ext(archive: &str) -> &str {
    for ext in [
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz", ".tbz2", ".txz", ".zip", ".tar",
    ] {
        if let Some(stem) = archive.strip_suffix(ext) {
            return stem;
        }
    }
    archive
}

/// Last-resort `02packages` lookup: the first entry whose archive path is a
/// release of the distribution named `dist` (its main module was not the
/// dash-to-`::` transform of the name).
fn find_in_index<'a>(
    index: &'a PackageDetails,
    dist: &str,
) -> Option<&'a cpan_packagedetails::Entry> {
    index.entries().find(|entry| {
        let archive = entry.path().rsplit('/').next().unwrap_or("");
        split_release_name(strip_archive_ext(archive)).is_some_and(|(name, _)| name == dist)
    })
}

/// Resolve `query` (a module name, a dashed distribution name, or a dashed name
/// whose main module differs) against a `02packages` index and build the
/// [`Resolved`] pointing at `<mirror_base_url>/authors/id/<path>`.
fn resolve_index_entry(
    index: &PackageDetails,
    mirror_base_url: &str,
    query: &str,
) -> Result<Resolved> {
    let entry = index
        .get(query)
        .or_else(|| {
            (!query.contains("::"))
                .then(|| index.get(&query.replace('-', "::")))
                .flatten()
        })
        .or_else(|| find_in_index(index, query))
        .with_context(|| format!("`{query}` is not in the mirror package index"))?;

    // `entry.path()` is relative to `authors/id/`, e.g.
    // `H/HA/HAARG/JSON-PP-4.16.tar.gz`.
    let rel_path = entry.path().trim_start_matches('/');
    let archive_name = rel_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("index entry for `{query}` has no archive path"))?
        .to_string();
    let (distribution, version) =
        name_and_version(None, None, Some(strip_archive_ext(&archive_name)), query)?;

    Ok(Resolved {
        url: format!(
            "{}/authors/id/{rel_path}",
            mirror_base_url.trim_end_matches('/')
        ),
        checksum: None,
        archive_name,
        distribution,
        version,
    })
}

/// The JSON envelope `upt dist <step> --json` emits: an optional `prereqs`
/// object, then `output`, `exit`, `success` — in that order.
fn envelope(prereqs: Option<Value>, output: &str, exit: u8, success: bool) -> Value {
    let mut map = Map::new();
    if let Some(prereqs) = prereqs {
        map.insert("prereqs".to_string(), prereqs);
    }
    map.insert("output".to_string(), Value::String(output.to_string()));
    map.insert("exit".to_string(), json!(exit));
    map.insert("success".to_string(), json!(success));
    Value::Object(map)
}

/// The exit code to record for a finished step: `0` on success, the child's code
/// (coerced into `1..=255`) on a non-zero exit, `1` when killed by a signal.
fn exit_code(result: &ExecuteResult) -> u8 {
    exit_code_parts(result.is_success, result.code)
}

fn exit_code_parts(is_success: bool, code: Option<i32>) -> u8 {
    if is_success {
        return 0;
    }
    match code {
        Some(code) => {
            let byte = u8::try_from(code).unwrap_or(1);
            if byte == 0 { 1 } else { byte }
        }
        None => 1,
    }
}

/// Whether `dep` is already met on `perl`'s module search path. A live probe of
/// the filesystem (`perl.module`) plus the CPAN version-range check — there is
/// no persisted state to track, because the whole pipeline runs in one process.
fn satisfied(perl: &Perl, dep: &Dependency) -> bool {
    if dep.module == "perl" {
        return true;
    }
    match perl.module(&dep.module) {
        None => false,
        // Installed with a recoverable version: compare against the range.
        Some(found) => match found.version {
            Some(version) => crate::dist::version_satisfies(&dep.version, &version),
            // Installed but no version in the source: only an "any version"
            // requirement is satisfied.
            None => crate::dist::version_satisfies(&dep.version, ""),
        },
    }
}

/// A step's captured, merged stdout+stderr as a lossy UTF-8 string (`""` when
/// nothing was captured).
fn captured(result: &ExecuteResult) -> String {
    result
        .output_lossy()
        .map(|text| text.into_owned())
        .unwrap_or_default()
}

/// Verify `bytes` against `expected` (hex SHA-256, case-insensitive). A missing
/// digest is a warning, not an error — matching `upt metacpan download` only in
/// spirit, since here we still proceed.
fn verify_sha256(bytes: &[u8], expected: Option<&str>, label: &str) -> Result<()> {
    let Some(expected) = expected else {
        eprintln!("warning: no SHA-256 for {label}; skipping checksum verification");
        return Ok(());
    };
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hex::encode(hasher.finalize());
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        bail!("checksum mismatch for {label}: expected {expected}, got {actual}")
    }
}

/// Render a missing-prerequisite list as `Foo::Bar (>= 1.0), Baz` — the version
/// is shown only when it is an actual constraint (not `0` / empty).
fn format_missing(missing: &[Dependency]) -> String {
    missing
        .iter()
        .map(|d| {
            let v = d.version.trim();
            if v.is_empty() || v == "0" {
                d.module.clone()
            } else {
                format!("{} ({v})", d.module)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn dep(module: &str, version: &str) -> Dependency {
        Dependency {
            module: module.to_string(),
            version: version.to_string(),
        }
    }

    // -- CLI ----------------------------------------------------------------

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
        // `--no-tests` is accepted as an alias.
        assert!(install_args(&["install", "--no-tests", "JSON::PP"]).no_test);
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
    fn rejects_an_unknown_source_value() {
        assert!(
            Cli::try_parse_from(["upt cpan", "install", "JSON::PP", "--source", "cpanm"]).is_err()
        );
    }

    // -- run id / cache layout -------------------------------------------

    #[test]
    fn format_utc_renders_a_sortable_stamp() {
        assert_eq!(format_utc(0), "19700101T000000Z");
        assert_eq!(format_utc(1_704_067_200), "20240101T000000Z");
        // 2024 is a leap year: 2024-02-29 exists.
        assert_eq!(format_utc(1_709_164_800), "20240229T000000Z");
        assert_eq!(format_utc(1_709_251_199), "20240229T235959Z");
        // Stamps are lexically ordered by time.
        assert!(format_utc(1_000) < format_utc(2_000_000_000));
    }

    #[test]
    fn unique_run_dir_suffixes_on_collision() {
        let base = std::env::temp_dir().join(format!("upt-cpan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);

        let a = unique_run_dir(&base, "20260101T000000Z").unwrap();
        let b = unique_run_dir(&base, "20260101T000000Z").unwrap();
        let c = unique_run_dir(&base, "20260101T000000Z").unwrap();

        assert_eq!(a.file_name().unwrap(), "20260101T000000Z");
        assert_eq!(b.file_name().unwrap(), "20260101T000000Z-01");
        assert_eq!(c.file_name().unwrap(), "20260101T000000Z-02");
        assert!(a.is_dir() && b.is_dir() && c.is_dir());
        // The bare stamp sorts before its suffixed siblings.
        assert!(a.file_name().unwrap() < b.file_name().unwrap());

        let _ = fs::remove_dir_all(&base);
    }

    // -- resolve helpers -----------------------------------------------

    #[test]
    fn split_release_name_splits_on_the_version() {
        assert_eq!(
            split_release_name("JSON-PP-4.16"),
            Some(("JSON-PP".to_string(), "4.16".to_string()))
        );
        assert_eq!(
            split_release_name("Foo-Bar-1.23_04"),
            Some(("Foo-Bar".to_string(), "1.23_04".to_string()))
        );
        // No trailing numeric segment: not a release-name split.
        assert_eq!(split_release_name("Acme-Foo"), None);
        assert_eq!(split_release_name("nodash"), None);
    }

    #[test]
    fn name_and_version_prefers_explicit_fields_then_the_release_name() {
        assert_eq!(
            name_and_version(Some("JSON-PP"), Some("4.16"), None, "q").unwrap(),
            ("JSON-PP".to_string(), "4.16".to_string())
        );
        assert_eq!(
            name_and_version(None, None, Some("JSON-PP-4.16"), "q").unwrap(),
            ("JSON-PP".to_string(), "4.16".to_string())
        );
        assert!(name_and_version(None, None, None, "q").is_err());
    }

    #[test]
    fn archive_name_from_the_url_or_a_fallback() {
        assert_eq!(
            archive_name(
                "https://cpan.metacpan.org/authors/id/H/HA/HAARG/JSON-PP-4.16.tar.gz?x=1",
                "JSON-PP",
                "4.16"
            ),
            "JSON-PP-4.16.tar.gz"
        );
        assert_eq!(
            archive_name("https://example/no-file-here/", "Foo-Bar", "1.2"),
            "Foo-Bar-1.2.tar.gz"
        );
    }

    #[test]
    fn strip_archive_ext_handles_every_cpan_extension() {
        assert_eq!(strip_archive_ext("Foo-Bar-1.23.tar.gz"), "Foo-Bar-1.23");
        assert_eq!(strip_archive_ext("Foo-Bar-1.23.tgz"), "Foo-Bar-1.23");
        assert_eq!(strip_archive_ext("Foo-Bar-1.23.tar.bz2"), "Foo-Bar-1.23");
        assert_eq!(strip_archive_ext("Foo-Bar-1.23.tar.xz"), "Foo-Bar-1.23");
        assert_eq!(strip_archive_ext("Foo-Bar-1.23.zip"), "Foo-Bar-1.23");
        assert_eq!(strip_archive_ext("Foo-Bar-1.23"), "Foo-Bar-1.23");
    }

    fn index(entries: &[(&str, &str, &str)]) -> cpan_packagedetails::PackageDetails {
        let mut pd = cpan_packagedetails::PackageDetails::new();
        for (package, version, path) in entries {
            pd.add_entry(cpan_packagedetails::Entry::new(
                *package,
                Some(version.to_string()),
                *path,
            ))
            .unwrap();
        }
        pd
    }

    #[test]
    fn resolve_index_entry_by_module_name() {
        let pd = index(&[("JSON::PP", "4.16", "H/HA/HAARG/JSON-PP-4.16.tar.gz")]);
        let r = resolve_index_entry(&pd, "https://mirror.example/cpan/", "JSON::PP").unwrap();
        assert_eq!(r.distribution, "JSON-PP");
        assert_eq!(r.version, "4.16");
        assert_eq!(r.archive_name, "JSON-PP-4.16.tar.gz");
        assert_eq!(
            r.url,
            "https://mirror.example/cpan/authors/id/H/HA/HAARG/JSON-PP-4.16.tar.gz"
        );
        assert_eq!(r.checksum, None);
    }

    #[test]
    fn resolve_index_entry_falls_back_to_the_main_module_then_a_scan() {
        let pd = index(&[
            ("LWP", "6.77", "O/OA/OALDERS/libwww-perl-6.77.tar.gz"),
            ("Try::Tiny", "0.31", "E/ET/ETHER/Try-Tiny-0.31.tar.gz"),
        ]);
        // Dashed name whose main module is the dash->:: transform.
        let r = resolve_index_entry(&pd, "https://m/", "Try-Tiny").unwrap();
        assert_eq!(r.distribution, "Try-Tiny");
        // Dashed name whose main module is *not* the transform: found by scan.
        let r = resolve_index_entry(&pd, "https://m/", "libwww-perl").unwrap();
        assert_eq!(r.distribution, "libwww-perl");
        assert_eq!(r.version, "6.77");
        assert_eq!(
            r.url,
            "https://m/authors/id/O/OA/OALDERS/libwww-perl-6.77.tar.gz"
        );
    }

    #[test]
    fn resolve_index_entry_errors_when_absent() {
        let pd = index(&[("Foo::Bar", "1.0", "A/AA/AAA/Foo-Bar-1.0.tar.gz")]);
        let err = resolve_index_entry(&pd, "https://m/", "No::Such::Module")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not in the mirror package index"), "{err}");
    }

    #[test]
    fn file_url_path_maps_only_file_urls_to_local_paths() {
        assert_eq!(file_url_path("https://cpan.example/x").unwrap(), None);
        assert_eq!(file_url_path("http://127.0.0.1:9/x").unwrap(), None);

        assert_eq!(
            file_url_path("file:///srv/CPAN/modules/02packages.details.txt.gz").unwrap(),
            Some(PathBuf::from("/srv/CPAN/modules/02packages.details.txt.gz"))
        );
        // Percent-encoded bytes are decoded.
        assert_eq!(
            file_url_path("file:///tmp/my%20mirror/x").unwrap(),
            Some(PathBuf::from("/tmp/my mirror/x"))
        );

        // A remote host in a file:// URL is not a local path (on unix).
        #[cfg(unix)]
        assert!(file_url_path("file://elsewhere/srv/CPAN/x").is_err());
    }

    // -- step bookkeeping --------------------------------------------------

    #[test]
    fn envelope_key_order_matches_upt_dist() {
        let v = envelope(Some(json!({ "configure": [] })), "out", 2, false);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["prereqs", "output", "exit", "success"]);
        assert_eq!(v["output"], json!("out"));
        assert_eq!(v["exit"], json!(2));
        assert_eq!(v["success"], json!(false));

        // No prereqs for build/test/install.
        let v = envelope(None, "", 0, true);
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["output", "exit", "success"]);
    }

    #[test]
    fn exit_code_maps_success_failure_and_signal() {
        assert_eq!(exit_code_parts(true, Some(0)), 0);
        assert_eq!(exit_code_parts(false, Some(2)), 2);
        assert_eq!(exit_code_parts(false, Some(256)), 1); // truncates to 0 -> 1
        assert_eq!(exit_code_parts(false, None), 1); // signal
    }

    #[test]
    fn verify_sha256_matches_is_case_insensitive_and_catches_mismatch() {
        // echo -n hello | sha256sum
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(b"hello", Some(&digest.to_uppercase()), "x").is_ok());
        assert!(verify_sha256(b"hello", Some(digest), "x").is_ok());
        assert!(verify_sha256(b"hello!", Some(digest), "x").is_err());
        // Missing digest: a warning, not an error.
        assert!(verify_sha256(b"hello", None, "x").is_ok());
    }

    #[test]
    fn format_missing_shows_real_constraints_only() {
        assert_eq!(
            format_missing(&[dep("Foo::Bar", ">= 1.0"), dep("Baz", "0"), dep("Q", "")]),
            "Foo::Bar (>= 1.0), Baz, Q"
        );
    }

    #[test]
    fn civil_from_days_round_trips_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
