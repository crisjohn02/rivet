//! Integration tests for `rivet context --json` (T30).
//!
//! They drive the built binary against a temporary copy of the authored PHP
//! fixture (plus a `.git` boundary), so argument validation, refresh, query
//! resolution, candidate collection, fitting, JSON rendering, exit codes, and
//! the stdout/stderr split are exercised end to end.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// `SurveyService::launch`: callers in two files and a parent class.
const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
/// `SurveyService::relaunch`: a callee, a parent, and depth-two callers.
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const SURVEY_SERVICE: &str = "SurveyService.php#App\\Services\\SurveyService";
const RUN_ALIAS: &str = "ReportService.php#App\\Reporting\\ReportService::runAlias";
const RUN_TYPED: &str = "ReportService.php#App\\Reporting\\ReportService::runTyped";
const RUN_UNKNOWN: &str = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
const TEST_LAUNCH: &str = "tests/SurveyLaunchTest.php#Tests\\testLaunch";

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
            "rivet-context-{label}-{}-{nanos}-{unique}",
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

/// Writes `contents` to `relative` under `root`, creating parent directories.
fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, contents).expect("write file");
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
        "stderr must stay empty on success"
    );
    assert!(output.stdout.ends_with(b"\n"), "stdout ends with a newline");
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Parses an error object, requiring `exit` and empty stdout.
fn parse_error(output: &Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "stdout must stay empty on failure"
    );
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON object")
}

/// Runs `rivet context <query> <extra...> --json` and parses the success.
fn context(dir: &Path, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["context", query];
    args.extend_from_slice(extra);
    args.push("--json");
    parse_success(&run(dir, &args))
}

/// The keys of a JSON object, in order.
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

/// Each segment's `(reason, symbol id, resolution, form)`, in order.
fn segments(value: &Value) -> Vec<(String, String, String, String)> {
    value["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|segment| {
            (
                segment["reason"].as_str().expect("reason").to_string(),
                segment["symbol"]["id"].as_str().expect("id").to_string(),
                segment["resolution"]
                    .as_str()
                    .expect("resolution")
                    .to_string(),
                segment["form"].as_str().expect("form").to_string(),
            )
        })
        .collect()
}

/// Builds the expected `segments` tuples.
fn expected(rows: &[(&str, &str, &str, &str)]) -> Vec<(String, String, String, String)> {
    rows.iter()
        .map(|(reason, id, resolution, form)| {
            (
                (*reason).to_string(),
                (*id).to_string(),
                (*resolution).to_string(),
                (*form).to_string(),
            )
        })
        .collect()
}

/// The `omitted` counts as `(budget, overlap, limit)`.
fn omitted(value: &Value) -> (u64, u64, u64) {
    let omitted = &value["omitted"];
    (
        omitted["budget"].as_u64().expect("budget"),
        omitted["overlap"].as_u64().expect("overlap"),
        omitted["limit"].as_u64().expect("limit"),
    )
}

/// The full default context for `launch` on the authored fixture.
fn launch_default() -> Vec<(String, String, String, String)> {
    expected(&[
        ("target", LAUNCH, "exact", "full"),
        ("caller", RUN_ALIAS, "scoped", "full"),
        ("caller", RUN_TYPED, "scoped", "full"),
        ("caller", RELAUNCH, "scoped", "full"),
        ("caller", RUN_UNKNOWN, "name_match", "full"),
        ("parent", SURVEY_SERVICE, "exact", "signature"),
    ])
}

// ---------------------------------------------------------------------------
// a. The done-when: one invocation, no earlier index/symbol/refs.
// ---------------------------------------------------------------------------

