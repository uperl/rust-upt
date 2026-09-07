//! `upt dist` — drive the build lifecycle of an unpacked CPAN distribution one
//! step at a time.
//!
//! Each subcommand maps to one phase of
//! [`cpan_distribution_build::Distribution`]:
//!
//! | subcommand      | EUMM                 | Module::Build          |
//! |-----------------|----------------------|------------------------|
//! | `pre-configure` | list configure deps  | list configure deps    |
//! | `configure`     | `perl Makefile.PL`   | `perl Build.PL`        |
//! | `build`         | `make`               | `perl Build`           |
//! | `test`          | `make test`          | `perl Build test`      |
//! | `install`       | `make install`       | `perl Build install`   |
//! | `clean`         | `make clean`         | `perl Build clean`     |
//! | `distclean`     | `make distclean`     | `perl Build distclean` |
//!
//! Every step but `pre-configure` lets the child's output through to this
//! process's stdout/stderr and exits with the child's status. `perl` and `make`
//! output is therefore live; a failing step is reported as a non-zero exit,
//! never as a panic.
//!
//! `pre-configure` and `configure` also print the prerequisites they compute:
//! by default as a `comfy-table` in the same house style as the rest of `upt`,
//! with an `installed` column giving each module's version on `dist.perl`'s
//! search path (`-` when it is not installed, `?` when it declares no version).
//! A module whose installed version does not satisfy the requirement gets a `*`
//! after its name — with coloured `module` / `installed` cells when colour is
//! on: white on red for a hard `requires`, black on yellow for an optional
//! `recommends` / `suggests` — and a legend line under the table. Both
//! `pre-configure` and `configure` list only the unmet prerequisites unless
//! `--all-prereqs` is given, and print `all prerequisites are satisfied` when
//! there are none. This flag is table-only.
//!
//! `--json` replaces all of that with a single JSON object on stdout: the
//! child's captured, merged stdout+stderr under `output` (the empty string for
//! `pre-configure`, which runs nothing), the numeric `exit` code and a boolean
//! `success`, plus a `prereqs` object for `pre-configure` and `configure`. The
//! child's output is captured rather than streamed in this mode, so stdout stays
//! valid JSON. `pre-configure` always reports `exit` 0 and `success` true.
//! Colour follows upt's `global.color` / `--color`.

mod status;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use cpan_distribution_build::{
    BuildTool, Dependencies, Dependency, Distribution, ExecuteResult, Perl, PhaseDependencies,
};
use serde_json::{Value, json};

use crate::json;
use status::{Phase, Status};

/// Entry point for the `dist` built-in: parse `args` with clap, resolve the
/// colour decision from `cx`, then run the requested build step.
pub fn run(cx: &crate::Cx, args: &[String]) -> Result<i32> {
    let argv = std::iter::once(OsString::from("upt dist")).chain(args.iter().map(OsString::from));
    let cli = match Cli::try_parse_from(argv) {
        Ok(cli) => cli,
        // clap prints `--help` / `--version` and usage errors itself; mirror
        // its own exit codes (0 for help/version, 2 for a usage error).
        Err(err) => {
            err.print().ok();
            return Ok(err.exit_code());
        }
    };
    let color = crate::style::resolve(cx.color, std::io::stdout().is_terminal());
    dispatch(cx, cli, color)
}

/// What a successful step does to the `dist_status` row.
enum Outcome {
    /// Set this phase's flag.
    Mark(Phase),
    /// `clean`: clear `build` / `test` / `install`.
    ClearedBuild,
    /// `distclean`: clear every flag.
    ClearedAll,
}

/// Apply a `dist_status` update, downgrading a failure to a warning — the build
/// step itself already ran, so it must not be turned into a command failure.
fn record(result: Result<()>) {
    if let Err(err) = result {
        eprintln!("upt dist: could not update dist_status: {err:#}");
    }
}

/// Step-by-step build and install of an unpacked CPAN distribution.
#[derive(Debug, Parser)]
#[command(
    name = "upt dist",
    version,
    about = "Step-by-step build and install of an unpacked CPAN distribution",
    long_about = None,
)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    #[command(subcommand)]
    command: Command,
}

