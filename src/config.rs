//! The user config file: `config.toml` with `[global]`, `[perlbuild]`,
//! `[dist]`, `[cpan]` and `[perl.<name>]` sections.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Starter `config.toml`, written to the default location on first run. Its
/// values match the built-in defaults, so writing it changes nothing; it just
/// gives the user a documented file to edit.
pub const DEFAULT_FILE: &str = r#"# upt configuration.
#
# The [global] section applies to upt itself and to every subcommand.

[global]
# Colorized terminal output: "always", "never", or "auto".
# "auto" colorizes when stdout is a terminal and NO_COLOR is unset.
color = "auto"

# The [perlbuild] section configures `upt perlbuild`.
[perlbuild]
# How to apply Devel::PatchPerl fix-ups to the Perl source tree:
#   "auto"     - the external `patchperl` if it is on PATH, otherwise the
#                bundled patch-perl crate (in-process)
#   "external" - only the external `patchperl`; warn and skip if it is missing
#   "internal" - only the bundled patch-perl crate
#   "off"      - do not apply any fix-ups
patch-perl = "auto"

# The [dist] section configures `upt dist`.
[dist]
# Which build tool to prefer for a distribution that ships BOTH `Build.PL`
# and `Makefile.PL` (ignored when only one is present):
#   "auto" - follow the build library's own choice
#   "mb"   - prefer `Build.PL` (Module::Build)
#   "eumm" - prefer `Makefile.PL` (ExtUtils::MakeMaker)
# `upt dist --prefer <tool>` overrides this.
prefer = "auto"

# The [cpan] section configures `upt cpan`.
[cpan]
# Where `upt cpan install` fetches releases from:
#   "metacpan" - resolve and download through the MetaCPAN API
#   "mirror"   - fetch from a configured CPAN mirror
source = "metacpan"
# Base URL of the CPAN mirror used when source = "mirror".
mirror-base-url = "https://www.cpan.org/"
# Base URL of the MetaCPAN API used when source = "metacpan".
metacpan-base-url = "https://fastapi.metacpan.org/v1/"

# The [perl] section defines named perl-wrapper configurations for
# `upt perl exec`. Each [perl.<name>] table builds one perl-wrapper object:
#
#   [perl.system]
#   perl = "/usr/bin/perl"
#
#   [perl.dev]
#   perl = "/opt/perl-5.40/bin/perl"
#   make = "/usr/bin/gmake"
#   install-base = "/home/me/perl5"
#   lib = ["/home/me/code/lib"]
#
# `perl.default` names the [perl.<name>] that `upt perl exec` runs when it is
# invoked without `--perl` (so a perl entry cannot itself be named "default"):
#
#   [perl]
#   default = "dev"
"#;

/// The parsed contents of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub global: Global,
    #[serde(default)]
    pub perlbuild: Perlbuild,
    #[serde(default)]
    pub dist: Dist,
    #[serde(default)]
    pub cpan: Cpan,
    #[serde(default)]
    pub perl: PerlSection,
}

/// The `[global]` section.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    #[serde(default)]
    pub color: ColorChoice,
}

/// The `[perlbuild]` section: settings for `upt perlbuild`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Perlbuild {
    /// How `upt perlbuild` applies Devel::PatchPerl fix-ups.
    #[serde(default, rename = "patch-perl")]
    pub patch_perl: PatchPerlMode,
}

/// The `[dist]` section: settings for `upt dist`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dist {
    /// Which build tool `upt dist` prefers for a distribution that ships both
    /// `Build.PL` and `Makefile.PL`. `upt dist --prefer` overrides it.
    #[serde(default)]
    pub prefer: DistPrefer,
}

/// Value of `dist.prefer`: which build tool `upt dist` prefers when a
/// distribution ships both `Build.PL` and `Makefile.PL` (ignored when only one
/// is present).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistPrefer {
    /// Follow the build library's own choice (currently `Module::Build`).
    #[default]
    Auto,
    /// Prefer `Build.PL` (`Module::Build`).
    Mb,
    /// Prefer `Makefile.PL` (`ExtUtils::MakeMaker`).
    Eumm,
}