#[test]
fn single_invocation_returns_target_and_related_source() {
    let temp = fixture_repo("done-when");
    assert!(
        !temp.path().join(".rivet").exists(),
        "no index exists before the first command"
    );
    let value = context(temp.path(), LAUNCH, &[]);

    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["index"]["freshness"], "content");
    // The copied README.md is the one unsupported file.
    assert_eq!(value["index"]["coverage"]["complete"], false);
    assert_eq!(value["index"]["coverage"]["files_seen"], 10);
    assert_eq!(value["index"]["coverage"]["files_indexed"], 9);
    assert_eq!(value["index"]["coverage"]["skipped"]["unsupported"], 1);
    assert_eq!(value["symbol"]["id"], LAUNCH);
    assert_eq!(value["symbol"]["kind"], "method");
    assert_eq!(value["symbol"]["language"], "php");
    assert_eq!(value["symbol"]["start_line"], 18);
    assert_eq!(value["symbol"]["end_line"], 21);
    assert_eq!(value["budget_tokens"], 4000);
    assert_eq!(value["estimated_tokens"], 175);
    assert_eq!(segments(&value), launch_default());
    assert_eq!(omitted(&value), (0, 0, 0));
    assert_eq!(value["candidate_limit_reached"], false);

    let items = value["segments"].as_array().expect("segments");
    // The top-level symbol is the target segment's symbol object.
    assert_eq!(items[0]["symbol"], value["symbol"]);
    assert_eq!(
        items[0]["source"],
        "public function launch(): void\n    {\n        $this->label = self::DEFAULT_LABEL;\n    }"
    );
    assert_eq!(items[0]["estimated_tokens"], 29);
    assert_eq!(
        items[4]["source"],
        "public function runUnknown(): void\n    {\n        $x->launch();\n    }"
    );
    // The parent is collapsed: its full body would repeat the target, and
    // its summary omits the members emitted separately (launch, relaunch).
    assert_eq!(
        items[5]["source"],
        "final class SurveyService\n{\n    public const DEFAULT_LABEL = 'survey';\n    private string $label = 'survey';\n}"
    );
    assert_eq!(items[5]["estimated_tokens"], 37);
}

// ---------------------------------------------------------------------------
// b. Key order and literals.
// ---------------------------------------------------------------------------

