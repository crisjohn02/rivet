//! Integration tests for T16 freshness modes and invalidation.
//!
//! They drive the built binary against temporary copies of the authored PHP
//! fixture so effective-config/language invalidation, `--freshness metadata`,
//! `--no-refresh` cached reads, and `--force` rebuilds are exercised end to end.

use std::fs::{self, OpenOptions};
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
            "rivet-freshness-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root with no `.rivet/` yet.
fn git_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// A temporary Git root holding a byte-for-byte copy of the authored fixture.
fn fixture_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    for entry in fs::read_dir(&source).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), temp.path().join(entry.file_name())).expect("copy fixture file");
        }
    }
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

/// Parses a success object, requiring exit 0 and empty stderr.
fn parse_success(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "success must not write stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Parses an error object, requiring `exit`, empty stdout, and JSON stderr.
fn parse_error(output: &Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "stdout must stay empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON object")
}

const BOOT_LAUNCH: &str = "App\\Boot\\launch";
const LABEL: &str = "App\\Services\\SurveyService::$label";

#[test]
fn config_invalidation_excludes_and_restores() {
    let temp = fixture_repo("config-invalidation");
    let root = temp.path();

    let before = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(before["index"]["coverage"]["files_seen"], 10);
    let found = parse_success(&run(root, &["symbol", BOOT_LAUNCH, "--json"]));
    assert_eq!(found["symbol"]["qualified_name"], "App\\Boot\\launch");

    // Adding an exclusion changes the effective-config fingerprint, so the next
    // refresh must drop boot.php's facts and lose the symbol.
    write(
        root,
        ".rivet/config.toml",
        b"[index]\nexclude = [\"boot.php\"]\n",
    );

    let excluded = parse_error(&run(root, &["symbol", BOOT_LAUNCH, "--json"]), 4);
    assert_eq!(excluded["error"], "symbol_not_found");

    let after = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(after["index"]["coverage"]["files_seen"], 9);

    // Removing the exclusion makes boot.php eligible again and it is reparsed.
    fs::remove_file(root.join(".rivet/config.toml")).expect("remove config");
    let restored = parse_success(&run(root, &["symbol", BOOT_LAUNCH, "--json"]));
    assert_eq!(restored["symbol"]["qualified_name"], "App\\Boot\\launch");
}

#[test]
fn language_invalidation_disables_php() {
    let temp = fixture_repo("language-invalidation");
    let root = temp.path();

    let before = parse_success(&run(root, &["index", "--json"]));
    // Only README.md is unsupported in the baseline.
    assert_eq!(before["index"]["coverage"]["skipped"]["unsupported"], 1);
    assert!(
        before["symbols"].as_u64().unwrap() > 0,
        "baseline must extract PHP symbols: {before}"
    );

    // Enabling only TypeScript makes every PHP file unsupported and drops its
    // facts; README.md was already unsupported.
    write(
        root,
        ".rivet/config.toml",
        b"[languages]\nenabled = [\"typescript\"]\n",
    );

    let missing = parse_error(&run(root, &["symbol", BOOT_LAUNCH, "--json"]), 4);
    assert_eq!(missing["error"], "symbol_not_found");

    let after = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(after["index"]["coverage"]["skipped"]["unsupported"], 10);
    assert_eq!(after["index"]["coverage"]["files_indexed"], 0);
    assert_eq!(after["symbols"], 0);

    fs::remove_file(root.join(".rivet/config.toml")).expect("remove config");
    let restored = parse_success(&run(root, &["symbol", BOOT_LAUNCH, "--json"]));
    assert_eq!(restored["symbol"]["qualified_name"], "App\\Boot\\launch");
}

