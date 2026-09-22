//! Integration tests for changed-content refresh (T15).
//!
//! They drive the built binary against temporary copies of the authored PHP
//! fixture so edits, deletions, renames, parse failures, same-size edits with
//! a restored mtime, and the no-reparse hook are exercised end to end through
//! the real query path.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use rivet_store::{Fingerprint, INDEX_FORMAT_VERSION, InventoryInput, Store};

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
            "rivet-refresh-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root holding a byte-for-byte copy of the authored fixture.
fn fixture_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    for entry in fs::read_dir(&source).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), temp.path().join(entry.file_name())).expect("copy fixture file");
        }
    }
    temp
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs the binary with extra environment variables.
fn run_with_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(RIVET);
    command.args(args).current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run the rivet binary")
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

/// Opens the committed cache created by a successful `index`.
fn open_store(root: &Path) -> Store {
    Store::open(&root.join(".rivet")).expect("open committed store")
}

const LAUNCH: &str = "App\\Services\\SurveyService::launch";
const BOOT_LAUNCH: &str = "App\\Boot\\launch";
const LABEL: &str = "App\\Services\\SurveyService::$label";

/// Appends a fresh `newMethod` to `SurveyService.php` inside the class body.
fn append_new_method(path: &Path) -> String {
    let original = fs::read_to_string(path).expect("read fixture");
    let marker = "    public function relaunch(): void\n    {\n        $this->launch();\n    }\n}";
    let replacement = "    public function relaunch(): void\n    {\n        $this->launch();\n    }\n\n    public function newMethod(): void\n    {\n    }\n}";
    assert!(original.contains(marker), "fixture shape changed");
    let edited = original.replacen(marker, replacement, 1);
    fs::write(path, &edited).expect("write edited fixture");
    edited
}

#[test]
fn edit_is_picked_up_by_the_next_query_without_explicit_index() {
    let temp = fixture_repo("edit");
    let path = temp.path().join("SurveyService.php");

    let before = run(temp.path(), &["symbol", LAUNCH, "--json"]);
    let before = parse_success(&before);
    let before_snapshot = before["index"]["snapshot"].clone();
    assert!(before_snapshot.is_string(), "{before}");

    append_new_method(&path);

    // No explicit `rivet index`: the query refreshes first.
    let after = run(
        temp.path(),
        &[
            "symbol",
            "App\\Services\\SurveyService::newMethod",
            "--json",
        ],
    );
    let after = parse_success(&after);
    assert_eq!(
        after["symbol"]["qualified_name"],
        "App\\Services\\SurveyService::newMethod"
    );
    assert_ne!(
        after["index"]["snapshot"], before_snapshot,
        "the committed snapshot must change after the edit"
    );
}

#[test]
fn deleted_file_loses_its_symbols() {
    let temp = fixture_repo("delete");
    let before = run(temp.path(), &["symbol", BOOT_LAUNCH, "--json"]);
    let _ = parse_success(&before);

    fs::remove_file(temp.path().join("boot.php")).expect("delete boot.php");

    let after = run(temp.path(), &["symbol", BOOT_LAUNCH, "--json"]);
    let value = parse_error(&after, 4);
    assert_eq!(value["error"], "symbol_not_found");
}

#[test]
fn renamed_file_moves_its_canonical_id() {
    let temp = fixture_repo("rename");
    fs::rename(
        temp.path().join("ReportService.php"),
        temp.path().join("Reports.php"),
    )
    .expect("rename fixture");

    let old = run(
        temp.path(),
        &[
            "symbol",
            "ReportService.php#App\\Reporting\\ReportService::launch",
            "--json",
        ],
    );
    let value = parse_error(&old, 4);
    assert_eq!(value["error"], "symbol_not_found");

    let new = run(
        temp.path(),
        &[
            "symbol",
            "Reports.php#App\\Reporting\\ReportService::launch",
            "--json",
        ],
    );
    let new = parse_success(&new);
    assert_eq!(new["symbol"]["file"], "Reports.php");
    assert_eq!(
        new["symbol"]["id"],
        "Reports.php#App\\Reporting\\ReportService::launch"
    );
}

#[test]
fn valid_to_malformed_clears_symbols_and_back() {
    let temp = fixture_repo("malformed");
    let path = temp.path().join("boot.php");
    let original = fs::read(&path).expect("read boot.php");

    // Valid first, so the store holds the function.
    let before = run(temp.path(), &["symbol", BOOT_LAUNCH, "--json"]);
    let _ = parse_success(&before);

    fs::write(
        &path,
        b"<?php\nclass Broken {\n  public function oops( {\n}",
    )
    .expect("write malformed boot.php");

    let lookup = run(temp.path(), &["symbol", BOOT_LAUNCH, "--json"]);
    let value = parse_error(&lookup, 4);
    assert_eq!(value["error"], "symbol_not_found");

    // The malformed file is recorded with a diagnostic naming it.
    let index = run(temp.path(), &["index", "--json"]);
    let index = parse_success(&index);
    assert_eq!(index["index"]["coverage"]["skipped"]["parse_error"], 1);
    let items = index["index"]["diagnostics"]["items"]
        .as_array()
        .expect("diagnostics items");
    assert!(
        items
            .iter()
            .any(|item| { item["file"] == "boot.php" && item["code"] == "parse_error" }),
        "no boot.php parse_error diagnostic in {index}"
    );

    // Restoring the file brings the symbol back.
    fs::write(&path, &original).expect("restore boot.php");
    let restored = run(temp.path(), &["symbol", BOOT_LAUNCH, "--json"]);
    let restored = parse_success(&restored);
    assert_eq!(restored["symbol"]["qualified_name"], "App\\Boot\\launch");
}