#[test]
fn keys_follow_the_contract_order() {
    let temp = fixture_repo("keys");
    let value = context(temp.path(), LAUNCH, &[]);

    assert_eq!(
        keys(&value),
        vec![
            "schema_version",
            "index",
            "symbol",
            "budget_tokens",
            "estimated_tokens",
            "tokenizer",
            "budget_scope",
            "segments",
            "omitted",
            "candidate_limit_reached",
        ]
    );
    assert_eq!(value["tokenizer"], "utf8-bytes-v1");
    assert_eq!(value["budget_scope"], "source");
    // The KEY order of `omitted` is the contract's shape, not the
    // overlap/limit/budget assignment precedence.
    assert_eq!(keys(&value["omitted"]), vec!["budget", "overlap", "limit"]);
    assert_eq!(
        keys(&value["symbol"]),
        vec![
            "id",
            "name",
            "qualified_name",
            "kind",
            "language",
            "file",
            "start_byte",
            "end_byte",
            "start_line",
            "end_line",
            "content_hash",
        ]
    );
    for segment in value["segments"].as_array().expect("segments") {
        assert_eq!(
            keys(segment),
            vec![
                "symbol",
                "form",
                "reason",
                "resolution",
                "estimated_tokens",
                "source",
            ]
        );
        assert_eq!(keys(&segment["symbol"]), keys(&value["symbol"]));
    }
    // Compact output: no insignificant whitespace between tokens.
    let output = run(temp.path(), &["context", LAUNCH, "--json"]);
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        text.starts_with("{\"schema_version\":1,\"index\":{"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// c. The estimate is the segment sum and never exceeds the budget.
// ---------------------------------------------------------------------------

#[test]
fn estimate_is_the_segment_sum_within_every_budget() {
    let temp = fixture_repo("sweep");
    // Build once, then sweep from the committed snapshot.
    parse_success(&run(temp.path(), &["index", "--json"]));
    for tokens in [
        10_u64, 11, 20, 28, 29, 30, 39, 40, 52, 62, 63, 64, 80, 100, 115, 137, 138, 150, 174, 175,
        176, 500, 4000, 1_000_000,
    ] {
        let budget = tokens.to_string();
        let value = context(temp.path(), LAUNCH, &["--tokens", &budget, "--no-refresh"]);
        assert_eq!(value["budget_tokens"], tokens);
        let items = value["segments"].as_array().expect("segments");
        let sum: u64 = items
            .iter()
            .map(|segment| {
                let estimate = segment["estimated_tokens"].as_u64().expect("estimate");
                // Each estimate is ceil(UTF-8 bytes / 3) of its final source.
                let bytes = segment["source"].as_str().expect("source").len() as u64;
                assert_eq!(estimate, bytes.div_ceil(3), "tokens {tokens}");
                estimate
            })
            .sum();
        let estimated = value["estimated_tokens"].as_u64().expect("estimated");
        assert_eq!(estimated, sum, "tokens {tokens}");
        assert!(estimated <= tokens, "tokens {tokens}: {estimated}");
        // Every explored candidate is either emitted or counted once.
        let (budget_omitted, overlap, limit) = omitted(&value);
        assert_eq!(
            items.len() as u64 + budget_omitted + overlap + limit,
            6,
            "tokens {tokens}"
        );
        assert_eq!(items[0]["reason"], "target");
    }

    // The smallest budgets collapse the target to its signature.
    let tight = context(temp.path(), LAUNCH, &["--tokens", "10", "--no-refresh"]);
    assert_eq!(
        segments(&tight),
        expected(&[("target", LAUNCH, "exact", "signature")])
    );
    assert_eq!(
        tight["segments"][0]["source"],
        "public function launch(): void"
    );
    assert_eq!(tight["estimated_tokens"], 10);
    assert_eq!(omitted(&tight), (5, 0, 0));

    // Exactly the default total fits everything.
    let exact = context(temp.path(), LAUNCH, &["--tokens", "175", "--no-refresh"]);
    assert_eq!(segments(&exact), launch_default());
    assert_eq!(exact["estimated_tokens"], 175);
}

// ---------------------------------------------------------------------------
// d. Each flag changes the result as the contract says.
// ---------------------------------------------------------------------------

#[test]
fn depth_one_removes_second_degree() {
    let temp = fixture_repo("depth");
    let deep = context(temp.path(), RELAUNCH, &[]);
    assert_eq!(
        segments(&deep),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
            ("second_degree", RUN_ALIAS, "scoped", "full"),
            ("second_degree", RUN_TYPED, "scoped", "full"),
        ])
    );
    // `--depth 2` is the default.
    assert_eq!(context(temp.path(), RELAUNCH, &["--depth", "2"]), deep);

    let shallow = context(temp.path(), RELAUNCH, &["--depth", "1"]);
    assert_eq!(
        segments(&shallow),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
        ])
    );
    assert_eq!(omitted(&shallow), (0, 0, 0));
}

#[test]
fn excluding_callees_removes_callee_links() {
    let temp = fixture_repo("callees");
    let value = context(temp.path(), RELAUNCH, &["--exclude-callees"]);
    // The callee is gone, and so is everything reached through it.
    assert_eq!(
        segments(&value),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
        ])
    );
    // `--include-callees` is the default.
    assert_eq!(
        context(temp.path(), RELAUNCH, &["--include-callees"]),
        context(temp.path(), RELAUNCH, &[])
    );
}

#[test]
fn excluding_callers_removes_caller_links() {
    let temp = fixture_repo("callers");
    let value = context(temp.path(), LAUNCH, &["--exclude-callers"]);
    assert_eq!(
        segments(&value),
        expected(&[
            ("target", LAUNCH, "exact", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
        ])
    );
    // Depth-two callers go too: relaunch's second-degree links are callers of
    // its callee.
    let relaunch = context(temp.path(), RELAUNCH, &["--exclude-callers"]);
    assert_eq!(
        segments(&relaunch),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
        ])
    );
    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &["--include-callers"])),
        launch_default()
    );
}

/// A test-file caller of `SurveyService::launch`, matched by `tests/**`.
const SURVEY_LAUNCH_TEST: &str = "<?php

namespace Tests;

use App\\Services\\SurveyService;

function testLaunch(SurveyService $svc): void
{
    $svc->launch();
}
";

