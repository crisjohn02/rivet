//! Integration tests for `rivet refs --json` (T23).
//!
//! They drive the built binary against a temporary copy of the authored PHP
//! fixture (plus a `.git` boundary), exactly like `symbol_json.rs` and
//! `bindings.rs`, so refresh, query resolution, mode selection, filters,
//! pagination, exit codes, and the stdout/stderr split are exercised end to end.
//! One separate temporary fixture checks that a use survives its binding target
//! being deleted (the T22 carry-forward assertion).

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// The unique method declaration the reference queries target.
const LAUNCH_QUERY: &str = "App\\Services\\SurveyService::launch";
/// Its canonical ID, as in the gold fixture.
const LAUNCH_TARGET: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
/// The class the alias/import queries target.
const CLASS_QUERY: &str = "App\\Services\\SurveyService";
/// The class's canonical ID.
const CLASS_TARGET: &str = "SurveyService.php#App\\Services\\SurveyService";
/// The same-name function in another declaration, bound by `boot.php`'s
/// top-level call.
const BOOT_TARGET: &str = "boot.php#App\\Boot\\launch";
/// The gold `[[not_a_use]]` comment/string spans in `boot.php`.
const NOT_A_USE_SPANS: [(u64, u64); 2] = [(300, 306), (347, 353)];

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
            "rivet-refs-{label}-{}-{nanos}-{unique}",
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

