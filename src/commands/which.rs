//! `upt which` — is a subcommand built in, or an external `upt-<name>` on PATH?

use anyhow::{Result, bail};

use crate::{Cx, commands, pathsearch};

pub const HELP: &str = "\
upt which - show whether a subcommand is built in or found in PATH

Usage:
    upt which <SUBCOMMAND>

Prints `internal` if <SUBCOMMAND> is a built-in upt command, and/or
`external <PATH>` if an executable named `upt-<SUBCOMMAND>` is on your PATH.
Built-ins take precedence when both exist; both lines are shown so shadowing
is visible. Exits non-zero if neither is found.
";

pub fn run(cx: &Cx, args: &[String]) -> Result<i32> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(0);
    }

    let Some(name) = args.iter().find(|a| !a.starts_with('-')) else {
        bail!("upt which: missing <SUBCOMMAND>\n\nUsage:\n    upt which <SUBCOMMAND>");
    };

    let s = &cx.style;
    let mut found = false;

    if commands::find(name).is_some() {
        println!("{name}: {}", s.green("internal"));
        found = true;
    }

    if let Some(path) = pathsearch::find_external(name) {
        println!("{name}: {} {}", s.cyan("external"), path.display());
        found = true;
    }

    if found {
        Ok(0)
    } else {
        eprintln!("{name}: {}", s.red("not found"));
        Ok(1)
    }
}