#[test]
fn excluding_tests_removes_test_links_and_config_sets_the_default() {
    let temp = fixture_repo("tests");
    write(
        temp.path(),
        "tests/SurveyLaunchTest.php",
        SURVEY_LAUNCH_TEST,
    );

    let with_tests = expected(&[
        ("target", LAUNCH, "exact", "full"),
        ("test", TEST_LAUNCH, "scoped", "full"),
        ("caller", RUN_ALIAS, "scoped", "full"),
        ("caller", RUN_TYPED, "scoped", "full"),
        ("caller", RELAUNCH, "scoped", "full"),
        ("caller", RUN_UNKNOWN, "name_match", "full"),
        ("parent", SURVEY_SERVICE, "exact", "signature"),
    ]);
    assert_eq!(segments(&context(temp.path(), LAUNCH, &[])), with_tests);
    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &["--include-tests"])),
        with_tests
    );
    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &["--exclude-tests"])),
        launch_default()
    );

    // `context.include_tests = false` changes the default, and the flag
    // still overrides it.
    write(
        temp.path(),
        ".rivet/config.toml",
        "[context]\ninclude_tests = false\n",
    );
    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &[])),
        launch_default()
    );
    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &["--include-tests"])),
        with_tests
    );
}

#[test]
fn collapse_modes_select_forms() {
    let temp = fixture_repo("collapse");
    let never = context(temp.path(), RELAUNCH, &["--collapse", "never"]);
    // Only full forms; the parent's full body would repeat the target, so it
    // is overlap.
    assert_eq!(
        segments(&never),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "full"),
            ("second_degree", RUN_ALIAS, "scoped", "full"),
            ("second_degree", RUN_TYPED, "scoped", "full"),
        ])
    );
    assert_eq!(omitted(&never), (0, 1, 0));
    for segment in never["segments"].as_array().expect("segments") {
        assert_eq!(segment["form"], "full");
    }

    // `always` collapses everything but the target, which still prefers full.
    let always = context(temp.path(), RELAUNCH, &["--collapse", "always"]);
    assert_eq!(
        segments(&always),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "signature"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
            ("second_degree", RUN_ALIAS, "scoped", "signature"),
            ("second_degree", RUN_TYPED, "scoped", "signature"),
        ])
    );
    assert_eq!(
        always["segments"][1]["source"],
        "public function launch(): void"
    );

    // `auto` is the default.
    assert_eq!(
        context(temp.path(), RELAUNCH, &["--collapse", "auto"]),
        context(temp.path(), RELAUNCH, &[])
    );
}

#[test]
fn limit_counts_segments_including_the_target() {
    let temp = fixture_repo("limit");
    let one = context(temp.path(), LAUNCH, &["--limit", "1"]);
    assert_eq!(
        segments(&one),
        expected(&[("target", LAUNCH, "exact", "full")])
    );
    assert_eq!(one["estimated_tokens"], 29);
    assert_eq!(omitted(&one), (0, 0, 5));

    let three = context(temp.path(), LAUNCH, &["--limit", "3"]);
    assert_eq!(segments(&three), launch_default()[..3].to_vec());
    assert_eq!(omitted(&three), (0, 0, 3));

    assert_eq!(
        segments(&context(temp.path(), LAUNCH, &["--limit", "1000"])),
        launch_default()
    );
}

#[test]
fn configuration_supplies_the_defaults() {
    let temp = fixture_repo("config");
    write(
        temp.path(),
        ".rivet/config.toml",
        "[context]\ndefault_token_budget = 10\nmax_depth = 1\ncollapse = \"never\"\n",
    );
    // A budget of 10 under `never` cannot fit the 29-token target.
    let error = parse_error(&run(temp.path(), &["context", LAUNCH, "--json"]), 8);
    assert_eq!(error["budget_tokens"], 10);
    assert_eq!(error["required_tokens"], 29);

    // Flags override every configured default.
    let value = context(
        temp.path(),
        RELAUNCH,
        &["--tokens", "4000", "--depth", "2", "--collapse", "auto"],
    );
    assert_eq!(value["budget_tokens"], 4000);
    assert_eq!(
        segments(&value),
        segments(&context(fixture_repo("config-plain").path(), RELAUNCH, &[]))
    );

    // Configured depth 1 applies when `--depth` is absent.
    let shallow = context(temp.path(), RELAUNCH, &["--tokens", "4000"]);
    assert_eq!(
        segments(&shallow),
        expected(&[
            ("target", RELAUNCH, "exact", "full"),
            ("callee", LAUNCH, "scoped", "full"),
        ])
    );
    // Under the configured `never`, the parent is overlap.
    assert_eq!(omitted(&shallow), (0, 1, 0));
}

