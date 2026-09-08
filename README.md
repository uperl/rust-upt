# upt

**Unified Perl Tool** — a `cargo`/`git`-style command multiplexer for working
with Perl, CPAN, and MetaCPAN from the shell.

`upt <command>` runs a built-in subcommand when one exists; otherwise it looks
for an executable named `upt-<command>` on your `PATH` and runs that, so
`upt foo` runs `upt-foo`. A handful of built-ins double as **drop-in
replacements** for existing CPAN command-line tools.

## Install

```sh
cargo install --git https://github.com/uperl/upt
```

## Usage

```
upt [OPTIONS] <COMMAND> [ARGS]...
```

Global options (given before the subcommand):

| Option            | Description                                     |
|-------------------|-------------------------------------------------|
| `--color <WHEN>`  | When to use colour: `always`, `never`, `auto`  |
| `--config <FILE>` | Use an alternate config file                   |
| `-V`, `--version` | Print version                                  |
| `-h`, `--help`    | Print help (`upt -h <topic>` == `upt help <topic>`) |

## Built-in commands

### `upt help [SUBCOMMAND]`

Show the general help, a built-in subcommand's help, or — for an external
`upt-<name>` — run `upt-<name> --help`.

### `upt which [--json] <SUBCOMMAND>`

Report whether a subcommand is built in (`internal`) and/or resolves to an
`upt-<name>` executable on `PATH` (`external <path>`). Built-ins win when both
exist, but both are shown so shadowing is visible. `--all` lists every known
subcommand; `--all-legacy` lists just the drop-in replacements.

### `upt metacpan <QUERY> [--json | --raw | --curl]`

