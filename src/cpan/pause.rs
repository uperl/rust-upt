//! Reading PAUSE upload credentials from `~/.pause`, in the same format read
//! by the `cpan-upload` script (part of `CPAN::Uploader`); see
//! <https://metacpan.org/dist/CPAN-Uploader/view/bin/cpan-upload#CONFIGURATION>.
//!
//! The file lives at `~/.pause` on every platform — not one of `upt`'s own
//! `dirs`-based config locations (see [`crate::paths`]) — so it stays
//! interchangeable with `cpan-upload` and other existing CPAN tooling.
//!
//! GPG-encrypted files (the `Config::Identity` integration `cpan-upload` also
//! supports) are not supported: [`read`] rejects them with an error.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// A `user` / `password` pair read from `~/.pause`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

/// The default location: `~/.pause`.
pub fn default_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine the user's home directory")?;
    Ok(home.join(".pause"))
}

/// Read and parse `path` as a `.pause` file, warning on stderr if its
/// permissions let other users read it.
pub fn read(path: &Path) -> Result<Credentials> {
    if let Some(warning) = world_readable_warning(path) {
        eprintln!("warning: {warning}");
    }

    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&contents).with_context(|| format!("parsing {}", path.display()))
}

/// A warning message if `path` is readable by users other than its owner —
/// `~/.pause` holds a PAUSE password in plain text. `None` if the file's
/// permissions can't be determined, or on platforms (Windows) where this
/// notion of "world-readable" doesn't apply.
#[cfg(unix)]
fn world_readable_warning(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path).ok()?.permissions().mode();
    (mode & 0o004 != 0).then(|| {
        format!(
            "{} is world-readable; run `chmod 600 {}` to keep your PAUSE password private",
            path.display(),
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn world_readable_warning(_path: &Path) -> Option<String> {
    None
}

/// Parse `.pause`-format `contents`: whitespace-tolerant `key value` lines,
/// blank lines and `#`-comments ignored, `user` and `password` required.
fn parse(contents: &str) -> Result<Credentials> {
    let mut user = None;
    let mut password = None;

    for (lineno, line) in contents.lines().enumerate() {
        let lineno = lineno + 1;

        if line.contains("BEGIN PGP MESSAGE") {
            bail!("line {lineno}: this file appears to be GPG-encrypted, which is not supported");
        }

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once(char::is_whitespace) else {
            bail!("line {lineno}: '{line}' is not in 'key value' format");
        };
        let value = value.trim_start();

        let slot = match key {
            "user" => &mut user,
            "password" => &mut password,
            _ => continue,
        };
        if slot.replace(value.to_string()).is_some() {
            bail!("line {lineno}: multiple entries for '{key}'");
        }
    }

    let user = user.context("missing 'user'")?;
    let password = password.context("missing 'password'")?;

    if user.chars().any(char::is_whitespace) {
        bail!("configured user '{user}' contains whitespace");
    }

    Ok(Credentials { user, password })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_and_password() {
        let creds = parse("user EXAMPLE\npassword hunter2\n").unwrap();
        assert_eq!(creds.user, "EXAMPLE");
        assert_eq!(creds.password, "hunter2");
    }

    #[test]
    fn ignores_blank_lines_and_comments() {
        let creds = parse(
            "\
            # a comment\n\
            \n\
            user EXAMPLE\n\
            \n\
            password hunter2\n",
        )
        .unwrap();
        assert_eq!(creds.user, "EXAMPLE");
        assert_eq!(creds.password, "hunter2");
    }

    #[test]
    fn ignores_unknown_keys() {
        let creds = parse("user EXAMPLE\npassword hunter2\nHIDDENNAME extra\n").unwrap();
        assert_eq!(creds.user, "EXAMPLE");
        assert_eq!(creds.password, "hunter2");
    }

    #[test]
    fn tolerates_extra_whitespace() {
        let creds = parse("  user    EXAMPLE  \npassword\thunter2\n").unwrap();
        assert_eq!(creds.user, "EXAMPLE");
        assert_eq!(creds.password, "hunter2");
    }

    #[test]
    fn requires_user() {
        let err = parse("password hunter2\n").unwrap_err();
        assert!(err.to_string().contains("missing 'user'"));
    }

    #[test]
    fn requires_password() {
        let err = parse("user EXAMPLE\n").unwrap_err();
        assert!(err.to_string().contains("missing 'password'"));
    }

    #[test]
    fn rejects_duplicate_entries() {
        let err = parse("user EXAMPLE\nuser OTHER\npassword hunter2\n").unwrap_err();
        assert!(err.to_string().contains("multiple entries for 'user'"));
    }

    #[test]
    fn rejects_malformed_lines() {
        let err = parse("user EXAMPLE\nnonsense\npassword hunter2\n").unwrap_err();
        assert!(err.to_string().contains("not in 'key value' format"));
    }

    #[test]
    fn rejects_gpg_encrypted_files() {
        let err = parse("-----BEGIN PGP MESSAGE-----\n...\n").unwrap_err();
        assert!(err.to_string().contains("GPG-encrypted"));
    }

    #[test]
    fn rejects_whitespace_in_user() {
        let err = parse("user EXAMPLE EXTRA\npassword hunter2\n").unwrap_err();
        assert!(err.to_string().contains("contains whitespace"));
    }

    #[test]
    fn read_reads_a_real_file() {
        let path = std::env::temp_dir().join(format!("upt-pause-test-{}", std::process::id()));
        fs::write(&path, "user EXAMPLE\npassword hunter2\n").unwrap();

        let creds = read(&path).unwrap();
        assert_eq!(creds.user, "EXAMPLE");
        assert_eq!(creds.password, "hunter2");

        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(unix)]
    fn warns_on_world_readable_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!("upt-pause-perm-test-{}", std::process::id()));
        fs::write(&path, "user EXAMPLE\npassword hunter2\n").unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(world_readable_warning(&path).unwrap().contains("chmod 600"));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(world_readable_warning(&path).is_none());

        let _ = fs::remove_file(&path);
    }
}