#[test]
fn an_out_of_range_configured_budget_is_invalid() {
    let temp = fixture_repo("config-zero");
    write(
        temp.path(),
        ".rivet/config.toml",
        "[context]\ndefault_token_budget = 0\n",
    );
    let error = parse_error(&run(temp.path(), &["context", LAUNCH, "--json"]), 2);
    assert_eq!(error["error"], "invalid_arguments");
    assert!(
        error["message"]
            .as_str()
            .expect("message")
            .contains("context.default_token_budget"),
        "{error}"
    );
    assert!(error.get("index").is_none(), "{error}");
    // An explicit `--tokens` does not consult the configured value.
    assert_eq!(
        context(temp.path(), LAUNCH, &["--tokens", "4000"])["budget_tokens"],
        4000
    );
}

// ---------------------------------------------------------------------------
// e. Argument errors precede any filesystem work.
// ---------------------------------------------------------------------------

#[test]
fn invalid_arguments_fail_before_filesystem_work() {
    // Not a repository: no `.git` or `.rivet` here or above. Any filesystem
    // work would fail with exit 3 instead.
    let temp = TempDir::new("not-a-repo");
    let cases: &[&[&str]] = &[
        &["--include-tests", "--exclude-tests"],
        &["--include-callers", "--exclude-callers"],
        &["--include-callees", "--exclude-callees"],
        &["--no-refresh", "--freshness", "content"],
        &["--offset", "0"],
        &["--offset", "5"],
        &["--tokens", "0"],
        &["--tokens", "1000001"],
        &["--limit", "0"],
        &["--limit", "1001"],
        &["--depth", "3"],
        &["--depth", "0"],
        &["--collapse", "sometimes"],
        &["--collapse", "AUTO"],
        &["--freshness", "bogus"],
        &["--tokens", "many"],
    ];
    for case in cases {
        let mut args = vec!["context", LAUNCH];
        args.extend_from_slice(case);
        args.push("--json");
        let error = parse_error(&run(temp.path(), &args), 2);
        assert_eq!(error["error"], "invalid_arguments", "{case:?}: {error}");
        assert_eq!(
            keys(&error),
            vec!["schema_version", "error", "message", "hint"],
            "{case:?}"
        );
    }
    // Nothing was created in the working directory.
    let entries: Vec<_> = fs::read_dir(temp.path()).expect("read dir").collect();
    assert!(entries.is_empty(), "{entries:?}");

    // The same directory without an argument error is exit 3, proving the
    // cases above never reached discovery.
    let error = parse_error(&run(temp.path(), &["context", LAUNCH, "--json"]), 3);
    assert_eq!(error["error"], "repository_unavailable");

    // Valid boundary values are accepted in a real repository.
    let repo = fixture_repo("boundaries");
    let value = context(
        repo.path(),
        LAUNCH,
        &["--tokens", "1000000", "--limit", "1000", "--depth", "1"],
    );
    assert_eq!(value["budget_tokens"], 1_000_000);
}

// ---------------------------------------------------------------------------
// f. Resolution and budget failures.
// ---------------------------------------------------------------------------

#[test]
fn ambiguity_returns_candidates_at_offset_zero() {
    let temp = fixture_repo("ambiguous");
    let error = parse_error(&run(temp.path(), &["context", "launch", "--json"]), 5);
    assert_eq!(error["error"], "ambiguous_symbol");
    assert_eq!(
        keys(&error),
        vec![
            "schema_version",
            "error",
            "message",
            "hint",
            "total",
            "truncated",
            "next_offset",
            "candidates",
            "index",
        ]
    );
    assert_eq!(error["message"], "query 'launch' matched 3 symbols");
    assert!(
        error["hint"]
            .as_str()
            .expect("hint")
            .contains("`rivet symbol`"),
        "{error}"
    );
    assert_eq!(error["total"], 3);
    assert_eq!(error["truncated"], false);
    assert_eq!(error["next_offset"], Value::Null);
    let ids: Vec<&str> = error["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| candidate["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        vec![
            "ReportService.php#App\\Reporting\\ReportService::launch",
            LAUNCH,
            "boot.php#App\\Boot\\launch",
        ]
    );
    assert_eq!(error["index"]["freshness"], "content");

    // `--limit` bounds the page, which still starts at zero.
    let paged = parse_error(
        &run(
            temp.path(),
            &["context", "launch", "--limit", "1", "--json"],
        ),
        5,
    );
    assert_eq!(paged["total"], 3);
    assert_eq!(paged["truncated"], true);
    assert_eq!(paged["next_offset"], 1);
    assert_eq!(
        paged["candidates"][0]["id"],
        "ReportService.php#App\\Reporting\\ReportService::launch"
    );
}

