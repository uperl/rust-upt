//! Finding external subcommand implementations on `PATH`.

use std::collections::BTreeMap;
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

/// Every external subcommand on `PATH`: each `upt-<name>` executable, mapped
/// `name -> path`. When a name is provided by more than one `PATH` entry the
/// first (the one that would run) wins. The map is ordered by name.
pub fn list_external() -> BTreeMap<String, PathBuf> {
    match std::env::var_os("PATH") {
        Some(path) => list_external_in(std::env::split_paths(&path)),
        None => BTreeMap::new(),
    }
}

fn list_external_in(dirs: impl Iterator<Item = PathBuf>) -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let Some(name) = subcommand_name(file_name) else {
                continue;
            };
            if let Some(path) = match_in_dir(&dir, file_name) {
                found.entry(name).or_insert(path);
            }
        }
    }
    found
}

/// The subcommand name a `upt-<name>` file provides, or `None` if the file name
/// is not `upt-<something>`.
#[cfg(not(windows))]
fn subcommand_name(file_name: &str) -> Option<String> {
    file_name
        .strip_prefix(crate::EXTERNAL_PREFIX)
        .filter(|name| !name.is_empty())
        .map(String::from)
}

#[cfg(windows)]
fn subcommand_name(file_name: &str) -> Option<String> {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str())?;
    stem.strip_prefix(crate::EXTERNAL_PREFIX)
        .filter(|name| !name.is_empty())
        .map(String::from)
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

    #[test]
    fn list_external_in_collects_executables_and_orders_by_name() {
        let base = std::env::temp_dir().join(format!("upt-list-test-{}", std::process::id()));
        let a = base.join("a");
        let b = base.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        let mk = |dir: &std::path::Path, name: &str, mode: u32| {
            let p = dir.join(name);
            fs::write(&p, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).unwrap();
        };
        mk(&a, "upt-zed", 0o755);
        mk(&a, "upt-shared", 0o755); // earlier dir wins
        mk(&a, "upt-plain", 0o644); // not executable -> skipped
        mk(&a, "not-a-subcommand", 0o755);
        mk(&b, "upt-alpha", 0o755);
        mk(&b, "upt-shared", 0o755);

        let found = super::list_external_in([a.clone(), b.clone()].into_iter());

        assert_eq!(
            found.keys().cloned().collect::<Vec<_>>(),
            ["alpha", "shared", "zed"]
        );
        assert_eq!(found["shared"], a.join("upt-shared"));
        assert_eq!(found["alpha"], b.join("upt-alpha"));

        fs::remove_dir_all(&base).ok();
    }
}
