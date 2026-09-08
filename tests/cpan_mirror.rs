//! Integration tests for `upt cpan install --source mirror`.
//!
//! A CPAN mirror is stood up two ways — a throwaway in-process HTTP server, and
//! a plain directory reached with a `file://` URL — each serving a
//! `modules/02packages.details.txt.gz` index and the `authors/id/...` tarballs
//! for a handful of `Acme-UPT-*` / `Acme::UPT::*` distributions built on the
//! fly, so nothing here can collide with real CPAN modules. `--metacpan-base-url`
//! points at a dead host, so the runs only succeed if mirror mode resolves
//! everything from the index without touching MetaCPAN.
//!
//! The `Acme-UPT-Top` distribution declares one prerequisite in each of the
//! `configure`, `build`, `test` and `runtime` phases; the tests check that all
//! four land in a throwaway install base, except that the `test`-phase one is
//! installed only when `--no-test` is *not* given.
//!
//! Requires `perl`, `make` and `tar` on `PATH`; skipped otherwise.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use cpan_packagedetails::{Entry, PackageDetails};

/// A self-deleting scratch directory under the system temp dir.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("upt-cpan-it-{tag}-{}-{nanos}", std::process::id()));
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

fn tool_available(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// One mock distribution: `Acme-UPT-<leaf>` providing `Acme::UPT::<leaf>`.
struct MockDist {
    /// e.g. `Acme-UPT-Top`.
    dist: &'static str,
    /// e.g. `Acme::UPT::Top`.
    module: String,
    /// Extra arguments spliced into `WriteMakefile(...)`.
    makefile_extra: &'static str,
    /// The `prereqs` object for `META.json`.
    prereqs: &'static str,
    /// `t/` test files as `(name, body)`.
    tests: &'static [(&'static str, &'static str)],
}

impl MockDist {
    fn new(
        dist: &'static str,
        makefile_extra: &'static str,
        prereqs: &'static str,
        tests: &'static [(&'static str, &'static str)],
    ) -> Self {
        MockDist {
            dist,
            module: dist.replace('-', "::"),
            makefile_extra,
            prereqs,
            tests,
        }
    }

    fn dir_name(&self) -> String {
        format!("{}-1.00", self.dist)
    }

    fn archive_name(&self) -> String {
        format!("{}-1.00.tar.gz", self.dist)
    }

    /// The archive path relative to `authors/id/`, and the `path` column in
    /// `02packages`.
    fn author_rel_path(&self) -> String {
        format!("A/AC/ACME/{}", self.archive_name())
    }

    /// Write the distribution source tree under `parent/<dist>-1.00/`.
    fn write_tree(&self, parent: &Path) {
        let root = parent.join(self.dir_name());
        let leaf = self.dist.rsplit('-').next().unwrap();
        std::fs::create_dir_all(root.join("lib/Acme/UPT")).unwrap();
        if !self.tests.is_empty() {
            std::fs::create_dir_all(root.join("t")).unwrap();
        }

        std::fs::write(
            root.join("Makefile.PL"),
            format!(
                "use ExtUtils::MakeMaker;\nWriteMakefile(\n  NAME => '{}',\n  VERSION => '1.00',\n{}\n);\n",
                self.module, self.makefile_extra
            ),
        )
        .unwrap();

        std::fs::write(
            root.join("META.json"),
            format!(
                "{{\"name\":\"{}\",\"version\":\"1.00\",\"dynamic_config\":0,\
                 \"release_status\":\"stable\",\"meta-spec\":{{\"version\":2}},\
                 \"prereqs\":{}}}",
                self.dist, self.prereqs
            ),
        )
        .unwrap();

        std::fs::write(
            root.join(format!("lib/Acme/UPT/{leaf}.pm")),
            format!("package {};\nour $VERSION = '1.00';\n1;\n", self.module),
        )
        .unwrap();

        for (name, body) in self.tests {
            std::fs::write(root.join("t").join(name), body).unwrap();
        }
    }

    /// `tar czf -` the source tree; returns the gzip bytes.
    fn tarball(&self, parent: &Path) -> Vec<u8> {
        let out = Command::new("tar")
            .args(["-czf", "-", "-C"])
            .arg(parent)
            .arg(self.dir_name())
            .output()
            .expect("run tar");
        assert!(
            out.status.success(),
            "tar failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }
}

/// What the mock mirror serves.
struct Mirror {
    /// `modules/02packages.details.txt.gz` bytes.
    index_gz: Vec<u8>,
    /// archive base name -> tarball bytes.
    archives: HashMap<String, Vec<u8>>,
}

/// Start the mock mirror on `127.0.0.1:0`; returns the bound port. The accept
/// loop runs on a detached thread for the life of the test process.
fn start_server(mirror: Mirror) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();
    let mirror = Arc::new(mirror);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mirror = Arc::clone(&mirror);
            std::thread::spawn(move || handle(&mut stream, &mirror));
        }
    });
    port
}

