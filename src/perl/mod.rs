//! `upt perl` — run `perl` through a configured
//! [`perl-wrapper`](https://github.com/uperl/rust-perl-wrapper).
//!
//! Each `[perl.<name>]` section of the config file describes one `perl-wrapper`
//! object (which `perl` and `make` to use, the install prefix, the extra
//! `PERL5LIB` directories).
//!
//! * `upt perl exec [--perl <name>] [-- <perl options>...]` builds that wrapper
//!   and executes `perl` with it. `--perl <name>` selects the `[perl.<name>]`
//!   section; without it, `perl.default` from the config is used. Everything
//!   after `--` is passed straight to `perl`; the command exits with `perl`'s
//!   own status.
//! * `upt perl register <perl binary> --perl <name> [...]` adds a new
//!   `[perl.<name>]` section to the config file.
//! * `upt perl select --perl <name>` points `perl.default` (the section
//!   `upt perl exec` uses without `--perl`) at an already-registered
//!   `[perl.<name>]`.
//! * `upt perl list [--json]` prints the configured `[perl.<name>]` names.
//! * `upt perl default [--json]` prints the name of `perl.default`.
//! * `upt perl info [--perl <name>] [--json]` shows every setting of one
//!   `[perl.<name>]` section, as a table or JSON.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use comfy_table::{Attribute, Cell, ContentArrangement, Table, presets::UTF8_FULL};
use perl_wrapper::Perl;
use serde_json::{Value, json as jsonv};

use crate::config::PerlConfig;
use crate::json;

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
        Command::Register {
            perl_bin,
            perl,
            make,
            install_base,
            lib,
        } => register(cx, &perl_bin, &perl, make, install_base, lib),
        Command::Select { perl } => select(cx, &perl),
        Command::List { json } => list(cx, json),
        Command::Default { json } => default(cx, json),
        Command::Info { perl, json } => info(cx, perl.as_deref(), json),
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

    /// Add a new `[perl.<name>]` section to the config file.
    ///
    /// `--make` defaults to `$Config{make}` of the given interpreter. The name
    /// given by `--perl` must not already be present in the config.
    Register {
        /// Full path to the `perl` binary to register.
        #[arg(value_name = "PERL_BINARY")]
        perl_bin: PathBuf,

        /// Name for the new `[perl.<name>]` section (must not already be in
        /// use).
        #[arg(long, value_name = "NAME", required = true)]
        perl: String,

        /// Path to `make` (default: `$Config{make}` of the interpreter).
        #[arg(long, value_name = "PATH")]
        make: Option<PathBuf>,

        /// `local::lib` / `INSTALL_BASE` prefix for newly built modules.
        #[arg(long, value_name = "DIR")]
        install_base: Option<PathBuf>,

        /// Directory to prepend to `PERL5LIB`; repeatable.
        #[arg(long = "lib", value_name = "DIR")]
        lib: Vec<PathBuf>,
    },

    /// Set `perl.default` in the config file to an existing `[perl.<name>]`.
    ///
    /// `perl.default` is the section `upt perl exec` runs when it is invoked
    /// without `--perl`. The name given by `--perl` must already be registered.
    Select {
        /// Name of the `[perl.<name>]` section to make the default.
        #[arg(long, value_name = "NAME", required = true)]
        perl: String,
    },

    /// List the names of the `[perl.<name>]` sections in the config file.
    ///
    /// Names are printed one per line, sorted. `perl.default` is not shown.
    List {
        /// Print the names as a JSON array of strings instead of one per line.
        #[arg(long, short = 'j')]
        json: bool,
    },

    /// Print the name of `perl.default` from the config file.
    ///
    /// This is the section `upt perl exec` runs when it is invoked without
    /// `--perl`. Exits non-zero when `perl.default` is not set.
    Default {
        /// Print the name as a single-element JSON array of strings.
        #[arg(long, short = 'j')]
        json: bool,
    },

    /// Show every setting of one `[perl.<name>]` section.
    ///
    /// With no `--perl`, the `perl.default` section is shown. The output is a
    /// table by default, or a JSON object with `--json`.
    Info {
        /// Name of the `[perl.<name>]` section to show (default:
        /// `perl.default`).
        #[arg(long, value_name = "NAME")]
        perl: Option<String>,

        /// Print the settings as a JSON object instead of a table.
        #[arg(long, short = 'j')]
        json: bool,
    },
}

