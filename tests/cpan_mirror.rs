//! Integration test for `upt cpan install --source mirror`.
//!
//! A throwaway in-process HTTP server plays both the MetaCPAN API
//! (`/download_url/<module>`) and the CPAN mirror (`/authors/id/...`), serving
//! only `Acme-UPT-*` / `Acme::UPT::*` distributions built on the fly so nothing
//! here can collide with real CPAN modules. The `download_url` responses point
//! their `download_url` field at a dead host, so the run only succeeds if
//! `--source mirror` actually rewrites the URL onto the mirror base.
//!
//! The `Acme-UPT-Top` distribution declares one prerequisite in each of the
//! `configure`, `build`, `test` and `runtime` phases; the test checks that all
//! four land in a throwaway install base, except that the `test`-phase one is
//! installed only when `--no-test` is *not* given.
//!
//! Requires `perl`, `make` and `tar` on `PATH`; skipped otherwise.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Start the mock API+mirror on `127.0.0.1:0`; returns the bound port. The
/// accept loop runs on a detached thread for the life of the test process.
fn start_server(
    download_url_json: HashMap<String, String>,
    archives: HashMap<String, Vec<u8>>,
) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();
    let json = Arc::new(download_url_json);
    let archives = Arc::new(archives);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let json = Arc::clone(&json);
            let archives = Arc::clone(&archives);
            std::thread::spawn(move || handle(&mut stream, &json, &archives));
        }
    });
    port
}

fn handle(
    stream: &mut TcpStream,
    json: &HashMap<String, String>,
    archives: &HashMap<String, Vec<u8>>,
) {
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

    if let Some(rest) = path.strip_prefix("/download_url/") {
        let module = rest.replace("%3A", ":").replace("%3a", ":");
        match json.get(&module) {
            Some(body) => respond(stream, "200 OK", "application/json", body.as_bytes()),
            None => respond(stream, "404 Not Found", "text/plain", b"unknown module"),
        }
    } else if let Some(rest) = path.strip_prefix("/authors/id/") {
        let base = rest.rsplit('/').next().unwrap_or("");
        match archives.get(base) {
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

/// Build the `upt cpan install Acme::UPT::Top` command against the mock, with a
/// fresh install base / cache / config for `tag`. Returns `(cmd, install_base,
/// cpan_cache_dir)`.
fn build_cmd(
    root: &Path,
    tag: &str,
    port: u16,
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

    let base = format!("http://127.0.0.1:{port}/");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_upt"));
    cmd.args(["--config"]).arg(&config).args([
        "cpan",
        "install",
        "--perl",
        "itest",
        "--source",
        source,
        "--metacpan-base-url",
        &base,
        "--mirror-base-url",
        &base,
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

/// Run a successful `--source mirror` install; returns
/// `(stdout, install_base, cpan_cache_dir)`.
fn run_install(root: &Path, tag: &str, port: u16, no_test: bool) -> (String, PathBuf, PathBuf) {
    let (mut cmd, install_base, cpan_cache) = build_cmd(root, tag, port, no_test, "mirror");
    let out = cmd.output().expect("run upt cpan install");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`upt cpan install` (no_test={no_test}) failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    (stdout, install_base, cpan_cache)
}

fn installed_module(install_base: &Path, module_path: &str) -> bool {
    install_base.join("lib/perl5").join(module_path).is_file()
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

    // Build every mock dist tree + tarball, and the matching download_url JSON.
    // `download_url` points at a dead host (port 1); only the mirror rewrite
    // makes the fetch reachable.
    let dists = mock_dists();
    let mut archives: HashMap<String, Vec<u8>> = HashMap::new();
    let mut download_url_json: HashMap<String, String> = HashMap::new();
    for dist in &dists {
        dist.write_tree(&src);
        let bytes = dist.tarball(&src);
        let sha = sha256_hex(&bytes);
        let archive = dist.archive_name();
        download_url_json.insert(
            dist.module.clone(),
            format!(
                "{{\"download_url\":\"http://127.0.0.1:1/authors/id/A/AC/ACME/{archive}\",\
                 \"version\":\"1.00\",\"release\":\"{}-1.00\",\"distribution\":\"{}\",\
                 \"checksum_sha256\":\"{sha}\"}}",
                dist.dist, dist.dist
            ),
        );
        archives.insert(archive, bytes);
    }

    let port = start_server(download_url_json, archives);

    // --- sanity: `--source metacpan` follows the (dead) download_url host and
    // cannot fetch anything, so only the mirror rewrite makes the rest work.
    let (mut cmd, no_install, _) = build_cmd(tmp.path(), "metacpan", port, true, "metacpan");
    let out = cmd.output().expect("run upt cpan install (metacpan mode)");
    assert!(
        !out.status.success(),
        "`--source metacpan` should fail against the dead download_url host"
    );
    assert!(
        !installed_module(&no_install, "Acme/UPT/Top.pm"),
        "nothing is installed when the fetch fails"
    );

    // --- with --no-test: the test-phase prerequisite must NOT be installed.
    let (stdout, install, cpan_cache) = run_install(tmp.path(), "notest", port, true);

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
    let (stdout, install, _) = run_install(tmp.path(), "full", port, false);

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