fn handle(stream: &mut TcpStream, mirror: &Mirror) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 64 * 1024 {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let path = text
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap_or("/");

    if path == "/modules/02packages.details.txt.gz" {
        respond(stream, "200 OK", "application/gzip", &mirror.index_gz);
    } else if let Some(rest) = path.strip_prefix("/authors/id/") {
        let base = rest.rsplit('/').next().unwrap_or("");
        match mirror.archives.get(base) {
            Some(bytes) => respond(stream, "200 OK", "application/gzip", bytes),
            None => respond(stream, "404 Not Found", "text/plain", b"unknown archive"),
        }
    } else {
        respond(stream, "404 Not Found", "text/plain", b"not found");
    }
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

/// The five mock distributions. `Acme-UPT-Top` carries one prerequisite per
/// phase; the leaf dists carry none but the build tool.
fn mock_dists() -> Vec<MockDist> {
    const LEAF_PREREQS: &str = r#"{"configure":{"requires":{"ExtUtils::MakeMaker":"0"}}}"#;
    vec![
        MockDist::new(
            "Acme-UPT-Top",
            "  CONFIGURE_REQUIRES => { 'Acme::UPT::Configure' => 0 },\n  \
             BUILD_REQUIRES => { 'Acme::UPT::Build' => 0 },\n  \
             TEST_REQUIRES => { 'Acme::UPT::Test' => 0 },\n  \
             PREREQ_PM => { 'Acme::UPT::Runtime' => 0 },",
            r#"{
                "configure":{"requires":{"ExtUtils::MakeMaker":"0","Acme::UPT::Configure":"0"}},
                "build":{"requires":{"Acme::UPT::Build":"0"}},
                "test":{"requires":{"Acme::UPT::Test":"0"}},
                "runtime":{"requires":{"Acme::UPT::Runtime":"0"}}
            }"#,
            &[(
                "00-load.t",
                "use strict; use warnings;\n\
                 use Test::More tests => 2;\n\
                 use_ok('Acme::UPT::Runtime');\n\
                 use_ok('Acme::UPT::Test');\n",
            )],
        ),
        MockDist::new("Acme-UPT-Configure", "", LEAF_PREREQS, &[]),
        MockDist::new("Acme-UPT-Build", "", LEAF_PREREQS, &[]),
        MockDist::new("Acme-UPT-Test", "", LEAF_PREREQS, &[]),
        MockDist::new("Acme-UPT-Runtime", "", LEAF_PREREQS, &[]),
    ]
}