/// Resolve a `--perl <name>` (or `perl.default` when `None`) to its config
/// section, returning the resolved name alongside it. Shared with `upt dist`,
/// which selects its build interpreter the same way.
pub(crate) fn resolve_perl<'a>(
    cx: &'a crate::Cx,
    name: Option<&'a str>,
) -> Result<(&'a str, &'a PerlConfig)> {
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

    Ok((name, config))
}

/// Resolve the perl name, build its wrapper, and exec `perl` with `perl_args`.
/// Returns `perl`'s exit status as this process's exit code.
fn exec(cx: &crate::Cx, name: Option<&str>, perl_args: &[String]) -> Result<i32> {
    let (name, config) = resolve_perl(cx, name)?;

    let perl = build_wrapper(config)
        .with_context(|| format!("building the perl-wrapper for `[perl.{name}]`"))?;

    let result = perl
        .execute_perl(perl_args)
        .with_context(|| format!("running perl for `[perl.{name}]`"))?;

    Ok(exit_code(result.is_success, result.code))
}

/// The settings for a new `[perl.<name>]` section, with `--make` already
/// resolved to a concrete path.
struct RegisterEntry {
    perl: PathBuf,
    make: Option<PathBuf>,
    install_base: Option<PathBuf>,
    lib: Vec<PathBuf>,
}

/// `upt perl register`: add a `[perl.<name>]` section to the config file.
fn register(
    cx: &crate::Cx,
    perl_bin: &Path,
    name: &str,
    make: Option<PathBuf>,
    install_base: Option<PathBuf>,
    lib: Vec<PathBuf>,
) -> Result<i32> {
    if name == "default" {
        bail!("`default` is a reserved key in the [perl] section and cannot name a perl");
    }
    if cx.perl.perls.contains_key(name) {
        bail!("`[perl.{name}]` is already in {}", cx.config_path.display());
    }
    if !perl_bin.is_file() {
        bail!("{}: not a file", perl_bin.display());
    }

    // `--make`, or `$Config{make}` of the interpreter being registered.
    let make = match make {
        Some(make) => make,
        None => config_make(perl_bin)?,
    };

    let entry = RegisterEntry {
        perl: perl_bin.to_path_buf(),
        make: Some(make),
        install_base,
        lib,
    };

    // Start from the current file (or the starter template when there is none)
    // so comments and unrelated sections are preserved.
    let base = read_config_or_template(&cx.config_path)?;
    let updated = insert_perl_section(&base, name, &entry)?;
    write_config(&cx.config_path, &updated)?;

    println!("registered `[perl.{name}]` in {}", cx.config_path.display());
    Ok(0)
}

/// `upt perl select`: point `perl.default` at an already-registered
/// `[perl.<name>]` section.
fn select(cx: &crate::Cx, name: &str) -> Result<i32> {
    if name == "default" {
        bail!("`default` is a reserved key in the [perl] section and cannot name a perl");
    }
    if !cx.perl.perls.contains_key(name) {
        bail!(
            "no `[perl.{name}]` section in {}{}; add one with `upt perl register` first",
            cx.config_path.display(),
            available(cx)
        );
    }

    let base = read_config_or_template(&cx.config_path)?;
    let updated = set_perl_default(&base, name)?;
    write_config(&cx.config_path, &updated)?;

    println!(
        "`perl.default` is now `{name}` in {}",
        cx.config_path.display()
    );
    Ok(0)
}

/// `upt perl list`: print the configured perl names, one per line (or as a JSON
/// array of strings with `--json`).
fn list(cx: &crate::Cx, as_json: bool) -> Result<i32> {
    let names: Vec<&str> = cx.perl.perls.keys().map(String::as_str).collect();
    print!("{}", render_list(&names, as_json, cx.style.enabled()));
    Ok(0)
}

/// `upt perl default`: print the name of `perl.default` (the section
/// `upt perl exec` uses without `--perl`).
fn default(cx: &crate::Cx, as_json: bool) -> Result<i32> {
    let name = cx.perl.default.as_deref().ok_or_else(|| {
        anyhow!(
            "no `perl.default` set in {}; set one with `upt perl select`",
            cx.config_path.display()
        )
    })?;
    print!("{}", render_list(&[name], as_json, cx.style.enabled()));
    Ok(0)
}

