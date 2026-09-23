//! Integration tests for SY1: `rivet symbol`'s `calls` and `called_by` lists
//! default to `--min-resolution scoped` and report the name-only rows they
//! hide as `hidden_name_match` (OUTPUT-CONTRACT "`rivet symbol`"; spec §14).
//!
//! Covered here:
//!
//! - the default lists only `exact` and `scoped` rows in text and JSON alike,
//!   and counts the hidden `name_match` rows in each list;
//! - a list whose only rows are name-only prints its heading with the count,
//!   never `none`;
//! - the count is taken before pagination and does not depend on the page;
//! - an explicit `--min-resolution` always wins, and under `exact` the count
//!   holds `name_match` rows only;
//! - `--min-resolution name_match` reproduces the pre-SY1 bytes apart from the
//!   new field, which is 0, and `refs`, `context`, and `symbol
//!   --signature-only` are byte-identical to the pre-SY1 binary. The goldens
//!   under `tests/golden/sy1-head/` were captured from the binary built at
//!   commit bf8541c, with the same arguments, against a fresh copy of the
//!   authored fixture;
//! - a receiver spanning source lines renders on one line in text and keeps
//!   its exact text in JSON;
//! - output is identical across an `index --force` rebuild.

#![cfg(feature = "lang-php")]

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::Value;

use support::{fixture_repo, git_repo, run, success, write};

const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const RUN_UNKNOWN: &str = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
const SECOND_LAUNCH: &str = "ReportService.php#App\\Reporting\\ReportService::launch";

/// The flag that lists name-only rows too, the pre-SY1 default.
const ALL_TIERS: [&str; 2] = ["--min-resolution", "name_match"];

/// The authored fixture's one skipped file makes its coverage incomplete.
const COVERAGE: &str =
    "coverage: incomplete; 9 of 10 files indexed; skipped 1 unsupported; 0 diagnostics\n";

/// `Mix\Service::hub` calls one function (`exact`), one method through
/// `$this` (`scoped`), and two methods on untyped receivers (`name_match`).
/// It is called once through a `new` receiver (`scoped`) and twice through
/// untyped receivers (`name_match`).
const MIX_PHP: &str = "<?php
declare(strict_types=1);
namespace Mix;

function leaf(): void
{
}

final class Service
{
    public function callee(): void
    {
    }

    public function hub(): void
    {
        leaf();
        $this->callee();
        $x->callee();
        $y->other();
    }
}

function wrapperA(): void
{
    $svc = new \\Mix\\Service();
    $svc->hub();
}

function wrapperB(): void
{
    $y->hub();
}

function wrapperC(): void
{
    $z->hub();
}
";

const HUB: &str = "mix.php#Mix\\Service::hub";

/// A temporary Git root holding `mix.php`.
fn mix_repo(label: &str) -> support::TempDir {
    let temp = git_repo(label);
    write(temp.path(), "mix.php", MIX_PHP.as_bytes());
    temp
}

/// Runs `symbol <query> --json` with extra flags, requiring strict success.
fn symbol(dir: &Path, query: &str, extra: &[&str]) -> Value {
    let mut args = vec!["symbol", query, "--json"];
    args.extend_from_slice(extra);
    success(&run(dir, &args))
}

