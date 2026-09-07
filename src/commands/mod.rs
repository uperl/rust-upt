//! The registry of built-in subcommands.

pub mod help;
pub mod which;

use anyhow::Result;

use crate::Cx;

/// A subcommand implemented inside `upt` itself.
pub struct Builtin {
    /// The name the user types: `upt <name>`.
    pub name: &'static str,
    /// One-line description, shown by `upt help`.
    pub summary: &'static str,
    /// Static help text for `upt help <name>` and `upt <name> --help`, or
    /// `None` for a subcommand with its own argument parser — `upt help <name>`
    /// then just runs it with `--help`.
    pub help: Option<&'static str>,
    /// Entry point. Receives the arguments that follow the subcommand name and
    /// returns the process exit code.
    pub run: fn(&Cx, &[String]) -> Result<i32>,
}

/// Every built-in, in the order `upt help` lists them.
pub static BUILTINS: &[Builtin] = &[
    Builtin {
        name: "help",
        summary: "Show help for upt or one of its subcommands",
        help: Some(help::HELP),
        run: help::run,
    },
    Builtin {
        name: "which",
        summary: "Show whether a subcommand is built in or found in PATH",
        help: Some(which::HELP),
        run: which::run,
    },
    Builtin {
        name: "metacpan",
        summary: "Command line interface to the MetaCPAN API",
        help: None,
        run: crate::metacpan::run,
    },
    Builtin {
        name: "dist",
        summary: "Step-by-step build and install of an unpacked CPAN distribution",
        help: None,
        run: crate::dist::run,
    },
];

/// Look up a built-in by name.
pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}
