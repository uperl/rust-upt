//! Integration tests for `upt completions SHELL`.
//!
//! Every run happens in an unconfigured environment: an empty `HOME` and XDG
//! directories under a throwaway temp dir, no proxy or credential variables,
//! and a `--config` path that does not exist. The tests check that each
//! supported shell gets a script on stdout with nothing on stderr, that the
//! script covers the subcommands, options and enumerated values upt accepts,
//! and that generating it leaves no config, cache or database files behind.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SHELLS: &[&str] = &["zsh", "bash", "fish", "powershell", "elvish"];

/// A self-deleting scratch directory under the system temp dir.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "upt-completions-it-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run `upt` with `args` in a clean environment rooted at `home`.
fn upt_in(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_upt"))
        .args(args)
        .env_clear()
        .env("PATH", "/nonexistent")
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home)
        .env("LOCALAPPDATA", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_HOME", home.join("data"))
        .output()
        .expect("running upt")
}

/// Generate the script for `shell`, asserting a clean, successful run.
fn script(shell: &str) -> String {
    let home = TempDir::new(shell);
    let out = upt_in(home.path(), &["completions", shell]);
    assert!(out.status.success(), "{shell}: exit {:?}", out.status);
    assert!(
        out.stderr.is_empty(),
        "{shell}: unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("script is UTF-8")
}

/// Assert that `text` mentions each of `needles`.
fn assert_covers(shell: &str, text: &str, needles: &[&str]) {
    for needle in needles {
        assert!(text.contains(needle), "{shell} script lacks {needle:?}");
    }
}

/// The subcommands every script must offer.
const WORDS: &[&str] = &[
    // Built-ins and drop-in replacements.
    "completions",
    "help",
    "which",
    "metacpan",
    "dist",
    "cpan",
    "perl",
    "perlbuild",
    "patchperl",
    // Nested subcommands.
    "install",
    "upload",
    "exec",
    "register",
];

/// Enumerated values the scripts must offer. `clap_complete`'s PowerShell and
/// elvish generators list no argument values at all, so only the zsh, bash
/// and fish scripts are held to these.
const VALUES: &[&str] = &[
    // `upt --color`.
    "always",
    "never",
    "auto",
    // `upt cpan install --source`, `upt dist --prefer`, `upt completions`.
    "mirror",
    "eumm",
    "powershell",
    "elvish",
];

/// Long options every script must offer, without their leading `--`.
const OPTIONS: &[&str] = &[
    // Global.
    "color",
    "config",
    "version",
    // Subcommands'.
    "json",
    "all-legacy",
    "prefer",
    "source",
    "no-test",
    "tarball-dir",
    "patchlevel",
];

/// How `shell`'s script spells the long option `name`.
fn long_option(shell: &str, name: &str) -> String {
    match shell {
        "fish" => format!("-l {name}"),
        _ => format!("--{name}"),
    }
}

#[test]
fn every_shell_gets_a_complete_script() {
    for shell in SHELLS {
        let text = script(shell);
        assert!(!text.trim().is_empty(), "{shell}: empty script");
        assert_covers(shell, &text, WORDS);
        if matches!(*shell, "zsh" | "bash" | "fish") {
            assert_covers(shell, &text, VALUES);
        }
        for name in OPTIONS {
            let option = long_option(shell, name);
            assert!(text.contains(&option), "{shell} script lacks {option:?}");
        }
    }
}

#[test]
fn scripts_have_the_shells_own_shape() {
    assert!(script("zsh").starts_with("#compdef upt"));
    assert_covers("bash", &script("bash"), &["_upt()", "complete -F _upt"]);
    assert_covers(
        "fish",
        &script("fish"),
        &[
            "complete -c upt",
            // Added by upt: fish's generator skips positional values.
            "__fish_upt_using_subcommand completions\" -f -a \"bash elvish fish powershell zsh\"",
        ],
    );
    assert_covers(
        "powershell",
        &script("powershell"),
        &["Register-ArgumentCompleter"],
    );
    assert_covers(
        "elvish",
        &script("elvish"),
        &["edit:completion:arg-completer[upt]"],
    );
}

#[test]
fn scripts_are_deterministic() {
    for shell in SHELLS {
        assert_eq!(script(shell), script(shell), "{shell}");
    }
}

#[test]
fn leaves_an_unconfigured_environment_untouched() {
    let home = TempDir::new("untouched");
    let missing = home.path().join("no-such-dir").join("upt.toml");
    for shell in SHELLS {
        let out = upt_in(home.path(), &["completions", shell]);
        assert!(out.status.success(), "{shell}");
        // An explicit config path that does not exist is not read or created.
        let out = upt_in(
            home.path(),
            &["--config", missing.to_str().unwrap(), "completions", shell],
        );
        assert!(out.status.success(), "{shell} with --config");
        assert!(out.stderr.is_empty(), "{shell} with --config");
    }
    let left: Vec<_> = std::fs::read_dir(home.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(left.is_empty(), "files created: {left:?}");
}

#[test]
fn works_with_an_unreadable_config() {
    // A broken config would make other subcommands fail; completions never
    // load it.
    let home = TempDir::new("broken");
    let config = home.path().join("upt.toml");
    std::fs::write(&config, "this is = = not toml").unwrap();
    let out = upt_in(
        home.path(),
        &["--config", config.to_str().unwrap(), "completions", "zsh"],
    );
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
}

#[test]
fn unknown_or_missing_shell_is_a_usage_error() {
    let home = TempDir::new("usage");
    for args in [&["completions", "tcsh"][..], &["completions"][..]] {
        let out = upt_in(home.path(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!out.stderr.is_empty(), "{args:?}");
    }
}

#[test]
fn help_documents_installation() {
    let home = TempDir::new("help");
    let out = upt_in(home.path(), &["completions", "--help"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("upt completions zsh > ~/.zfunc/_upt"),
        "{text}"
    );
}