A command-line interface to the [MetaCPAN](https://metacpan.org) API. Each
document type is a sub-subcommand — `author`, `release`, `module`, `file`,
`source`, `pod`, `distribution`, `changes`, `download-url`, `download`,
`mirrors`, `search` — plus higher-level queries: `river` (reverse dependencies
of a distribution, or an author's current distributions, ranked by CPAN River
figures), `permissions` (PAUSE `06perms`), and `adoptable` (distributions up
for adoption). Results print as a table by default; `--json` emits
pretty-printed JSON, `--raw` dumps the HTTP request and response, and `--curl`
prints the equivalent `curl` command without making the request. Responses are
cached on disk (`upt metacpan cache path|status|clear`).

### `upt dist <STEP>`

Drive the build lifecycle of an *unpacked* CPAN distribution one step at a
time: `pre-configure`, `configure`, `build`, `test`, `install`, `clean`,
`distclean`. The steps form a pipeline — running one first runs any earlier
step that has not run yet — and progress is tracked per directory in the user
database, so re-running a step is cheap. `pre-configure` and `configure` also
print a prerequisite table (only the unmet rows unless `--all-prereqs`).
`--json` replaces the tables and live output with a single JSON envelope.

The interpreter to build with comes from a `[perl.<name>]` config section:
`--perl <name>` selects it, and without it `perl.default` is used — the same
resolution as [`upt perl exec`](#upt-perl).

### `upt cpan install <SPEC>...`

Install distributions from CPAN by name: resolve each `SPEC` (a module or
distribution name) through MetaCPAN, download and unpack the release, then run
the [`upt dist`](#upt-dist-step) pipeline through `install` on it. `--perl
<name>` selects the interpreter to build with (without it, `perl.default`) and
`--no-test` skips the test suite — the same options as `upt dist install`.

The `[cpan]` config section supplies the defaults; `--source <metacpan|mirror>`,
`--metacpan-base-url <url>` and `--mirror-base-url <url>` override
`cpan.source`, `cpan.metacpan-base-url` and `cpan.mirror-base-url` for a single
invocation.

*Work in progress:* only the command-line surface exists so far; running it
reports that the installer is not built yet.

### `upt perl <SUBCOMMAND>`

Run a configured `perl` and manage the named `perl-wrapper` configurations in
the `[perl.<name>]` sections of the config file. See [`upt perl`](#upt-perl)
below for the individual subcommands.

## Drop-in replacements

These built-ins reproduce the command-line interface of an existing CPAN tool.
Run them as `upt <name>`, or symlink/copy the `upt` binary to the legacy name
and run it directly.

### `upt perlbuild` (also as `perl-build`)

Build and install a `perl` from source — a drop-in replacement for
[`perl-build`](https://metacpan.org/dist/Perl-Build). Accepts a version
(resolved through MetaCPAN), a URL, a source tarball, or `blead`, followed by
an install prefix and optional `./Configure` options. Devel::PatchPerl
fix-ups are applied via the external `patchperl` when available, otherwise
in-process with the bundled [`patch-perl`](https://github.com/uperl/rust-patch-perl)
crate; see `perlbuild.patch-perl` in the config.

### `upt patchperl` (also as `patchperl`)

Patch a Perl source tree so it builds on a modern toolchain — a drop-in
replacement for [`patchperl`](https://metacpan.org/dist/Devel-PatchPerl).
Takes the source tree (default `.`) and, optionally, the Perl version to patch
as (otherwise read from `patchlevel.h`).

## `upt perl`

Work with the named [`perl-wrapper`][perl-wrapper] configurations in the
`[perl.<name>]` sections of the config file.

### `upt perl exec`

Run `perl` through the wrapper built from a `[perl.<name>]` section.

```
upt perl exec [--perl <name>] [-- <perl options>...]
```

* `--perl <name>` selects the `[perl.<name>]` config section. Without it, the
  section named by `perl.default` is used.
* Everything after `--` is passed straight to `perl`.
* The command exits with `perl`'s own status.

```sh
upt perl exec --perl dev -- -E 'say "$^X $]"'
```

### `upt perl register`

Add a new `[perl.<name>]` section to the config file (comments and other
sections are preserved).

```
upt perl register <perl binary> --perl <name> [--make <path>]
                   [--install-base <dir>] [--lib <dir>]...
```

* `--perl <name>` is required and must not already be a `[perl.<name>]` in the
  config (nor the reserved name `default`).
* `--make` defaults to `$Config{make}` of the given interpreter.
* `--install-base` and `--lib` are optional; `--lib` may be repeated.

```sh
upt perl register /opt/perl-5.40/bin/perl --perl dev \
    --install-base ~/perl5 --lib ~/code/lib
```

### `upt perl select`

Point `perl.default` at an already-registered `[perl.<name>]` — the section
`upt perl exec` uses when it is run without `--perl` (comments and other
sections are preserved).

```
upt perl select --perl <name>
```

* `--perl <name>` is required and must already be a `[perl.<name>]` in the
  config (register it first; the reserved name `default` is rejected).

```sh
upt perl select --perl dev
```

### `upt perl list`

List the names of the `[perl.<name>]` sections in the config file, one per
line and sorted. `perl.default` is not shown.

```
upt perl list [--json]
```

* `--json` (`-j`) prints the names as a JSON array of strings instead.

```sh
upt perl list
upt perl list --json
```

### `upt perl default`

Print the name of `perl.default` — the section `upt perl exec` uses when it
is run without `--perl`. Exits non-zero when `perl.default` is not set.

```
upt perl default [--json]
```

* `--json` (`-j`) prints the name as a single-element JSON array of strings.

```sh
upt perl default
upt perl default --json
```

### `upt perl info`

Show every setting of one `[perl.<name>]` section — a `Field` / `Value`
table by default, or a JSON object with `--json`.

```
upt perl info [--perl <name>] [--json]
```

* `--perl <name>` selects the section. Without it, the `perl.default`
  section is shown (an error if `perl.default` is unset).
* Unset optional settings render as a fallback note in the table
  (`(first perl on PATH)`, `(none)`, …) and as `null` / `[]` in JSON.
* `--json` (`-j`) keys: `name`, `default` (bool), `perl`, `make`,
  `install-base`, `lib` (array).

```sh
upt perl info
upt perl info --perl dev --json
```

[perl-wrapper]: https://github.com/uperl/rust-perl-wrapper

## External commands

Any executable named `upt-<name>` on your `PATH` can be run as `upt <name>`,
with every following argument forwarded verbatim.

## Configuration

`upt` writes a starter `config.toml` on first run and reads it on every
invocation:

```toml
[global]
# "always", "never", or "auto" (colour when stdout is a TTY and NO_COLOR is unset)
color = "auto"

[perlbuild]
# How `upt perlbuild` applies Devel::PatchPerl fix-ups:
#   "auto"     - external `patchperl` if on PATH, else the bundled crate
#   "external" - only the external `patchperl` (warn and skip if missing)
#   "internal" - only the bundled patch-perl crate
#   "off"      - apply no fix-ups
patch-perl = "auto"

[dist]
# Which build tool `upt dist` prefers for a distribution shipping BOTH
# `Build.PL` and `Makefile.PL` (ignored otherwise); `upt dist --prefer`
# overrides it:
#   "auto" - follow the build library's own choice
#   "mb"   - prefer `Build.PL` (Module::Build)
#   "eumm" - prefer `Makefile.PL` (ExtUtils::MakeMaker)
prefer = "auto"

[cpan]
# Where `upt cpan install` fetches releases from:
#   "metacpan" - resolve and download through the MetaCPAN API
#   "mirror"   - fetch from a configured CPAN mirror
source = "metacpan"
mirror-base-url = "https://www.cpan.org/"          # used when source = "mirror"
metacpan-base-url = "https://fastapi.metacpan.org/v1/"  # used when source = "metacpan"

# Named perl-wrapper configurations for `upt perl`. Each [perl.<name>] table
# builds one perl-wrapper object.
[perl]
# The [perl.<name>] used when `upt perl exec` runs without `--perl`
# (so a perl entry cannot itself be named "default").
default = "dev"

[perl.dev]
perl = "/opt/perl-5.40/bin/perl"   # default: first `perl` on PATH
make = "/usr/bin/gmake"            # default: first `make` on PATH
install-base = "/home/me/perl5"    # local::lib / INSTALL_BASE prefix
lib = ["/home/me/code/lib"]        # prepended to PERL5LIB
```

Platform locations:

| Purpose      | Linux                              | macOS                                  | Windows                       |
|--------------|-----------------------------------|----------------------------------------|------------------------------|
| Config file  | `~/.config/upt/config.toml`       | `~/Library/Application Support/upt/`   | `%APPDATA%\upt\`             |
| Cache        | `~/.cache/upt/`                   | `~/Library/Caches/upt/`                | `%LOCALAPPDATA%\upt\`        |
| State (SQLite) | `~/.local/state/upt/upt.sqlite`  | `~/Library/Application Support/upt/`   | `%LOCALAPPDATA%\upt\`        |

(`XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME` are honoured on
Linux.) The database is created lazily, the first time a subcommand needs it.

## License

MIT — see [LICENSE](LICENSE).