/// Writes one PHP file under `root`.
fn write_php(root: &Path, name: &str, source: &str) {
    fs::write(root.join(name), source.as_bytes()).expect("write PHP fixture");
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

/// Runs `refs` with `query` and the given extra flags, requiring success.
fn refs(dir: &Path, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["refs", query];
    args.extend_from_slice(extra);
    args.push("--json");
    parse_success(&run(dir, &args))
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

/// The `(file, start_byte, end_byte, resolution, resolved_target)` of each
/// reference, in output order.
fn reference_spans(value: &Value) -> Vec<(String, u64, u64, String, Option<String>)> {
    value["references"]
        .as_array()
        .expect("references array")
        .iter()
        .map(|reference| {
            (
                reference["file"].as_str().unwrap().to_string(),
                reference["start_byte"].as_u64().unwrap(),
                reference["end_byte"].as_u64().unwrap(),
                reference["resolution"].as_str().unwrap().to_string(),
                reference["resolved_target"].as_str().map(str::to_string),
            )
        })
        .collect()
}

#[test]
fn reference_mode_keeps_bound_and_unresolved_same_name_uses() {
    let temp = fixture_repo("default");
    let value = refs(temp.path(), LAUNCH_QUERY, &[]);

    assert_eq!(value["mode"], "references");
    assert_eq!(value["symbol"]["id"], LAUNCH_TARGET);
    assert_eq!(value["total"], 6);
    assert_eq!(value["truncated"], false);
    assert!(value["next_offset"].is_null());
    assert_eq!(
        value["by_resolution"],
        json!({"exact": 0, "scoped": 5, "name_match": 1})
    );

    // Exact spans, resolution, and target, in contract order. The
    // `$x->launch()` at [862, 868) is unresolved `name_match`; every other
    // included use is bound to the queried method as `scoped`.
    let expected: [(&str, u64, u64, &str, Option<&str>); 6] = [
        ("ReportService.php", 591, 597, "scoped", Some(LAUNCH_TARGET)),
        ("ReportService.php", 736, 742, "scoped", Some(LAUNCH_TARGET)),
        ("ReportService.php", 862, 868, "name_match", None),
        ("SurveyService.php", 640, 646, "scoped", Some(LAUNCH_TARGET)),
        ("boot.php", 267, 273, "scoped", Some(LAUNCH_TARGET)),
        ("boot.php", 430, 436, "scoped", Some(LAUNCH_TARGET)),
    ];
    let references = value["references"].as_array().expect("references array");
    assert_eq!(references.len(), expected.len());
    for (reference, (file, start, end, resolution, target)) in references.iter().zip(expected) {
        assert_eq!(reference["file"], file);
        assert_eq!(reference["start_byte"], start);
        assert_eq!(reference["end_byte"], end);
        assert_eq!(reference["ref_kind"], "call");
        assert_eq!(reference["resolution"], resolution);
        assert_eq!(reference["resolved_target"].as_str(), target);
        assert!(
            reference["content_hash"]
                .as_str()
                .unwrap()
                .starts_with("blake3:"),
            "{reference}"
        );
    }

    // Coordinates are one-based, and `containing_symbol` is a full symbol or
    // null for a top-level use.
    assert_eq!(references[0]["line"], 24);
    assert_eq!(references[0]["column"], 15);
    assert_eq!(
        references[0]["containing_symbol"]["id"],
        "ReportService.php#App\\Reporting\\ReportService::runAlias"
    );
    assert_eq!(references[0]["receiver"], "$svc");
    assert!(
        references[4]["containing_symbol"].is_null(),
        "top-level use"
    );

    // A same-name use bound to a *different* declaration is absent.
    assert!(
        !references
            .iter()
            .any(|r| r["file"] == "boot.php" && r["start_byte"] == 179),
        "a use bound to App\\Boot\\launch must not be a reference"
    );
}

#[test]
fn candidate_mode_reports_a_use_bound_elsewhere_as_query_relative_name_match() {
    let temp = fixture_repo("candidates");
    let value = refs(temp.path(), LAUNCH_QUERY, &["--mode", "candidates"]);

    assert_eq!(value["mode"], "candidates");
    assert_eq!(value["total"], 7);
    assert_eq!(
        value["by_resolution"],
        json!({"exact": 0, "scoped": 5, "name_match": 2})
    );

    let references = value["references"].as_array().expect("references array");
    let excluded = references
        .iter()
        .find(|r| r["file"] == "boot.php" && r["start_byte"] == 179)
        .expect("the use bound to App\\Boot\\launch must appear in candidate mode");
    assert_eq!(excluded["end_byte"], 185);
    assert_eq!(excluded["ref_kind"], "call");
    assert_eq!(excluded["resolution"], "name_match");
    // The real binding is retained for inspection.
    assert_eq!(excluded["resolved_target"], BOOT_TARGET);

    // Candidate mode is a superset of reference mode: the unresolved use keeps
    // its null target.
    let unresolved = references
        .iter()
        .find(|r| r["file"] == "ReportService.php" && r["start_byte"] == 862)
        .expect("unresolved same-name use");
    assert_eq!(unresolved["resolution"], "name_match");
    assert!(unresolved["resolved_target"].is_null());
}

#[test]
fn aliased_uses_are_retained_through_their_binding() {
    let temp = fixture_repo("alias");
    let value = refs(temp.path(), CLASS_QUERY, &[]);

    assert_eq!(value["total"], 5);
    assert_eq!(
        value["by_resolution"],
        json!({"exact": 5, "scoped": 0, "name_match": 0})
    );

    // The alias spelling `SurveySvc` differs from the target's name, yet its
    // import and `new` type uses are linked through the binding.
    let expected: [(&str, u64, u64, &str); 5] = [
        ("ReportService.php", 165, 174, "import"),
        ("ReportService.php", 256, 269, "import"),
        ("ReportService.php", 564, 573, "type"),
        ("ReportService.php", 690, 703, "type"),
        ("boot.php", 230, 257, "type"),
    ];
    let spans = reference_spans(&value);
    assert_eq!(spans.len(), expected.len());
    for ((file, start, end, resolution, target), (want_file, want_start, want_end, kind)) in
        spans.iter().zip(expected)
    {
        assert_eq!(file, want_file);
        assert_eq!(*start, want_start);
        assert_eq!(*end, want_end);
        assert_eq!(resolution, "exact");
        assert_eq!(target.as_deref(), Some(CLASS_TARGET));
        let reference = value["references"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["start_byte"] == *start)
            .unwrap();
        assert_eq!(reference["ref_kind"], kind);
    }
}

#[test]
fn kind_filter_can_yield_an_empty_page() {
    let temp = fixture_repo("kind");

    let calls = refs(temp.path(), LAUNCH_QUERY, &["--kind", "call"]);
    assert_eq!(calls["total"], 6, "{calls}");

    // No `type` use is bound to the queried method, so this matches nothing.
    let types = refs(temp.path(), LAUNCH_QUERY, &["--kind", "type"]);
    assert_eq!(types["total"], 0, "{types}");
    assert_eq!(
        types["references"].as_array().expect("array").len(),
        0,
        "empty list, not null"
    );
    let by_resolution = types["by_resolution"].as_object().expect("object");
    assert_eq!(
        by_resolution.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["exact", "scoped", "name_match"],
        "the three keys are always present"
    );
    assert_eq!(
        types["by_resolution"],
        json!({"exact": 0, "scoped": 0, "name_match": 0})
    );
}

#[test]
fn minimum_resolution_drops_name_matches() {
    let temp = fixture_repo("min-resolution");
    let value = refs(temp.path(), LAUNCH_QUERY, &["--min-resolution", "scoped"]);

    assert_eq!(value["total"], 5);
    assert_eq!(
        value["by_resolution"],
        json!({"exact": 0, "scoped": 5, "name_match": 0})
    );
    assert!(
        value["references"]
            .as_array()
            .unwrap()
            .iter()
            .all(|reference| reference["resolution"] != "name_match"),
        "{value}"
    );
}

#[test]
fn pagination_is_ordered_and_counts_before_slicing() {
    let temp = fixture_repo("pagination");

    let all = refs(temp.path(), LAUNCH_QUERY, &[]);
    let all_spans = reference_spans(&all);
    assert_eq!(all_spans.len(), 6);

    let expected_by_resolution = json!({"exact": 0, "scoped": 5, "name_match": 1});
    let mut collected: Vec<(String, u64, u64, String, Option<String>)> = Vec::new();

    // First page: two of six, later results exist.
    let first = refs(temp.path(), LAUNCH_QUERY, &["--limit", "2"]);
    assert_eq!(first["total"], 6);
    assert_eq!(first["truncated"], true);
    assert_eq!(first["next_offset"], 2);
    assert_eq!(first["by_resolution"], expected_by_resolution);
    collected.extend(reference_spans(&first));

    // Second page continues with no overlap or gap.
    let second = refs(
        temp.path(),
        LAUNCH_QUERY,
        &["--limit", "2", "--offset", "2"],
    );
    assert_eq!(second["truncated"], true);
    assert_eq!(second["next_offset"], 4);
    assert_eq!(second["by_resolution"], expected_by_resolution);
    collected.extend(reference_spans(&second));

    // Final page: earlier pages still make `truncated` true, but there is no
    // later page.
    let third = refs(
        temp.path(),
        LAUNCH_QUERY,
        &["--limit", "2", "--offset", "4"],
    );
    assert_eq!(third["truncated"], true);
    assert!(third["next_offset"].is_null());
    assert_eq!(third["by_resolution"], expected_by_resolution);
    collected.extend(reference_spans(&third));

    assert_eq!(collected, all_spans, "pages must cover every match once");

    // An offset beyond the end is an empty page, not an error.
    let beyond = refs(temp.path(), LAUNCH_QUERY, &["--offset", "6"]);
    assert_eq!(beyond["total"], 6);
    assert_eq!(beyond["truncated"], true);
    assert!(beyond["next_offset"].is_null());
    assert_eq!(beyond["references"].as_array().unwrap().len(), 0);
    assert_eq!(beyond["by_resolution"], expected_by_resolution);
}

#[test]
fn invalid_arguments_are_rejected_before_filesystem_work() {
    // No `.git` or `.rivet/` anywhere above: a filesystem-first implementation
    // would report repository_unavailable (exit 3) instead.
    let temp = TempDir::new("invalid");
    for args in [
        vec!["refs", "Foo", "--limit", "0", "--json"],
        vec!["refs", "Foo", "--limit", "1001", "--json"],
        vec!["refs", "Foo", "--mode", "everything", "--json"],
        vec!["refs", "Foo", "--kind", "bogus", "--json"],
    ] {
        let output = run(temp.path(), &args);
        let value = parse_error(&output, 2);
        assert_eq!(value["error"], "invalid_arguments", "{args:?}");
    }
}

#[test]
fn a_use_survives_its_binding_targets_deletion() {
    let temp = TempDir::new("target-deleted");
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
    let c_source =
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nlaunch();\n";
    write_php(root, "C.php", c_source);
    let call_start = c_source.find("launch();").expect("call in C.php") as u64;

    // While A\launch exists, the C.php call is bound elsewhere and is not a
    // reference to B\launch.
    let before = refs(root, "B\\launch", &[]);
    assert_eq!(before["symbol"]["id"], "B.php#B\\launch");
    assert_eq!(
        before["total"], 0,
        "bound to A\\launch, not B\\launch: {before}"
    );

    // Delete the binding target and re-resolve: the use becomes unresolved but
    // is still reported, with query-relative name-match evidence.
    fs::remove_file(root.join("A.php")).expect("delete the binding target");
    let after = refs(root, "B\\launch", &[]);
    assert_eq!(after["symbol"]["id"], "B.php#B\\launch");
    assert_eq!(after["total"], 2, "{after}");
    let call = after["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reference| {
            reference["file"] == "C.php" && reference["start_byte"].as_u64() == Some(call_start)
        })
        .expect("the orphaned call must still be reported");
    assert_eq!(call["end_byte"], call_start + 6);
    assert_eq!(call["ref_kind"], "call");
    assert_eq!(call["resolution"], "name_match");
    assert!(call["resolved_target"].is_null(), "{call}");
}

#[test]
fn gold_not_a_use_spans_appear_in_neither_mode() {
    let temp = fixture_repo("not-a-use");

    for mode in ["references", "candidates"] {
        let value = refs(temp.path(), LAUNCH_QUERY, &["--mode", mode]);
        for reference in value["references"].as_array().expect("references array") {
            if reference["file"] != "boot.php" {
                continue;
            }
            let start = reference["start_byte"].as_u64().unwrap();
            assert!(
                !NOT_A_USE_SPANS.iter().any(|(s, _)| *s == start),
                "a [[not_a_use]] span reached {mode} output: {reference}"
            );
        }
    }
}

#[test]
fn repeated_queries_are_byte_identical() {
    let temp = fixture_repo("determinism");
    let first = run(temp.path(), &["refs", LAUNCH_QUERY, "--json"]);
    assert_eq!(first.status.code(), Some(0));
    let second = run(temp.path(), &["refs", LAUNCH_QUERY, "--json"]);
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        first.stdout, second.stdout,
        "the same query must serialize byte-identically"
    );
}
