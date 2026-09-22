//! T18 integration tests: persisted uses and lexical scopes.
//!
//! The authored PHP fixture is copied into a temporary Git root, indexed with
//! the real binary, and inspected by opening the committed `.rivet/index.db`
//! directly with [`rivet_store::Store::open`]. Every gold `[[use]]` span must
//! have a persisted row with the same ref kind; the top-level `launch()` row
//! must carry a NULL container; re-indexing unchanged content must reproduce
//! byte-identical use rows; and `ReportService.php` scopes must carry the alias
//! import `SurveySvc -> App\Services\SurveyService`.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_store::{Store, UseRow};
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
            "rivet-index-uses-{label}-{}-{nanos}-{unique}",
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

#[derive(serde::Deserialize)]
struct GoldFile {
    #[serde(default, rename = "use")]
    uses: Vec<GoldUse>,
}

#[derive(serde::Deserialize)]
struct GoldUse {
    file: String,
    start_byte: u32,
    end_byte: u32,
    ref_kind: String,
}

/// The gold `[[use]]` entries for the authored fixture.
fn gold_uses() -> Vec<GoldUse> {
    let gold_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/gold/php-authored.toml");
    let gold_text = fs::read_to_string(&gold_path).expect("read gold TOML");
    let gold: GoldFile = toml::from_str(&gold_text).expect("parse gold TOML");
    gold.uses
}

/// The authored fixture file names, in byte order.
const FIXTURE_FILES: [&str; 5] = [
    "Documented.php",
    "ReportService.php",
    "SurveyService.php",
    "boot.php",
    "README.md",
];

/// Every persisted use row for the fixture, grouped by file in byte order.
fn all_use_rows(store: &Store) -> Vec<(String, Vec<UseRow>)> {
    FIXTURE_FILES
        .iter()
        .map(|file| {
            let rows = store.list_uses_for_file(file).expect("list uses");
            ((*file).to_string(), rows)
        })
        .collect()
}

#[test]
fn index_persists_gold_uses_scopes_and_reindexes_identically() {
    let temp = fixture_repo("fixture");
    let root = temp.path();

    let value = index_json(root);
    let gold = gold_uses();
    let reported = value["uses"].as_u64().expect("uses is a number");
    assert!(
        reported >= gold.len() as u64,
        "reported uses {reported} must cover every gold use ({})",
        gold.len()
    );

    let store = open_store(root);
    // T19 resolves real bindings; the reported count must match the table.
    let binding_rows = store.list_bindings().expect("list bindings");
    assert_eq!(
        value["bindings"].as_u64().expect("bindings is a number"),
        binding_rows.len() as u64,
        "reported bindings must match the persisted bindings table"
    );

    // Every gold (file, start_byte, end_byte, ref_kind) has exactly one row.
    for gold_use in &gold {
        let rows = store
            .list_uses_for_file(&gold_use.file)
            .expect("list uses for gold file");
        let matches = rows
            .iter()
            .filter(|row| {
                row.start_byte == gold_use.start_byte
                    && row.end_byte == gold_use.end_byte
                    && row.ref_kind.as_str() == gold_use.ref_kind
            })
            .count();
        assert_eq!(
            matches, 1,
            "gold {} [{}..{}] in {} matched {matches} rows",
            gold_use.ref_kind, gold_use.start_byte, gold_use.end_byte, gold_use.file
        );
    }

    // The top-level `launch();` call has no containing symbol.
    let boot = store
        .list_uses_for_file("boot.php")
        .expect("list boot uses");
    let top_level = boot
        .iter()
        .find(|row| row.start_byte == 179 && row.end_byte == 185)
        .expect("top-level launch() use must be persisted");
    assert_eq!(
        top_level.containing_symbol, None,
        "a top-level use must persist a NULL container"
    );
    assert_eq!(top_level.ref_kind.as_str(), "call");

    // The alias import is visible in a ReportService.php scope.
    let scopes = store
        .list_scopes_for_file("ReportService.php")
        .expect("list report scopes");
    let alias_visible = scopes.iter().any(|scope| {
        let facts: Value = serde_json::from_str(&scope.facts_json).expect("facts are JSON");
        facts["imports"].as_array().is_some_and(|imports| {
            imports.iter().any(|import| {
                import["alias"] == "SurveySvc"
                    && import["target_qualified"] == "App\\Services\\SurveyService"
                    && import["kind"] == "class"
            })
        })
    });
    assert!(
        alias_visible,
        "ReportService.php scopes must expose the SurveySvc alias import: {scopes:?}"
    );

    // Re-indexing with no edits reproduces every persisted use row.
    let before = all_use_rows(&store);
    drop(store);
    let _ = index_json(root);
    let store = open_store(root);
    let after = all_use_rows(&store);
    assert_eq!(
        before, after,
        "a no-edit re-index must keep byte-identical use rows"
    );
    assert!(
        after.iter().any(|(_, rows)| !rows.is_empty()),
        "uses must survive a no-edit re-index"
    );
}
