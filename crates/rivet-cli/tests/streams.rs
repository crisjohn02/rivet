//! T36 acceptance matrix, "Output": stream discipline for every command,
//! empty results, exact limit/offset boundaries, and ambiguity pages
//! (spec §§20.1, 20.4, 21; OUTPUT-CONTRACT "Transport and common rules",
//! "Pagination and resolution", "Errors").
//!
//! `support::success` and `support::failure` hold every run to the contract:
//! exactly one compact JSON object and one LF on the chosen stream, and
//! nothing on the other.

mod support;

use serde_json::{Value, json};
use support::{TempDir, failure, fixture_repo, git_repo, run, success, write};

const LAUNCH: &str = "App\\Services\\SurveyService::launch";

/// A repository with one of each kind of direct-target failure.
fn error_repo() -> TempDir {
    let temp = git_repo("stream-errors");
    let root = temp.path();
    write(
        root,
        "good.php",
        b"<?php\nnamespace App;\nfunction good(): void { good2(); }\nfunction good2(): void {}\nclass A { function dup() {} }\nclass B { function dup() {} }\n",
    );
    write(
        root,
        "broken.php",
        b"<?php\nnamespace App;\nfunction broken( {\n",
    );
    write(root, "notes.txt", b"plain text\n");
    temp
}

/// Every command's success writes one JSON object and LF to stdout and
/// nothing to stderr, including the flag variants that change the shape.
#[test]
fn every_command_success_is_one_json_line_on_stdout_and_nothing_on_stderr() {
    let temp = fixture_repo("stream-success");
    let root = temp.path();
    let cases: Vec<Vec<&str>> = vec![
        vec!["init", "--json"],
        vec!["init", "--write-snippet", "--json"],
        vec!["index", "--json"],
        vec!["index", "--force", "--json"],
        vec!["index", "--timing", "--json"],
        vec!["index", "--freshness", "metadata", "--json"],
        vec!["symbol", LAUNCH, "--json"],
        vec!["symbol", LAUNCH, "--source", "--json"],
        vec!["symbol", LAUNCH, "--signature-only", "--json"],
        vec!["symbol", LAUNCH, "--no-refresh", "--json"],
        vec!["symbol", "SurveyService.php:20", "--json"],
        vec!["refs", LAUNCH, "--json"],
        vec![
            "refs",
            LAUNCH,
            "--mode",
            "candidates",
            "--kind",
            "call",
            "--json",
        ],
        vec!["refs", LAUNCH, "--min-resolution", "exact", "--json"],
        vec!["context", LAUNCH, "--json"],
        vec![
            "context",
            LAUNCH,
            "--tokens",
            "40",
            "--collapse",
            "always",
            "--json",
        ],
        vec![
            "context",
            LAUNCH,
            "--depth",
            "1",
            "--exclude-callers",
            "--json",
        ],
        vec!["snippet", "--json"],
    ];
    for args in &cases {
        let value = success(&run(root, args));
        match args[0] {
            "init" => assert!(value["created"].is_array(), "{args:?}"),
            "snippet" => assert!(value["snippet"].is_string(), "{args:?}"),
            _ => assert!(value["index"]["snapshot"].is_string(), "{args:?}"),
        }
    }
    // `--timing` is the only field outside the fixed shape, and only on index.
    let timed = success(&run(root, &["index", "--timing", "--json"]));
    assert!(timed["elapsed_ms"].is_u64(), "{timed}");
    let untimed = success(&run(root, &["index", "--json"]));
    assert!(untimed.get("elapsed_ms").is_none());
}

