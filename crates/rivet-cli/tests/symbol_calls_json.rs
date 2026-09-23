//! Integration tests for the `rivet symbol` `calls`/`called_by` lists (T24).
//!
//! SY1 made both lists default to `--min-resolution scoped`; tests about a
//! list's full contents pass `--min-resolution name_match`, and
//! `symbol_tiers.rs` covers the default and `hidden_name_match`.
//!
//! The lists reuse the T23 reference pipeline (`crate::references`), so these
//! tests assert exact spans, target IDs, resolutions, and containing symbols
//! rather than only counts. They drive the built binary against a temporary
//! copy of the authored PHP fixture (plus a `.git` boundary) and against small
//! purpose-built fixtures, exactly like `refs_json.rs`.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// Gold targets in the authored fixture.
const LAUNCH_TARGET: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH_TARGET: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const RUN_ALIAS_TARGET: &str = "ReportService.php#App\\Reporting\\ReportService::runAlias";
const RUN_TYPED_TARGET: &str = "ReportService.php#App\\Reporting\\ReportService::runTyped";
const RUN_UNKNOWN_TARGET: &str = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
const BOOT_LAUNCH_TARGET: &str = "boot.php#App\\Boot\\launch";

/// The eleven reference-object keys in contract order.
const REFERENCE_KEYS: [&str; 11] = [
    "file",
    "content_hash",
    "start_byte",
    "end_byte",
    "line",
    "column",
    "containing_symbol",
    "ref_kind",
    "resolution",
    "resolved_target",
    "receiver",
];

/// The five call-list keys in contract order (`hidden_name_match` is SY1's).
const CALL_LIST_KEYS: [&str; 5] = [
    "total",
    "truncated",
    "next_offset",
    "hidden_name_match",
    "items",
];

/// The flag that lists name-only rows too, the pre-SY1 default.
const ALL_TIERS: [&str; 2] = ["--min-resolution", "name_match"];

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
            "rivet-symbol-calls-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root holding one authored PHP source, returned with the
/// source so a test can derive exact spans from it.
fn custom_repo(label: &str, name: &str, source: &str) -> (TempDir, String) {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    fs::write(temp.path().join(name), source.as_bytes()).expect("write PHP fixture");
    (temp, source.to_string())
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs `symbol <query>` with extra flags, requiring success.
fn symbol(dir: &Path, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["symbol", query];
    args.extend_from_slice(extra);
    args.push("--json");
    parse_success(&run(dir, &args))
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

/// One reference item's `(file, start_byte, end_byte, ref_kind, resolution,
/// resolved_target, containing_symbol.id)`.
type ItemSpan = (
    String,
    u64,
    u64,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// The borrowed shape used to write an expected item inline.
type ExpectedItem<'a> = (&'a str, u64, u64, &'a str, Option<&'a str>, Option<&'a str>);

/// The `(file, start_byte, end_byte, ref_kind, resolution, resolved_target,
/// containing_symbol.id)` of each item in `list`, in output order.
fn item_spans(list: &Value) -> Vec<ItemSpan> {
    list["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().unwrap().to_string(),
                item["start_byte"].as_u64().unwrap(),
                item["end_byte"].as_u64().unwrap(),
                item["ref_kind"].as_str().unwrap().to_string(),
                item["resolution"].as_str().unwrap().to_string(),
                item["resolved_target"].as_str().map(str::to_string),
                item["containing_symbol"]["id"].as_str().map(str::to_string),
            )
        })
        .collect()
}

/// Every byte offset of `needle` in `source`, in order.
fn offsets_of(source: &str, needle: &str) -> Vec<u64> {
    source
        .match_indices(needle)
        .map(|(offset, _)| offset as u64)
        .collect()
}

#[test]
fn calls_lists_the_callees_contained_by_the_target() {
    let temp = fixture_repo("calls");

    // `relaunch` contains exactly one call, scoped to `launch`.
    let value = symbol(temp.path(), "App\\Services\\SurveyService::relaunch", &[]);
    assert_eq!(value["symbol"]["id"], RELAUNCH_TARGET);
    assert_eq!(value["calls"]["total"], 1);
    assert_eq!(value["calls"]["truncated"], false);
    assert!(value["calls"]["next_offset"].is_null());
    let calls = item_spans(&value["calls"]);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            "SurveyService.php".to_string(),
            640,
            646,
            "call".to_string(),
            "scoped".to_string(),
            Some(LAUNCH_TARGET.to_string()),
            Some(RELAUNCH_TARGET.to_string()),
        )
    );
    let item = &value["calls"]["items"][0];
    assert_eq!(item["line"], 26);
    assert_eq!(item["column"], 16);
    assert_eq!(item["receiver"], "$this");
    assert!(
        item["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:"),
        "{item}"
    );

    // `runAlias` also holds a `type` use ([564, 573)); `calls` keeps only the
    // call site.
    let run_alias = symbol(temp.path(), "App\\Reporting\\ReportService::runAlias", &[]);
    assert_eq!(run_alias["calls"]["total"], 1);
    let calls = item_spans(&run_alias["calls"]);
    assert_eq!(
        calls[0],
        (
            "ReportService.php".to_string(),
            591,
            597,
            "call".to_string(),
            "scoped".to_string(),
            Some(LAUNCH_TARGET.to_string()),
            Some(RUN_ALIAS_TARGET.to_string()),
        )
    );

    // An untyped receiver keeps `name_match` and a null target.
    let run_unknown = symbol(
        temp.path(),
        "App\\Reporting\\ReportService::runUnknown",
        &ALL_TIERS,
    );
    let calls = item_spans(&run_unknown["calls"]);
    assert_eq!(
        calls[0],
        (
            "ReportService.php".to_string(),
            862,
            868,
            "call".to_string(),
            "name_match".to_string(),
            None,
            Some(RUN_UNKNOWN_TARGET.to_string()),
        )
    );

    // A method with no extracted call uses reports an empty list.
    let launch = symbol(temp.path(), "App\\Services\\SurveyService::launch", &[]);
    assert_eq!(
        launch["calls"],
        json!({
            "total": 0,
            "truncated": false,
            "next_offset": null,
            "hidden_name_match": 0,
            "items": []
        })
    );
}

