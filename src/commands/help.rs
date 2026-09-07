//! `upt help` — top-level help, or help for one subcommand.

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::{Cx, commands, external, pathsearch};

pub const HELP: &str = "\
upt help - show help for upt or one of its subcommands

Usage:
    upt help [SUBCOMMAND]

With no argument, print the general help. For a built-in subcommand, print
that subcommand's help. For an external `upt-<name>` command, run
`upt-<name> --help`.
";

pub fn run(cx: &Cx, args: &[String]) -> Result<i32> {
    // Ignore leading flags such as `-h`; the first bare word is the topic.
    let Some(topic) = args.iter().find(|a| !a.starts_with('-')) else {
        print!("{}", general(cx));
        return Ok(0);
    };

    if let Some(builtin) = commands::find(topic) {
        return match builtin.help {
            Some(text) => {
                print!("{text}");
                Ok(0)
            }
            // No static help: let the subcommand's own parser render it.
            None => (builtin.run)(cx, &["--help".to_string()]),
        };
    }

    if let Some(path) = pathsearch::find_external(topic) {
        return external::exec(cx, &path, &["--help".to_string()]);
    }

    bail!("no help for '{topic}': not a built-in command and no `upt-{topic}` on PATH");
}

/// Render the top-level help text.
pub fn general(cx: &Cx) -> String {
    let s = &cx.style;
    let mut out = String::new();

    let _ = writeln!(
        out,
        "{} {}\n{}\n",
        s.bold("upt"),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_DESCRIPTION"),
    );

    let _ = writeln!(
        out,
        "{}\n    upt [OPTIONS] <COMMAND> [ARGS]...\n",
        s.bold("Usage:")
    );

    let _ = writeln!(out, "{}", s.bold("Options:"));
    out.push_str("    --color <WHEN>     When to use color: always, never, auto\n");
    out.push_str("    --config <FILE>    Use an alternate config file\n");
    out.push_str("    -V, --version      Print version\n");
    out.push_str("    -h, --help         Print help\n\n");

    // One column width across both command sections so they line up.
    let width = commands::BUILTINS
        .iter()
        .map(|b| b.name.len())
        .max()
        .unwrap_or(0);

    let _ = writeln!(out, "{}", s.bold("Built-in commands:"));
    for b in commands::ordinary() {
        let _ = writeln!(out, "    {:<width$}    {}", b.name, b.summary);
    }
    out.push('\n');

    let _ = writeln!(out, "{}", s.bold("Drop-in replacements:"));
    for b in commands::drop_in_replacements() {
        let original = b.original_name.unwrap_or(b.name);
        let _ = writeln!(
            out,
            "    {:<width$}    {} (also as `{original}`)",
            b.name, b.summary
        );
    }
    out.push('\n');

    let _ = writeln!(out, "{}", s.bold("External commands:"));
    out.push_str(
        "    Any executable named `upt-<name>` on your PATH can be run as `upt <name>`.\n\n",
    );

    let _ = writeln!(
        out,
        "{}\n    {}",
        s.bold("Config file:"),
        cx.config_path.display()
    );

    out
}
