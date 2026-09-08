//! `upt which` — is a subcommand built in, or an external `upt-<name>` on PATH?

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::Value;

use crate::commands::Builtin;
use crate::{Cx, commands, json, pathsearch};

pub const HELP: &str = "\
upt which - show whether a subcommand is built in or found in PATH

Usage:
    upt which [--json] <SUBCOMMAND>
    upt which (--all | --all-legacy) [--json]

Prints `internal` if <SUBCOMMAND> is a built-in upt command, and/or
`external <PATH>` if an executable named `upt-<SUBCOMMAND>` is on your PATH.
Built-ins take precedence when both exist; both lines are shown so shadowing
is visible. A drop-in replacement resolves under either its `upt` name or the
legacy command name it stands in for. Exits non-zero if neither is found.

With --all, every known subcommand (built-in and every `upt-*` on PATH) is
listed, sorted by name. With --all-legacy, only the drop-in replacement
subcommands are listed.

Options:
    -a, --all       List every subcommand instead of looking one up.
    --all-legacy    List only the drop-in replacement subcommands.
    -j, --json      Print JSON: an object, or with --all[-legacy] an array.
";

pub fn run(cx: &Cx, args: &[String]) -> Result<i32> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }

    let as_json = args.iter().any(|a| a == "-j" || a == "--json");
    let all = args.iter().any(|a| a == "-a" || a == "--all");
    let all_legacy = args.iter().any(|a| a == "--all-legacy");

    if all || all_legacy {
        // `--all` is the superset, so it wins when both are given.
        let entries = all_entries(all_legacy && !all);
        if as_json {
            let array: Vec<Value> = entries.iter().map(Entry::to_json).collect();
            print!(
                "{}",
                json::to_string(&Value::Array(array), cx.style.enabled())
            );
        } else {
            for entry in &entries {
                entry.print_text(cx);
            }
        }
        return Ok(0);
    }

    let Some(name) = args.iter().find(|a| !a.starts_with('-')) else {
        bail!(
            "upt which: missing <SUBCOMMAND>\n\nUsage:\n    upt which [--json] <SUBCOMMAND>\n    upt which --all [--json]"
        );
    };

    let entry = Entry {
        subcommand: name.clone(),
        builtin: commands::find(name).or_else(|| commands::find_by_legacy_name(name)),
        external: pathsearch::find_external(name),
    };

    if as_json {
        print!("{}", json::to_string(&entry.to_json(), cx.style.enabled()));
    } else {
        entry.print_text(cx);
    }
    Ok(entry.exit_code())
}

/// How one subcommand name resolves: whether it names a built-in, and/or where
/// its `upt-<name>` executable lives on `PATH`.
struct Entry {
    subcommand: String,
    builtin: Option<&'static Builtin>,
    external: Option<PathBuf>,
}

impl Entry {
    fn internal(&self) -> bool {
        self.builtin.is_some()
    }

    fn found(&self) -> bool {
        self.internal() || self.external.is_some()
    }

    fn exit_code(&self) -> i32 {
        i32::from(!self.found())
    }

    fn to_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert("subcommand".into(), self.subcommand.as_str().into());
        obj.insert("found".into(), self.found().into());
        obj.insert("internal".into(), self.internal().into());
        // A drop-in replacement: report both its canonical `upt` name and the
        // legacy command name it stands in for, whichever was queried.
        if let Some(builtin) = self.builtin
            && let Some(legacy) = builtin.legacy_name
        {
            obj.insert("name".into(), builtin.name.into());
            obj.insert("legacy_name".into(), legacy.into());
        }
        // `path` only makes sense for a command resolved from PATH; a built-in
        // takes precedence and has no path.
        if !self.internal() {
            obj.insert(
                "path".into(),
                match &self.external {
                    Some(path) => path.display().to_string().into(),
                    None => Value::Null,
                },
            );
        }
        Value::Object(obj)
    }

    fn print_text(&self, cx: &Cx) {
        let s = &cx.style;
        let name = &self.subcommand;
        if let Some(builtin) = self.builtin {
            let note = match builtin.legacy_name {
                None => String::new(),
                // The legacy name is the `upt` name (e.g. `patchperl`).
                Some(legacy) if legacy == builtin.name => " (drop-in replacement)".to_string(),
                // Queried by the legacy name (`which perl-build`).
                Some(legacy) if name.as_str() == legacy => {
                    format!(" (drop-in replacement; run as `upt {}`)", builtin.name)
                }
                // Queried by the `upt` name, or listed by `--all`.
                Some(legacy) => format!(" (drop-in replacement for `{legacy}`)"),
            };
            println!("{name}: {}{note}", s.green("internal"));
        }
        if let Some(path) = &self.external {
            println!("{name}: {} {}", s.cyan("external"), path.display());
        }
        if !self.found() {
            eprintln!("{name}: {}", s.red("not found"));
        }
    }
}

/// Known subcommands as `Entry`s, sorted by name. With `legacy_only`, just the
/// drop-in replacements; otherwise every built-in plus every `upt-*` on `PATH`.
/// Drop-in replacements are listed under their canonical `upt` name only.
fn all_entries(legacy_only: bool) -> Vec<Entry> {
    let externals = pathsearch::list_external();

    let names: BTreeSet<&str> = if legacy_only {
        commands::drop_in_replacements().map(|b| b.name).collect()
    } else {
        let mut names: BTreeSet<&str> = commands::BUILTINS.iter().map(|b| b.name).collect();
        names.extend(externals.keys().map(String::as_str));
        names
    };

    names
        .into_iter()
        .map(|name| Entry {
            subcommand: name.to_string(),
            builtin: commands::find(name),
            external: externals.get(name).cloned(),
        })
        .collect()
}