/// Build the `upt cpan install Acme::UPT::Top` command against `mirror_base`,
/// with a fresh install base / cache / config for `tag`. Returns
/// `(cmd, install_base, cpan_cache_dir)`.
fn build_cmd(
    root: &Path,
    tag: &str,
    mirror_base: &str,
    no_test: bool,
    source: &str,
) -> (Command, PathBuf, PathBuf) {
    let install_base = root.join(format!("install-{tag}"));
    let cache_home = root.join(format!("cache-{tag}"));
    let home = root.join(format!("home-{tag}"));
    std::fs::create_dir_all(&cache_home).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let config = root.join(format!("config-{tag}.toml"));
    std::fs::write(
        &config,
        format!(
            "[perl.itest]\n\
             perl = \"perl\"\n\
             make = \"make\"\n\
             install-base = {:?}\n\
             lib = [{:?}]\n",
            install_base,
            install_base.join("lib/perl5"),
        ),
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_upt"));
    cmd.args(["--config"]).arg(&config).args([
        "cpan",
        "install",
        "--perl",
        "itest",
        "--source",
        source,
        // A dead host: mirror mode must not touch MetaCPAN.
        "--metacpan-base-url",
        "http://127.0.0.1:1/",
        "--mirror-base-url",
        mirror_base,
    ]);
    if no_test {
        cmd.arg("--no-test");
    }
    cmd.arg("Acme::UPT::Top");

    cmd.env("XDG_CACHE_HOME", &cache_home)
        .env("HOME", &home)
        .env_remove("PERL5LIB")
        .env_remove("PERLLIB")
        .env_remove("PERL_MM_OPT")
        .env_remove("PERL_MB_OPT")
        .env_remove("PERL_LOCAL_LIB_ROOT");

    (cmd, install_base, cache_home.join("upt/cpan"))
}

/// Run a `--source mirror` install and assert it succeeded; returns
/// `(stdout, install_base, cpan_cache_dir)`.
fn run_install(
    root: &Path,
    tag: &str,
    mirror_base: &str,
    no_test: bool,
) -> (String, PathBuf, PathBuf) {
    let (mut cmd, install_base, cpan_cache) = build_cmd(root, tag, mirror_base, no_test, "mirror");
    let out = cmd.output().expect("run upt cpan install");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`upt cpan install` (no_test={no_test}) failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    (stdout, install_base, cpan_cache)
}

/// Lay out a CPAN mirror on disk under `dir`: `modules/02packages.details.txt.gz`
/// plus every dist's tarball under `authors/id/`.
fn write_disk_mirror(dir: &Path, src: &Path, dists: &[MockDist]) {
    std::fs::create_dir_all(dir.join("modules")).unwrap();
    let mut index = PackageDetails::new();
    for dist in dists {
        dist.write_tree(src);
        let archive = dir.join("authors/id").join(dist.author_rel_path());
        std::fs::create_dir_all(archive.parent().unwrap()).unwrap();
        std::fs::write(&archive, dist.tarball(src)).unwrap();
        index
            .add_entry(Entry::new(
                dist.module.clone(),
                Some("1.00".to_string()),
                dist.author_rel_path(),
            ))
            .unwrap();
    }
    std::fs::write(
        dir.join("modules/02packages.details.txt.gz"),
        index.to_gz_bytes().unwrap(),
    )
    .unwrap();
}

fn installed_module(install_base: &Path, module_path: &str) -> bool {
    install_base.join("lib/perl5").join(module_path).is_file()
}

/// A throwaway anonymous FTP server: greeting, `USER`/`PASS`/`TYPE`, `PASV`,
/// `RETR <path>` from an in-memory map, `QUIT`. Just enough for `fetch_ftp`.
fn start_ftp_server(files: HashMap<String, Vec<u8>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ftp");
    let port = listener.local_addr().unwrap().port();
    let files = Arc::new(files);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let files = Arc::clone(&files);
            std::thread::spawn(move || ftp_session(stream, &files));
        }
    });
    port
}

