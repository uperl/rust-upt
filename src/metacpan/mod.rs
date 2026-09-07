//! `upt metacpan` — a command line interface to the MetaCPAN API.
//!
//! Each MetaCPAN document type is a sub-subcommand. Results print as a
//! formatted table by default; `--json` switches to pretty-printed JSON,
//! coloured per `--color` (which defaults to upt's effective `global.color`);
//! `--raw` prints the underlying HTTP request and response instead, and
//! `--curl` prints the equivalent `curl` command line without making the
//! request. Responses are cached under `<cache home>/upt/metacpan`. A failing
//! request returns its error to `upt`, so it is reported like any other `upt`
//! error.

mod diskusage;
mod render;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use metacpan_api_modern::reqwest::Url;
use metacpan_api_modern::types::{DownloadUrl, Permission};
use metacpan_api_modern::{Client, PodFormat};
use serde_json::{Value, json};

use crate::json;
use sha2::{Digest, Sha256};

/// `User-Agent` sent with every request; also shown in `--raw` output.
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Entry point for the `metacpan` built-in: parse `args` with clap, then run the
/// request on a fresh Tokio runtime.
pub fn run(cx: &crate::Cx, args: &[String]) -> Result<i32> {
    let argv =
        std::iter::once(OsString::from("upt metacpan")).chain(args.iter().map(OsString::from));
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        // clap prints `--help` / `--version` and usage errors itself; mirror
        // its own exit codes (0 for help/version, 2 for a usage error).
        Err(err) => {
            err.print().ok();
            return Ok(err.exit_code());
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    // Let the error propagate so `upt`'s top-level handler reports it the same
    // way as any other subcommand failure (`upt: <chain>`, exit 1).
    runtime.block_on(dispatch(cx, cli))?;
    Ok(0)
}

/// Command line interface to the MetaCPAN API.
#[derive(Parser)]
#[command(
    name = "upt metacpan",
    version,
    about = "Command line interface to the MetaCPAN API",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[command(flatten)]
    global: GlobalOpts,
}

#[derive(Args)]
struct GlobalOpts {
    /// Print pretty-printed JSON instead of a table.
    #[arg(long, short = 'j', global = true)]
    json: bool,

    /// When to colourise JSON output. Defaults to upt's `global.color` setting.
    #[arg(long, value_enum, global = true, value_name = "WHEN")]
    color: Option<ColorWhen>,

    /// Directory for the on-disk response cache.
    ///
    /// Defaults to a platform cache location (e.g. ~/.cache/upt/metacpan on
    /// Linux, ~/Library/Caches/upt/metacpan on macOS,
    /// %LOCALAPPDATA%\upt\metacpan on Windows).
    #[arg(long, global = true, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    /// Do not read from or write to the response cache.
    #[arg(long, global = true)]
    no_cache: bool,

    /// Override the API base URL.
    #[arg(long, global = true, value_name = "URL")]
    base_url: Option<String>,

    /// Print the raw HTTP request and response — request line, headers, and
    /// body — for each request the command makes, instead of a table or JSON.
    #[arg(long, global = true)]
    raw: bool,

