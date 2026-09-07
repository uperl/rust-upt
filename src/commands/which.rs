//! `upt which` — is a subcommand built in, or an external `upt-<name>` on PATH?

use anyhow::{Result, bail};

use crate::{Cx, commands, json, pathsearch};

pub const HELP: &str = "\
upt which - show whether a subcommand is built in or found in PATH

Usage:
    upt which [--json] <SUBCOMMAND>

Prints `internal` if <SUBCOMMAND> is a built-in upt command, and/or
`external <PATH>` if an executable named `upt-<SUBCOMMAND>` is on your PATH.
Built-ins take precedence when both exist; both lines are shown so shadowing
is visible. A drop-in replacement resolves under either its `upt` name or the
original command name it stands in for. Exits non-zero if neither is found.

Options:
    -j, --json    Print the result as a JSON object.
";

pub fn run(cx: &Cx, args: &[String]) -> Result<i32> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }

    let as_json = args.iter().any(|a| a == "-j" || a == "--json");

    let Some(name) = args.iter().find(|a| !a.starts_with('-')) else {
        bail!("upt which: missing <SUBCOMMAND>\n\nUsage:\n    upt which [--json] <SUBCOMMAND>");
    };

    let builtin = commands::find(name).or_else(|| commands::find_by_legacy_name(name));
    let internal = builtin.is_some();
    let external = pathsearch::find_external(name);
    let found = internal || external.is_some();
    let exit = if found { 0 } else { 1 };

    if as_json {
        let mut obj = serde_json::Map::new();
        obj.insert("subcommand".into(), name.as_str().into());
        obj.insert("found".into(), found.into());
        obj.insert("internal".into(), internal.into());
        // A drop-in replacement: report both its canonical `upt` name and the
        // original command name it stands in for, whichever was queried.
        if let Some(builtin) = builtin
            && let Some(original) = builtin.legacy_name
        {
            obj.insert("name".into(), builtin.name.into());
            obj.insert("legacy_name".into(), original.into());
        }
        // `path` only makes sense for a command resolved from PATH; a built-in
        // takes precedence and has no path.
        if !internal {
            obj.insert(
                "path".into(),
                match &external {
                    Some(p) => p.display().to_string().into(),
                    None => serde_json::Value::Null,
                },
            );
        }
        print!(
            "{}",
            json::to_string(&serde_json::Value::Object(obj), cx.style.enabled())
        );
        return Ok(exit);
    }

    let s = &cx.style;
    if let Some(builtin) = builtin {
        let note = match builtin.legacy_name {
            Some(original) if name.as_str() == original => {
                format!(" (drop-in replacement; run as `upt {}`)", builtin.name)
            }
            Some(original) => format!(" (drop-in replacement for `{original}`)"),
            None => String::new(),
        };
        println!("{name}: {}{note}", s.green("internal"));
    }
    if let Some(path) = &external {
        println!("{name}: {} {}", s.cyan("external"), path.display());
    }
    if !found {
        eprintln!("{name}: {}", s.red("not found"));
    }
    Ok(exit)
}