fn ftp_session(mut ctrl: TcpStream, files: &HashMap<String, Vec<u8>>) {
    let mut reader = BufReader::new(ctrl.try_clone().unwrap());
    let _ = ctrl.write_all(b"220 mock ftp\r\n");
    let mut data_listener: Option<TcpListener> = None;

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end();
        let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
        match cmd.to_ascii_uppercase().as_str() {
            "USER" => {
                let _ = ctrl.write_all(b"331 need password\r\n");
            }
            "PASS" => {
                let _ = ctrl.write_all(b"230 logged in\r\n");
            }
            "TYPE" => {
                let _ = ctrl.write_all(b"200 ok\r\n");
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                let p = l.local_addr().unwrap().port();
                data_listener = Some(l);
                let _ = ctrl.write_all(
                    format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                        p >> 8,
                        p & 0xff
                    )
                    .as_bytes(),
                );
            }
            "RETR" => match files.get(arg) {
                Some(bytes) => {
                    let _ = ctrl.write_all(b"150 opening data connection\r\n");
                    if let Some(l) = data_listener.take()
                        && let Ok((mut d, _)) = l.accept()
                    {
                        let _ = d.write_all(bytes);
                        let _ = d.shutdown(Shutdown::Both);
                    }
                    let _ = ctrl.write_all(b"226 transfer complete\r\n");
                }
                None => {
                    let _ = ctrl.write_all(b"550 no such file\r\n");
                }
            },
            "QUIT" => {
                let _ = ctrl.write_all(b"221 bye\r\n");
                break;
            }
            other => {
                eprintln!("mock ftp: unhandled command {other:?}");
                let _ = ctrl.write_all(b"200 ok\r\n");
            }
        }
    }
}

#[test]
fn mirror_mode_installs_phase_deps_and_respects_no_test() {
    for tool in ["perl", "make", "tar"] {
        if !tool_available(tool) {
            eprintln!("skipping cpan mirror integration test: `{tool}` not on PATH");
            return;
        }
    }

    let tmp = TempDir::new("mirror");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // Build every mock dist tree + tarball, and a `02packages` index pointing at
    // each one under `authors/id/A/AC/ACME/`.
    let dists = mock_dists();
    let mut archives: HashMap<String, Vec<u8>> = HashMap::new();
    let mut index = PackageDetails::new();
    for dist in &dists {
        dist.write_tree(&src);
        archives.insert(dist.archive_name(), dist.tarball(&src));
        index
            .add_entry(Entry::new(
                dist.module.clone(),
                Some("1.00".to_string()),
                dist.author_rel_path(),
            ))
            .unwrap();
    }
    let mirror = Mirror {
        index_gz: index.to_gz_bytes().unwrap(),
        archives,
    };
    let base = format!("http://127.0.0.1:{}/", start_server(mirror));

    // --- sanity: `--source metacpan` points at a dead host and cannot resolve
    // anything, so only mirror mode (the `02packages` index) makes this work.
    let (mut cmd, no_install, _) = build_cmd(tmp.path(), "metacpan", &base, true, "metacpan");
    let out = cmd.output().expect("run upt cpan install (metacpan mode)");
    assert!(
        !out.status.success(),
        "`--source metacpan` should fail against the dead MetaCPAN host"
    );
    assert!(
        !installed_module(&no_install, "Acme/UPT/Top.pm"),
        "nothing is installed when resolution fails"
    );

    // --- with --no-test: the test-phase prerequisite must NOT be installed.
    let (stdout, install, cpan_cache) = run_install(tmp.path(), "notest", &base, true);

    assert!(
        installed_module(&install, "Acme/UPT/Top.pm"),
        "Top itself was installed\n{stdout}"
    );
    for phase_mod in ["Configure", "Build", "Runtime"] {
        assert!(
            installed_module(&install, &format!("Acme/UPT/{phase_mod}.pm")),
            "{phase_mod}-phase prerequisite installed (--no-test)\n{stdout}"
        );
    }
    assert!(
        !installed_module(&install, "Acme/UPT/Test.pm"),
        "test-phase prerequisite must be skipped with --no-test\n{stdout}"
    );
    assert!(
        stdout.contains("Acme-UPT-Top-1.00  install  ok"),
        "per-step summary line on stdout\n{stdout}"
    );
    assert!(
        !stdout.contains("Acme-UPT-Test-1.00  install  ok"),
        "the test dist must not be installed with --no-test\n{stdout}"
    );

    // The run directory carries the per-step JSON, with the same body
    // `upt dist <step> --json` produces.
    let run_dir = std::fs::read_dir(&cpan_cache)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("a run directory was created");
    for step in ["pre-configure", "configure", "build", "install"] {
        let json = run_dir.join(format!("Acme-UPT-Top-1.00.{step}.json"));
        let text = std::fs::read_to_string(&json)
            .unwrap_or_else(|e| panic!("read {}: {e}", json.display()));
        assert!(
            text.contains("\"success\": true"),
            "{step}.json records success\n{text}"
        );
    }
    assert!(
        !run_dir.join("Acme-UPT-Top-1.00.test.json").exists(),
        "no test.json when the test step is skipped"
    );

    // --- without --no-test: the test-phase prerequisite IS installed.
    let (stdout, install, _) = run_install(tmp.path(), "full", &base, false);

    for phase_mod in ["Configure", "Build", "Runtime", "Test"] {
        assert!(
            installed_module(&install, &format!("Acme/UPT/{phase_mod}.pm")),
            "{phase_mod}-phase prerequisite installed (full run)\n{stdout}"
        );
    }
    assert!(
        stdout.contains("Acme-UPT-Test-1.00  install  ok"),
        "the test dist is installed on a full run\n{stdout}"
    );
    assert!(
        stdout.contains("Acme-UPT-Top-1.00  test  ok"),
        "Top's own test step runs on a full run\n{stdout}"
    );
}