#[test]
fn called_by_returns_each_call_site_with_its_containing_symbol() {
    let temp = fixture_repo("called-by");
    let value = symbol(
        temp.path(),
        "App\\Services\\SurveyService::launch",
        &ALL_TIERS,
    );

    assert_eq!(value["called_by"]["total"], 6);
    assert_eq!(value["called_by"]["truncated"], false);
    assert!(value["called_by"]["next_offset"].is_null());

    let expected: [ExpectedItem; 6] = [
        (
            "ReportService.php",
            591,
            597,
            "scoped",
            Some(LAUNCH_TARGET),
            Some(RUN_ALIAS_TARGET),
        ),
        (
            "ReportService.php",
            736,
            742,
            "scoped",
            Some(LAUNCH_TARGET),
            Some(RUN_TYPED_TARGET),
        ),
        (
            "ReportService.php",
            862,
            868,
            "name_match",
            None,
            Some(RUN_UNKNOWN_TARGET),
        ),
        (
            "SurveyService.php",
            640,
            646,
            "scoped",
            Some(LAUNCH_TARGET),
            Some(RELAUNCH_TARGET),
        ),
        ("boot.php", 267, 273, "scoped", Some(LAUNCH_TARGET), None),
        ("boot.php", 430, 436, "scoped", Some(LAUNCH_TARGET), None),
    ];
    let items = item_spans(&value["called_by"]);
    assert_eq!(items.len(), expected.len());
    for (item, (file, start, end, resolution, target, container)) in items.iter().zip(expected) {
        assert_eq!(item.0, file);
        assert_eq!(item.1, start);
        assert_eq!(item.2, end);
        assert_eq!(item.3, "call");
        assert_eq!(item.4, resolution);
        assert_eq!(item.5.as_deref(), target);
        assert_eq!(item.6.as_deref(), container);
    }

    // A same-name use bound to a different declaration stays excluded (the
    // reference-mode rule).
    assert!(
        !items.iter().any(|item| item.1 == 179),
        "the boot.php call bound to App\\Boot\\launch must not be a caller"
    );
}

