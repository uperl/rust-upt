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
    /// For a *legacy drop-in replacement*, the name of the original command it
    /// stands in for. `upt` invoked under this name (a symlink or copy of the
    /// `upt` binary) runs this subcommand directly, and `upt help` lists it
    /// under its own heading. `None` for an ordinary built-in.
    pub legacy_name: Option<&'static str>,
    /// Static help text for `upt help <name>` and `upt <name> --help`, or
    /// `None` for a subcommand with its own argument parser — `upt help <name>`
    /// then just runs it with `--help`.
    pub help: Option<&'static str>,
    /// Entry point. Receives the arguments that follow the subcommand name and
    /// returns the process exit code.
    pub run: fn(&Cx, &[String]) -> Result<i32>,
}

/// Every built-in. `upt help` sorts these by name for display; the order here
/// is not significant.
pub static BUILTINS: &[Builtin] = &[
    Builtin {
        name: "help",
        summary: "Show help for upt or one of its subcommands",
        legacy_name: None,
        help: Some(help::HELP),
        run: help::run,
    },
    Builtin {
        name: "which",
        summary: "Show whether a subcommand is built in or found in PATH",
        legacy_name: None,
        help: Some(which::HELP),
        run: which::run,
    },
    Builtin {
        name: "metacpan",
        summary: "Command line interface to the MetaCPAN API",
        legacy_name: None,
        help: None,
        run: crate::metacpan::run,
    },
    Builtin {
        name: "dist",
        summary: "Step-by-step build and install of an unpacked CPAN distribution",
        legacy_name: None,
        help: None,
        run: crate::dist::run,
    },
    Builtin {
        name: "cpan",
        summary: "Install distributions from CPAN by name",
        legacy_name: None,
        help: None,
        run: crate::cpan::run,
    },
    Builtin {
        name: "perl",
        summary: "Run perl through a configured perl-wrapper",
        legacy_name: None,
        help: None,
        run: crate::perl::run,
    },
    Builtin {
        name: "perlbuild",
        summary: "Build and install a perl from source",
        legacy_name: Some("perl-build"),
        help: None,
        run: crate::perlbuild::run,
    },
    Builtin {
        name: "patchperl",
        summary: "Patch a Perl source tree so it builds on a modern toolchain",
        legacy_name: Some("patchperl"),
        help: None,
        run: crate::patchperl::run,
    },
];

/// Look up a built-in by the name typed after `upt`.
pub fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Look up a legacy drop-in replacement by the original command name it
/// replaces (the name `upt` may be symlinked/copied to).
pub fn find_by_legacy_name(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.legacy_name == Some(name))
}

/// The legacy drop-in replacements, in registry order.
pub fn drop_in_replacements() -> impl Iterator<Item = &'static Builtin> {
    BUILTINS.iter().filter(|b| b.legacy_name.is_some())
}

/// The ordinary (non-drop-in) built-ins, in registry order.
pub fn ordinary() -> impl Iterator<Item = &'static Builtin> {
    BUILTINS.iter().filter(|b| b.legacy_name.is_none())
}