#[test]
fn mirror_mode_works_against_a_file_url() {
    for tool in ["perl", "make", "tar"] {
        if !tool_available(tool) {
            eprintln!("skipping cpan file-url integration test: `{tool}` not on PATH");
            return;
        }
    }

    let tmp = TempDir::new("fileurl");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // A CPAN mirror on local disk, reached with no HTTP server at all.
    let mirror_dir = tmp.path().join("mirror");
    write_disk_mirror(&mirror_dir, &src, &mock_dists());
    let base = format!("file://{}", mirror_dir.display());

    let (stdout, install, _) = run_install(tmp.path(), "fileurl", &base, true);

    for phase_mod in ["Top", "Configure", "Build", "Runtime"] {
        assert!(
            installed_module(&install, &format!("Acme/UPT/{phase_mod}.pm")),
            "{phase_mod} installed from the file:// mirror\n{stdout}"
        );
    }
    assert!(
        !installed_module(&install, "Acme/UPT/Test.pm"),
        "test-phase prerequisite skipped with --no-test\n{stdout}"
    );
    assert!(
        stdout.contains("Acme-UPT-Top-1.00  install  ok"),
        "per-step summary line on stdout\n{stdout}"
    );
}

#[test]
fn mirror_mode_works_against_an_ftp_url() {
    for tool in ["perl", "make", "tar"] {
        if !tool_available(tool) {
            eprintln!("skipping cpan ftp integration test: `{tool}` not on PATH");
            return;
        }
    }

    let tmp = TempDir::new("ftp");
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();

    // The mirror lives entirely in the mock FTP server: RETR paths -> bytes.
    let dists = mock_dists();
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let mut index = PackageDetails::new();
    for dist in &dists {
        dist.write_tree(&src);
        files.insert(
            format!("/authors/id/{}", dist.author_rel_path()),
            dist.tarball(&src),
        );
        index
            .add_entry(Entry::new(
                dist.module.clone(),
                Some("1.00".to_string()),
                dist.author_rel_path(),
            ))
            .unwrap();
    }
    files.insert(
        "/modules/02packages.details.txt.gz".to_string(),
        index.to_gz_bytes().unwrap(),
    );

    let base = format!("ftp://127.0.0.1:{}", start_ftp_server(files));
    let (stdout, install, _) = run_install(tmp.path(), "ftp", &base, true);

    for phase_mod in ["Top", "Configure", "Build", "Runtime"] {
        assert!(
            installed_module(&install, &format!("Acme/UPT/{phase_mod}.pm")),
            "{phase_mod} installed from the ftp:// mirror\n{stdout}"
        );
    }
    assert!(
        !installed_module(&install, "Acme/UPT/Test.pm"),
        "test-phase prerequisite skipped with --no-test\n{stdout}"
    );
    assert!(
        stdout.contains("Acme-UPT-Top-1.00  install  ok"),
        "per-step summary line on stdout\n{stdout}"
    );
}