/// Default value of `cpan.mirror-base-url`: the canonical CPAN mirror.
pub const DEFAULT_MIRROR_BASE_URL: &str = "https://www.cpan.org/";

/// Default value of `cpan.metacpan-base-url`: the MetaCPAN v1 API.
pub const DEFAULT_METACPAN_BASE_URL: &str = "https://fastapi.metacpan.org/v1/";

/// The `[cpan]` section: settings for `upt cpan`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cpan {
    /// Where `upt cpan install` fetches releases from.
    #[serde(default)]
    pub source: CpanSource,
    /// Base URL of the CPAN mirror used when `source = "mirror"`.
    #[serde(default = "default_mirror_base_url", rename = "mirror-base-url")]
    pub mirror_base_url: String,
    /// Base URL of the MetaCPAN API used when `source = "metacpan"`.
    #[serde(default = "default_metacpan_base_url", rename = "metacpan-base-url")]
    pub metacpan_base_url: String,
}

impl Default for Cpan {
    fn default() -> Self {
        Cpan {
            source: CpanSource::default(),
            mirror_base_url: default_mirror_base_url(),
            metacpan_base_url: default_metacpan_base_url(),
        }
    }
}

fn default_mirror_base_url() -> String {
    DEFAULT_MIRROR_BASE_URL.to_string()
}

fn default_metacpan_base_url() -> String {
    DEFAULT_METACPAN_BASE_URL.to_string()
}

/// Value of `cpan.source`: where `upt cpan install` gets releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CpanSource {
    /// Resolve and download through the MetaCPAN API.
    #[default]
    Metacpan,
    /// Fetch from a configured CPAN mirror.
    Mirror,
}

/// The `[perl]` section: named `perl-wrapper` configurations for `upt perl
/// exec`, plus the name of the one it uses when `--perl` is not given.
///
/// `default` is a reserved key holding `perl.default`; every other key is a
/// `[perl.<name>]` sub-table describing one `perl-wrapper` object.
#[derive(Debug, Default, Deserialize)]
pub struct PerlSection {
    /// `perl.default`: the `[perl.<name>]` entry `upt perl exec` runs when it is
    /// invoked without `--perl`. `None` when the config does not set it.
    pub default: Option<String>,
    /// Every `[perl.<name>]` sub-table, keyed by `<name>`.
    #[serde(flatten)]
    pub perls: BTreeMap<String, PerlConfig>,
}

/// One `[perl.<name>]` sub-table: the inputs used to build a
/// [`perl_wrapper::Perl`]. Every field is optional; an empty table builds a
/// wrapper around the first `perl` (and `make`) on `PATH`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerlConfig {
    /// Path to the `perl` executable. When omitted, the first `perl` on `PATH`
    /// is used.
    pub perl: Option<PathBuf>,
    /// Path to `make`. When omitted, the first `make` on `PATH` is used.
    pub make: Option<PathBuf>,
    /// Install prefix for newly built modules, `local::lib` / `INSTALL_BASE`
    /// style. When omitted, the interpreter's own directories are used.
    #[serde(rename = "install-base")]
    pub install_base: Option<PathBuf>,
    /// Directories to prepend to `PERL5LIB` when running `perl`.
    #[serde(default)]
    pub lib: Vec<PathBuf>,
}

/// Value of `perlbuild.patch-perl`: which Devel::PatchPerl implementation
/// `upt perlbuild` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PatchPerlMode {
    /// The external `patchperl` when on `PATH`, otherwise the bundled
    /// `patch-perl` crate.
    #[default]
    Auto,
    /// Apply no fix-ups at all.
    Off,
    /// Always use the bundled `patch-perl` crate (in-process).
    Internal,
    /// Always use the external `patchperl` program; warn and skip patching if
    /// it is not found.
    External,
}

/// Value of `global.color`: whether to emit ANSI color escapes. The spellings
/// match the `metacpan` subcommand's `--color` flag (`always` / `never` /
/// `auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorChoice {
    /// Always colorize.
    Always,
    /// Never colorize.
    Never,
    /// Colorize only when writing to a terminal.
    #[default]
    Auto,
}

