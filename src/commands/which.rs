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
is visible. Exits non-zero if neither is found.

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

    let internal = commands::find(name).is_some();
    let external = pathsearch::find_external(name);
    let found = internal || external.is_some();
    let exit = if found { 0 } else { 1 };

    if as_json {
        let value = serde_json::json!({
            "subcommand": name,
            "found": found,
            "internal": internal,
            "external": external.as_ref().map(|p| p.display().to_string()),
        });
        print!("{}", json::to_string(&value, cx.style.enabled()));
        return Ok(exit);
    }

    let s = &cx.style;
    if internal {
        println!("{name}: {}", s.green("internal"));
    }
    if let Some(path) = &external {
        println!("{name}: {} {}", s.cyan("external"), path.display());
    }
    if !found {
        eprintln!("{name}: {}", s.red("not found"));
    }
    Ok(exit)
}