/// Options shared by every subcommand. They may be given before or after the
/// subcommand name.
#[derive(Debug, Args)]
struct CommonArgs {
    /// Directory holding the unpacked distribution (its `Makefile.PL` /
    /// `Build.PL` and `META.json`).
    #[arg(
        short = 'C',
        long = "directory",
        global = true,
        value_name = "DIR",
        default_value = "."
    )]
    directory: PathBuf,

    /// Perl interpreter to build with (default: the first `perl` on `PATH`).
    #[arg(long, global = true, value_name = "PATH")]
    perl: Option<PathBuf>,

    /// `make` to use for `ExtUtils::MakeMaker` distributions (default: the first
    /// `make` on `PATH`).
    #[arg(long, global = true, value_name = "PATH")]
    make: Option<PathBuf>,

    /// Install newly built modules under this prefix, the way `local::lib` /
    /// `INSTALL_BASE` would (default: the interpreter's own site directories).
    #[arg(long, global = true, value_name = "DIR")]
    install_base: Option<PathBuf>,

    /// Directory to add to `PERL5LIB` when running build steps; repeatable.
    #[arg(long = "lib", global = true, value_name = "DIR")]
    lib: Vec<PathBuf>,

    /// Which build tool to use when the distribution ships *both* `Build.PL` and
    /// `Makefile.PL` (ignored when only one is present).
    #[arg(long, global = true, value_name = "TOOL", default_value_t = Prefer::Mb)]
    prefer: Prefer,

    /// Emit a single JSON object on stdout instead of tables and live output:
    /// the captured command `output`, plus `prereqs` for `pre-configure` and
    /// `configure`.
    #[arg(long, short = 'j', global = true)]
    json: bool,
}

/// Build-tool preference for a dual-config distribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Prefer {
    /// `ExtUtils::MakeMaker` (`Makefile.PL`).
    Eumm,
    /// `Module::Build` (`Build.PL`).
    Mb,
}

impl std::fmt::Display for Prefer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Prefer::Eumm => "eumm",
            Prefer::Mb => "mb",
        };
        f.write_str(s)
    }
}

