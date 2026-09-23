//! Shared helpers for the T36 acceptance-matrix regression suites.
//!
//! Every helper drives the built binary against a temporary directory the test
//! creates. Each run is guarded by a deadline so a regression that blocks (for
//! example on a FIFO) fails the test instead of hanging it. The stream
//! assertions are strict: a success is exactly one compact JSON object and one
//! LF on stdout with nothing on stderr, and a failure is the mirror image on
//! stderr (spec §20.1; OUTPUT-CONTRACT "Transport and common rules").

#![allow(dead_code)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
pub const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// The longest any single `rivet` run may take before the test fails.
pub const RUN_DEADLINE: Duration = Duration::from_secs(120);

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named temporary directory removed when dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> TempDir {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rivet-t36-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        // Canonical form, so paths compared against rivet's output agree even
        // where the temporary directory sits behind a symlink (macOS `/var`).
        let path = fs::canonicalize(&path).expect("canonicalize temporary directory");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        restore_permissions(&self.path);
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Makes every directory under `path` writable again so cleanup can succeed
/// after a test that removed permissions.
#[cfg(unix)]
fn restore_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                restore_permissions(&entry.path());
            }
        }
    }
}

#[cfg(not(unix))]
fn restore_permissions(_path: &Path) {}

/// A temporary Git root (a bare `.git` directory) with no `.rivet/` yet.
pub fn git_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// The authored PHP fixture directory.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored")
}

/// Copies the authored fixture's files into `root`.
pub fn copy_fixture(root: &Path) {
    for entry in fs::read_dir(fixture_dir()).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), root.join(entry.file_name())).expect("copy fixture file");
        }
    }
}

/// A temporary Git root holding a byte-for-byte copy of the authored fixture.
pub fn fixture_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    copy_fixture(temp.path());
    temp
}

/// Writes `contents` at `rel` under `root`, creating parent directories.
pub fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

/// Runs the binary in `dir` under [`RUN_DEADLINE`].
pub fn run(dir: &Path, args: &[&str]) -> Output {
    run_env(dir, args, &[])
}

/// Runs the binary in `dir` with extra environment, under [`RUN_DEADLINE`].
///
/// The debug limit overrides are never inherited from the environment running
/// the tests.
pub fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(RIVET);
    command
        .args(args)
        .current_dir(dir)
        .env_remove("RIVET_DEBUG_MAX_NODES")
        .env_remove("RIVET_DEBUG_MAX_USES");
    for (key, value) in env {
        command.env(key, value);
    }
    run_guarded(command, RUN_DEADLINE, &format!("rivet {args:?}"))
}

/// Runs `command` to completion, killing it and failing the test if it is
/// still running after `deadline`.
pub fn run_guarded(mut command: Command, deadline: Duration, label: &str) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn process");
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let out_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let err_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll process") {
            break status;
        }
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{label} did not finish within {deadline:?}; it was killed");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: out_reader.join().expect("stdout reader"),
        stderr: err_reader.join().expect("stderr reader"),
    }
}

/// Asserts `bytes` are exactly one compact JSON object followed by one LF and
/// returns it. Re-serializing (key order is preserved) must reproduce the
/// bytes, which rules out insignificant whitespace and a second object.
pub fn one_json_line(bytes: &[u8], stream: &str) -> Value {
    let text = std::str::from_utf8(bytes).unwrap_or_else(|_| panic!("{stream} is not UTF-8"));
    assert!(
        text.ends_with('\n'),
        "{stream} must end with one LF: {text:?}"
    );
    let body = &text[..text.len() - 1];
    assert!(
        !body.contains('\n'),
        "{stream} must hold exactly one line: {text:?}"
    );
    let value: Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("{stream} is not one JSON value ({error}): {text:?}"));
    assert!(value.is_object(), "{stream} must be an object: {text:?}");
    assert_eq!(
        serde_json::to_string(&value).expect("serialize"),
        body,
        "{stream} must be compact JSON with no insignificant whitespace"
    );
    assert_eq!(value["schema_version"], 1, "{stream}: {text:?}");
    value
}

/// A strict success: exit 0, nothing on stderr, one JSON object on stdout.
pub fn success(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "success must not write stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    one_json_line(&output.stdout, "stdout")
}

/// A strict failure: exit `exit`, nothing on stdout, one JSON error on stderr
/// carrying the four common fields.
pub fn failure(output: &Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "failure must leave stdout empty: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value = one_json_line(&output.stderr, "stderr");
    for key in ["error", "message", "hint"] {
        assert!(value[key].is_string(), "missing {key}: {value}");
    }
    value
}

/// The top-level keys of a JSON object, in order.
pub fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

/// Runs `git` in `dir` isolated from the user's and system configuration.
///
/// `HOME` points at `home`, so no global config, hooks, or templates apply,
/// and identity/branch settings are passed per invocation. Nothing outside the
/// temporary directories is read or written.
pub fn git(dir: &Path, home: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "user.name=rivet-test",
            "-c",
            "user.email=rivet-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    let output = run_guarded(command, RUN_DEADLINE, &format!("git {args:?}"));
    assert!(
        output.status.success(),
        "git {args:?} failed (Git is a documented build prerequisite): {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The reference spans `(file, start_byte, end_byte, ref_kind, resolution,
/// resolved_target)` of a refs response.
pub fn reference_rows(value: &Value) -> Vec<(String, u64, u64, String, String, Value)> {
    value["references"]
        .as_array()
        .expect("references array")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().expect("file").to_string(),
                item["start_byte"].as_u64().expect("start_byte"),
                item["end_byte"].as_u64().expect("end_byte"),
                item["ref_kind"].as_str().expect("ref_kind").to_string(),
                item["resolution"].as_str().expect("resolution").to_string(),
                item["resolved_target"].clone(),
            )
        })
        .collect()
}
