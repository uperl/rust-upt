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
    /// Full help text, shown by `upt help <name>` and `upt <name> --help`.
    pub help: &'static str,
    /// Entry point. Receives the arguments that follow the subcommand name and
    /// returns the process exit code.
    pub run: fn(&Cx, &[String]) -> Result<i32>,
}

/// Every built-in, in the order `upt help` lists them.
pub static BUILTINS: &[Builtin] = &[
    Builtin {
        name: "help",
        summary: "Show help for upt or one of its subcommands",
        help: help::HELP,
        run: help::run,
    },
    Builtin {
        name: "which",
        summary: "Show whether a subcommand is built in or found in PATH",
        help: which::HELP,
        run: which::run,
    },
];

/// Look up a built-in by name.
pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}