    /// Print the equivalent `curl` command line instead of making the request.
    #[arg(long, global = true, conflicts_with = "raw")]
    curl: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ColorWhen {
    /// Colour when stdout is a terminal.
    Auto,
    /// Always colour.
    Always,
    /// Never colour.
    Never,
}

impl From<crate::config::ColorChoice> for ColorWhen {
    fn from(choice: crate::config::ColorChoice) -> Self {
        match choice {
            crate::config::ColorChoice::Always => ColorWhen::Always,
            crate::config::ColorChoice::Never => ColorWhen::Never,
            crate::config::ColorChoice::Auto => ColorWhen::Auto,
        }
    }
}

#[derive(Copy, Clone, ValueEnum)]
enum PodFmt {
    Plain,
    Html,
    Markdown,
    Pod,
}

impl From<PodFmt> for PodFormat {
    fn from(f: PodFmt) -> Self {
        match f {
            PodFmt::Plain => PodFormat::Plain,
            PodFmt::Html => PodFormat::Html,
            PodFmt::Markdown => PodFormat::Markdown,
            PodFmt::Pod => PodFormat::Pod,
        }
    }
}

#[derive(Subcommand)]
enum CacheAction {
    /// Delete every cached response.
    Clear,
    /// Print the cache directory path.
    Path,
    /// Show the cache location, entry count, and disk space it uses.
    Status,
}

#[derive(Copy, Clone, ValueEnum)]
enum RiverSort {
    /// CPAN River total: transitive downstream count.
    Total,
    /// CPAN River immediate: direct dependent count.
    Immediate,
}

#[derive(Subcommand)]
enum RiverAction {
    /// List a distribution's direct reverse dependencies, ordered by a CPAN
    /// River figure (transitive `total` by default), highest first.
    ///
    /// Pages through the reverse-dependency list and then looks up the River
    /// figures for those distributions, so it makes several requests.
    Distribution {
        /// Distribution name, e.g. Try-Tiny.
        distribution: String,
        /// Which CPAN River figure to sort on.
        #[arg(long, value_enum, default_value = "total")]
        by: RiverSort,
        /// Sort ascending (smallest first) instead of descending.
        #[arg(long)]
        reverse: bool,
        /// Print only the top N rows (with --reverse, the top N shown smallest-first).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// List the distributions whose current (latest, non-dev) release is by an
    /// author, ordered by a CPAN River figure (transitive `total` by default),
    /// highest first.
    ///
    /// Pages through the author's latest releases and then looks up the River
    /// figures for those distributions, so it makes several requests.
    Author {
        /// PAUSE id, e.g. PLICEASE (case-insensitive).
        pauseid: String,
        /// Which CPAN River figure to sort on.
        #[arg(long, value_enum, default_value = "total")]
        by: RiverSort,
        /// Sort ascending (smallest first) instead of descending.
        #[arg(long)]
        reverse: bool,
        /// Print only the top N rows (with --reverse, the top N shown smallest-first).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },
}

#[derive(Subcommand)]
enum PermissionsAction {
    /// PAUSE upload permissions for one or more module namespaces.
    ///
    /// One module is a `GET /permission/{module}` (a missing namespace is an
    /// error); two or more use `GET /permission/by_module` in a single
    /// request, where a namespace with no `06perms` entry is just absent.
    Module {
        /// Module namespace, e.g. Moose. Repeat for a batch lookup.
        #[arg(required = true, value_name = "MODULE")]
        modules: Vec<String>,
    },

    /// Every module namespace a PAUSE id owns or co-maintains.
    ///
    /// `GET /permission/by_author/{pauseid}`. `--owner` and `--comaint` filter
    /// the result client-side; giving both is the same as giving neither.
    Author {
        /// PAUSE id, e.g. PLICEASE (case-insensitive).
        pauseid: String,
        /// Keep only namespaces where PAUSEID is the primary owner.
        #[arg(long)]
        owner: bool,
        /// Keep only namespaces where PAUSEID is a co-maintainer.
        #[arg(long)]
        comaint: bool,
    },
}

#[derive(Subcommand)]
enum Command {
    /// Look up a CPAN author by PAUSE id.
    Author {
        /// PAUSE id, e.g. PLICEASE (case-insensitive).
        pauseid: String,
    },

    /// Fetch a distribution release.
    ///
    /// With no --author this is the latest release of the distribution. With
    /// --author, NAME is a full release name including the version, e.g.
    /// FFI-Platypus-2.10.
    Release {
        /// Distribution name, or full release name when --author is given.
        name: String,
        /// PAUSE id of the uploading author.
        #[arg(long)]
        author: Option<String>,
    },

    /// Resolve a module name to the file that provides it.
    Module {
        /// Module name, e.g. FFI::Platypus.
        module: String,
    },

    /// Fetch metadata for one file within a release.
    File {
        /// PAUSE id of the release author.
        author: String,
        /// Full release name, e.g. FFI-Platypus-2.10.
        release: String,
        /// Archive-relative path, e.g. lib/FFI/Platypus.pm.
        path: String,
    },

    /// Print the raw source of one file within a release.
    Source {
        author: String,
        release: String,
        path: String,
    },

    /// Print rendered documentation for a module.
    Pod {
        /// Module name, e.g. FFI::Platypus.
        module: String,
        /// Rendering format.
        #[arg(long, value_enum, default_value = "plain")]
        format: PodFmt,
    },

    /// Fetch distribution-level aggregate data (CPAN River, bugs, ...).
    Distribution {
        /// Distribution name, e.g. FFI-Platypus.
        distribution: String,
    },

    /// Fetch a distribution's change log.
    ///
    /// With no --author this is the change log of the latest release. With
    /// --author, NAME is a full release name including the version.
    Changes {
        /// Distribution name, or full release name when --author is given.
        name: String,
        /// PAUSE id of the release author.
        #[arg(long)]
        author: Option<String>,
    },

    /// Resolve the download URL for the release providing a module.
    DownloadUrl {
        /// Module name, e.g. FFI::Platypus.
        module: String,
        /// Version constraint, e.g. "== 2.08" or "<= 2.10".
        #[arg(long)]
        version: Option<String>,
        /// Allow developer (trial) releases.
        #[arg(long)]
        dev: bool,
    },

    /// Download the release providing a module and verify its checksum.
    ///
    /// Resolves the same URL as `download-url`, fetches the tarball into the
    /// current directory, and checks its SHA-256 against the digest reported by
    /// the API. Exits non-zero if any request fails or the checksum mismatches;
    /// on a mismatch nothing is written.
    Download {
        /// Module name, e.g. FFI::Platypus.
        module: String,
        /// Version constraint, e.g. "== 2.08" or "<= 2.10".
        #[arg(long)]
        version: Option<String>,
        /// Allow developer (trial) releases.
        #[arg(long)]
        dev: bool,
    },

    /// List known CPAN mirrors.
    Mirrors,

    /// Inspect or clear the on-disk response cache.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// CPAN River queries: reverse dependencies of a distribution, or the
    /// distributions currently released by an author.
    River {
        #[command(subcommand)]
        action: RiverAction,
    },

    /// PAUSE upload permissions (`06perms`): who may release a module.
    Permissions {
        #[command(subcommand)]
        action: PermissionsAction,
    },

    /// List distributions up for adoption — those with a current release
    /// providing a module namespace that ADOPTME or HANDOFF owns or
    /// co-maintains — with their CPAN River figures, most-depended-on first.
    ///
    /// Reads both authors' permissions, resolves the namespaces to the
    /// distributions that currently provide them, and looks up River figures,
    /// so it makes multiple requests.
    Adoptable {
        /// Which CPAN River figure to sort on.
        #[arg(long, value_enum, default_value = "total")]
        by: RiverSort,
        /// Sort ascending (smallest first) instead of descending.
        #[arg(long)]
        reverse: bool,
        /// Print only the top N rows (with --reverse, the top N shown smallest-first).
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
    },

    /// Search a document type with a Lucene query string.
    Search {
        /// Document type: release, author, module, file, distribution, favorite.
        #[arg(long, short = 't', value_name = "TYPE")]
        r#type: String,
        /// Lucene query, e.g. "author:PLICEASE AND status:latest".
        #[arg(long, short = 'q', value_name = "QUERY")]
        query: String,
        /// Maximum number of hits to return.
        #[arg(long, short = 's')]
        size: Option<u32>,
        /// Offset of the first hit (for pagination).
        #[arg(long)]
        from: Option<u32>,
    },
}

async fn dispatch(cx: &crate::Cx, cli: Cli) -> Result<()> {
    let g = &cli.global;
    // `--color` wins; without it, follow upt's effective `global.color`.
    let color = match g.color.unwrap_or_else(|| cx.color.into()) {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => std::io::stdout().is_terminal(),
    };
    let client = build_client(cx, g)?;

    if g.raw {
        return run_raw(&client, &cli.command).await;
    }

    if g.curl {
        return run_curl(&client, &cli.command);
    }

    match &cli.command {
        Command::Author { pauseid } => {
            let v = get(&client, &format!("author/{pauseid}")).await?;
            emit(v, g.json, color, |v| render::author(v, color))?;
        }

        Command::Release { name, author } => {
            let v = match author {
                Some(author) => {
                    let v = get(&client, &format!("release/{author}/{name}")).await?;
                    unwrap_key(v, "release")
                }
                None => get(&client, &format!("release/{name}")).await?,
            };
            emit(v, g.json, color, |v| render::release(v, color))?;
        }

        Command::Module { module } => {
            let v = get(&client, &format!("module/{module}")).await?;
            emit(v, g.json, color, |v| render::file_doc(v, color))?;
        }

        Command::File {
            author,
            release,
            path,
        } => {
            let path = path.trim_start_matches('/');
            let v = get(&client, &format!("file/{author}/{release}/{path}")).await?;
            emit(v, g.json, color, |v| render::file_doc(v, color))?;
        }

        Command::Source {
            author,
            release,
            path,
        } => {
            let text = client
                .source(author, release, path.trim_start_matches('/'))
                .await
                .context("fetching source")?;
            emit_text(&text, g.json, color);
        }

        Command::Pod { module, format } => {
            let text = client
                .pod(module, (*format).into())
                .await
                .context("fetching pod")?;
            emit_text(&text, g.json, color);
        }

        Command::Distribution { distribution } => {
            let v = get(&client, &format!("distribution/{distribution}")).await?;
            emit(v, g.json, color, |v| render::distribution(v, color))?;
        }

        Command::Changes { name, author } => {
            let path = match author {
                Some(author) => format!("changes/{author}/{name}"),
                None => format!("changes/{name}"),
            };
            let v = get(&client, &path).await?;
            emit(v, g.json, color, |v| render::changes(v, color))?;
        }

        Command::DownloadUrl {
            module,
            version,
            dev,
        } => {
            let mut query: Vec<(&str, String)> = Vec::new();
            if let Some(version) = version {
                query.push(("version", version.clone()));
            }
            if *dev {
                query.push(("dev", "1".to_string()));
            }
            let v = get_query(&client, &format!("download_url/{module}"), &query).await?;
            emit(v, g.json, color, |v| render::download_url(v, color))?;
        }

        Command::Download {
            module,
            version,
            dev,
        } => {
            let mut query: Vec<(&str, String)> = Vec::new();
            if let Some(version) = version {
                query.push(("version", version.clone()));
            }
            if *dev {
                query.push(("dev", "1".to_string()));
            }
            let v = get_query(&client, &format!("download_url/{module}"), &query).await?;
            let d: DownloadUrl =
                serde_json::from_value(v).context("decoding download_url response")?;

            let url = d
                .download_url
                .as_deref()
                .context("API response contained no download_url")?;
            let expected = d.checksum_sha256.as_deref().context(
                "API response contained no checksum_sha256; refusing to download unverified",
            )?;
            let file_name = url
                .split('?')
                .next()
                .unwrap_or(url)
                .rsplit('/')
                .find(|s| !s.is_empty())
                .context("could not derive a file name from the download URL")?;

            let response = client
                .http()
                .get(url)
                .send()
                .await
                .with_context(|| format!("downloading {url}"))?;
            let status = response.status();
            if !status.is_success() {
                anyhow::bail!("downloading {url}: HTTP {}", status.as_u16());
            }
            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("reading body of {url}"))?;

            let mut hasher = Sha256::new();
            hasher.update(&bytes);
            let actual = hex::encode(hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected) {
                anyhow::bail!(
                    "checksum mismatch for {file_name}: expected {expected}, got {actual}"
                );
            }

            std::fs::write(file_name, &bytes).with_context(|| format!("writing {file_name}"))?;

            if g.json {
                print!(
                    "{}",
                    json::to_string(
                        &json!({
                            "file": file_name,
                            "bytes": bytes.len(),
                            "checksum_sha256": actual,
                            "release": d.release,
                            "version": d.version,
                            "download_url": url,
                        }),
                        color
                    )
                );
            } else {
                println!(
                    "{file_name}  {}  sha256 ok",
                    human_bytes(bytes.len() as u64)
                );
            }
        }

        Command::Mirrors => {
            let v = get(&client, "mirror").await?;
            let v = unwrap_key(v, "mirrors");
            emit(v, g.json, color, |v| render::mirrors(v, color))?;
        }

        Command::Cache { action } => {
            let dir = resolve_cache_dir(cx, g)
                .context("could not determine a cache directory; pass --cache-dir")?;
            match action {
                CacheAction::Path => println!("{}", dir.display()),
                CacheAction::Clear => {
                    Client::builder()
                        .cache_dir(dir.clone())
                        .build()?
                        .clear_cache()
                        .with_context(|| format!("clearing cache at {}", dir.display()))?;
                    println!("cleared cache at {}", dir.display());
                }
                CacheAction::Status => {
                    println!("location:   {}", dir.display());
                    if dir.exists() {
                        let (files, bytes) = diskusage::measure(&dir)
                            .with_context(|| format!("measuring {}", dir.display()))?;
                        println!("entries:    {files}");
                        println!("disk usage: {} ({bytes} bytes)", human_bytes(bytes));
                    } else {
                        println!("entries:    0");
                        println!("disk usage: 0 B (not created yet)");
                    }
                }
            }
        }

        Command::River { action } => match action {
            RiverAction::Distribution {
                distribution,
                by,
                reverse,
                limit,
            } => {
                let mut rows = river_distribution(&client, distribution).await?;
                let total = finish_river(&mut rows, *by, *reverse, *limit);
                if g.json {
                    print!(
                        "{}",
                        json::to_string(&river_json(&rows, Some("author")), color)
                    );
                } else {
                    render::river(&rows, Some("author"), color);
                    count_line(rows.len(), total, "direct reverse dependencies");
                }
            }

            RiverAction::Author {
                pauseid,
                by,
                reverse,
                limit,
            } => {
                let mut rows = river_author(&client, pauseid).await?;
                let total = finish_river(&mut rows, *by, *reverse, *limit);
                if g.json {
                    print!("{}", json::to_string(&river_json(&rows, None), color));
                } else {
                    render::river(&rows, None, color);
                    count_line(
                        rows.len(),
                        total,
                        &format!("distributions with a current release by {pauseid}"),
                    );
                }
            }
        },

        Command::Permissions { action } => {
            let perms = match action {
                PermissionsAction::Module { modules } => match modules.as_slice() {
                    [only] => vec![
                        client
                            .permission(only)
                            .await
                            .with_context(|| format!("fetching permissions for {only}"))?,
                    ],
                    many => client
                        .permissions_by_module(many)
                        .await
                        .context("fetching permissions")?,
                },
                PermissionsAction::Author {
                    pauseid,
                    owner,
                    comaint,
                } => {
                    // The by_author endpoint matches the PAUSE id exactly, and
                    // PAUSE ids are upper-case.
                    let pauseid = pauseid.to_uppercase();
                    let mut perms = client
                        .permissions_by_author(&pauseid)
                        .await
                        .with_context(|| format!("fetching permissions for {pauseid}"))?;
                    if *owner || *comaint {
                        perms.retain(|p| {
                            let is_owner = p
                                .owner
                                .as_deref()
                                .is_some_and(|o| o.eq_ignore_ascii_case(&pauseid));
                            let is_comaint = p
                                .co_maintainers
                                .iter()
                                .any(|c| c.eq_ignore_ascii_case(&pauseid));
                            (*owner && is_owner) || (*comaint && is_comaint)
                        });
                    }
                    perms
                }
            };
            if g.json {
                let arr: Vec<Value> = perms.iter().map(permission_json).collect();
                print!("{}", json::to_string(&Value::Array(arr), color));
            } else {
                render::permissions(&perms, color);
            }
        }

        Command::Adoptable { by, reverse, limit } => {
            let mut rows = adoptable_rows(&client).await?;
            let total = finish_river(&mut rows, *by, *reverse, *limit);
            if g.json {
                print!(
                    "{}",
                    json::to_string(&river_json(&rows, Some("pauseid")), color)
                );
            } else {
                render::river(&rows, Some("pause id"), color);
                count_line(rows.len(), total, "adoptable distributions");
            }
        }

        Command::Search {
            r#type,
            query,
            size,
            from,
        } => {
            let mut params: Vec<(&str, String)> = vec![("q", query.clone())];
            if let Some(size) = size {
                params.push(("size", size.to_string()));
            }
            if let Some(from) = from {
                params.push(("from", from.to_string()));
            }
            let v = get_query(&client, &format!("{type}/_search"), &params).await?;
            if g.json {
                print!("{}", json::to_string(&v, color));
            } else {
                render::search(v, r#type, color)?;
            }
        }
    }

    Ok(())
}

fn build_client(cx: &crate::Cx, g: &GlobalOpts) -> Result<Client> {
    let mut builder = Client::builder().user_agent(USER_AGENT);
    if let Some(base) = &g.base_url {
        builder = builder.base_url(base.clone());
    }
    if !g.no_cache
        && let Some(dir) = resolve_cache_dir(cx, g)
    {
        builder = builder.cache_dir(dir);
    }
    builder.build().context("building HTTP client")
}

/// The cache directory to use: an explicit `--cache-dir`, otherwise
/// `metacpan/` under `upt`'s per-platform cache directory
/// (`~/.cache/upt/metacpan` and equivalents). `None` only if the platform
/// cache home cannot be determined and nothing was passed.
fn resolve_cache_dir(cx: &crate::Cx, g: &GlobalOpts) -> Option<PathBuf> {
    g.cache_dir
        .clone()
        .or_else(|| cx.cache_dir.clone().map(|base| base.join("metacpan")))
}

/// `--raw`: print the raw HTTP request and response for every request the
/// command makes, and nothing else — no table, no JSON. The response cache is
/// bypassed, so the exchange shown is one that actually happened.
async fn run_raw(client: &Client, command: &Command) -> Result<()> {
    match command {
        Command::Cache { .. } => {
            anyhow::bail!("--raw does not apply to `cache` subcommands; they make no HTTP requests")
        }
        Command::River { .. } => {
            anyhow::bail!("--raw does not apply to `river` subcommands; they make several requests")
        }
        Command::Adoptable { .. } => {
            anyhow::bail!("--raw does not apply to `adoptable`; it makes multiple requests")
        }

        // `download` makes two requests: the download_url lookup, then a GET of
        // the tarball it resolves to. Show both.
        Command::Download {
            module,
            version,
            dev,
        } => {
            let url = download_url_endpoint(client, module, version.as_deref(), *dev)?;
            let (status, body) = raw_get(client, url).await?;
            bail_on_http_error(status)?;
            if let Ok(d) = serde_json::from_slice::<DownloadUrl>(&body)
                && let Some(tarball) = d.download_url.as_deref()
            {
                let tarball = Url::parse(tarball)
                    .with_context(|| format!("parsing download URL {tarball}"))?;
                println!();
                let (status, _) = raw_get(client, tarball).await?;
                bail_on_http_error(status)?;
            }
            Ok(())
        }

        other => {
            let url = request_url(client, other)?;
            let (status, _) = raw_get(client, url).await?;
            bail_on_http_error(status)
        }
    }
}

/// `--curl`: print the `curl` command line equivalent to the request the
/// command would make, without making it. `download` and `download-url` print
/// the `download_url` lookup; the tarball URL it resolves to is only knowable
/// by running that request.
fn run_curl(client: &Client, command: &Command) -> Result<()> {
    match command {
        Command::Cache { .. } => {
            anyhow::bail!(
                "--curl does not apply to `cache` subcommands; they make no HTTP requests"
            )
        }
        Command::River { .. } => {
            anyhow::bail!(
                "--curl does not apply to `river` subcommands; they make several requests"
            )
        }
        Command::Adoptable { .. } => {
            anyhow::bail!("--curl does not apply to `adoptable`; it makes multiple requests")
        }
        _ => {}
    }
    let url = request_url(client, command)?;
    println!("curl {}", shell_quote(url.as_str()));
    Ok(())
}

/// Single-quote `s` for a POSIX shell, so it survives the shell verbatim.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The single request URL for each command that makes exactly one GET (and, for
/// `download`, the first of its two). Mirrors the path and query each command's
/// normal code path builds.
fn request_url(client: &Client, command: &Command) -> Result<Url> {
    let url = match command {
        Command::Author { pauseid } => client.url(&format!("author/{pauseid}"))?,

        Command::Release { name, author } => match author {
            Some(author) => client.url(&format!("release/{author}/{name}"))?,
            None => client.url(&format!("release/{name}"))?,
        },

        Command::Module { module } => client.url(&format!("module/{module}"))?,

        Command::File {
            author,
            release,
            path,
        } => client.url(&format!(
            "file/{author}/{release}/{}",
            path.trim_start_matches('/')
        ))?,

        Command::Source {
            author,
            release,
            path,
        } => client.url(&format!(
            "source/{author}/{release}/{}",
            path.trim_start_matches('/')
        ))?,

        Command::Pod { module, format } => {
            let mut url = client.url(&format!("pod/{module}"))?;
            url.query_pairs_mut()
                .append_pair("content-type", PodFormat::from(*format).mime());
            url
        }

        Command::Distribution { distribution } => {
            client.url(&format!("distribution/{distribution}"))?
        }

        Command::Changes { name, author } => match author {
            Some(author) => client.url(&format!("changes/{author}/{name}"))?,
            None => client.url(&format!("changes/{name}"))?,
        },

        Command::DownloadUrl {
            module,
            version,
            dev,
        }
        | Command::Download {
            module,
            version,
            dev,
        } => download_url_endpoint(client, module, version.as_deref(), *dev)?,

        Command::Mirrors => client.url("mirror")?,

        Command::Permissions { action } => match action {
            PermissionsAction::Module { modules } => match modules.as_slice() {
                [only] => client.url(&format!("permission/{only}"))?,
                many => {
                    let mut url = client.url("permission/by_module")?;
                    {
                        let mut pairs = url.query_pairs_mut();
                        for module in many {
                            pairs.append_pair("module", module);
                        }
                    }
                    url
                }
            },
            PermissionsAction::Author { pauseid, .. } => {
                client.url(&format!("permission/by_author/{}", pauseid.to_uppercase()))?
            }
        },

        Command::Search {
            r#type,
            query,
            size,
            from,
        } => {
            let mut url = client.url(&format!("{type}/_search"))?;
            {
                let mut pairs = url.query_pairs_mut();
                pairs.append_pair("q", query);
                if let Some(size) = size {
                    pairs.append_pair("size", &size.to_string());
                }
                if let Some(from) = from {
                    pairs.append_pair("from", &from.to_string());
                }
            }
            url
        }

        Command::Cache { .. } | Command::River { .. } | Command::Adoptable { .. } => {
            unreachable!("--raw / --curl reject these subcommands before this point")
        }
    };
    Ok(url)
}

/// Build the `download_url/{module}` URL with its optional query parameters,
/// matching what `download-url` and `download` send.
fn download_url_endpoint(
    client: &Client,
    module: &str,
    version: Option<&str>,
    dev: bool,
) -> Result<Url> {
    let mut url = client.url(&format!("download_url/{module}"))?;
    if version.is_some() || dev {
        let mut pairs = url.query_pairs_mut();
        if let Some(version) = version {
            pairs.append_pair("version", version);
        }
        if dev {
            pairs.append_pair("dev", "1");
        }
    }
    Ok(url)
}

/// GET `url`, printing the raw request and response to stdout: the request line
/// and headers, a blank line, the response status line and headers, a blank
/// line, then the body verbatim. The full exchange is always printed, including
/// for an error response; the returned status code lets the caller still exit
/// non-zero on a `4xx`/`5xx`.
async fn raw_get(client: &Client, url: Url) -> Result<(u16, Vec<u8>)> {
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    let target = match url.query() {
        Some(query) => format!("{}?{}", url.path(), query),
        None => url.path().to_string(),
    };

    // Set the headers reqwest would otherwise add at send time ourselves, so
    // what we print is what actually goes on the wire.
    let request = client
        .http()
        .get(url.clone())
        .header("host", &host)
        .header("user-agent", USER_AGENT)
        .header("accept", "*/*")
        .build()
        .context("building request")?;

    let mut dump = format!("GET {target} HTTP/1.1\n");
    append_headers(&mut dump, request.headers());
    dump.push('\n');

    let response = client
        .http()
        .execute(request)
        .await
        .with_context(|| format!("GET {url}"))?;

    let status = response.status();
    dump.push_str(&format!("{:?} {status}\n", response.version()));
    append_headers(&mut dump, response.headers());
    dump.push('\n');

    let body = response.bytes().await.context("reading response body")?;

    use std::io::Write;
    let mut out = std::io::stdout().lock();
    out.write_all(dump.as_bytes())?;
    out.write_all(&body)?;
    if !body.ends_with(b"\n") {
        out.write_all(b"\n")?;
    }
    out.flush()?;

    Ok((status.as_u16(), body.to_vec()))
}

/// After a raw exchange has been printed, turn a `4xx`/`5xx` status into a
/// non-zero exit.
fn bail_on_http_error(status: u16) -> Result<()> {
    if status >= 400 {
        anyhow::bail!("HTTP {status}");
    }
    Ok(())
}

/// Append `name: value` lines for every header to `dump`.
fn append_headers(dump: &mut String, headers: &metacpan_api_modern::reqwest::header::HeaderMap) {
    for (name, value) in headers {
        dump.push_str(name.as_str());
        dump.push_str(": ");
        dump.push_str(value.to_str().unwrap_or("<non-utf8>"));
        dump.push('\n');
    }
}

/// GET a path and parse the body as JSON.
async fn get(client: &Client, path: &str) -> Result<Value> {
    client
        .get_json::<Value>(path)
        .await
        .with_context(|| format!("GET {path}"))
}

/// GET a path with query parameters and parse the body as JSON. Used for the
/// endpoints the typed crate methods build query strings for (`download_url`,
/// `_search`).
async fn get_query(client: &Client, path: &str, query: &[(&str, String)]) -> Result<Value> {
    let url = client.url(path)?;
    let response = client
        .http()
        .get(url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("GET {path}"))?;
    let status = response.status();
    let body = response.text().await.context("reading response body")?;
    if !status.is_success() {
        anyhow::bail!("MetaCPAN API error {}: {}", status.as_u16(), body.trim());
    }
    serde_json::from_str(&body).with_context(|| format!("parsing {path} response as JSON"))
}

/// The direct reverse dependencies of `distribution` — each with the author of
/// its most recent production release and its CPAN River figures. Unordered;
/// the caller runs them through [`finish_river`].
async fn river_distribution(client: &Client, distribution: &str) -> Result<Vec<render::RiverRow>> {
    let mut rows = reverse_dependency_rows(client, distribution).await?;
    fill_river(client, &mut rows).await?;
    Ok(rows)
}

/// The distributions whose current (latest, non-dev) release is by `pauseid`,
/// with their CPAN River figures. Unordered; the caller runs them through
/// [`finish_river`].
async fn river_author(client: &Client, pauseid: &str) -> Result<Vec<render::RiverRow>> {
    let mut rows = author_release_rows(client, pauseid).await?;
    fill_river(client, &mut rows).await?;
    Ok(rows)
}

/// Sort river rows by the `by` figure descending (a missing figure counts as
/// 0, name breaks ties), keep the top `limit`, then — for `reverse` — flip the
/// kept rows for display. So `--reverse --limit N` still shows the top N, just
/// smallest-first. Returns the row count before `limit`.
fn finish_river(
    rows: &mut Vec<render::RiverRow>,
    by: RiverSort,
    reverse: bool,
    limit: Option<usize>,
) -> usize {
    let key = |r: &render::RiverRow| match by {
        RiverSort::Total => r.total.unwrap_or(0),
        RiverSort::Immediate => r.immediate.unwrap_or(0),
    };
    rows.sort_by(|a, b| {
        key(b)
            .cmp(&key(a))
            .then_with(|| a.distribution.cmp(&b.distribution))
    });
    let total = rows.len();
    if let Some(n) = limit {
        rows.truncate(n);
    }
    if reverse {
        rows.reverse();
    }
    total
}

/// The trailing count line: `N <noun>`, or `showing X of N <noun>` when
/// `--limit` trimmed the list.
fn count_line(shown: usize, total: usize, noun: &str) {
    if shown < total {
        println!("showing {shown} of {total} {noun}");
    } else {
        println!("{total} {noun}");
    }
}

/// `[{ "distribution", [<label_key>,] "river": { total, immediate, bucket } }]`
/// for `--json`. `label_key` names the optional label field — `"author"` for
/// `river distribution`, `"pauseid"` for `adoptable` — and is omitted for
/// `river author`, where every row is the same queried author.
fn river_json(rows: &[render::RiverRow], label_key: Option<&str>) -> Value {
    let arr = rows
        .iter()
        .map(|r| {
            let mut obj = serde_json::Map::new();
            obj.insert("distribution".into(), json!(r.distribution));
            if let Some(key) = label_key {
                obj.insert(key.to_owned(), json!(r.label));
            }
            obj.insert(
                "river".into(),
                json!({ "total": r.total, "immediate": r.immediate, "bucket": r.bucket }),
            );
            Value::Object(obj)
        })
        .collect();
    Value::Array(arr)
}

/// `{ "module_name", "owner", "co_maintainers": [...] }` for `--json`. The
/// crate's [`Permission`] is deserialize-only, so its fields are rebuilt by
/// hand.
fn permission_json(p: &Permission) -> Value {
    json!({
        "module_name": p.module_name,
        "owner": p.owner,
        "co_maintainers": p.co_maintainers,
    })
}

/// Page the `release` index for `pauseid`'s current (latest, non-dev) releases
/// and return one river row per distribution (river figures filled in later).
async fn author_release_rows(client: &Client, pauseid: &str) -> Result<Vec<render::RiverRow>> {
    const PAGE: usize = 500;
    let author = pauseid.to_uppercase();
    let mut rows: Vec<render::RiverRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut from = 0usize;
    loop {
        let body = json!({
            "query": { "bool": { "must": [
                { "term": { "author": author } },
                { "term": { "status": "latest" } },
            ]}},
            "size": PAGE,
            "from": from,
            "_source": ["distribution"],
            "sort": ["distribution"],
        });
        let v: Value = client
            .post_json("release/_search", &body)
            .await
            .context("searching author releases")?;
        let empty = Vec::new();
        let hits = v
            .pointer("/hits/hits")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        if hits.is_empty() {
            break;
        }
        for hit in hits {
            let Some(dist) = hit.pointer("/_source/distribution").and_then(Value::as_str) else {
                continue;
            };
            if seen.insert(dist.to_owned()) {
                rows.push(render::RiverRow {
                    distribution: dist.to_owned(),
                    label: None,
                    total: None,
                    immediate: None,
                    bucket: None,
                });
            }
        }
        from += hits.len();
        let total = v
            .pointer("/hits/total/value")
            .or_else(|| v.pointer("/hits/total"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if from as u64 >= total {
            break;
        }
        if from > 100_000 {
            anyhow::bail!("release search pagination for {author} did not terminate");
        }
    }
    Ok(rows)
}

/// Page through `reverse_dependencies/dist/{distribution}` and return one row
/// per distribution whose latest release depends directly on it, carrying the
/// distribution name and that release's author (river figures are filled in
/// later). Makes one request per page.
///
/// The endpoint only serves its first ~900 results however large `page_size`
/// is (and mis-reports `total` on the page past the end), so this pages at 100
/// until a short or empty page, and warns on stderr when it could not get
/// everything the first page's `total` promised.
async fn reverse_dependency_rows(
    client: &Client,
    distribution: &str,
) -> Result<Vec<render::RiverRow>> {
    const PAGE_SIZE: usize = 100;
    const MAX_PAGES: usize = 50;
    let mut rows: Vec<render::RiverRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut reported_total = 0u64;
    for page in 1..=MAX_PAGES {
        let path =
            format!("reverse_dependencies/dist/{distribution}?page={page}&page_size={PAGE_SIZE}");
        let v = get(client, &path).await?;
        if page == 1 {
            reported_total = v.get("total").and_then(Value::as_u64).unwrap_or(0);
        }
        let empty = Vec::new();
        let data = v.get("data").and_then(Value::as_array).unwrap_or(&empty);
        for item in data {
            let Some(name) = item.get("distribution").and_then(Value::as_str) else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                rows.push(render::RiverRow {
                    distribution: name.to_owned(),
                    label: item
                        .get("author")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    total: None,
                    immediate: None,
                    bucket: None,
                });
            }
        }
        if data.len() < PAGE_SIZE {
            break;
        }
    }
    if reported_total > rows.len() as u64 {
        eprintln!(
            "note: got {} of {reported_total} reverse dependencies for {distribution}; \
             MetaCPAN's reverse_dependencies endpoint caps results at ~900",
            rows.len()
        );
    }
    Ok(rows)
}

/// Fill in the CPAN River figures on `rows` from the `distribution` index, in
/// batches so the query stays well inside Elasticsearch's term and result
/// limits. Rows whose distribution has no document keep their `None` figures.
async fn fill_river(client: &Client, rows: &mut [render::RiverRow]) -> Result<()> {
    use std::collections::HashMap;
    const BATCH: usize = 1000;

    let index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.distribution.clone(), i))
        .collect();
    let names: Vec<String> = rows.iter().map(|r| r.distribution.clone()).collect();

    for chunk in names.chunks(BATCH) {
        let body = json!({
            "query": { "terms": { "name": chunk } },
            "size": chunk.len(),
            "_source": ["name", "river"],
        });
        let v = post_json_retry(client, "distribution/_search", &body)
            .await
            .context("searching distribution river data")?;
        let empty = Vec::new();
        let hits = v
            .pointer("/hits/hits")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for hit in hits {
            let src = hit.get("_source").unwrap_or(&Value::Null);
            let Some(i) = src
                .get("name")
                .and_then(Value::as_str)
                .and_then(|n| index.get(n).copied())
            else {
                continue;
            };
            rows[i].total = src.pointer("/river/total").and_then(Value::as_u64);
            rows[i].immediate = src.pointer("/river/immediate").and_then(Value::as_u64);
            rows[i].bucket = src.pointer("/river/bucket").and_then(Value::as_u64);
        }
    }
    Ok(())
}

/// `POST {path}` with a JSON body, parsed as `Value`, retrying briefly on a
/// `5xx` — the `_search` endpoints return one intermittently under load.
async fn post_json_retry(client: &Client, path: &str, body: &Value) -> Result<Value> {
    for attempt in 1..=3u32 {
        match client.post_json::<Value, Value>(path, body).await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < 3 && e.status().is_some_and(|s| s >= 500) => {
                let backoff = std::time::Duration::from_millis(u64::from(attempt) * 400);
                tokio::time::sleep(backoff).await;
            }
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!("the final attempt returns Ok or Err")
}

/// Rows for `adoptable`: every distribution with a current release providing a
/// module namespace that ADOPTME or HANDOFF owns or co-maintains, with River
/// figures filled in and the label set to which of the two (or both) applies.
/// Unordered.
async fn adoptable_rows(client: &Client) -> Result<Vec<render::RiverRow>> {
    use std::collections::BTreeMap;

    // 1. namespace -> (has ADOPTME perm, has HANDOFF perm)
    let mut namespaces: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for (who, adoptme) in [("ADOPTME", true), ("HANDOFF", false)] {
        let perms = client
            .permissions_by_author(who)
            .await
            .with_context(|| format!("fetching {who} permissions"))?;
        for module in perms.into_iter().filter_map(|p| p.module_name) {
            let flags = namespaces.entry(module).or_default();
            if adoptme {
                flags.0 = true;
            } else {
                flags.1 = true;
            }
        }
    }
    let module_names: Vec<&str> = namespaces.keys().map(String::as_str).collect();

    // 2. Resolve namespaces to the distribution that currently provides them
    //    (authorized file in the latest release), carrying the flags across.
    let mut dists: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for chunk in module_names.chunks(250) {
        let body = json!({
            "query": { "bool": { "filter": [
                { "terms": { "module.name": chunk } },
                { "term": { "authorized": true } },
                { "term": { "status": "latest" } },
            ]}},
            "size": chunk.len() * 3,
            "_source": ["distribution", "module.name"],
        });
        let v = post_json_retry(client, "file/_search", &body)
            .await
            .context("resolving namespaces to distributions")?;
        let empty = Vec::new();
        let hits = v
            .pointer("/hits/hits")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        for hit in hits {
            let Some(dist) = hit.pointer("/_source/distribution").and_then(Value::as_str) else {
                continue;
            };
            let Some(mods) = hit.pointer("/_source/module").and_then(Value::as_array) else {
                continue;
            };
            for m in mods {
                if let Some(name) = m.get("name").and_then(Value::as_str)
                    && let Some(&(adoptme, handoff)) = namespaces.get(name)
                {
                    let flags = dists.entry(dist.to_owned()).or_insert((false, false));
                    flags.0 |= adoptme;
                    flags.1 |= handoff;
                }
            }
        }
    }

    // 3. River figures.
    let mut rows: Vec<render::RiverRow> = dists
        .into_iter()
        .map(|(distribution, (adoptme, handoff))| {
            let label = match (adoptme, handoff) {
                (true, true) => "ADOPTME,HANDOFF",
                (true, false) => "ADOPTME",
                (false, true) => "HANDOFF",
                (false, false) => "-",
            };
            render::RiverRow {
                distribution,
                label: Some(label.to_owned()),
                total: None,
                immediate: None,
                bucket: None,
            }
        })
        .collect();
    fill_river(client, &mut rows).await?;
    Ok(rows)
}

/// Some endpoints wrap their payload in a single-key envelope (`release`,
/// `mirrors`). Return the inner value, or the original if the key is absent.
fn unwrap_key(value: Value, key: &str) -> Value {
    match value {
        Value::Object(mut map) if map.contains_key(key) => map.remove(key).unwrap(),
        other => other,
    }
}

/// Print `value` as JSON, or hand it to `as_table` for the default form.
fn emit(
    value: Value,
    as_json: bool,
    color: bool,
    as_table: impl FnOnce(Value) -> Result<()>,
) -> Result<()> {
    if as_json {
        print!("{}", json::to_string(&value, color));
        Ok(())
    } else {
        as_table(value)
    }
}

/// Format a byte count with a binary unit (`KiB`, `MiB`, ...), two decimals
/// past the first kibibyte.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// Content endpoints (`source`, `pod`) return text, not a document. Print it
/// verbatim, or wrap it in `{ "content": ... }` for `--json`.
fn emit_text(text: &str, as_json: bool, color: bool) {
    if as_json {
        print!("{}", json::to_string(&json!({ "content": text }), color));
    } else {
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
    }
}
