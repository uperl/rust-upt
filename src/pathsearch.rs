//! Finding external subcommand implementations on `PATH`.

use std::path::{Path, PathBuf};

/// Look for an external subcommand implementation named `upt-<subcommand>` on `PATH`.
///
/// Returns the first match in `PATH` order — the one that would actually be run.
pub fn find_external(subcommand: &str) -> Option<PathBuf> {
    let stem = format!("{}{subcommand}", crate::EXTERNAL_PREFIX);
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .find_map(|dir| match_in_dir(&dir, &stem))
}

#[cfg(unix)]
fn match_in_dir(dir: &Path, stem: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let candidate = dir.join(stem);
    let meta = std::fs::metadata(&candidate).ok()?;
    (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
}

#[cfg(windows)]
fn match_in_dir(dir: &Path, stem: &str) -> Option<PathBuf> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());

    // Honor an explicit extension if one was somehow given.
    let direct = dir.join(stem);
    if direct.is_file() {
        return Some(direct);
    }
    pathext
        .split(';')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| dir.join(format!("{stem}{ext}")))
        .find(|candidate| candidate.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn finds_executable_and_skips_non_executable() {
        let dir = std::env::temp_dir().join(format!("upt-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let good = dir.join("upt-demo");
        fs::write(&good, b"#!/bin/sh\n").unwrap();
        fs::set_permissions(&good, fs::Permissions::from_mode(0o755)).unwrap();

        let bad = dir.join("upt-plain");
        fs::write(&bad, b"nope").unwrap();
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(super::match_in_dir(&dir, "upt-demo"), Some(good));
        assert_eq!(super::match_in_dir(&dir, "upt-plain"), None);
        assert_eq!(super::match_in_dir(&dir, "upt-missing"), None);

        fs::remove_dir_all(&dir).ok();
    }
}