impl Config {
    /// Load the configuration from `path`.
    ///
    /// A missing file is not an error and yields the default configuration; a
    /// file that exists but does not parse is.
    pub fn load(path: &Path) -> Result<Config> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("failed to parse config file {}", path.display())),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(anyhow::Error::new(err)
                .context(format!("failed to read config file {}", path.display()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_auto() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.global.color, ColorChoice::Auto);
    }

    #[test]
    fn starter_file_parses_and_matches_defaults() {
        let cfg: Config = toml::from_str(DEFAULT_FILE).unwrap();
        assert_eq!(cfg.global.color, ColorChoice::Auto);
        assert_eq!(cfg.perlbuild.patch_perl, PatchPerlMode::Auto);
        assert_eq!(cfg.dist.prefer, DistPrefer::Auto);
        assert_eq!(cfg.cpan.source, CpanSource::Metacpan);
        assert_eq!(cfg.cpan.mirror_base_url, DEFAULT_MIRROR_BASE_URL);
        assert_eq!(cfg.cpan.metacpan_base_url, DEFAULT_METACPAN_BASE_URL);
    }

    #[test]
    fn reads_cpan_source() {
        let cfg: Config = toml::from_str("[cpan]\nsource = \"mirror\"\n").unwrap();
        assert_eq!(cfg.cpan.source, CpanSource::Mirror);
        assert_eq!(
            toml::from_str::<Config>("[cpan]\nsource = \"metacpan\"\n")
                .unwrap()
                .cpan
                .source,
            CpanSource::Metacpan
        );
        // Absent section / key defaults to `metacpan`.
        assert_eq!(
            toml::from_str::<Config>("").unwrap().cpan.source,
            CpanSource::Metacpan
        );
    }

    #[test]
    fn rejects_unknown_cpan_source() {
        assert!(toml::from_str::<Config>("[cpan]\nsource = \"cpanm\"\n").is_err());
        assert!(toml::from_str::<Config>("[cpan]\nbogus = \"mirror\"\n").is_err());
    }

    #[test]
    fn reads_cpan_base_urls_and_defaults_them() {
        let cfg: Config = toml::from_str(
            "[cpan]\n\
             mirror-base-url = \"https://cpan.example/\"\n\
             metacpan-base-url = \"https://api.example/v1/\"\n",
        )
        .unwrap();
        assert_eq!(cfg.cpan.mirror_base_url, "https://cpan.example/");
        assert_eq!(cfg.cpan.metacpan_base_url, "https://api.example/v1/");

        // Each key defaults independently when the other is set.
        let only_mirror: Config =
            toml::from_str("[cpan]\nmirror-base-url = \"https://cpan.example/\"\n").unwrap();
        assert_eq!(only_mirror.cpan.mirror_base_url, "https://cpan.example/");
        assert_eq!(
            only_mirror.cpan.metacpan_base_url,
            DEFAULT_METACPAN_BASE_URL
        );

        // Absent section: both keys take their defaults.
        let empty: Config = toml::from_str("").unwrap();
        assert_eq!(empty.cpan.mirror_base_url, DEFAULT_MIRROR_BASE_URL);
        assert_eq!(empty.cpan.metacpan_base_url, DEFAULT_METACPAN_BASE_URL);

        // The snake_case spellings are not accepted.
        assert!(toml::from_str::<Config>("[cpan]\nmirror_base_url = \"x\"\n").is_err());
        assert!(toml::from_str::<Config>("[cpan]\nmetacpan_base_url = \"x\"\n").is_err());
    }

    #[test]
    fn reads_dist_prefer() {
        let cfg: Config = toml::from_str("[dist]\nprefer = \"eumm\"\n").unwrap();
        assert_eq!(cfg.dist.prefer, DistPrefer::Eumm);
        assert_eq!(
            toml::from_str::<Config>("[dist]\nprefer = \"mb\"\n")
                .unwrap()
                .dist
                .prefer,
            DistPrefer::Mb
        );
        // Absent section / key defaults to `auto`.
        assert_eq!(
            toml::from_str::<Config>("").unwrap().dist.prefer,
            DistPrefer::Auto
        );
    }

    #[test]
    fn rejects_unknown_dist_prefer() {
        assert!(toml::from_str::<Config>("[dist]\nprefer = \"cmake\"\n").is_err());
        assert!(toml::from_str::<Config>("[dist]\nbogus = \"mb\"\n").is_err());
    }

    #[test]
    fn reads_perlbuild_patch_perl() {
        let cfg: Config = toml::from_str("[perlbuild]\npatch-perl = \"internal\"\n").unwrap();
        assert_eq!(cfg.perlbuild.patch_perl, PatchPerlMode::Internal);
        assert_eq!(
            toml::from_str::<Config>("").unwrap().perlbuild.patch_perl,
            PatchPerlMode::Auto
        );
    }

    #[test]
    fn rejects_unknown_patch_perl() {
        assert!(toml::from_str::<Config>("[perlbuild]\npatch-perl = \"maybe\"\n").is_err());
        // The snake_case spelling is not accepted; the key is `patch-perl`.
        assert!(toml::from_str::<Config>("[perlbuild]\npatch_perl = \"auto\"\n").is_err());
    }

    #[test]
    fn reads_global_color() {
        let cfg: Config = toml::from_str("[global]\ncolor = \"never\"\n").unwrap();
        assert_eq!(cfg.global.color, ColorChoice::Never);
    }

    #[test]
    fn rejects_unknown_color() {
        assert!(toml::from_str::<Config>("[global]\ncolor = \"maybe\"\n").is_err());
    }

    #[test]
    fn rejects_legacy_on_off() {
        assert!(toml::from_str::<Config>("[global]\ncolor = \"on\"\n").is_err());
        assert!(toml::from_str::<Config>("[global]\ncolor = \"off\"\n").is_err());
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(toml::from_str::<Config>("[global]\ncolour = \"on\"\n").is_err());
    }

    #[test]
    fn perl_section_defaults_to_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.perl.default.is_none());
        assert!(cfg.perl.perls.is_empty());
    }

    #[test]
    fn reads_named_perls_and_default() {
        let cfg: Config = toml::from_str(
            "[perl]\n\
             default = \"dev\"\n\
             [perl.system]\n\
             perl = \"/usr/bin/perl\"\n\
             [perl.dev]\n\
             perl = \"/opt/perl/bin/perl\"\n\
             make = \"/usr/bin/gmake\"\n\
             install-base = \"/home/me/perl5\"\n\
             lib = [\"/a\", \"/b\"]\n",
        )
        .unwrap();

        assert_eq!(cfg.perl.default.as_deref(), Some("dev"));
        assert_eq!(cfg.perl.perls.len(), 2);

        let system = &cfg.perl.perls["system"];
        assert_eq!(system.perl.as_deref(), Some(Path::new("/usr/bin/perl")));
        assert!(system.make.is_none());
        assert!(system.lib.is_empty());

        let dev = &cfg.perl.perls["dev"];
        assert_eq!(dev.perl.as_deref(), Some(Path::new("/opt/perl/bin/perl")));
        assert_eq!(dev.make.as_deref(), Some(Path::new("/usr/bin/gmake")));
        assert_eq!(
            dev.install_base.as_deref(),
            Some(Path::new("/home/me/perl5"))
        );
        assert_eq!(dev.lib, [PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn rejects_unknown_key_in_a_named_perl() {
        assert!(
            toml::from_str::<Config>("[perl.dev]\ninstall_base = \"/x\"\n").is_err(),
            "the key is `install-base`, not `install_base`"
        );
        assert!(toml::from_str::<Config>("[perl.dev]\nbogus = \"/x\"\n").is_err());
    }

    #[test]
    fn starter_file_has_no_active_perl_section() {
        let cfg: Config = toml::from_str(DEFAULT_FILE).unwrap();
        assert!(cfg.perl.default.is_none());
        assert!(cfg.perl.perls.is_empty());
    }
}