#[test]
fn repeated_call_sites_are_counted_individually() {
    let source = "<?php\ndeclare(strict_types=1);\nnamespace Repeated;\n\nfunction leaf(): void\n{\n}\n\nfunction caller(): void\n{\n    leaf();\n    leaf();\n}\n";
    let (temp, source) = custom_repo("repeated", "repeated.php", source);
    let offsets = offsets_of(&source, "leaf();");
    assert_eq!(offsets.len(), 2, "fixture must hold two call sites");

    // The callee is called twice from one caller: two items, not one.
    let leaf = symbol(temp.path(), "Repeated\\leaf", &[]);
    assert_eq!(leaf["called_by"]["total"], 2, "{leaf}");
    let callers = item_spans(&leaf["called_by"]);
    assert_eq!(callers.len(), 2);
    for (offset, caller) in offsets.iter().zip(&callers) {
        assert_eq!(caller.0, "repeated.php");
        assert_eq!(caller.1, *offset);
        // The extracted span is the identifier only (`leaf`, four bytes).
        assert_eq!(caller.2, offset + 4);
        assert_eq!(caller.3, "call");
        assert_eq!(caller.4, "exact");
        assert_eq!(caller.5.as_deref(), Some("repeated.php#Repeated\\leaf"));
        // Both call sites share the single caller.
        assert_eq!(caller.6.as_deref(), Some("repeated.php#Repeated\\caller"));
    }

    // The caller reports the same two call sites in `calls`.
    let caller = symbol(temp.path(), "Repeated\\caller", &[]);
    assert_eq!(caller["calls"]["total"], 2, "{caller}");
    let calls = item_spans(&caller["calls"]);
    assert_eq!(calls.len(), 2);
    for (offset, call) in offsets.iter().zip(&calls) {
        assert_eq!(call.1, *offset);
        assert_eq!(call.2, offset + 4);
        assert_eq!(call.3, "call");
        assert_eq!(call.4, "exact");
        assert_eq!(call.5.as_deref(), Some("repeated.php#Repeated\\leaf"));
        assert_eq!(call.6.as_deref(), Some("repeated.php#Repeated\\caller"));
    }
}

#[test]
fn top_level_calls_are_retained_in_called_by() {
    let temp = fixture_repo("top-level");

    // The `boot.php` function is called once at file scope, and that call is
    // retained with a null containing symbol. The unresolved same-name
    // `$x->launch()` use is a method call, which cannot name a function, so
    // reference mode excludes it by its form (LR2).
    let value = symbol(temp.path(), "App\\Boot\\launch", &[]);
    assert_eq!(value["symbol"]["id"], BOOT_LAUNCH_TARGET);
    assert_eq!(value["called_by"]["total"], 1);
    let top_level = value["called_by"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == "boot.php")
        .expect("the top-level call");
    assert_eq!(top_level["start_byte"], 179);
    assert_eq!(top_level["end_byte"], 185);
    assert_eq!(top_level["ref_kind"], "call");
    assert_eq!(top_level["resolution"], "exact");
    assert_eq!(top_level["resolved_target"], BOOT_LAUNCH_TARGET);
    assert!(
        top_level["containing_symbol"].is_null(),
        "a top-level use has containing_symbol null: {top_level}"
    );

    // `launch` is also reached from two top-level uses in `boot.php`.
    let launch = symbol(
        temp.path(),
        "App\\Services\\SurveyService::launch",
        &ALL_TIERS,
    );
    let top_level: Vec<&Value> = launch["called_by"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["containing_symbol"].is_null())
        .collect();
    assert_eq!(top_level.len(), 2, "{launch}");
    assert_eq!(top_level[0]["start_byte"], 267);
    assert_eq!(top_level[1]["start_byte"], 430);
}