#[test]
fn metadata_mode_misses_a_restored_mtime_edit() {
    let temp = fixture_repo("metadata-miss");
    let root = temp.path();
    let path = root.join("SurveyService.php");

    let before = parse_success(&run(root, &["symbol", LABEL, "--source", "--json"]));
    assert_eq!(before["source"], "private string $label = 'survey';");
    let before_hash = before["symbol"]["content_hash"].clone();

    let original_mtime = fs::metadata(&path)
        .expect("stat fixture")
        .modified()
        .expect("mtime");
    let original = fs::read_to_string(&path).expect("read fixture");
    // A same-length byte change: 'survey' -> 'survex'.
    let edited = original.replacen("$label = 'survey'", "$label = 'survex'", 1);
    assert_ne!(edited, original);
    assert_eq!(
        edited.len(),
        original.len(),
        "the edit must not change size"
    );
    fs::write(&path, &edited).expect("write same-size edit");
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("reopen for utime")
        .set_modified(original_mtime)
        .expect("restore mtime");

    // Metadata mode trusts the restored size+mtime and returns the OLD byte.
    let metadata = parse_success(&run(
        root,
        &[
            "symbol",
            LABEL,
            "--source",
            "--freshness",
            "metadata",
            "--json",
        ],
    ));
    assert_eq!(metadata["index"]["freshness"], "metadata");
    assert_eq!(metadata["source"], "private string $label = 'survey';");
    assert_eq!(metadata["symbol"]["content_hash"], before_hash);

    // Default content mode reads and hashes, catching the edit.
    let content = parse_success(&run(root, &["symbol", LABEL, "--source", "--json"]));
    assert_eq!(content["index"]["freshness"], "content");
    assert_eq!(content["source"], "private string $label = 'survex';");
    assert_ne!(content["symbol"]["content_hash"], before_hash);
}

#[test]
fn cached_answers_from_committed_snapshot() {
    let temp = fixture_repo("cached");
    let root = temp.path();

    let indexed = parse_success(&run(root, &["index", "--json"]));
    let snapshot = indexed["index"]["snapshot"].clone();

    // Delete the file on disk: a cached query must still answer from the
    // committed snapshot, while the same query without `--no-refresh` refreshes
    // and fails.
    fs::remove_file(root.join("boot.php")).expect("delete boot.php");

    let cached = parse_success(&run(
        root,
        &["symbol", BOOT_LAUNCH, "--no-refresh", "--json"],
    ));
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["snapshot"], snapshot);
    assert_eq!(cached["symbol"]["qualified_name"], "App\\Boot\\launch");

    let refreshed = parse_error(&run(root, &["symbol", BOOT_LAUNCH, "--json"]), 4);
    assert_eq!(refreshed["error"], "symbol_not_found");
}

#[test]
fn cached_without_index_is_repository_unavailable() {
    // A Git root with no `.rivet/` has no compatible committed snapshot.
    let temp = git_repo("cached-missing");
    write(temp.path(), "a.php", b"<?php echo 1;\n");

    let output = run(temp.path(), &["symbol", "Nope", "--no-refresh", "--json"]);
    let value = parse_error(&output, 3);
    assert_eq!(value["error"], "repository_unavailable");
    assert!(
        value["hint"]
            .as_str()
            .expect("hint")
            .contains("rivet index"),
        "hint must direct the user to build the index: {value}"
    );
}

#[test]
fn no_refresh_flag_validation() {
    let temp = git_repo("flag-validation");
    write(temp.path(), "a.php", b"<?php echo 1;\n");

    // `--no-refresh` conflicts with `--freshness`.
    let conflict = run(
        temp.path(),
        &[
            "symbol",
            "X",
            "--no-refresh",
            "--freshness",
            "metadata",
            "--json",
        ],
    );
    let value = parse_error(&conflict, 2);
    assert_eq!(value["error"], "invalid_arguments");

    // `index` rejects `--no-refresh`.
    let index = run(temp.path(), &["index", "--no-refresh", "--json"]);
    let value = parse_error(&index, 2);
    assert_eq!(value["error"], "invalid_arguments");
}

#[test]
fn force_rebuild_reports_all_updated_and_keeps_digest() {
    let temp = fixture_repo("force");
    let root = temp.path();

    let before = parse_success(&run(root, &["index", "--json"]));
    let files_seen = before["index"]["coverage"]["files_seen"].as_u64().unwrap();
    let snapshot = before["index"]["snapshot"].clone();

    let forced = parse_success(&run(root, &["index", "--force", "--json"]));
    assert_eq!(forced["updated"], files_seen);
    assert_eq!(forced["unchanged"], 0);
    assert_eq!(
        forced["symbols"], before["symbols"],
        "a forced rebuild must republish the extracted symbols"
    );
    assert_eq!(
        forced["index"]["snapshot"], snapshot,
        "a forced rebuild of unchanged content must keep the snapshot digest"
    );

    // The rebuilt facts remain queryable.
    let found = parse_success(&run(root, &["symbol", BOOT_LAUNCH, "--json"]));
    assert_eq!(found["symbol"]["qualified_name"], "App\\Boot\\launch");
}