/// Every documented exit code a command can produce from a single process
/// writes one JSON error object and LF to stderr and nothing to stdout, with
/// the documented `error` string. Exit 9 needs a concurrent writer and is
/// covered by `concurrency::continued_mutation_is_exit_9_and_keeps_the_previous_snapshot`.
#[test]
fn every_command_failure_is_one_json_error_on_stderr_and_nothing_on_stdout() {
    let repo = error_repo();
    let outside = TempDir::new("stream-outside");
    let cases: Vec<(&TempDir, Vec<&str>, i32, &str)> = vec![
        // 2: argument errors, for every command.
        (
            &repo,
            vec!["init", "--bogus", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["init", "--snippet-file", "AGENTS.md", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["index", "--no-refresh", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["index", "stray", "--json"],
            2,
            "invalid_arguments",
        ),
        (&repo, vec!["symbol", "--json"], 2, "invalid_arguments"),
        (
            &repo,
            vec!["symbol", "x", "--source", "--signature-only", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["refs", "x", "--limit", "1001", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["refs", "x", "--offset", "-1", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["context", "x", "--offset", "1", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["context", "x", "--tokens", "-1", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["context", "x", "--tokens", "0", "--json"],
            2,
            "invalid_arguments",
        ),
        (
            &repo,
            vec!["snippet", "extra", "--json"],
            2,
            "invalid_arguments",
        ),
        (&repo, vec!["bogus", "--json"], 2, "invalid_arguments"),
        (&repo, vec!["--json"], 2, "invalid_arguments"),
        // `--help`/`--version` cannot be combined with `--json` (T36 fix).
        (&repo, vec!["--help", "--json"], 2, "invalid_arguments"),
        (&repo, vec!["--version", "--json"], 2, "invalid_arguments"),
        (
            &repo,
            vec!["symbol", "--help", "--json"],
            2,
            "invalid_arguments",
        ),
        // 3: no repository, for every repository-dependent command.
        (
            &outside,
            vec!["index", "--json"],
            3,
            "repository_unavailable",
        ),
        (
            &outside,
            vec!["symbol", "x", "--json"],
            3,
            "repository_unavailable",
        ),
        (
            &outside,
            vec!["refs", "x", "--json"],
            3,
            "repository_unavailable",
        ),
        (
            &outside,
            vec!["context", "x", "--json"],
            3,
            "repository_unavailable",
        ),
        // 4, 5, 6, 7 for each query command.
        (
            &repo,
            vec!["symbol", "App\\missing", "--json"],
            4,
            "symbol_not_found",
        ),
        (
            &repo,
            vec!["refs", "App\\missing", "--json"],
            4,
            "symbol_not_found",
        ),
        (
            &repo,
            vec!["context", "App\\missing", "--json"],
            4,
            "symbol_not_found",
        ),
        (
            &repo,
            vec!["symbol", "dup", "--json"],
            5,
            "ambiguous_symbol",
        ),
        (&repo, vec!["refs", "dup", "--json"], 5, "ambiguous_symbol"),
        (
            &repo,
            vec!["context", "dup", "--json"],
            5,
            "ambiguous_symbol",
        ),
        (
            &repo,
            vec!["symbol", "broken.php:3", "--json"],
            6,
            "parse_failure",
        ),
        (
            &repo,
            vec!["refs", "broken.php:3", "--json"],
            6,
            "parse_failure",
        ),
        (
            &repo,
            vec!["context", "broken.php:3", "--json"],
            6,
            "parse_failure",
        ),
        (
            &repo,
            vec!["symbol", "notes.txt:1", "--json"],
            7,
            "unsupported_language",
        ),
        (
            &repo,
            vec!["refs", "notes.txt:1", "--json"],
            7,
            "unsupported_language",
        ),
        (
            &repo,
            vec!["context", "notes.txt:1", "--json"],
            7,
            "unsupported_language",
        ),
        // 8: the target cannot fit.
        (
            &repo,
            vec!["context", "App\\good", "--tokens", "1", "--json"],
            8,
            "budget_too_small",
        ),
    ];
    for (dir, args, exit, code) in &cases {
        let error = failure(&run(dir.path(), args), *exit);
        assert_eq!(error["error"], *code, "{args:?}: {error}");
        assert!(!error["hint"].as_str().unwrap().is_empty(), "{args:?}");
    }
    // Without `--json`, help and version stay text on stdout with exit 0.
    for args in [vec!["--help"], vec!["--version"], vec!["symbol", "--help"]] {
        let output = run(repo.path(), &args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert!(!output.stdout.is_empty(), "{args:?}");
        assert!(
            serde_json::from_slice::<Value>(&output.stdout).is_err(),
            "{args:?} stays text"
        );
    }
    // An argument error names the command it came from in its hint.
    let error = failure(&run(repo.path(), &["refs", "x", "--bogus", "--json"]), 2);
    assert_eq!(error["hint"], "Run `rivet refs --help` for usage.");
    let error = failure(&run(repo.path(), &["bogus", "--json"]), 2);
    assert_eq!(error["hint"], "Run `rivet --help` for usage.");
}

/// Empty results are present, empty lists with zero counts, never nulls or
/// missing fields.
#[test]
fn empty_results_are_empty_lists_with_zero_counts() {
    let temp = git_repo("stream-empty");
    let root = temp.path();

    // An empty repository indexes to complete, zero coverage.
    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(
        index["index"]["coverage"],
        json!({
            "complete": true, "files_seen": 0, "files_indexed": 0,
            "skipped": {
                "unsupported": 0, "binary": 0, "size": 0, "encoding": 0,
                "parse_error": 0, "resource_limit": 0,
            },
        })
    );
    assert_eq!(
        index["index"]["diagnostics"],
        json!({"total": 0, "truncated": false, "items": []})
    );
    for key in [
        "symbols",
        "uses",
        "bindings",
        "updated",
        "unchanged",
        "deleted",
    ] {
        assert_eq!(index[key], 0, "{key}");
    }
    let error = failure(&run(root, &["symbol", "anything", "--json"]), 4);
    assert_eq!(error["suggestions"], json!([]));

    // A lone symbol with no uses: every list is empty.
    write(
        root,
        "a.php",
        b"<?php\nnamespace App;\nfunction lone(): void {}\n",
    );
    let refs = success(&run(root, &["refs", "App\\lone", "--json"]));
    assert_eq!(
        (
            &refs["total"],
            &refs["truncated"],
            &refs["next_offset"],
            &refs["references"]
        ),
        (&json!(0), &json!(false), &Value::Null, &json!([]))
    );
    assert_eq!(
        refs["by_resolution"],
        json!({"exact": 0, "scoped": 0, "name_match": 0})
    );
    let beyond = success(&run(
        root,
        &["refs", "App\\lone", "--offset", "5", "--json"],
    ));
    assert_eq!(beyond["references"], json!([]));
    assert_eq!(
        beyond["truncated"], false,
        "nothing exists outside the page"
    );
    let candidates = success(&run(
        root,
        &["refs", "App\\lone", "--mode", "candidates", "--json"],
    ));
    assert_eq!(candidates["references"], json!([]));

    let symbol = success(&run(root, &["symbol", "App\\lone", "--json"]));
    let empty_list = json!({"total": 0, "truncated": false, "next_offset": null, "items": []});
    assert_eq!(symbol["calls"], empty_list);
    assert_eq!(symbol["called_by"], empty_list);
    assert_eq!(symbol["doc_comment"], Value::Null);

    let context = success(&run(root, &["context", "App\\lone", "--json"]));
    let segments = context["segments"].as_array().expect("segments");
    assert_eq!(segments.len(), 1, "only the target: {context}");
    assert_eq!(segments[0]["reason"], "target");
    assert_eq!(
        context["omitted"],
        json!({"budget": 0, "overlap": 0, "limit": 0})
    );
    assert_eq!(context["candidate_limit_reached"], false);
}

/// `(file, start_byte)` of each reference, in order.
fn spans(value: &Value) -> Vec<(String, u64)> {
    value["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().unwrap().to_string(),
                item["start_byte"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// `--limit` and `--offset` at their exact boundaries: a page that ends at
/// the last match is not followed by a `next_offset`, one short of the end
/// is, the last single-item page is truncated only by earlier pages, and the
/// accepted ranges are exactly 1–1000 and 0–u64::MAX.
#[test]
fn limit_and_offset_are_exact_at_their_boundaries() {
    let temp = fixture_repo("stream-bounds");
    let root = temp.path();
    let page = |extra: &[&str]| {
        let mut args = vec!["refs", LAUNCH];
        args.extend_from_slice(extra);
        args.push("--json");
        success(&run(root, &args))
    };
    let all = page(&[]);
    assert_eq!(all["total"], 6);
    assert_eq!(all["truncated"], false);
    assert!(all["next_offset"].is_null());
    let every = spans(&all);

    let exact = page(&["--limit", "6"]);
    assert_eq!(
        (&exact["truncated"], &exact["next_offset"]),
        (&json!(false), &Value::Null)
    );
    assert_eq!(spans(&exact), every);

    let short = page(&["--limit", "5"]);
    assert_eq!(
        (&short["truncated"], &short["next_offset"]),
        (&json!(true), &json!(5))
    );
    assert_eq!(spans(&short), every[..5].to_vec());

    let last = page(&["--offset", "5"]);
    assert_eq!(
        (&last["truncated"], &last["next_offset"]),
        (&json!(true), &Value::Null)
    );
    assert_eq!(spans(&last), every[5..].to_vec());

    let past = page(&["--offset", "6"]);
    assert_eq!(spans(&past), Vec::<(String, u64)>::new());
    assert_eq!(
        (&past["total"], &past["next_offset"]),
        (&json!(6), &Value::Null)
    );

    let one = page(&["--limit", "1", "--offset", "5"]);
    assert_eq!(spans(&one), every[5..].to_vec());
    assert!(one["next_offset"].is_null());

    assert_eq!(spans(&page(&["--limit", "1000"])), every);
    assert_eq!(spans(&page(&["--offset", "0"])), every);
    let max = page(&["--offset", "18446744073709551615"]);
    assert_eq!(max["references"], json!([]));
    assert_eq!(max["total"], 6);

    for bad in [
        vec!["--limit", "0"],
        vec!["--limit", "1001"],
        vec!["--limit", "-1"],
        vec!["--offset", "-1"],
        vec!["--offset", "18446744073709551616"],
    ] {
        let mut args = vec!["refs", LAUNCH];
        args.extend_from_slice(&bad);
        args.push("--json");
        let error = failure(&run(root, &args), 2);
        assert_eq!(error["error"], "invalid_arguments", "{bad:?}");
    }

    // The same boundaries on a symbol call list.
    let callers = |extra: &[&str]| {
        let mut args = vec!["symbol", LAUNCH];
        args.extend_from_slice(extra);
        args.push("--json");
        success(&run(root, &args))["called_by"].clone()
    };
    let full = callers(&[]);
    let total = full["total"].as_u64().expect("called_by total");
    assert!(total >= 2, "{full}");
    let limit = total.to_string();
    let exact = callers(&["--limit", &limit]);
    assert_eq!(
        (&exact["truncated"], &exact["next_offset"]),
        (&json!(false), &Value::Null)
    );
    let short_limit = (total - 1).to_string();
    let short = callers(&["--limit", &short_limit]);
    assert_eq!(short["next_offset"], json!(total - 1));
    let last = callers(&["--offset", &short_limit]);
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        (&last["truncated"], &last["next_offset"]),
        (&json!(true), &Value::Null)
    );
}

/// The ids of an ambiguity page.
fn candidate_ids(error: &Value) -> Vec<String> {
    error["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| candidate["id"].as_str().unwrap().to_string())
        .collect()
}

/// Ambiguity candidates page identically through `symbol` and `refs`, cover
/// every candidate once in order, and hit the same boundaries as result
/// lists; `context` returns the offset-zero page with the same bytes.
#[test]
fn ambiguity_pages_are_exact_and_agree_across_commands() {
    let temp = fixture_repo("stream-ambiguity");
    let root = temp.path();
    let ambiguous = |command: &str, extra: &[&str]| {
        let mut args = vec![command, "launch"];
        args.extend_from_slice(extra);
        args.push("--json");
        failure(&run(root, &args), 5)
    };
    let all = ambiguous("symbol", &[]);
    assert_eq!(all["total"], 3);
    assert_eq!(
        (&all["truncated"], &all["next_offset"]),
        (&json!(false), &Value::Null)
    );
    let every = candidate_ids(&all);
    assert_eq!(every.len(), 3);

    for command in ["symbol", "refs"] {
        let mut paged = Vec::new();
        for offset in 0..3 {
            let offset_text = offset.to_string();
            let page = ambiguous(command, &["--limit", "1", "--offset", &offset_text]);
            assert_eq!(page["total"], 3, "{command} {offset}");
            assert_eq!(page["truncated"], true, "{command} {offset}");
            let next = if offset < 2 {
                json!(offset + 1)
            } else {
                Value::Null
            };
            assert_eq!(page["next_offset"], next, "{command} {offset}");
            paged.extend(candidate_ids(&page));
        }
        assert_eq!(paged, every, "{command}");
        let exact = ambiguous(command, &["--limit", "3"]);
        assert_eq!(
            (&exact["truncated"], &exact["next_offset"]),
            (&json!(false), &Value::Null)
        );
        let past = ambiguous(command, &["--offset", "3"]);
        assert_eq!(candidate_ids(&past), Vec::<String>::new());
        assert_eq!(
            (&past["truncated"], &past["next_offset"]),
            (&json!(true), &Value::Null)
        );
    }

    // Identical candidate bytes from symbol and refs for the same page.
    let symbol_page = ambiguous("symbol", &["--limit", "2"]);
    let refs_page = ambiguous("refs", &["--limit", "2"]);
    assert_eq!(symbol_page["candidates"], refs_page["candidates"]);
    assert_eq!(symbol_page["index"], refs_page["index"]);

    // Context: offset zero only, paged by `--limit`.
    let context = ambiguous("context", &["--limit", "2"]);
    assert_eq!(context["candidates"], symbol_page["candidates"]);
    assert_eq!(context["next_offset"], 2);
    let context_all = ambiguous("context", &[]);
    assert_eq!(candidate_ids(&context_all), every);
}