#[test]
fn call_lists_paginate_independently() {
    let source = "<?php\ndeclare(strict_types=1);\nnamespace Page;\n\nfunction alpha(): void {}\nfunction beta(): void {}\nfunction gamma(): void {}\n\nfunction hub(): void\n{\n    alpha();\n    beta();\n    gamma();\n}\n\nfunction caller1(): void { hub(); }\nfunction caller2(): void { hub(); }\nfunction caller3(): void { hub(); }\nfunction caller4(): void { hub(); }\nfunction caller5(): void { hub(); }\n";
    let (temp, _source) = custom_repo("paging", "paging.php", source);

    // calls has three items; called_by has five. They must page separately.
    let all = symbol(temp.path(), "Page\\hub", &[]);
    assert_eq!(all["calls"]["total"], 3);
    assert_eq!(all["called_by"]["total"], 5);

    // limit 2: both lists page, each with its own next_offset.
    let first = symbol(temp.path(), "Page\\hub", &["--limit", "2"]);
    assert_eq!(first["calls"]["total"], 3);
    assert_eq!(first["calls"]["truncated"], true);
    assert_eq!(first["calls"]["next_offset"], 2);
    assert_eq!(first["calls"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["called_by"]["total"], 5);
    assert_eq!(first["called_by"]["truncated"], true);
    assert_eq!(first["called_by"]["next_offset"], 2);
    assert_eq!(first["called_by"]["items"].as_array().unwrap().len(), 2);

    // limit 3: calls fits exactly (next_offset null) while called_by still
    // pages. This is the independence assertion: one list's next_offset is set
    // while the other's is null.
    let third = symbol(temp.path(), "Page\\hub", &["--limit", "3"]);
    assert_eq!(third["calls"]["total"], 3);
    assert_eq!(third["calls"]["truncated"], false);
    assert!(third["calls"]["next_offset"].is_null(), "{third}");
    assert_eq!(third["called_by"]["total"], 5);
    assert_eq!(third["called_by"]["truncated"], true);
    assert_eq!(third["called_by"]["next_offset"], 3, "{third}");

    // The later pages continue without overlap or gaps.
    let calls_tail = symbol(temp.path(), "Page\\hub", &["--limit", "2", "--offset", "2"]);
    assert_eq!(calls_tail["calls"]["total"], 3);
    assert_eq!(calls_tail["calls"]["truncated"], true);
    assert!(calls_tail["calls"]["next_offset"].is_null());
    assert_eq!(calls_tail["calls"]["items"].as_array().unwrap().len(), 1);

    let called_by_tail = symbol(temp.path(), "Page\\hub", &["--limit", "2", "--offset", "2"]);
    assert_eq!(called_by_tail["called_by"]["total"], 5);
    assert_eq!(called_by_tail["called_by"]["truncated"], true);
    assert_eq!(called_by_tail["called_by"]["next_offset"], 4);
    assert_eq!(
        called_by_tail["called_by"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Reconstruct both full lists from their pages, in order.
    let mut calls: Vec<ItemSpan> = item_spans(&first["calls"]);
    calls.extend(item_spans(&calls_tail["calls"]));
    assert_eq!(calls, item_spans(&all["calls"]));

    let mut called_by = item_spans(&first["called_by"]);
    called_by.extend(item_spans(&called_by_tail["called_by"]));
    let last = symbol(temp.path(), "Page\\hub", &["--limit", "2", "--offset", "4"]);
    called_by.extend(item_spans(&last["called_by"]));
    assert!(last["called_by"]["next_offset"].is_null());
    assert_eq!(called_by, item_spans(&all["called_by"]));
}

#[test]
fn minimum_resolution_filters_both_lists() {
    let source = "<?php\ndeclare(strict_types=1);\nnamespace Min;\n\nfinal class Service\n{\n    public function callee(): void\n    {\n    }\n\n    public function hub(): void\n    {\n        $this->callee();\n        $x->callee();\n    }\n}\n\nfunction wrapperA(): void\n{\n    $svc = new \\Min\\Service();\n    $svc->hub();\n}\n\nfunction wrapperB(): void\n{\n    $y->hub();\n}\n";
    let (temp, source) = custom_repo("min-resolution", "mins.php", source);

    // `name_match`: both lists keep the scoped and the unresolved name match.
    let all = symbol(temp.path(), "Min\\Service::hub", &ALL_TIERS);
    assert_eq!(all["calls"]["total"], 2);
    assert_eq!(all["called_by"]["total"], 2);
    assert_eq!(all["calls"]["hidden_name_match"], 0);
    assert_eq!(all["called_by"]["hidden_name_match"], 0);
    let calls = item_spans(&all["calls"]);
    assert_eq!(calls[0].4, "scoped");
    assert_eq!(calls[0].5.as_deref(), Some("mins.php#Min\\Service::callee"));
    assert_eq!(calls[1].4, "name_match");
    assert!(calls[1].5.is_none());

    // `scoped` filters both lists and both totals, and is the default (SY1).
    let scoped = symbol(
        temp.path(),
        "Min\\Service::hub",
        &["--min-resolution", "scoped"],
    );
    assert_eq!(
        symbol(temp.path(), "Min\\Service::hub", &[]),
        scoped,
        "the default is scoped"
    );
    assert_eq!(scoped["calls"]["total"], 1, "{scoped}");
    assert_eq!(scoped["called_by"]["total"], 1, "{scoped}");
    assert_eq!(scoped["calls"]["hidden_name_match"], 1, "{scoped}");
    assert_eq!(scoped["called_by"]["hidden_name_match"], 1, "{scoped}");
    let calls = item_spans(&scoped["calls"]);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, offsets_of(&source, "callee();")[0]);
    assert_eq!(calls[0].4, "scoped");
    assert_eq!(calls[0].5.as_deref(), Some("mins.php#Min\\Service::callee"));
    assert_eq!(calls[0].6.as_deref(), Some("mins.php#Min\\Service::hub"));
    let callers = item_spans(&scoped["called_by"]);
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].1, offsets_of(&source, "hub();")[0]);
    assert_eq!(callers[0].4, "scoped");
    assert_eq!(callers[0].5.as_deref(), Some("mins.php#Min\\Service::hub"));
    assert_eq!(callers[0].6.as_deref(), Some("mins.php#Min\\wrapperA"));
}

