//! Integration tests for `rivet symbol --json` (T12).
//!
//! They drive the built binary against a temporary copy of the authored PHP
//! fixture (plus a `.git` boundary) so refresh, persistence, query resolution,
//! exit codes, and the stdout/stderr split are exercised end to end.

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
            "rivet-symbol-{label}-{}-{nanos}-{unique}",
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

/// The gold `App\Services\SurveyService::launch` span from `tests/gold`.
const LAUNCH_START_BYTE: u64 = 460;
const LAUNCH_END_BYTE: u64 = 546;

#[test]
fn native_qualified_name_returns_the_gold_symbol() {
    let temp = fixture_repo("native");
    let output = run(
        temp.path(),
        &["symbol", "App\\Services\\SurveyService::launch", "--json"],
    );
    let value = parse_success(&output);

    assert_eq!(value["schema_version"], 1);
    assert_eq!(
        value["symbol"]["qualified_name"],
        "App\\Services\\SurveyService::launch"
    );
    assert_eq!(value["symbol"]["kind"], "method");
    assert_eq!(value["symbol"]["language"], "php");
    assert_eq!(value["symbol"]["file"], "SurveyService.php");
    assert_eq!(value["symbol"]["start_byte"], LAUNCH_START_BYTE);
    assert_eq!(value["symbol"]["end_byte"], LAUNCH_END_BYTE);
    assert_eq!(value["symbol"]["start_line"], 18);
    assert_eq!(value["symbol"]["end_line"], 21);
    assert!(
        value["symbol"]["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:"),
        "{value}"
    );
    // T14 fills the stored signature; this method has no docblock.
    assert_eq!(value["signature"], "public function launch(): void");
    assert!(value["doc_comment"].is_null());
    assert_eq!(
        value["parent"],
        "SurveyService.php#App\\Services\\SurveyService"
    );
    // T24 fills the call lists: `launch` contains no extracted call use, but it
    // is called from six places (five scoped, one unresolved name match).
    assert_eq!(value["calls"]["total"], 0);
    assert_eq!(value["calls"]["items"].as_array().unwrap().len(), 0);
    assert_eq!(value["called_by"]["total"], 6);
    assert!(
        value.get("development_note").is_none(),
        "the T12 not-implemented note is gone: {value}"
    );
}

#[test]
fn duplicate_short_names_are_ambiguous_and_byte_ordered() {
    let temp = fixture_repo("ambiguous");
    let output = run(temp.path(), &["symbol", "launch", "--json"]);
    let value = parse_error(&output, 5);

    assert_eq!(value["error"], "ambiguous_symbol");
    assert_eq!(value["total"], 3);
    assert_eq!(value["truncated"], false);
    assert!(value["next_offset"].is_null());

    let candidates = value["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates.len(), 3);
    // Ordered by file bytes, then start_byte: 'R' < 'S' < 'b'.
    let files: Vec<&str> = candidates
        .iter()
        .map(|c| c["file"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        vec!["ReportService.php", "SurveyService.php", "boot.php"]
    );
    let starts: Vec<u64> = candidates
        .iter()
        .map(|c| c["start_byte"].as_u64().unwrap())
        .collect();
    assert_eq!(starts, vec![409, 460, 95]);
    // Candidates are full symbol objects.
    assert_eq!(candidates[2]["kind"], "function");
    assert_eq!(candidates[2]["qualified_name"], "App\\Boot\\launch");
}

#[test]
fn limit_truncates_the_candidate_page() {
    let temp = fixture_repo("pagination");
    let output = run(temp.path(), &["symbol", "launch", "--limit", "2", "--json"]);
    let value = parse_error(&output, 5);

    assert_eq!(value["total"], 3);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["next_offset"], 2);
    assert_eq!(value["candidates"].as_array().unwrap().len(), 2);
}

#[test]
fn dotted_path_matches_a_trailing_component() {
    let temp = fixture_repo("dotted");
    let output = run(temp.path(), &["symbol", "SurveyService.launch", "--json"]);
    let value = parse_success(&output);

    // Only App\Services\SurveyService::launch ends with SurveyService.launch.
    assert_eq!(value["symbol"]["start_byte"], LAUNCH_START_BYTE);
    assert_eq!(value["symbol"]["end_byte"], LAUNCH_END_BYTE);
}

#[test]
fn unknown_query_is_symbol_not_found_with_suggestions() {
    let temp = fixture_repo("not-found");
    let output = run(temp.path(), &["symbol", "Nope", "--json"]);
    let value = parse_error(&output, 4);

    assert_eq!(value["error"], "symbol_not_found");
    let suggestions = value["suggestions"].as_array().expect("suggestions array");
    assert!(!suggestions.is_empty(), "{value}");
    assert!(suggestions.len() <= 5, "{value}");
    assert!(
        suggestions.iter().all(|suggestion| suggestion.is_string()),
        "{value}"
    );
}

#[test]
fn canonical_id_round_trips() {
    let temp = fixture_repo("canonical");
    let first = run(
        temp.path(),
        &["symbol", "App\\Services\\SurveyService::launch", "--json"],
    );
    let first = parse_success(&first);
    let id = first["symbol"]["id"]
        .as_str()
        .expect("canonical id")
        .to_string();

    let second = run(temp.path(), &["symbol", &id, "--json"]);
    let second = parse_success(&second);
    assert_eq!(second["symbol"], first["symbol"]);
}

#[test]
fn source_and_signature_only_flags() {
    let temp = fixture_repo("source");

    let output = run(
        temp.path(),
        &[
            "symbol",
            "App\\Services\\SurveyService::launch",
            "--source",
            "--json",
        ],
    );
    let value = parse_success(&output);
    let source = value["source"].as_str().expect("source string");
    assert!(source.starts_with("public function launch"));
    assert!(source.contains("self::DEFAULT_LABEL"));
    // `--source` is the exact stored span [460, 546) from the gold fixture.
    assert_eq!(source.len() as u64, LAUNCH_END_BYTE - LAUNCH_START_BYTE);
    let live = fs::read(temp.path().join("SurveyService.php")).expect("read fixture");
    assert_eq!(
        source.as_bytes(),
        &live[LAUNCH_START_BYTE as usize..LAUNCH_END_BYTE as usize]
    );
    assert!(source.ends_with('}'), "{source:?}");

    // `--signature-only` omits the call lists entirely.
    let signature_only = run(
        temp.path(),
        &[
            "symbol",
            "App\\Services\\SurveyService::launch",
            "--signature-only",
            "--json",
        ],
    );
    let value = parse_success(&signature_only);
    assert!(value.get("calls").is_none(), "{value}");
    assert!(value.get("called_by").is_none(), "{value}");
    assert!(value.get("source").is_none(), "{value}");

    // Both flags together are an argument error before any filesystem work.
    let conflict = run(
        temp.path(),
        &["symbol", "launch", "--source", "--signature-only", "--json"],
    );
    let value = parse_error(&conflict, 2);
    assert_eq!(value["error"], "invalid_arguments");
}
