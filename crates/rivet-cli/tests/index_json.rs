//! Integration tests for the first real `rivet index --json` path (T10).
//!
//! These drive the built binary through `std::process::Command` so argument
//! parsing, exit codes, and the stdout/stderr split are exercised end to end.
//! They use a small hand-built repository rather than the workspace fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named temporary directory removed when dropped.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> TempDir {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rivet-cli-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// A temporary directory that is a Git root (has `.git` but no `.rivet/`).
fn git_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// Writes `contents` at `rel` under `root`, creating parent directories.
fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write fixture");
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// The stdout/exit-key order of a success object, parsed from `serde_json`.
fn parse_stdout(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// The populated five-file fixture from the T10 test plan: two readable `.php`
/// files, one unsupported `.md`, one binary `.php`, and one oversize `.php`.
fn populated_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    let root = temp.path();
    fs::create_dir_all(root.join(".rivet")).expect("create .rivet");
    fs::write(
        root.join(".rivet/config.toml"),
        "[index]\nmax_file_size_kb = 1\n",
    )
    .expect("write config");
    write(root, "a.php", b"<?php\necho 1;\n");
    write(root, "b.php", b"<?php\nfunction f() {}\n");
    write(root, "note.md", b"# readme\n");
    write(root, "bin.php", b"<?php\n\0binary\n");
    write(
        root,
        "big.php",
        format!("<?php\n{}\n", "x".repeat(2000)).as_bytes(),
    );
    temp
}

#[test]
fn index_json_reports_inventory_and_is_stable() {
    let temp = populated_repo("inventory");
    let root = temp.path();

    let first = run(root, &["index", "--json"]);
    assert!(
        first.stderr.is_empty(),
        "success must not write stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let value = parse_stdout(&first);

    // `schema_version` is the first key.
    let keys: Vec<&str> = value
        .as_object()
        .expect("top level is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.first().copied(), Some("schema_version"), "{keys:?}");
    assert_eq!(value["schema_version"], 1);

    assert_eq!(value["index"]["coverage"]["files_seen"], 5);
    assert_eq!(value["index"]["coverage"]["files_indexed"], 2);
    assert_eq!(value["index"]["coverage"]["skipped"]["unsupported"], 1);
    assert_eq!(value["index"]["coverage"]["skipped"]["binary"], 1);
    assert_eq!(value["index"]["coverage"]["skipped"]["size"], 1);
    assert_eq!(value["index"]["coverage"]["complete"], false);
    assert_eq!(value["index"]["diagnostics"]["total"], 2);
    assert_eq!(value["index"]["diagnostics"]["truncated"], false);
    // b.php declares one function; T12 extracts and persists it.
    assert_eq!(value["symbols"], 1);
    assert_eq!(value["uses"], 0);
    assert_eq!(value["bindings"], 0);
    assert_eq!(value["updated"], 5);
    assert_eq!(value["unchanged"], 0);
    assert!(value.get("elapsed_ms").is_none(), "no --timing: {value}");
    let snapshot = value["index"]["snapshot"].clone();
    assert!(
        snapshot.as_str().unwrap().starts_with("blake3:"),
        "{snapshot}"
    );

    // A second run changes nothing and reuses the same snapshot.
    let second = run(root, &["index", "--json"]);
    let value = parse_stdout(&second);
    assert_eq!(value["updated"], 0);
    assert_eq!(value["unchanged"], 5);
    assert_eq!(value["index"]["snapshot"], snapshot);

    // `--timing` adds `elapsed_ms`.
    let timed = run(root, &["index", "--timing", "--json"]);
    let value = parse_stdout(&timed);
    assert!(value.get("elapsed_ms").is_some(), "{value}");
}

#[test]
fn unimplemented_command_emits_json_error() {
    let temp = git_repo("unimplemented");
    let output = run(temp.path(), &["refs", "Foo", "--json"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let value: Value = serde_json::from_slice(&output.stderr).expect("stderr is JSON");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["error"], "general");
    assert!(
        value["message"]
            .as_str()
            .unwrap()
            .contains("not implemented"),
        "{value}"
    );
}

#[test]
fn missing_repository_is_repository_unavailable() {
    // No `.git` and no `.rivet` anywhere above the working directory.
    let temp = TempDir::new("no-repo");
    let output = run(temp.path(), &["index", "--json"]);

    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let value: Value = serde_json::from_slice(&output.stderr).expect("stderr is JSON");
    assert_eq!(value["error"], "repository_unavailable");
}

#[test]
fn uncompiled_language_is_invalid_arguments() {
    let temp = git_repo("bad-language");
    let output = run(temp.path(), &["index", "--languages", "cobol", "--json"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let value: Value = serde_json::from_slice(&output.stderr).expect("stderr is JSON");
    assert_eq!(value["error"], "invalid_arguments");
    assert!(
        value["message"].as_str().unwrap().contains("cobol"),
        "{value}"
    );
}

#[test]
fn metadata_freshness_is_recorded() {
    let temp = git_repo("metadata");
    write(temp.path(), "a.php", b"<?php echo 1;\n");

    let output = run(temp.path(), &["index", "--freshness", "metadata", "--json"]);
    let value = parse_stdout(&output);
    assert_eq!(value["index"]["freshness"], "metadata");
}

#[test]
fn creates_rivet_dir_at_git_root_without_touching_gitignore() {
    let temp = git_repo("auto-create");
    write(temp.path(), "a.php", b"<?php echo 1;\n");

    let output = run(temp.path(), &["index", "--json"]);
    let _ = parse_stdout(&output);

    assert!(temp.path().join(".rivet").is_dir(), ".rivet was created");
    assert!(
        !temp.path().join(".gitignore").exists(),
        "index must never edit .gitignore"
    );
}

#[test]
fn malformed_php_is_a_parse_error_without_symbols() {
    let temp = git_repo("parse-error");
    write(
        temp.path(),
        "broken.php",
        b"<?php\nclass Broken {\n  public function oops( {\n}",
    );

    let output = run(temp.path(), &["index", "--json"]);
    let value = parse_stdout(&output);
    assert_eq!(value["index"]["coverage"]["skipped"]["parse_error"], 1);
    assert_eq!(value["index"]["coverage"]["files_indexed"], 0);
    assert_eq!(value["index"]["coverage"]["complete"], false);
    assert_eq!(value["symbols"], 0);
    assert_eq!(value["index"]["diagnostics"]["total"], 1);
    assert_eq!(
        value["index"]["diagnostics"]["items"][0]["code"],
        "parse_error"
    );
    // No symbols means a lookup cannot find anything in the failed file.
    let lookup = run(temp.path(), &["symbol", "Broken", "--json"]);
    assert_eq!(lookup.status.code(), Some(4));
}