#[test]
fn signature_and_source_flags_shape_the_lists() {
    let temp = fixture_repo("signature-only");

    // `--signature-only` omits both keys entirely.
    let signature_only = symbol(
        temp.path(),
        "App\\Services\\SurveyService::launch",
        &["--signature-only"],
    );
    assert!(signature_only.get("calls").is_none(), "{signature_only}");
    assert!(
        signature_only.get("called_by").is_none(),
        "{signature_only}"
    );
    assert!(signature_only.get("source").is_none(), "{signature_only}");

    // `--source` adds the source slice and keeps both lists.
    let source = symbol(
        temp.path(),
        "App\\Services\\SurveyService::launch",
        &["--source"],
    );
    assert!(source.get("source").is_some(), "{source}");
    assert_eq!(source["calls"]["total"], 0);
    // Five scoped callers listed; the name-only one is counted (SY1).
    assert_eq!(source["called_by"]["total"], 5);
    assert_eq!(source["called_by"]["hidden_name_match"], 1);

    // `--source` with `--signature-only` is invalid before any filesystem work.
    let conflict = run(
        temp.path(),
        &["symbol", "launch", "--source", "--signature-only", "--json"],
    );
    let error = parse_error(&conflict, 2);
    assert_eq!(error["error"], "invalid_arguments");

    // An unknown `--min-resolution` is also invalid before filesystem work.
    let bad = run(
        temp.path(),
        &["symbol", "launch", "--min-resolution", "strong", "--json"],
    );
    let error = parse_error(&bad, 2);
    assert_eq!(error["error"], "invalid_arguments");
}

#[test]
fn call_objects_use_contract_key_order() {
    let temp = fixture_repo("key-order");
    let value = symbol(temp.path(), "App\\Services\\SurveyService::launch", &[]);

    let keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "schema_version",
            "index",
            "symbol",
            "signature",
            "doc_comment",
            "parent",
            "calls",
            "called_by",
        ],
        "top-level success key order"
    );

    for list in ["calls", "called_by"] {
        let keys: Vec<&str> = value[list]
            .as_object()
            .expect("call list object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, CALL_LIST_KEYS, "{list} key order");
    }

    let item = &value["called_by"]["items"][0];
    let keys: Vec<&str> = item
        .as_object()
        .expect("reference object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, REFERENCE_KEYS, "reference object key order");
}

#[test]
fn repeated_symbol_queries_are_byte_identical() {
    let temp = fixture_repo("determinism");
    let first = run(
        temp.path(),
        &["symbol", "App\\Services\\SurveyService::launch", "--json"],
    );
    assert_eq!(first.status.code(), Some(0));
    let second = run(
        temp.path(),
        &["symbol", "App\\Services\\SurveyService::launch", "--json"],
    );
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(
        first.stdout, second.stdout,
        "the same query must serialize byte-identically"
    );
}
