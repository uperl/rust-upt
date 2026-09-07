//! Executing an external `upt-<name>` subcommand.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, anyhow};

use crate::Cx;

/// Run an external subcommand, forwarding `args`.
///
/// On Unix this replaces the current process image (so signals and exit status
/// pass straight through); elsewhere it spawns a child, waits, and returns its
/// exit code.
///
/// The child inherits two environment variables, mirroring how `cargo` exposes
/// `CARGO` to its subcommands:
///
/// * `UPT` — the path to this executable
/// * `UPT_CONFIG` — the config file path in effect
pub fn exec(cx: &Cx, program: &Path, args: &[String]) -> Result<i32> {
    let mut command = Command::new(program);
    command.args(args);
    if let Ok(exe) = std::env::current_exe() {
        command.env("UPT", exe);
    }
    command.env("UPT_CONFIG", &cx.config_path);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns if it failed.
        Err(anyhow!(
            "failed to execute {}: {}",
            program.display(),
            command.exec()
        ))
    }

    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .map_err(|err| anyhow!("failed to execute {}: {err}", program.display()))?;
        Ok(status.code().unwrap_or(1))
    }
}