impl From<Prefer> for BuildTool {
    fn from(prefer: Prefer) -> Self {
        match prefer {
            Prefer::Eumm => BuildTool::Eumm,
            Prefer::Mb => BuildTool::ModuleBuild,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the prerequisites that must be installed before `configure` can run
    /// (the distribution's `configure` requires, plus the build tool itself), as
    /// a `module` / `required` / `installed` table. By default only the unmet
    /// ones are listed; `--all-prereqs` lists all of them. Nothing is executed.
    PreConfigure {
        /// List every prerequisite, not just the unmet ones.
        #[arg(long)]
        all_prereqs: bool,
    },

    /// Run the configure step: `perl Makefile.PL` or `perl Build.PL`.
    ///
    /// Afterwards the resolved prerequisites (taken from `MYMETA` when the
    /// configure step wrote one, otherwise from `META`) are printed as a
    /// `phase` / `relationship` / `module` / `required` / `installed` table.
    /// By default only the unmet prerequisites are listed; `--all-prereqs`
    /// lists every non-`develop` one, and `--no-prereqs` prints none.
    Configure {
        /// Don't print the resolved prerequisites after configuring.
        #[arg(long)]
        no_prereqs: bool,

        /// List every non-`develop` prerequisite, not just the unmet ones.
        #[arg(long, conflicts_with = "no_prereqs")]
        all_prereqs: bool,

        /// Include `develop`-phase prerequisites in the table (skipped by
        /// default; `--json` always includes them).
        #[arg(long)]
        include_develop: bool,
    },

    /// Run the build step: `make` or `perl Build`.
    Build,

    /// Run the test suite: `make test` or `perl Build test`.
    Test,

    /// Install the built distribution: `make install` or `perl Build install`.
    Install,

    /// Remove build products: `make clean` or `perl Build clean`.
    Clean,

    /// Remove build products and the generated `Makefile` / `Build` script:
    /// `make distclean` or `perl Build distclean`.
    Distclean,
}

fn dispatch(cx: &crate::Cx, cli: Cli, color: bool) -> Result<i32> {
    let Cli { common, command } = cli;

    let perl = build_perl(&common)?;
    let mut dist = Distribution::with_preference(&common.directory, perl, common.prefer.into())
        .with_context(|| {
            format!(
                "failed to open a CPAN distribution in {}",
                common.directory.display()
            )
        })?;

    // `dist_status` tracking: reconcile the row for this directory (and prune
    // rows for directories that have since been removed). Best effort — a
    // database problem must not stop a build.
    let status = match Status::open(cx, &dist) {
        Ok(status) => Some(status),
        Err(err) => {
            eprintln!("upt dist: status tracking unavailable: {err:#}");
            None
        }
    };

    match command {
        Command::PreConfigure { all_prereqs } => {
            let deps = dist.execute_pre_configure();
            if common.json {
                // `pre-configure` runs nothing: empty output, always successful.
                print_json(
                    &json!({
                        "prereqs": pre_configure_prereqs_json(&deps),
                        "output": "",
                        "exit": 0,
                        "success": true,
                    }),
                    color,
                );
            } else {
                print_pre_configure_table(&deps, all_prereqs, &dist.perl, color);
            }
            if let Some(status) = &status {
                record(status.mark(Phase::PreConfigure));
            }
            Ok(0)
        }
        Command::Configure {
            no_prereqs,
            all_prereqs,
            include_develop,
        } => {
            let (result, deps) = dist
                .execute_configure()
                .context("the configure step could not be started")?;
            let code = step_exit_code("configure", &result);
            if common.json {
                let mut obj = serde_json::Map::new();
                if !no_prereqs {
                    obj.insert("prereqs".to_string(), resolved_prereqs_json(&deps));
                }
                obj.insert(
                    "output".to_string(),
                    Value::String(captured_output(&result)),
                );
                obj.insert("exit".to_string(), json!(code));
                obj.insert("success".to_string(), json!(result.is_success));
                print_json(&Value::Object(obj), color);
            } else if !no_prereqs {
                print_resolved_prereqs_table(
                    &deps,
                    include_develop,
                    all_prereqs,
                    &dist.perl,
                    color,
                );
            }
            if result.is_success
                && let Some(status) = &status
            {
                record(status.mark(Phase::Configure));
            }
            Ok(i32::from(code))
        }
        Command::Build => finish_step(
            "build",
            &common,
            dist.execute_build()?,
            color,
            status.as_ref(),
            Outcome::Mark(Phase::Build),
        ),
        Command::Test => finish_step(
            "test",
            &common,
            dist.execute_test()?,
            color,
            status.as_ref(),
            Outcome::Mark(Phase::Test),
        ),
        Command::Install => finish_step(
            "install",
            &common,
            dist.execute_install()?,
            color,
            status.as_ref(),
            Outcome::Mark(Phase::Install),
        ),
        Command::Clean => finish_step(
            "clean",
            &common,
            dist.execute_clean()?,
            color,
            status.as_ref(),
            Outcome::ClearedBuild,
        ),
        Command::Distclean => finish_step(
            "distclean",
            &common,
            dist.execute_distclean()?,
            color,
            status.as_ref(),
            Outcome::ClearedAll,
        ),
    }
}

/// Emit the JSON envelope for a bare build step when `--json` is set, map the
/// [`ExecuteResult`] to a process exit code, and — on success — apply
/// `outcome` to the `dist_status` row.
fn finish_step(
    step: &str,
    common: &CommonArgs,
    result: ExecuteResult,
    color: bool,
    status: Option<&Status>,
    outcome: Outcome,
) -> Result<i32> {
    let code = step_exit_code(step, &result);
    if common.json {
        print_json(
            &json!({
                "output": captured_output(&result),
                "exit": code,
                "success": result.is_success,
            }),
            color,
        );
    }
    if result.is_success
        && let Some(status) = status
    {
        record(match outcome {
            Outcome::Mark(phase) => status.mark(phase),
            Outcome::ClearedBuild => status.cleared_build(),
            Outcome::ClearedAll => status.clear_all(),
        });
    }
    Ok(i32::from(code))
}

/// The child's captured, merged stdout+stderr as a lossy UTF-8 string, or `""`
/// when output was not captured (i.e. it went straight to the terminal).
fn captured_output(result: &ExecuteResult) -> String {
    result
        .output_lossy()
        .map(|text| text.into_owned())
        .unwrap_or_default()
}

/// Assemble the [`Perl`] wrapper from the shared options. Command output is
/// captured (rather than inherited) when `--json` is in effect, so it can be
/// folded into the JSON envelope.
fn build_perl(common: &CommonArgs) -> Result<Perl> {
    let mut perl = match &common.perl {
        Some(path) => Perl::with_perl(path),
        None => Perl::new().context("could not locate a `perl` interpreter on PATH")?,
    };

    perl = perl.with_capture_output(common.json);

    if let Some(make) = &common.make {
        perl = perl.with_make(make);
    }
    if let Some(base) = &common.install_base {
        perl = perl.with_install_base(base);
    }
    if !common.lib.is_empty() {
        perl = perl.with_lib(common.lib.clone());
    }

    Ok(perl)
}

/// The pre-configure prerequisites as `{ "configure": [ { "module", "version" },
/// ... ] }` — they are all configure-phase requirements.
fn pre_configure_prereqs_json(deps: &[Dependency]) -> Value {
    let rows: Vec<Value> = deps
        .iter()
        .map(|d| json!({ "module": d.module, "version": d.version }))
        .collect();
    json!({ "configure": rows })
}

/// Print the pre-configure prerequisites as a `module` / `required` /
/// `installed` table. Unless `show_all` is set only the unmet prerequisites are
/// listed. A module whose installed version does not satisfy the requirement
/// (including "not installed") gets a `*` after its name and, when colour is
/// enabled, a white-on-red `module` / `installed` cell.
fn print_pre_configure_table(deps: &[Dependency], show_all: bool, perl: &Perl, color: bool) {
    let mut table = house_style_table();
    table.set_header(header_row(["module", "required", "installed"], color));
    let mut any_unmet = false;
    let mut rows = 0usize;
    for dep in deps {
        let (installed, satisfied) = installed_status(perl, dep);
        any_unmet |= !satisfied;
        if !show_all && satisfied {
            continue;
        }
        rows += 1;
        // Pre-configure prerequisites are all hard configure requirements.
        let style = (!satisfied && color).then_some((Color::White, Color::Red));
        table.add_row([
            paint(Cell::new(module_cell(&dep.module, satisfied)), style),
            Cell::new(&dep.version),
            paint(Cell::new(installed), style),
        ]);
    }

    if !show_all && rows == 0 {
        println!("all prerequisites are satisfied");
        return;
    }
    println!("{table}");
    print_unmet_legend(any_unmet);
}

/// The resolved prerequisites as the full picture:
/// `{ "<phase>": [ { "relationship", "module", "version" }, ... ], ... }` with
/// every CPAN phase present as a key (empty phases map to `[]`).
fn resolved_prereqs_json(deps: &Dependencies) -> Value {
    let phases = [
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
        ("develop", &deps.develop),
    ];

    let mut prereqs = serde_json::Map::new();
    for (phase, group) in phases {
        prereqs.insert(phase.to_string(), Value::Array(phase_entries(group)));
    }
    Value::Object(prereqs)
}

/// Print the resolved prerequisites as a `phase` / `relationship` / `module` /
/// `required` / `installed` table. The `develop` phase is omitted unless
/// `include_develop` is set; unless `show_all` is set only the unmet
/// prerequisites are listed. A module whose installed version does not satisfy
/// the requirement (including "not installed") gets a `*` after its name and,
/// when colour is enabled, coloured `module` / `installed` cells: white on red
/// for `requires`, black on yellow for the optional `recommends` / `suggests`.
fn print_resolved_prereqs_table(
    deps: &Dependencies,
    include_develop: bool,
    show_all: bool,
    perl: &Perl,
    color: bool,
) {
    let mut table = house_style_table();
    table.set_header(header_row(
        ["phase", "relationship", "module", "required", "installed"],
        color,
    ));
    let mut any_unmet = false;
    let mut rows = 0usize;
    for (phase, relationship, dep) in flatten_prereqs(deps, include_develop) {
        let (installed, satisfied) = installed_status(perl, dep);
        // `conflicts` has inverted semantics, so it is never flagged.
        let flag = !satisfied && matches!(relationship, "requires" | "recommends" | "suggests");
        any_unmet |= flag;
        if !show_all && !flag {
            continue;
        }
        rows += 1;
        let style = if flag && color {
            unmet_style(relationship)
        } else {
            None
        };
        table.add_row([
            Cell::new(phase),
            Cell::new(relationship),
            paint(Cell::new(module_cell(&dep.module, !flag)), style),
            Cell::new(&dep.version),
            paint(Cell::new(installed), style),
        ]);
    }

    if !show_all && rows == 0 {
        println!("all prerequisites are satisfied");
        return;
    }
    println!("{table}");
    print_unmet_legend(any_unmet);
}

/// The `installed` column value for `dep` and whether it satisfies `dep`'s
/// required range. The value is the `$VERSION` declared in the module's source,
/// `"?"` when it is installed but declares none, or `"-"` when it is not
/// installed; a missing module never satisfies the requirement.
///
/// The `perl` pseudo-prerequisite is special-cased to the interpreter's own
/// version (`$]`) rather than a `perl.pm` lookup.
fn installed_status(perl: &Perl, dep: &Dependency) -> (String, bool) {
    if dep.module == "perl" {
        return match interpreter_version(perl) {
            Some(version) => {
                let satisfied = version_satisfies(&dep.version, &version);
                (version, satisfied)
            }
            // Could not ask the interpreter its version; don't flag it.
            None => ("-".to_string(), true),
        };
    }

    match perl.module(&dep.module) {
        None => ("-".to_string(), false),
        Some(found) => match found.version {
            Some(version) => {
                let satisfied = version_satisfies(&dep.version, &version);
                (version, satisfied)
            }
            // Installed but no recoverable version: satisfied only if any version
            // will do.
            None => ("?".to_string(), version_satisfies(&dep.version, "")),
        },
    }
}

/// The running interpreter's version string (`perl -e 'print $]'`, e.g.
/// `5.042003`), or `None` when it cannot be queried.
fn interpreter_version(perl: &Perl) -> Option<String> {
    let output = perl
        .perl_command()
        .arg("-e")
        .arg("print $]")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

/// A `module` cell's text: the name, with a trailing ` *` when `satisfied` is
/// false.
fn module_cell(name: &str, satisfied: bool) -> String {
    if satisfied {
        name.to_string()
    } else {
        format!("{name} *")
    }
}

/// Apply an optional `(foreground, background)` style to `cell`.
fn paint(cell: Cell, style: Option<(Color, Color)>) -> Cell {
    match style {
        Some((fg, bg)) => cell.fg(fg).bg(bg),
        None => cell,
    }
}

/// The highlight for an *unmet* prerequisite of the given relationship: white on
/// red for a hard `requires`, black on yellow for the softer `recommends` /
/// `suggests`, and none for `conflicts` (its "satisfied" sense is inverted).
fn unmet_style(relationship: &str) -> Option<(Color, Color)> {
    match relationship {
        "requires" => Some((Color::White, Color::Red)),
        "recommends" | "suggests" => Some((Color::Black, Color::Yellow)),
        _ => None,
    }
}

/// Print the `*` legend under a table when it flagged at least one row.
fn print_unmet_legend(any_unmet: bool) {
    if any_unmet {
        println!("* installed version does not satisfy the requirement");
    }
}

/// Returns `true` when `installed` satisfies the CPAN prerequisite version range
/// `required`.
///
/// `required` is a CPAN Meta version range: a bare version means "at least that"
/// (`0` or empty means any version), and comma-separated `>=`, `>`, `<=`, `<`,
/// `==`, `!=` clauses are ANDed. Versions are compared the way Perl's `version`
/// module does — a decimal version such as `5.010` is read as `v5.10.0`, its
/// fractional part split into 3-digit groups (the last zero-padded), while a
/// `v`-string or a version with two or more dots is compared component-wise.
///
/// An `installed` string that cannot be parsed never satisfies a non-empty
/// range; an empty range (or just `0`) is always satisfied.
fn version_satisfies(required: &str, installed: &str) -> bool {
    use std::cmp::Ordering;

    let clauses: Vec<(&str, &str)> = required
        .split(',')
        .filter_map(|clause| {
            let clause = clause.trim();
            if clause.is_empty() {
                return None;
            }
            for op in ["<=", ">=", "==", "!=", "<", ">"] {
                if let Some(rest) = clause.strip_prefix(op) {
                    return Some((op, rest.trim()));
                }
            }
            if clause == "0" {
                return None; // "any version" marker
            }
            Some((">=", clause)) // bare version means "at least"
        })
        .collect();

    if clauses.is_empty() {
        return true;
    }

    let Some(installed) = parse_perl_version(installed) else {
        return false;
    };

    clauses.iter().all(|(op, req)| {
        let Some(req) = parse_perl_version(req) else {
            return true; // ignore a clause we can't parse, as the CPAN parser does
        };
        let ord = cmp_versions(&installed, &req);
        match *op {
            "<" => ord == Ordering::Less,
            "<=" => ord != Ordering::Greater,
            ">" => ord == Ordering::Greater,
            ">=" => ord != Ordering::Less,
            "==" => ord == Ordering::Equal,
            "!=" => ord != Ordering::Equal,
            _ => true,
        }
    })
}

/// Parse a Perl version string into comparable integer components, following
/// `version.pm`: a leading `v` or two-plus dots means dotted-decimal (each
/// segment is one component); otherwise it is a decimal version whose fractional
/// digits are grouped in threes, the final group right-padded with zeros
/// (`5.010` -> `[5, 10]`, `1.302210` -> `[1, 302, 210]`). `_` (alpha releases)
/// is stripped. `None` if the string isn't a version.
fn parse_perl_version(raw: &str) -> Option<Vec<u64>> {
    let trimmed = raw.trim();
    let dotted = trimmed.starts_with(['v', 'V']);
    let body: String = trimmed
        .trim_start_matches(['v', 'V'])
        .chars()
        .filter(|c| *c != '_')
        .collect();
    if body.is_empty() {
        return None;
    }

    if dotted || body.matches('.').count() >= 2 {
        return body.split('.').map(|seg| seg.parse::<u64>().ok()).collect();
    }

    let mut it = body.splitn(2, '.');
    let mut parts = vec![it.next().unwrap_or("0").parse::<u64>().ok()?];
    if let Some(frac) = it.next().filter(|f| !f.is_empty()) {
        for chunk in frac.as_bytes().chunks(3) {
            let mut group = String::from_utf8_lossy(chunk).into_owned();
            while group.len() < 3 {
                group.push('0');
            }
            parts.push(group.parse::<u64>().ok()?);
        }
    }
    Some(parts)
}

/// Compare two version-component vectors element-wise, missing trailing
/// components counting as 0.
fn cmp_versions(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    (0..a.len().max(b.len()))
        .map(|i| {
            a.get(i)
                .copied()
                .unwrap_or(0)
                .cmp(&b.get(i).copied().unwrap_or(0))
        })
        .find(|o| o.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// The `{ "relationship", "module", "version" }` entries of one phase, in a
/// stable relationship-then-module order.
fn phase_entries(group: &PhaseDependencies) -> Vec<Value> {
    let relationships = [
        ("requires", &group.requires),
        ("recommends", &group.recommends),
        ("suggests", &group.suggests),
        ("conflicts", &group.conflicts),
    ];

    let mut out = Vec::new();
    for (relationship, list) in relationships {
        for dep in list {
            out.push(json!({
                "relationship": relationship,
                "module": dep.module,
                "version": dep.version,
            }));
        }
    }
    out
}

/// Flatten [`Dependencies`] into `(phase, relationship, dependency)` triples in
/// a stable phase-then-relationship order. The `develop` phase is included only
/// when `include_develop` is set.
fn flatten_prereqs(
    deps: &Dependencies,
    include_develop: bool,
) -> Vec<(&'static str, &'static str, &Dependency)> {
    let mut phases = vec![
        ("configure", &deps.configure),
        ("build", &deps.build),
        ("test", &deps.test),
        ("runtime", &deps.runtime),
    ];
    if include_develop {
        phases.push(("develop", &deps.develop));
    }

    let mut out = Vec::new();
    for (phase, group) in phases {
        let relationships = [
            ("requires", &group.requires),
            ("recommends", &group.recommends),
            ("suggests", &group.suggests),
            ("conflicts", &group.conflicts),
        ];
        for (relationship, list) in relationships {
            for dep in list {
                out.push((phase, relationship, dep));
            }
        }
    }
    out
}

/// A fresh [`Table`] in the same house style the rest of `upt` uses: the
/// `UTF8_FULL` preset, dynamic column arrangement, and a fixed width when the
/// output is not a terminal (so piped output wraps rather than sprawls).
fn house_style_table() -> Table {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    if !std::io::stdout().is_terminal() {
        table.set_width(100);
    }
    table
}

/// Header cells, emphasised when `color` is set.
fn header_row<'a>(cells: impl IntoIterator<Item = &'a str>, color: bool) -> Vec<Cell> {
    cells
        .into_iter()
        .map(|c| {
            let cell = Cell::new(c);
            if color {
                cell.add_attribute(Attribute::Bold)
            } else {
                cell
            }
        })
        .collect()
}

/// Print `value` as pretty JSON with a trailing newline, coloured when `color`
/// is set — the same printer the other `upt` subcommands use for `--json`.
fn print_json(value: &Value, color: bool) {
    print!("{}", json::to_string(value, color));
}

/// The exit code this process should use for `step`'s [`ExecuteResult`],
/// reporting failures on stderr as a side effect. `0` on success; the child's
/// code (coerced into `1..=255`) on a non-zero exit; `1` when it was killed by a
/// signal.
fn step_exit_code(step: &str, result: &ExecuteResult) -> u8 {
    if result.is_success {
        return 0;
    }

    match result.code {
        Some(code) => {
            eprintln!("upt dist: the {step} step exited with status {code}");
            let byte = u8::try_from(code).unwrap_or(1);
            if byte == 0 { 1 } else { byte }
        }
        None => {
            eprintln!("upt dist: the {step} step was terminated by a signal");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cmp_versions, parse_perl_version, version_satisfies};
    use std::cmp::Ordering;

    #[test]
    fn perl_decimal_versions_split_into_three_digit_groups() {
        assert_eq!(parse_perl_version("5.010"), Some(vec![5, 10]));
        assert_eq!(parse_perl_version("1.302210"), Some(vec![1, 302, 210]));
        assert_eq!(parse_perl_version("1.23"), Some(vec![1, 230]));
        assert_eq!(parse_perl_version("7"), Some(vec![7]));
        // `_` (alpha releases) is dropped.
        assert_eq!(parse_perl_version("1.23_01"), Some(vec![1, 230, 100]));
    }

    #[test]
    fn perl_dotted_versions_compare_component_wise() {
        assert_eq!(parse_perl_version("v5.10.0"), Some(vec![5, 10, 0]));
        assert_eq!(parse_perl_version("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(
            cmp_versions(
                &parse_perl_version("v5.10.1").unwrap(),
                &parse_perl_version("v5.10.0").unwrap()
            ),
            Ordering::Greater
        );
    }

    #[test]
    fn non_versions_do_not_parse() {
        assert_eq!(parse_perl_version(""), None);
        assert_eq!(parse_perl_version("undef"), None);
        assert_eq!(parse_perl_version("1.x"), None);
    }

    #[test]
    fn any_version_range_is_always_satisfied() {
        assert!(version_satisfies("0", "1.0"));
        assert!(version_satisfies("", "0.01"));
        assert!(version_satisfies("0", "anything"));
    }

    #[test]
    fn bare_version_means_at_least() {
        assert!(version_satisfies("1.09", "1.27"));
        assert!(version_satisfies("1.09", "1.09"));
        assert!(!version_satisfies("1.09", "1.05"));
        assert!(version_satisfies("6.58", "7.76"));
    }

    #[test]
    fn perl_decimal_gotchas() {
        // 5.10 normalises to v5.100.0, which is newer than 5.010 (v5.10.0).
        assert!(version_satisfies("5.010", "5.10"));
        assert!(!version_satisfies("5.10", "5.010"));
    }

    #[test]
    fn anded_operator_clauses() {
        assert!(version_satisfies(">= 1.2, < 2.0", "1.5"));
        assert!(!version_satisfies(">= 1.2, < 2.0", "2.5"));
        assert!(!version_satisfies(">= 1.2, < 2.0", "1.1"));
        assert!(!version_satisfies("!= 1.5", "1.5"));
        assert!(version_satisfies("!= 1.5", "1.6"));
        assert!(version_satisfies("== 1.302210", "1.302210"));
        assert!(!version_satisfies("== 1.302210", "1.302211"));
    }

    #[test]
    fn unparseable_installed_never_satisfies_a_real_range() {
        assert!(!version_satisfies("1.0", ""));
        assert!(!version_satisfies("1.0", "undef"));
        assert!(!version_satisfies(">= 1.2", "?"));
    }
}