#[test]
fn not_found_returns_suggestions_with_index() {
    let temp = fixture_repo("not-found");
    let error = parse_error(&run(temp.path(), &["context", "launchh", "--json"]), 4);
    assert_eq!(error["error"], "symbol_not_found");
    assert_eq!(
        keys(&error),
        vec![
            "schema_version",
            "error",
            "message",
            "hint",
            "suggestions",
            "index"
        ]
    );
    assert_eq!(
        error["suggestions"],
        serde_json::json!([
            "App\\Boot\\launch",
            "App\\Reporting\\ReportService::launch",
            "App\\Services\\SurveyService::launch",
            "App\\Services\\SurveyService::relaunch",
            "App\\Documented\\Documented::plain",
        ])
    );
    assert_eq!(error["index"]["coverage"]["files_indexed"], 9);
}

#[test]
fn budget_too_small_reports_required_tokens_and_index() {
    let temp = fixture_repo("too-small");
    // Under `auto` the smallest target form is its 10-token signature.
    let error = parse_error(
        &run(temp.path(), &["context", LAUNCH, "--tokens", "9", "--json"]),
        8,
    );
    assert_eq!(error["error"], "budget_too_small");
    assert_eq!(
        keys(&error),
        vec![
            "schema_version",
            "error",
            "message",
            "hint",
            "budget_tokens",
            "required_tokens",
            "index",
        ]
    );
    assert_eq!(error["budget_tokens"], 9);
    assert_eq!(error["required_tokens"], 10);
    assert_eq!(error["index"]["freshness"], "content");
    let snapshot = error["index"]["snapshot"].clone();
    assert!(
        snapshot.as_str().expect("snapshot").starts_with("blake3:"),
        "{snapshot}"
    );

    // Under `never` only the 29-token full body is allowed.
    let never = parse_error(
        &run(
            temp.path(),
            &[
                "context",
                LAUNCH,
                "--tokens",
                "28",
                "--collapse",
                "never",
                "--json",
            ],
        ),
        8,
    );
    assert_eq!(never["budget_tokens"], 28);
    assert_eq!(never["required_tokens"], 29);
    assert_eq!(never["index"]["snapshot"], snapshot);

    // A cached read carries the cached label.
    let cached = parse_error(
        &run(
            temp.path(),
            &["context", LAUNCH, "--tokens", "9", "--no-refresh", "--json"],
        ),
        8,
    );
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["snapshot"], snapshot);
}

// ---------------------------------------------------------------------------
// g. `--no-refresh`.
// ---------------------------------------------------------------------------

#[test]
fn no_refresh_is_labeled_cached() {
    let temp = fixture_repo("cached");
    let refreshed = context(temp.path(), LAUNCH, &[]);
    let cached = context(temp.path(), LAUNCH, &["--no-refresh"]);
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["snapshot"], refreshed["index"]["snapshot"]);
    assert_eq!(cached["segments"], refreshed["segments"]);

    // A cached answer does not see a working-tree edit; a refreshed one does.
    fs::remove_file(temp.path().join("ReportService.php")).expect("remove file");
    let stale = context(temp.path(), LAUNCH, &["--no-refresh"]);
    assert_eq!(stale["segments"], refreshed["segments"]);
    let fresh = context(temp.path(), LAUNCH, &[]);
    assert_eq!(
        segments(&fresh),
        expected(&[
            ("target", LAUNCH, "exact", "full"),
            ("caller", RELAUNCH, "scoped", "full"),
            ("parent", SURVEY_SERVICE, "exact", "signature"),
        ])
    );

    // `--freshness metadata` is accepted and labeled.
    let metadata = context(temp.path(), LAUNCH, &["--freshness", "metadata"]);
    assert_eq!(metadata["index"]["freshness"], "metadata");
}