/// `upt perl info`: show every setting of one `[perl.<name>]` section, as a
/// table or (with `--json`) a JSON object.
fn info(cx: &crate::Cx, name: Option<&str>, as_json: bool) -> Result<i32> {
    let (name, config) = resolve_perl(cx, name)?;
    let is_default = cx.perl.default.as_deref() == Some(name);

    if as_json {
        print!(
            "{}",
            json::to_string(&info_json(name, is_default, config), cx.style.enabled())
        );
    } else {
        println!(
            "{}",
            info_table(name, is_default, config, cx.style.enabled())
        );
    }
    Ok(0)
}

/// A `[perl.<name>]` section as a JSON object. Unset optional paths are `null`;
/// `lib` is always an array.
fn info_json(name: &str, is_default: bool, config: &PerlConfig) -> Value {
    let path = |p: &Option<PathBuf>| match p {
        Some(p) => Value::String(p.to_string_lossy().into_owned()),
        None => Value::Null,
    };
    jsonv!({
        "name": name,
        "default": is_default,
        "perl": path(&config.perl),
        "make": path(&config.make),
        "install-base": path(&config.install_base),
        "lib": config
            .lib
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    })
}

/// A `[perl.<name>]` section as a two-column "Field / Value" table. Unset
/// optional settings show a parenthesised note describing the fallback.
fn info_table(name: &str, is_default: bool, config: &PerlConfig, color: bool) -> String {
    let path = |p: &Option<PathBuf>, fallback: &str| match p {
        Some(p) => p.to_string_lossy().into_owned(),
        None => fallback.to_string(),
    };
    let lib = if config.lib.is_empty() {
        "(none)".to_string()
    } else {
        config
            .lib
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let rows = [
        ("name", name.to_string()),
        ("default", if is_default { "yes" } else { "no" }.to_string()),
        ("perl", path(&config.perl, "(first perl on PATH)")),
        ("make", path(&config.make, "(first make on PATH)")),
        (
            "install-base",
            path(&config.install_base, "(interpreter default)"),
        ),
        ("lib", lib),
    ];

    // Same house style as `upt metacpan`: a full UTF-8 box that reads the
    // terminal width itself, with a fixed fallback when stdout is not a TTY.
    let mut t = Table::new();
    t.load_preset(UTF8_FULL);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    if !std::io::stdout().is_terminal() {
        t.set_width(100);
    }
    let head = |s: &str| {
        let cell = Cell::new(s);
        if color {
            cell.add_attribute(Attribute::Bold)
        } else {
            cell
        }
    };
    t.set_header(vec![head("Field"), head("Value")]);
    for (k, v) in rows {
        t.add_row(vec![Cell::new(k), Cell::new(v)]);
    }
    t.to_string()
}

/// Format the perl-name list: a JSON array of strings when `as_json`, otherwise
/// one name per line. The plain form is empty when there are no perls; the JSON
/// form is `[]`.
fn render_list(names: &[&str], as_json: bool, color: bool) -> String {
    if as_json {
        let array = names
            .iter()
            .map(|n| Value::String((*n).to_owned()))
            .collect();
        json::to_string(&Value::Array(array), color)
    } else {
        let mut out = String::new();
        for name in names {
            out.push_str(name);
            out.push('\n');
        }
        out
    }
}

/// Read the current config file text, falling back to the starter template when
/// the file does not exist yet so a rewrite still keeps its comments.
fn read_config_or_template(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(crate::config::DEFAULT_FILE.to_string())
        }
        Err(err) => Err(anyhow::Error::new(err).context(format!("reading {}", path.display()))),
    }
}