/// Stdout of a successful human run; requires exit 0 and empty stderr.
fn human(dir: &Path, args: &[&str]) -> String {
    let output = run(dir, args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "{args:?}: stderr must stay empty");
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// The resolution of every item in a list, in order.
fn tiers(list: &Value) -> Vec<String> {
    list["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["resolution"].as_str().expect("resolution").to_string())
        .collect()
}

/// `(total, hidden_name_match)` of a list.
fn counts(list: &Value) -> (u64, u64) {
    (
        list["total"].as_u64().expect("total"),
        list["hidden_name_match"]
            .as_u64()
            .expect("hidden_name_match is an integer"),
    )
}

// ---------------------------------------------------------------------------
// Default and restore.
// ---------------------------------------------------------------------------

#[test]
fn the_default_lists_resolved_rows_and_counts_the_name_only_ones() {
    let temp = mix_repo("default");
    let value = symbol(temp.path(), HUB, &[]);
    assert_eq!(counts(&value["calls"]), (2, 2), "{value}");
    assert_eq!(tiers(&value["calls"]), ["exact", "scoped"]);
    assert_eq!(counts(&value["called_by"]), (1, 2), "{value}");
    assert_eq!(tiers(&value["called_by"]), ["scoped"]);
    assert_eq!(
        value["called_by"]["items"][0]["containing_symbol"]["id"],
        "mix.php#Mix\\wrapperA"
    );

    // The same lists under `name_match` hold exactly the hidden rows more.
    let all = symbol(temp.path(), HUB, &ALL_TIERS);
    assert_eq!(counts(&all["calls"]), (4, 0), "{all}");
    assert_eq!(
        tiers(&all["calls"]),
        ["exact", "scoped", "name_match", "name_match"]
    );
    assert_eq!(counts(&all["called_by"]), (3, 0), "{all}");
    assert_eq!(
        tiers(&all["called_by"]),
        ["scoped", "name_match", "name_match"]
    );
    // The listed rows are the resolved rows of the full list, in order.
    for list in ["calls", "called_by"] {
        let resolved: Vec<&Value> = all[list]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["resolution"] != "name_match")
            .collect();
        let listed: Vec<&Value> = value[list]["items"].as_array().unwrap().iter().collect();
        assert_eq!(listed, resolved, "{list}");
    }

    // Text and JSON share the default: the headings carry the same counts.
    let text = human(temp.path(), &["symbol", HUB]);
    assert!(
        text.contains(
            "calls: (+2 name-only not listed)\n\
             \x20 mix.php:17:9   mix.php#Mix\\leaf             exact\n\
             \x20 mix.php:18:16  mix.php#Mix\\Service::callee  scoped\n\
             \n\
             called by: (+2 name-only not listed)\n\
             \x20 mix.php:27:11  Mix\\wrapperA  scoped\n"
        ),
        "{text}"
    );
    assert!(!text.contains('?'), "no name-only row is listed: {text}");

    // The authored fixture: `launch` has five scoped callers and one
    // name-only caller, and contains no call.
    let fixture = fixture_repo("default-fixture");
    let launch = symbol(fixture.path(), LAUNCH, &[]);
    assert_eq!(counts(&launch["calls"]), (0, 0));
    assert_eq!(counts(&launch["called_by"]), (5, 1));
    assert!(
        tiers(&launch["called_by"])
            .iter()
            .all(|tier| tier == "scoped")
    );
}

#[test]
fn name_match_restores_the_pre_sy1_output_apart_from_the_new_field() {
    let temp = fixture_repo("restore");
    let cases: [(&str, Vec<&str>); 8] = [
        ("restore-01-symbol-json", vec!["symbol", LAUNCH, "--json"]),
        ("restore-02-symbol-text", vec!["symbol", LAUNCH]),
        (
            "restore-03-symbol-page-json",
            vec!["symbol", LAUNCH, "--json", "--limit", "2", "--offset", "2"],
        ),
        (
            "restore-04-symbol-page-text",
            vec!["symbol", LAUNCH, "--limit", "2", "--offset", "2"],
        ),
        (
            "restore-05-run-unknown-source-json",
            vec!["symbol", RUN_UNKNOWN, "--json", "--source"],
        ),
        (
            "restore-06-run-unknown-source-text",
            vec!["symbol", RUN_UNKNOWN, "--source"],
        ),
        (
            "restore-07-second-launch-json",
            vec!["symbol", SECOND_LAUNCH, "--json"],
        ),
        ("restore-08-relaunch-text", vec!["symbol", RELAUNCH]),
    ];
    for (name, mut args) in cases {
        args.extend_from_slice(&ALL_TIERS);
        let output = run(temp.path(), &args);
        let actual = transcript(&output);
        // The only difference allowed is the new field, 0 in both lists.
        let field = "\"hidden_name_match\":0,";
        let expected_fields = if args.contains(&"--json") { 2 } else { 0 };
        assert_eq!(
            actual.matches(field).count(),
            expected_fields,
            "{name}: {actual}"
        );
        assert!(
            !actual.contains("name-only not listed"),
            "{name}: nothing is hidden: {actual}"
        );
        assert_eq!(actual.replace(field, ""), golden(name), "{name}: {args:?}");
    }
}

#[test]
fn refs_context_and_signature_only_are_unchanged() {
    let temp = fixture_repo("unchanged");
    let cases: [(&str, &[&str]); 9] = [
        ("unchanged-01-refs-text", &["refs", LAUNCH]),
        ("unchanged-02-refs-json", &["refs", LAUNCH, "--json"]),
        (
            "unchanged-03-refs-candidates-text",
            &["refs", LAUNCH, "--mode", "candidates"],
        ),
        (
            "unchanged-04-refs-scoped-json",
            &["refs", LAUNCH, "--json", "--min-resolution", "scoped"],
        ),
        ("unchanged-05-context-text", &["context", LAUNCH]),
        ("unchanged-06-context-json", &["context", LAUNCH, "--json"]),
        (
            "unchanged-07-context-depth-text",
            &["context", RELAUNCH, "--depth", "2"],
        ),
        (
            "unchanged-08-signature-only-text",
            &["symbol", LAUNCH, "--signature-only"],
        ),
        (
            "unchanged-09-signature-only-json",
            &["symbol", LAUNCH, "--signature-only", "--json"],
        ),
    ];
    for (name, args) in cases {
        let actual = transcript(&run(temp.path(), args));
        assert_eq!(actual, golden(name), "{name}: {args:?}");
    }
    // `--signature-only` ignores the tier: every minimum gives the same bytes.
    for minimum in ["exact", "scoped", "name_match"] {
        for json in [true, false] {
            let mut args = vec![
                "symbol",
                LAUNCH,
                "--signature-only",
                "--min-resolution",
                minimum,
            ];
            let name = if json {
                args.push("--json");
                "unchanged-09-signature-only-json"
            } else {
                "unchanged-08-signature-only-text"
            };
            assert_eq!(transcript(&run(temp.path(), &args)), golden(name));
        }
    }
}

/// `exit N`, then `--- stdout` and the stdout bytes, then `--- stderr` and
/// the stderr bytes, as the golden files hold them.
fn transcript(output: &Output) -> String {
    format!(
        "exit {}\n--- stdout\n{}--- stderr\n{}",
        output.status.code().expect("exit code"),
        String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout"),
        String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr"),
    )
}

/// One golden captured from the pre-SY1 binary.
fn golden(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/sy1-head")
        .join(format!("{name}.out"));
    fs::read_to_string(&path).unwrap_or_else(|_| panic!("read {}", path.display()))
}

// ---------------------------------------------------------------------------
// Hidden counts.
// ---------------------------------------------------------------------------

#[test]
fn a_list_of_only_name_only_rows_keeps_its_heading_and_count() {
    let temp = fixture_repo("only-name-only");

    // `runUnknown` contains one call, on an untyped receiver.
    let value = symbol(temp.path(), RUN_UNKNOWN, &[]);
    assert_eq!(counts(&value["calls"]), (0, 1), "{value}");
    assert_eq!(value["calls"]["items"], Value::Array(Vec::new()));
    assert_eq!(value["calls"]["truncated"], false);
    assert!(value["calls"]["next_offset"].is_null());
    // `called_by` is really empty: nothing hidden, so it still says `none`.
    assert_eq!(counts(&value["called_by"]), (0, 0));

    // The second `launch` is reached only by that untyped call.
    let second = symbol(temp.path(), SECOND_LAUNCH, &[]);
    assert_eq!(counts(&second["called_by"]), (0, 1), "{second}");
    assert_eq!(counts(&second["calls"]), (0, 0));

    let text = human(temp.path(), &["symbol", SECOND_LAUNCH]);
    assert!(
        text.ends_with(&format!(
            "\ncalls: none\n\ncalled by: (+1 name-only not listed)\n\n{COVERAGE}"
        )),
        "{text}"
    );
    let text = human(temp.path(), &["symbol", RUN_UNKNOWN]);
    assert!(
        text.ends_with(&format!(
            "\ncalls: (+1 name-only not listed)\n\ncalled by: none\n\n{COVERAGE}"
        )),
        "{text}"
    );
    // A hidden-only list never reads as empty.
    assert!(!text.contains("calls: none"), "{text}");
}

#[test]
fn hidden_counts_do_not_depend_on_the_page() {
    let temp = mix_repo("page");
    let whole = symbol(temp.path(), HUB, &[]);
    for page in [
        &["--limit", "1", "--offset", "1"][..],
        &["--limit", "1"],
        &["--offset", "1"],
        &["--offset", "99"],
        &["--limit", "1000"],
    ] {
        let value = symbol(temp.path(), HUB, page);
        for list in ["calls", "called_by"] {
            assert_eq!(
                value[list]["hidden_name_match"], whole[list]["hidden_name_match"],
                "{list} {page:?}: {value}"
            );
            assert_eq!(
                value[list]["total"], whole[list]["total"],
                "{list} {page:?}"
            );
        }
    }

    // `--limit 1 --offset 1`: `calls` shows its second listed row, and
    // `called_by` (one listed row) is past its end. Both keep their counts.
    let value = symbol(temp.path(), HUB, &["--limit", "1", "--offset", "1"]);
    assert_eq!(tiers(&value["calls"]), ["scoped"]);
    assert_eq!(value["calls"]["truncated"], true);
    assert!(value["calls"]["next_offset"].is_null());
    assert_eq!(tiers(&value["called_by"]), Vec::<String>::new());
    assert_eq!(value["called_by"]["truncated"], true);
    let text = human(
        temp.path(),
        &["symbol", HUB, "--limit", "1", "--offset", "1"],
    );
    assert!(
        text.contains(
            "calls: (+2 name-only not listed)\n\
             \x20 mix.php:18:16  mix.php#Mix\\Service::callee  scoped\n\
             \x20 showing 2-2 of 2; this is the last page\n\
             \n\
             called by: (+2 name-only not listed)\n\
             \x20 showing none of 1: --offset is past the end; start again at --offset 0\n"
        ),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// Explicit tiers.
// ---------------------------------------------------------------------------

#[test]
fn an_explicit_minimum_always_wins() {
    let temp = mix_repo("explicit");

    // `scoped` is the default, byte for byte, in text and JSON.
    for json in [true, false] {
        let mut default = vec!["symbol", HUB];
        let mut scoped = vec!["symbol", HUB, "--min-resolution", "scoped"];
        if json {
            default.push("--json");
            scoped.push("--json");
        }
        assert_eq!(
            run(temp.path(), &default).stdout,
            run(temp.path(), &scoped).stdout,
            "json: {json}"
        );
    }

    // `exact` hides the scoped and the name-only rows, but counts only the
    // name-only ones: `hidden_name_match` means what its name says.
    let exact = symbol(temp.path(), HUB, &["--min-resolution", "exact"]);
    assert_eq!(counts(&exact["calls"]), (1, 2), "{exact}");
    assert_eq!(tiers(&exact["calls"]), ["exact"]);
    assert_eq!(counts(&exact["called_by"]), (0, 2), "{exact}");
    // The mix repository's coverage is complete, so the heading ends the text.
    let text = human(temp.path(), &["symbol", HUB, "--min-resolution", "exact"]);
    assert!(
        text.ends_with(
            "calls: (+2 name-only not listed)\n\
             \x20 mix.php:17:9  mix.php#Mix\\leaf  exact\n\
             \n\
             called by: (+2 name-only not listed)\n"
        ),
        "{text}"
    );

    // `name_match` hides nothing.
    let all = symbol(temp.path(), HUB, &ALL_TIERS);
    assert_eq!(counts(&all["calls"]).1, 0);
    assert_eq!(counts(&all["called_by"]).1, 0);
    let text = human(
        temp.path(),
        &["symbol", HUB, "--min-resolution", "name_match"],
    );
    assert!(!text.contains("not listed"), "{text}");
    assert!(
        text.contains("calls:\n") && text.contains("called by:\n"),
        "{text}"
    );
}

#[test]
fn listed_and_hidden_rows_add_up_to_the_full_list() {
    let fixture = fixture_repo("sums-fixture");
    let mix = mix_repo("sums-mix");
    let queries: [(&Path, &str); 12] = [
        (fixture.path(), LAUNCH),
        (fixture.path(), RELAUNCH),
        (fixture.path(), RUN_UNKNOWN),
        (fixture.path(), SECOND_LAUNCH),
        (
            fixture.path(),
            "ReportService.php#App\\Reporting\\ReportService::runAlias",
        ),
        (fixture.path(), "boot.php#App\\Boot\\launch"),
        (mix.path(), HUB),
        (mix.path(), "mix.php#Mix\\leaf"),
        (mix.path(), "mix.php#Mix\\Service::callee"),
        (mix.path(), "mix.php#Mix\\wrapperA"),
        (mix.path(), "mix.php#Mix\\wrapperB"),
        (mix.path(), "mix.php#Mix\\Service"),
    ];
    for (dir, query) in queries {
        let all = symbol(dir, query, &ALL_TIERS);
        let scoped = symbol(dir, query, &[]);
        let exact = symbol(dir, query, &["--min-resolution", "exact"]);
        for list in ["calls", "called_by"] {
            let full = tiers(&all[list]);
            let count = |tier: &str| full.iter().filter(|t| *t == tier).count() as u64;
            let (name_match, scoped_rows, exact_rows) =
                (count("name_match"), count("scoped"), count("exact"));
            assert_eq!(counts(&all[list]).1, 0, "{query} {list}");
            assert_eq!(
                counts(&scoped[list]),
                (exact_rows + scoped_rows, name_match),
                "{query} {list}"
            );
            assert_eq!(
                counts(&exact[list]),
                (exact_rows, name_match),
                "{query} {list}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Receivers.
// ---------------------------------------------------------------------------

#[test]
fn a_multi_line_receiver_renders_on_one_line_and_keeps_its_json_text() {
    let temp = git_repo("receiver");
    let lf = "<?php\ndeclare(strict_types=1);\nnamespace Chain;\n\nfunction chain(): void\n{\n    $builder\n        ->where()\n        ->get();\n    $a->b();\n}\n";
    let crlf = "<?php\r\ndeclare(strict_types=1);\r\nnamespace Crlf;\r\n\r\nfunction crlf(): void\r\n{\r\n    $q\r\n\t\t->first()  \t ->mid()\r\n\t\t->second();\r\n}\r\n";
    write(temp.path(), "chain.php", lf.as_bytes());
    write(temp.path(), "crlf.php", crlf.as_bytes());

    // JSON keeps each receiver's exact source text.
    let value = symbol(temp.path(), "Chain\\chain", &ALL_TIERS);
    let receivers: Vec<&str> = value["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receiver"].as_str().expect("receiver"))
        .collect();
    assert_eq!(receivers, ["$builder", "$builder\n        ->where()", "$a"]);
    let value = symbol(temp.path(), "Crlf\\crlf", &ALL_TIERS);
    let receivers: Vec<&str> = value["calls"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receiver"].as_str().expect("receiver"))
        .collect();
    assert_eq!(
        receivers,
        [
            "$q",
            "$q\r\n\t\t->first()",
            "$q\r\n\t\t->first()  \t ->mid()"
        ]
    );

    // Text puts each receiver on its row, with every whitespace run collapsed
    // to one space, and pads the columns from the collapsed text.
    let text = human(
        temp.path(),
        &["symbol", "Chain\\chain", "--min-resolution", "name_match"],
    );
    assert!(
        text.contains(
            "calls:\n\
             \x20 chain.php:8:11  (unresolved; receiver $builder)            name_match ?\n\
             \x20 chain.php:9:11  (unresolved; receiver $builder ->where())  name_match ?\n\
             \x20 chain.php:10:9  (unresolved; receiver $a)                  name_match ?\n\
             \n\
             called by: none\n"
        ),
        "{text}"
    );
    let text = human(
        temp.path(),
        &["symbol", "Crlf\\crlf", "--min-resolution", "name_match"],
    );
    assert!(!text.contains('\r') && !text.contains('\t'), "{text:?}");
    assert!(
        text.contains(
            "calls:\n\
             \x20 crlf.php:8:5   (unresolved; receiver $q)                    name_match ?\n\
             \x20 crlf.php:8:18  (unresolved; receiver $q ->first())          name_match ?\n\
             \x20 crlf.php:9:5   (unresolved; receiver $q ->first() ->mid())  name_match ?\n"
        ),
        "{text}"
    );

    // By default those rows are name-only, so they are counted, not listed.
    let text = human(temp.path(), &["symbol", "Chain\\chain"]);
    assert!(
        text.contains("calls: (+3 name-only not listed)\n\ncalled by: none\n"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// Determinism.
// ---------------------------------------------------------------------------

#[test]
fn output_is_identical_across_a_force_refresh() {
    let fixture = fixture_repo("force-fixture");
    let mix = mix_repo("force-mix");
    let queries: Vec<(&Path, Vec<&str>)> = vec![
        (fixture.path(), vec!["symbol", LAUNCH]),
        (fixture.path(), vec!["symbol", LAUNCH, "--json"]),
        (
            fixture.path(),
            vec!["symbol", RUN_UNKNOWN, "--json", "--source"],
        ),
        (fixture.path(), vec!["symbol", SECOND_LAUNCH]),
        (mix.path(), vec!["symbol", HUB]),
        (mix.path(), vec!["symbol", HUB, "--json"]),
        (
            mix.path(),
            vec!["symbol", HUB, "--json", "--min-resolution", "exact"],
        ),
        (
            mix.path(),
            vec!["symbol", HUB, "--json", "--min-resolution", "name_match"],
        ),
        (
            mix.path(),
            vec!["symbol", HUB, "--json", "--limit", "1", "--offset", "1"],
        ),
    ];
    let before: Vec<Vec<u8>> = queries
        .iter()
        .map(|(dir, args)| checked_stdout(dir, args))
        .collect();
    for ((dir, args), expected) in queries.iter().zip(&before) {
        assert_eq!(&checked_stdout(dir, args), expected, "repeated: {args:?}");
    }
    for dir in [fixture.path(), mix.path()] {
        success(&run(dir, &["index", "--force", "--json"]));
    }
    for ((dir, args), expected) in queries.iter().zip(&before) {
        assert_eq!(
            &checked_stdout(dir, args),
            expected,
            "after --force: {args:?}"
        );
    }
}

/// Stdout of a run that must succeed with nothing on stderr.
fn checked_stdout(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = run(dir, args);
    assert_eq!(output.status.code(), Some(0), "{args:?}");
    assert!(output.stderr.is_empty(), "{args:?}");
    output.stdout
}

// ---------------------------------------------------------------------------
// Help.
// ---------------------------------------------------------------------------

#[test]
fn symbol_help_states_the_default_tier_and_how_to_list_name_only_rows() {
    let temp = git_repo("help");
    let text = human(temp.path(), &["symbol", "--help"]);
    assert!(
        text.contains("Call lists default to --min-resolution scoped (exact and scoped rows)"),
        "{text}"
    );
    assert!(
        text.contains("--min-resolution name_match lists those rows too."),
        "{text}"
    );
    // The flag's description stays on its own line: a longer one makes clap
    // move every description below its flag, adding a line per flag.
    assert!(
        text.contains(
            "\n      --min-resolution <exact|scoped|name_match>  \
             Minimum tier kept in call lists (default: scoped)\n"
        ),
        "{text}"
    );
    assert!(text.lines().count() <= 30, "{text}");
    // `refs` keeps its own help: nothing about a scoped default.
    let refs = human(temp.path(), &["refs", "--help"]);
    assert!(!refs.contains("default: scoped"), "{refs}");
}