#[test]
fn same_size_edit_with_restored_mtime_is_detected() {
    let temp = fixture_repo("same-size");
    let path = temp.path().join("SurveyService.php");

    let before = run(temp.path(), &["symbol", LABEL, "--source", "--json"]);
    let before = parse_success(&before);
    let before_hash = before["symbol"]["content_hash"].clone();
    assert_eq!(before["source"], "private string $label = 'survey';");

    let original_mtime = fs::metadata(&path)
        .expect("stat fixture")
        .modified()
        .expect("mtime");

    // A same-length byte change: 'survey' -> 'survex'.
    let original = fs::read_to_string(&path).expect("read fixture");
    let edited = original.replacen("$label = 'survey'", "$label = 'survex'", 1);
    assert_ne!(edited, original);
    assert_eq!(
        edited.len(),
        original.len(),
        "the edit must not change size"
    );
    fs::write(&path, &edited).expect("write same-size edit");

    // Restore the original mtime so only content hashing can detect the edit.
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("reopen for utime")
        .set_modified(original_mtime)
        .expect("restore mtime");
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        original_mtime
    );

    let after = run(temp.path(), &["symbol", LABEL, "--source", "--json"]);
    let after = parse_success(&after);
    assert_eq!(after["source"], "private string $label = 'survex';");
    assert_ne!(
        after["symbol"]["content_hash"], before_hash,
        "content hashing must detect a same-size edit with a restored mtime"
    );
}

#[test]
#[cfg(debug_assertions)]
fn no_reparse_hook_proves_equal_content_is_not_reparsed() {
    let temp = fixture_repo("reparse");
    let log_dir = TempDir::new("reparse-log");
    let log = log_dir.path().join("reparsed.txt");
    fs::write(&log, b"").expect("create reparse log");

    // The first run parses every enabled-language file from scratch.
    let first = run_with_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log.as_path())],
    );
    let _ = parse_success(&first);
    let first_lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        first_lines.contains(&"SurveyService.php".to_string()),
        "first run must reparse the fixture: {first_lines:?}"
    );

    // A no-change run must reparse nothing.
    fs::write(&log, b"").expect("clear reparse log");
    let second = run_with_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log.as_path())],
    );
    let value = parse_success(&second);
    assert_eq!(value["updated"], 0);
    assert_eq!(value["unchanged"], 10);
    let second_lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        second_lines.is_empty(),
        "unchanged refresh reparsed: {second_lines:?}"
    );

    // Editing exactly one file reparses exactly that file.
    append_new_method(&temp.path().join("SurveyService.php"));
    let third = run_with_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log.as_path())],
    );
    let value = parse_success(&third);
    assert_eq!(value["updated"], 1);
    let third_lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(third_lines, vec!["SurveyService.php".to_string()]);
}

/// Even with no content or membership change and no reparsed file, a refresh
/// recomputes and republishes every binding, so a changed resolver rule (or a
/// stale committed binding set) is picked up without a separate fingerprint
/// trigger.
///
/// The committed snapshot is rewritten to hold the current file/symbol/use/scope
/// rows under a stale resolver fingerprint with an empty `bindings` table. The
/// next no-edit refresh reparses nothing, yet must restore the full binding set
/// and advance the stored resolver fingerprint.
#[test]
fn stale_bindings_are_re_resolved_without_reparsing() {
    let temp = fixture_repo("resolver");
    let root = temp.path();

    let _ = parse_success(&run(root, &["index", "--json"]));
    let reference = {
        let store = open_store(root);
        store.list_bindings().expect("list bindings")
    };
    assert!(
        !reference.is_empty(),
        "the authored fixture must produce bindings"
    );

    // Rewrite the snapshot: same facts, stale resolver fingerprint, no
    // bindings. `publish_inventory` proves the bindings table can hold nothing
    // while the facts remain reusable.
    {
        let mut store = open_store(root);
        let files = store.list_files().expect("list files");
        let symbols = store.list_symbols().expect("list symbols");
        let mut uses = Vec::new();
        let mut scopes = Vec::new();
        for file in &files {
            uses.extend(store.list_uses_for_file(&file.path).expect("list uses"));
            scopes.extend(store.list_scopes_for_file(&file.path).expect("list scopes"));
        }
        let effective_config = store
            .get_meta("effective_config_fingerprint")
            .expect("read meta")
            .expect("effective-config fingerprint present");
        let extractor = store
            .get_meta("extractor_fingerprint")
            .expect("read meta")
            .expect("extractor fingerprint present");
        store
            .publish_inventory(InventoryInput {
                fingerprint: Fingerprint {
                    index_format_version: INDEX_FORMAT_VERSION.to_string(),
                    effective_config,
                    extractor,
                    resolver: "php-rules-v0".to_string(),
                },
                files,
                symbols,
                uses,
                scopes,
                bindings: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .expect("publish a stale binding state");
    }

    // A no-edit refresh must not reparse anything.
    let log_dir = TempDir::new("resolver-log");
    let log = log_dir.path().join("reparsed.txt");
    fs::write(&log, b"").expect("create reparse log");
    let output = run_with_env(
        root,
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log.as_path())],
    );
    let _ = parse_success(&output);
    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        lines.is_empty(),
        "an unchanged refresh must reparse nothing: {lines:?}"
    );

    let store = open_store(root);
    assert_eq!(
        store.list_bindings().expect("list bindings"),
        reference,
        "re-resolution must rebuild the cleared bindings without reparsing"
    );
    assert_ne!(
        store.get_meta("resolver_fingerprint").expect("read meta"),
        Some("php-rules-v0".to_string()),
        "the re-resolving publish must advance the stored resolver fingerprint"
    );
}