/// Write `text` to the config file, creating the parent directory if needed.
fn write_config(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Query `$Config{make}` from `perl_bin` (`perl -MConfig -e 'print
/// $Config{make}'`).
fn config_make(perl_bin: &Path) -> Result<PathBuf> {
    let output = std::process::Command::new(perl_bin)
        .args(["-MConfig", "-e", "print $Config{make}"])
        .output()
        .with_context(|| {
            format!(
                "running {} to read $Config{{make}}; pass --make explicitly to skip this",
                perl_bin.display()
            )
        })?;

    if !output.status.success() {
        bail!(
            "{} exited with {} while reading $Config{{make}}; pass --make explicitly",
            perl_bin.display(),
            output.status
        );
    }

    let make = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if make.is_empty() {
        bail!(
            "{} reported an empty $Config{{make}}; pass --make explicitly",
            perl_bin.display()
        );
    }
    Ok(PathBuf::from(make))
}

/// Insert a `[perl.<name>]` table into the TOML document `base` (the current
/// config file text, or the starter template when the file does not exist yet),
/// returning the new file text. Comments and unrelated content are preserved.
///
/// Errors if `name` is the reserved `default` key or a `[perl.<name>]` table is
/// already present.
fn insert_perl_section(base: &str, name: &str, entry: &RegisterEntry) -> Result<String> {
    use toml_edit::{Array, DocumentMut, Item, Table, value};

    if name == "default" {
        bail!("`default` is a reserved key in the [perl] section and cannot name a perl");
    }

    let mut doc: DocumentMut = base
        .parse()
        .context("the existing config file is not valid TOML")?;

    if doc.get("perl").is_none() {
        let mut table = Table::new();
        // No bare `[perl]` header when it only holds sub-tables.
        table.set_implicit(true);
        doc.insert("perl", Item::Table(table));
    }

    let perl = doc["perl"]
        .as_table_mut()
        .context("the `perl` config entry is not a table")?;

    if perl.contains_key(name) {
        bail!("`[perl.{name}]` is already in the config file");
    }

    let mut table = Table::new();
    table.insert("perl", value(path_str(&entry.perl)));
    if let Some(make) = &entry.make {
        table.insert("make", value(path_str(make)));
    }
    if let Some(install_base) = &entry.install_base {
        table.insert("install-base", value(path_str(install_base)));
    }
    if !entry.lib.is_empty() {
        let mut lib = Array::new();
        for dir in &entry.lib {
            lib.push(path_str(dir));
        }
        table.insert("lib", value(lib));
    }

    perl.insert(name, Item::Table(table));

    Ok(doc.to_string())
}

/// Set the reserved `default` key of the `[perl]` table in the TOML document
/// `base` to `name`, returning the new file text. An existing `perl.default` is
/// replaced in place; comments and unrelated content are preserved.
fn set_perl_default(base: &str, name: &str) -> Result<String> {
    use toml_edit::{DocumentMut, Item, Table, value};

    let mut doc: DocumentMut = base
        .parse()
        .context("the existing config file is not valid TOML")?;

    if doc.get("perl").is_none() {
        doc.insert("perl", Item::Table(Table::new()));
    }

    let perl = doc["perl"]
        .as_table_mut()
        .context("the `perl` config entry is not a table")?;

    // `default` is a leaf key on `[perl]`, so the header has to be emitted even
    // when the table otherwise holds only `[perl.<name>]` sub-tables. toml_edit
    // renders a table's own key/value pairs above its child tables, so the new
    // key lands above any `[perl.<name>]` headers rather than inside one.
    perl.set_implicit(false);
    perl.insert("default", value(name));

    Ok(doc.to_string())
}

/// A path as a UTF-8 string for storing in the TOML config (lossy, matching how
/// the rest of `upt` treats config paths).
fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build a [`Perl`] wrapper from a `[perl.<name>]` config section. A missing
/// `perl` key falls back to the first `perl` on `PATH`. Shared with `upt dist`.
pub(crate) fn build_wrapper(config: &PerlConfig) -> Result<Perl> {
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
            dist_prefer: crate::config::DistPrefer::Auto,
            cpan_source: crate::config::CpanSource::Metacpan,
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

    fn entry(perl: &str) -> RegisterEntry {
        RegisterEntry {
            perl: PathBuf::from(perl),
            make: None,
            install_base: None,
            lib: Vec::new(),
        }
    }

    /// Parse `text` with the real config deserializer, so the test also checks
    /// that `insert_perl_section` writes the key names the loader expects.
    fn parse(text: &str) -> crate::config::Config {
        toml::from_str(text).expect("insert_perl_section produced invalid config TOML")
    }

    #[test]
    fn insert_adds_a_perl_table_and_keeps_existing_content() {
        let base = "[global]\ncolor = \"never\"\n";
        let mut e = entry("/opt/perl/bin/perl");
        e.make = Some(PathBuf::from("/usr/bin/make"));
        let out = insert_perl_section(base, "dev", &e).unwrap();

        assert!(out.contains("[global]"), "unrelated section preserved");
        let cfg = parse(&out);
        let dev = &cfg.perl.perls["dev"];
        assert_eq!(dev.perl.as_deref(), Some(Path::new("/opt/perl/bin/perl")));
        assert_eq!(dev.make.as_deref(), Some(Path::new("/usr/bin/make")));
        assert!(dev.install_base.is_none());
        assert!(dev.lib.is_empty());
    }

    #[test]
    fn insert_writes_install_base_and_a_repeatable_lib_array() {
        let mut e = entry("/p");
        e.install_base = Some(PathBuf::from("/base"));
        e.lib = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let out = insert_perl_section("", "x", &e).unwrap();

        let cfg = parse(&out);
        let x = &cfg.perl.perls["x"];
        assert_eq!(x.install_base.as_deref(), Some(Path::new("/base")));
        assert_eq!(x.lib, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(x.make.is_none(), "no make key when --make was not resolved");
    }

    #[test]
    fn insert_preserves_perl_default_and_sibling_entries() {
        let base = "[perl]\ndefault = \"a\"\n\n[perl.a]\nperl = \"/a\"\n";
        let out = insert_perl_section(base, "b", &entry("/b")).unwrap();

        let cfg = parse(&out);
        assert_eq!(cfg.perl.default.as_deref(), Some("a"));
        assert!(cfg.perl.perls.contains_key("a"));
        assert!(cfg.perl.perls.contains_key("b"));
    }

    #[test]
    fn insert_rejects_a_name_already_in_the_file() {
        let base = "[perl.dev]\nperl = \"/x\"\n";
        let err = insert_perl_section(base, "dev", &entry("/y"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already"), "{err}");
    }

    #[test]
    fn insert_rejects_the_reserved_default_name() {
        let err = insert_perl_section("", "default", &entry("/y"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn select_rejects_the_reserved_default_name() {
        let cx = cx_with(crate::config::PerlSection::default());
        let err = select(&cx, "default").unwrap_err().to_string();
        assert!(err.contains("reserved"), "{err}");
    }

    #[test]
    fn select_rejects_an_unregistered_name() {
        let mut section = crate::config::PerlSection::default();
        section
            .perls
            .insert("alpha".to_string(), PerlConfig::default());
        let cx = cx_with(section);

        let err = select(&cx, "beta").unwrap_err().to_string();
        assert!(err.contains("[perl.beta]"), "{err}");
        assert!(err.contains("available: alpha"), "{err}");
        assert!(err.contains("register"), "{err}");
    }

    #[test]
    fn set_default_replaces_an_existing_value_in_place() {
        let base =
            "[perl]\ndefault = \"a\"\n\n[perl.a]\nperl = \"/a\"\n\n[perl.b]\nperl = \"/b\"\n";
        let out = set_perl_default(base, "b").unwrap();

        let cfg = parse(&out);
        assert_eq!(cfg.perl.default.as_deref(), Some("b"));
        assert!(cfg.perl.perls.contains_key("a"));
        assert!(cfg.perl.perls.contains_key("b"));
    }

    #[test]
    fn set_default_adds_the_key_above_existing_perl_sub_tables() {
        // No explicit `[perl]` header, only sub-tables: the new `default` key
        // must not be swallowed by `[perl.dev]`.
        let base = "[perl.dev]\nperl = \"/dev\"\n";
        let out = set_perl_default(base, "dev").unwrap();

        let cfg = parse(&out);
        assert_eq!(cfg.perl.default.as_deref(), Some("dev"));
        assert_eq!(
            cfg.perl.perls["dev"].perl.as_deref(),
            Some(Path::new("/dev"))
        );
    }

    #[test]
    fn render_list_plain_is_one_sorted_name_per_line() {
        assert_eq!(render_list(&["dev", "sys"], false, false), "dev\nsys\n");
        assert_eq!(render_list(&[], false, false), "");
    }

    #[test]
    fn render_list_json_is_an_array_of_strings() {
        let out = render_list(&["dev", "sys"], true, false);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v, serde_json::json!(["dev", "sys"]));
        assert_eq!(render_list(&[], true, false).trim(), "[]");
        // `upt perl default --json` reuses this: one name -> one-element array.
        let one: Value = serde_json::from_str(&render_list(&["dev"], true, false)).unwrap();
        assert_eq!(one, serde_json::json!(["dev"]));
    }

    #[test]
    fn default_errors_when_perl_default_is_unset() {
        let cx = cx_with(crate::config::PerlSection::default());
        let err = default(&cx, false).unwrap_err().to_string();
        assert!(err.contains("perl.default"), "{err}");
        assert!(err.contains("/tmp/upt/config.toml"), "{err}");
    }

    #[test]
    fn default_prints_the_configured_name() {
        let section = crate::config::PerlSection {
            default: Some("dev".to_string()),
            ..Default::default()
        };
        let cx = cx_with(section);
        assert_eq!(default(&cx, false).unwrap(), 0);
    }

    fn full_config() -> PerlConfig {
        PerlConfig {
            perl: Some(PathBuf::from("/opt/perl/bin/perl")),
            make: Some(PathBuf::from("/usr/bin/gmake")),
            install_base: Some(PathBuf::from("/home/me/perl5")),
            lib: vec![PathBuf::from("/a"), PathBuf::from("/b")],
        }
    }

    #[test]
    fn info_json_reports_every_setting_and_the_default_flag() {
        let v = info_json("dev", true, &full_config());
        assert_eq!(
            v,
            serde_json::json!({
                "name": "dev",
                "default": true,
                "perl": "/opt/perl/bin/perl",
                "make": "/usr/bin/gmake",
                "install-base": "/home/me/perl5",
                "lib": ["/a", "/b"],
            })
        );
    }

    #[test]
    fn info_json_uses_null_for_unset_paths_and_an_empty_lib_array() {
        let v = info_json("bare", false, &PerlConfig::default());
        assert_eq!(v["default"], serde_json::json!(false));
        assert_eq!(v["perl"], Value::Null);
        assert_eq!(v["make"], Value::Null);
        assert_eq!(v["install-base"], Value::Null);
        assert_eq!(v["lib"], serde_json::json!([]));
    }

    #[test]
    fn info_table_shows_all_fields_with_fallback_notes() {
        let bare = info_table("bare", false, &PerlConfig::default(), false);
        assert!(bare.contains("name"));
        assert!(bare.contains("install-base"));
        assert!(bare.contains("(first perl on PATH)"), "{bare}");
        assert!(bare.contains("(none)"), "{bare}");
        assert!(bare.contains(" no "), "default flag: {bare}");

        let full = info_table("dev", true, &full_config(), false);
        assert!(full.contains("/opt/perl/bin/perl"), "{full}");
        assert!(full.contains("/home/me/perl5"), "{full}");
        assert!(full.contains(" yes "), "default flag: {full}");
    }

    #[test]
    fn info_errors_on_an_unknown_perl_name() {
        let mut section = crate::config::PerlSection::default();
        section
            .perls
            .insert("alpha".to_string(), PerlConfig::default());
        let cx = cx_with(section);

        let err = info(&cx, Some("beta"), false).unwrap_err().to_string();
        assert!(err.contains("[perl.beta]"), "{err}");
        assert!(err.contains("available: alpha"), "{err}");
    }

    #[test]
    fn info_without_perl_flag_needs_a_default() {
        let cx = cx_with(crate::config::PerlSection::default());
        let err = info(&cx, None, true).unwrap_err().to_string();
        assert!(err.contains("--perl"), "{err}");
        assert!(err.contains("perl.default"), "{err}");
    }

    #[test]
    fn set_default_into_the_starter_template_round_trips() {
        let out = set_perl_default(crate::config::DEFAULT_FILE, "dev").unwrap();
        let cfg = parse(&out);
        assert_eq!(cfg.perl.default.as_deref(), Some("dev"));
        // The starter template's other sections still load.
        assert_eq!(cfg.global.color, crate::config::ColorChoice::Auto);
        assert_eq!(cfg.perlbuild.patch_perl, crate::config::PatchPerlMode::Auto);
    }

    #[test]
    fn insert_into_the_starter_template_round_trips() {
        let out =
            insert_perl_section(crate::config::DEFAULT_FILE, "dev", &entry("/opt/perl")).unwrap();
        let cfg = parse(&out);
        assert_eq!(
            cfg.perl.perls["dev"].perl.as_deref(),
            Some(Path::new("/opt/perl"))
        );
        // The starter template's other sections still load.
        assert_eq!(cfg.global.color, crate::config::ColorChoice::Auto);
        assert_eq!(cfg.perlbuild.patch_perl, crate::config::PatchPerlMode::Auto);
    }
}
