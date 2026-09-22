//! Integration tests for the T13 symbol query forms: `file:line`, query
//! normalization, deterministic ambiguity pagination, and not-found
//! suggestions (spec §10.2, §10.3; OUTPUT-CONTRACT "Errors" and "Ordering and
//! versioning").
//!
//! They drive the built binary against a temporary copy of the authored PHP
//! fixture so indexing, resolution, exit codes, and the JSON envelope are
//! exercised end to end.

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
            "rivet-query-forms-{label}-{}-{nanos}-{unique}",
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

#[test]
fn file_line_inside_a_method_body_resolves_to_the_method() {
    let temp = fixture_repo("innermost");
    let output = run(temp.path(), &["symbol", "SurveyService.php:19", "--json"]);
    let value = parse_success(&output);

    assert_eq!(
        value["symbol"]["qualified_name"],
        "App\\Services\\SurveyService::launch"
    );
    assert_eq!(value["symbol"]["kind"], "method");
    assert_eq!(value["symbol"]["start_line"], 18);
    assert_eq!(value["symbol"]["end_line"], 21);
}

#[test]
fn file_line_on_the_class_line_resolves_to_the_class() {
    let temp = fixture_repo("class-line");
    let output = run(temp.path(), &["symbol", "SurveyService.php:9", "--json"]);
    let value = parse_success(&output);

    assert_eq!(
        value["symbol"]["qualified_name"],
        "App\\Services\\SurveyService"
    );
    assert_eq!(value["symbol"]["kind"], "class");
    assert_eq!(value["symbol"]["start_line"], 9);
}

#[test]
fn file_line_without_an_enclosing_symbol_suggests_the_nearest() {
    let temp = fixture_repo("no-symbol");
    let output = run(temp.path(), &["symbol", "SurveyService.php:2", "--json"]);
    let value = parse_error(&output, 4);

    assert_eq!(value["error"], "symbol_not_found");
    let suggestions = value["suggestions"].as_array().expect("suggestions array");
    assert!(!suggestions.is_empty(), "{value}");
    assert!(
        suggestions
            .iter()
            .any(|suggestion| suggestion == "App\\Services\\SurveyService"),
        "{value}"
    );
}

#[test]
fn file_line_on_an_unindexed_path_names_the_path() {
    let temp = fixture_repo("not-indexed");
    let output = run(temp.path(), &["symbol", "nope.php:3", "--json"]);
    let value = parse_error(&output, 4);

    assert_eq!(value["error"], "symbol_not_found");
    let hint = value["hint"].as_str().expect("hint string");
    assert!(hint.contains("nope.php"), "{value}");
    assert!(
        hint.contains("not indexed") || hint.contains("excluded"),
        "{value}"
    );
}

#[test]
fn file_line_resolves_a_top_level_function() {
    let temp = fixture_repo("boot");
    let output = run(temp.path(), &["symbol", "boot.php:8", "--json"]);
    let value = parse_success(&output);

    assert_eq!(value["symbol"]["qualified_name"], "App\\Boot\\launch");
    assert_eq!(value["symbol"]["kind"], "function");
}

#[test]
fn dotted_query_does_not_match_across_a_component_boundary() {
    let temp = fixture_repo("boundary");
    let output = run(temp.path(), &["symbol", "Service.launch", "--json"]);
    let value = parse_error(&output, 4);
    assert_eq!(value["error"], "symbol_not_found");
}

#[test]
fn dotted_query_is_case_insensitive_for_methods() {
    let temp = fixture_repo("case-fold");
    let output = run(
        temp.path(),
        &["symbol", "services.surveyservice.LAUNCH", "--json"],
    );
    let value = parse_success(&output);

    assert_eq!(
        value["symbol"]["qualified_name"],
        "App\\Services\\SurveyService::launch"
    );
    assert_eq!(value["symbol"]["kind"], "method");
}

#[test]
fn wrong_case_on_a_property_is_not_found() {
    let temp = fixture_repo("property-case");
    let output = run(
        temp.path(),
        &["symbol", "App\\Services\\SurveyService::$Label", "--json"],
    );
    let value = parse_error(&output, 4);
    assert_eq!(value["error"], "symbol_not_found");
}

#[test]
fn offset_pages_ambiguous_candidates_deterministically() {
    let temp = fixture_repo("offset");
    let output = run(
        temp.path(),
        &[
            "symbol", "launch", "--limit", "1", "--offset", "1", "--json",
        ],
    );
    let value = parse_error(&output, 5);

    assert_eq!(value["error"], "ambiguous_symbol");
    assert_eq!(value["total"], 3);
    assert_eq!(value["next_offset"], 2);
    let candidates = value["candidates"].as_array().expect("candidates array");
    assert_eq!(candidates.len(), 1);
    // Page two is SurveyService.php (file bytes then start_byte order).
    assert_eq!(candidates[0]["file"], "SurveyService.php");
}

#[test]
fn offset_beyond_the_end_is_an_empty_page() {
    let temp = fixture_repo("offset-past-end");
    let output = run(
        temp.path(),
        &["symbol", "launch", "--offset", "99", "--json"],
    );
    let value = parse_error(&output, 5);

    assert_eq!(value["total"], 3);
    assert!(value["next_offset"].is_null(), "{value}");
    assert_eq!(value["candidates"].as_array().unwrap().len(), 0);
}

#[test]
fn limit_zero_is_rejected_before_any_work() {
    let temp = fixture_repo("limit-zero");
    let output = run(temp.path(), &["symbol", "launch", "--limit", "0", "--json"]);
    let value = parse_error(&output, 2);
    assert_eq!(value["error"], "invalid_arguments");
}

#[test]
fn negative_offset_is_rejected() {
    let temp = fixture_repo("negative-offset");
    let output = run(
        temp.path(),
        &["symbol", "launch", "--offset", "-1", "--json"],
    );
    let value = parse_error(&output, 2);
    assert_eq!(value["error"], "invalid_arguments");
}

#[test]
fn absolute_and_parent_file_paths_are_invalid_arguments() {
    let temp = fixture_repo("bad-path");
    for query in ["/etc/passwd:1", "../SurveyService.php:1"] {
        let output = run(temp.path(), &["symbol", query, "--json"]);
        let value = parse_error(&output, 2);
        assert_eq!(value["error"], "invalid_arguments", "{query}");
    }
}