#[test]
fn no_refresh_refuses_a_missing_or_incompatible_cache() {
    let temp = fixture_repo("cache-missing");
    let error = parse_error(
        &run(temp.path(), &["context", LAUNCH, "--no-refresh", "--json"]),
        3,
    );
    assert_eq!(error["error"], "repository_unavailable");
    assert!(error.get("index").is_none(), "{error}");
    assert!(!temp.path().join(".rivet").join("index.db").exists());

    let temp = fixture_repo("cache-incompatible");
    parse_success(&run(temp.path(), &["index", "--json"]));
    let store = rivet_store::Store::open(&temp.path().join(".rivet")).expect("open store");
    store
        .set_meta("extractor_fingerprint", "older-extractor")
        .expect("set meta");
    drop(store);
    let error = parse_error(
        &run(temp.path(), &["context", LAUNCH, "--no-refresh", "--json"]),
        3,
    );
    assert_eq!(error["error"], "repository_unavailable");
    assert_eq!(
        keys(&error),
        vec!["schema_version", "error", "message", "hint"]
    );
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("extractor_fingerprint"), "{message}");
    assert!(message.contains("older-extractor"), "{message}");
}

// ---------------------------------------------------------------------------
// h. Determinism.
// ---------------------------------------------------------------------------

#[test]
fn repeated_invocations_are_byte_identical() {
    let temp = fixture_repo("determinism");
    for extra in [
        &[][..],
        &["--tokens", "60"][..],
        &["--collapse", "always", "--depth", "1"][..],
    ] {
        let mut args = vec!["context", RELAUNCH];
        args.extend_from_slice(extra);
        args.push("--json");
        let first = run(temp.path(), &args);
        let second = run(temp.path(), &args);
        assert_eq!(first.status.code(), Some(0), "{extra:?}");
        assert_eq!(first.stdout, second.stdout, "{extra:?}");
        assert!(first.stderr.is_empty() && second.stderr.is_empty());
    }

    // A fresh copy of the same fixture yields the same bytes.
    let other = fixture_repo("determinism-copy");
    assert_eq!(
        run(temp.path(), &["context", LAUNCH, "--json"]).stdout,
        run(other.path(), &["context", LAUNCH, "--json"]).stdout
    );
}

// ---------------------------------------------------------------------------
// Human mode (T34, spec §16.1): a header per segment followed by its
// source; `human_output.rs` holds the full golden. Human errors.
// ---------------------------------------------------------------------------

#[test]
fn human_mode_prints_a_compact_summary() {
    let temp = fixture_repo("human");
    let output = run(temp.path(), &["context", LAUNCH]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).expect("UTF-8");
    let headers: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("── ") || line.starts_with("estimated_tokens"))
        .collect();
    assert_eq!(
        headers,
        vec![
            "── SurveyService.php:18-21  App\\Services\\SurveyService::launch  [target, full]",
            "── ReportService.php:21-25  App\\Reporting\\ReportService::runAlias  [caller, full]",
            "── ReportService.php:28-31  App\\Reporting\\ReportService::runTyped  [caller, full]",
            "── SurveyService.php:24-27  App\\Services\\SurveyService::relaunch  [caller, full]",
            "── ReportService.php:34-37  App\\Reporting\\ReportService::runUnknown  [caller, full] ?",
            "── SurveyService.php:9-28  App\\Services\\SurveyService  [parent, signature]",
            "estimated_tokens: 175 / 4000  (tokenizer: utf8-bytes-v1; budget_scope: source)",
        ]
    );

    let error = run(temp.path(), &["context", LAUNCH, "--tokens", "9"]);
    assert_eq!(error.status.code(), Some(8));
    assert!(error.stdout.is_empty());
    let stderr = String::from_utf8(error.stderr).expect("UTF-8");
    assert!(stderr.starts_with("rivet context: "), "{stderr}");
}
