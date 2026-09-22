//! T19 integration tests: persisted resolution bindings.
//!
//! The authored PHP fixture is copied into a temporary Git root, indexed with
//! the real binary, and inspected by opening the committed `.rivet/index.db`
//! directly with [`rivet_store::Store::open`]. T19 must:
//!
//! - bind the `new SurveySvc()` alias type use in `ReportService::runAlias` to
//!   `App\Services\SurveyService` as `exact`;
//! - bind the top-level `launch();` in `boot.php` to `App\Boot\launch` as
//!   `exact`;
//! - leave every receiver-based `launch` method call unresolved (T20/T21);
//! - report the real `bindings` count in `index --json`; and
//! - reproduce byte-identical binding rows on a no-edit re-index.
//!
//! A separate temporary fixture checks the `use function` alias rule and the
//! PHP namespaced-to-global fallback.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_store::{BindingRow, Store, UseRow};
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
            "rivet-bindings-{label}-{}-{nanos}-{unique}",
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

/// The `index --json` object, requiring a clean exit.
fn index_json(dir: &Path) -> Value {
    let output = run(dir, &["index", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Opens the committed cache created by a successful `index`.
fn open_store(root: &Path) -> Store {
    Store::open(&root.join(".rivet")).expect("open committed store")
}

/// Every use row for the authored fixture files.
fn all_use_rows(store: &Store) -> Vec<UseRow> {
    let mut rows = Vec::new();
    for file in store.list_files().expect("list files") {
        rows.extend(store.list_uses_for_file(&file.path).expect("list uses"));
    }
    rows
}

/// The use at `[start, end)` in `file` with the given ref kind.
fn use_at<'a>(uses: &'a [UseRow], file: &str, start: u32, end: u32, kind: &str) -> &'a UseRow {
    uses.iter()
        .find(|row| {
            row.file == file
                && row.start_byte == start
                && row.end_byte == end
                && row.ref_kind.as_str() == kind
        })
        .unwrap_or_else(|| panic!("missing {kind} use {file} [{start}..{end}]"))
}

/// The binding for `use_row`, if any.
fn binding_for<'a>(bindings: &'a [BindingRow], use_row: &UseRow) -> Option<&'a BindingRow> {
    let use_id = use_row.use_id.expect("persisted uses carry an ID");
    bindings.iter().find(|binding| binding.use_id == use_id)
}

#[test]
fn resolves_imports_and_top_level_calls_but_not_methods() {
    let temp = fixture_repo("fixture");
    let root = temp.path();

    let value = index_json(root);
    // Six bindings, all `exact`:
    //   1. ReportService.php import alias `SurveySvc`
    //   2. ReportService.php import `SurveyService`
    //   3. ReportService.php `new SurveySvc()` type use (the gold alias case)
    //   4. ReportService.php `runTyped(SurveyService $svc)` parameter type
    //   5. boot.php top-level `launch()` call (the gold top-level case)
    //   6. boot.php `new \App\Services\SurveyService()` fully qualified type
    assert_eq!(value["bindings"], 6, "T19 binding count: {value}");

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");

    // The gold (b) alias use binds to the aliased class as `exact`.
    let alias = use_at(&uses, "ReportService.php", 564, 573, "type");
    let alias_binding = binding_for(&bindings, alias).expect("alias use must bind");
    assert_eq!(
        alias_binding.target_id, "SurveyService.php#App\\Services\\SurveyService",
        "alias must resolve through its import"
    );
    assert_eq!(alias_binding.resolution.as_str(), "exact");

    // The gold (c) top-level call binds to the namespaced function as `exact`.
    let top_level = use_at(&uses, "boot.php", 179, 185, "call");
    let call_binding = binding_for(&bindings, top_level).expect("top-level call must bind");
    assert_eq!(call_binding.target_id, "boot.php#App\\Boot\\launch");
    assert_eq!(call_binding.resolution.as_str(), "exact");

    // Every receiver-based `launch` call is unresolved in T19.
    let method_calls: Vec<&UseRow> = uses
        .iter()
        .filter(|row| {
            row.spelling == "launch" && row.ref_kind.as_str() == "call" && row.receiver.is_some()
        })
        .collect();
    assert!(
        !method_calls.is_empty(),
        "the fixture has receiver-based launch calls"
    );
    for row in method_calls {
        assert!(
            binding_for(&bindings, row).is_none(),
            "T20/T21 must own receiver-based calls: {row:?}"
        );
    }

    // A no-edit re-index reproduces byte-identical binding rows.
    drop(store);
    let _ = index_json(root);
    let store = open_store(root);
    assert_eq!(
        store.list_bindings().expect("list bindings"),
        bindings,
        "a no-edit re-index must keep identical bindings"
    );
}

/// Writes one PHP file under `root`.
fn write_php(root: &Path, name: &str, source: &str) {
    fs::write(root.join(name), source.as_bytes()).expect("write PHP fixture");
}

#[test]
fn function_import_alias_binds_and_without_it_stays_unresolved() {
    let temp = TempDir::new("function-alias");
    let root = temp.path();
    fs::create_dir_all(root.join(".git")).expect("create .git");
    write_php(
        root,
        "A.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    write_php(
        root,
        "B.php",
        "<?php\ndeclare(strict_types=1);\nnamespace B;\nfunction launch(): void {}\n",
    );

    // With `use function A\launch;` the call resolves to A\launch.
    write_php(
        root,
        "C.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nlaunch();\n",
    );
    let value = index_json(root);
    assert_eq!(
        value["bindings"], 2,
        "import use and call both bind: {value}"
    );

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");
    let call = uses
        .iter()
        .find(|row| {
            row.file == "C.php" && row.ref_kind.as_str() == "call" && row.spelling == "launch"
        })
        .expect("C.php has a launch call");
    let binding = binding_for(&bindings, call).expect("imported call must bind");
    assert_eq!(binding.target_id, "A.php#A\\launch");
    assert_eq!(binding.resolution.as_str(), "exact");
    drop(store);

    // Without the import, namespace C has no `launch`, so the call is
    // unresolved rather than name-matched to A or B.
    write_php(
        root,
        "C.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nlaunch();\n",
    );
    let value = index_json(root);
    assert_eq!(value["bindings"], 0, "no import means no binding: {value}");

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");
    let call = uses
        .iter()
        .find(|row| {
            row.file == "C.php" && row.ref_kind.as_str() == "call" && row.spelling == "launch"
        })
        .expect("C.php has a launch call");
    assert!(
        binding_for(&bindings, call).is_none(),
        "an unimported call must stay unresolved"
    );
}
