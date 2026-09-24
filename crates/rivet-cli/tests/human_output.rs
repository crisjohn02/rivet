//! Integration tests for T34: compact human output and model-oriented help.
//!
//! They drive the built binary against temporary copies of the authored PHP
//! fixture and assert exact text: the golden human forms of `symbol`, `refs`,
//! and `context`; the `?` mark on `name_match`; pagination and coverage lines;
//! human errors; every help text's size, layout, and runnable examples; and
//! byte-identical `--json` output against goldens captured from the pre-T34
//! binary (commit df58b42) and, for a repository with diagnostics, from the
//! pre-CV1 binary (commit 2581e00).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";

/// The fixture's one skipped file (`README.md`, unsupported) makes its
/// coverage incomplete.
const COVERAGE: &str =
    "coverage incomplete: 9/10 files indexed; skipped 1 unsupported; 0 diagnostics\n";

/// Every command with its own help text.
const COMMANDS: [&str; 6] = ["init", "index", "symbol", "refs", "context", "snippet"];

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
            "rivet-human-{label}-{}-{nanos}-{unique}",
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

// ---------------------------------------------------------------------------
// (a) Golden human output.
// ---------------------------------------------------------------------------

#[test]
fn refs_golden_marks_name_match_and_distinguishes_scoped() {
    let temp = fixture_repo("refs-golden");
    assert_eq!(
        human(temp.path(), &["refs", LAUNCH]),
        format!(
            "App\\Services\\SurveyService::launch  method  SurveyService.php:18-21\n\
             6 references  (5 scoped, 1 name_match)\n\
             \n\
             ReportService.php:24:15  App\\Reporting\\ReportService::runAlias    call  scoped\n\
             ReportService.php:30:15  App\\Reporting\\ReportService::runTyped    call  scoped\n\
             ReportService.php:36:13  App\\Reporting\\ReportService::runUnknown  call  name_match ?\n\
             SurveyService.php:26:16  App\\Services\\SurveyService::relaunch     call  scoped\n\
             boot.php:17:7            (file scope)                             call  scoped\n\
             boot.php:22:17           (file scope)                             call  scoped\n\
             \n\
             {COVERAGE}"
        )
    );
}

#[test]
fn refs_candidates_name_the_declaration_a_candidate_is_bound_to() {
    let temp = fixture_repo("refs-candidates");
    let output = human(temp.path(), &["refs", LAUNCH, "--mode", "candidates"]);
    assert!(
        output.contains("7 references  (5 scoped, 2 name_match)  mode: candidates\n"),
        "{output}"
    );
    assert!(
        output.contains(
            "boot.php:13:1            (file scope)                             call  \
             name_match ?  -> boot.php#App\\Boot\\launch\n"
        ),
        "{output}"
    );
}

#[test]
fn empty_refs_say_the_result_is_not_proof() {
    let temp = fixture_repo("refs-empty");
    assert_eq!(
        human(temp.path(), &["refs", LAUNCH, "--min-resolution", "exact"]),
        format!(
            "App\\Services\\SurveyService::launch  method  SurveyService.php:18-21\n\
             0 references\n\
             an empty result does not prove there are no references\n\
             \n\
             {COVERAGE}"
        )
    );
}

#[test]
fn symbol_golden_lists_calls_and_callers_with_marks() {
    let temp = fixture_repo("symbol-golden");
    // SY1: by default the call lists keep exact and scoped rows, and the
    // heading counts the name-only rows left out.
    assert_eq!(
        human(temp.path(), &["symbol", LAUNCH]),
        format!(
            "App\\Services\\SurveyService::launch\n\
             method\n\
             SurveyService.php:18-21\n\
             id: SurveyService.php#App\\Services\\SurveyService::launch\n\
             parent: SurveyService.php#App\\Services\\SurveyService\n\
             \n\
             signature:\n  public function launch(): void\n\
             \n\
             calls: none\n\
             \n\
             called by: (+1 name-only not listed)\n\
             \x20 ReportService.php:24:15  App\\Reporting\\ReportService::runAlias  scoped\n\
             \x20 ReportService.php:30:15  App\\Reporting\\ReportService::runTyped  scoped\n\
             \x20 SurveyService.php:26:16  App\\Services\\SurveyService::relaunch   scoped\n\
             \x20 boot.php:17:7            (file scope)                           scoped\n\
             \x20 boot.php:22:17           (file scope)                           scoped\n\
             \n\
             {COVERAGE}"
        )
    );
    // `--min-resolution name_match` lists the name-only row with its mark.
    assert_eq!(
        human(
            temp.path(),
            &["symbol", LAUNCH, "--min-resolution", "name_match"]
        ),
        format!(
            "App\\Services\\SurveyService::launch\n\
             method\n\
             SurveyService.php:18-21\n\
             id: SurveyService.php#App\\Services\\SurveyService::launch\n\
             parent: SurveyService.php#App\\Services\\SurveyService\n\
             \n\
             signature:\n  public function launch(): void\n\
             \n\
             calls: none\n\
             \n\
             called by:\n\
             \x20 ReportService.php:24:15  App\\Reporting\\ReportService::runAlias    scoped\n\
             \x20 ReportService.php:30:15  App\\Reporting\\ReportService::runTyped    scoped\n\
             \x20 ReportService.php:36:13  App\\Reporting\\ReportService::runUnknown  name_match ?\n\
             \x20 SurveyService.php:26:16  App\\Services\\SurveyService::relaunch     scoped\n\
             \x20 boot.php:17:7            (file scope)                             scoped\n\
             \x20 boot.php:22:17           (file scope)                             scoped\n\
             \n\
             {COVERAGE}"
        )
    );
}

#[test]
fn symbol_calls_name_their_target_or_say_unresolved() {
    let temp = fixture_repo("symbol-calls");
    assert_eq!(
        human(temp.path(), &["symbol", RELAUNCH, "--signature-only"]),
        format!(
            "App\\Services\\SurveyService::relaunch\n\
             method\n\
             SurveyService.php:24-27\n\
             id: SurveyService.php#App\\Services\\SurveyService::relaunch\n\
             parent: SurveyService.php#App\\Services\\SurveyService\n\
             \n\
             signature:\n  public function relaunch(): void\n\
             \n\
             {COVERAGE}"
        )
    );
    let output = human(temp.path(), &["symbol", RELAUNCH]);
    assert!(
        output.contains(
            "calls:\n  SurveyService.php:26:16  \
             SurveyService.php#App\\Services\\SurveyService::launch  scoped\n"
        ),
        "{output}"
    );
    let run_unknown = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
    let head = "App\\Reporting\\ReportService::runUnknown\n\
                method\n\
                ReportService.php:34-37\n\
                id: ReportService.php#App\\Reporting\\ReportService::runUnknown\n\
                parent: ReportService.php#App\\Reporting\\ReportService\n\
                \n\
                signature:\n  public function runUnknown(): void\n\
                \n\
                source:\n\
                public function runUnknown(): void\n    {\n        $x->launch();\n    }\n";
    let output = human(
        temp.path(),
        &[
            "symbol",
            run_unknown,
            "--source",
            "--min-resolution",
            "name_match",
        ],
    );
    assert_eq!(
        output,
        format!(
            "{head}\
             \n\
             calls:\n  ReportService.php:36:13  (unresolved; receiver $x)  name_match ?\n\
             \n\
             called by: none\n\
             \n\
             {COVERAGE}"
        )
    );
    // SY1: by default a list whose only row is name-only prints its heading
    // with the count, never `none`.
    let output = human(temp.path(), &["symbol", run_unknown, "--source"]);
    assert_eq!(
        output,
        format!(
            "{head}\
             \n\
             calls: (+1 name-only not listed)\n\
             \n\
             called by: none\n\
             \n\
             {COVERAGE}"
        )
    );
}

#[test]
fn context_golden_prints_headers_and_source() {
    let temp = fixture_repo("context-golden");
    assert_eq!(
        human(temp.path(), &["context", RELAUNCH]),
        format!(
            "── SurveyService.php:24-27  App\\Services\\SurveyService::relaunch  [target, full]\n\
             public function relaunch(): void\n    {{\n        $this->launch();\n    }}\n\
             \n\
             ── SurveyService.php:18-21  App\\Services\\SurveyService::launch  [callee, full]\n\
             public function launch(): void\n    {{\n        $this->label = self::DEFAULT_LABEL;\n    }}\n\
             \n\
             ── SurveyService.php:9-28  App\\Services\\SurveyService  [parent, signature]\n\
             final class SurveyService\n{{\n    public const DEFAULT_LABEL = 'survey';\n    \
             private string $label = 'survey';\n}}\n\
             \n\
             ── ReportService.php:21-25  App\\Reporting\\ReportService::runAlias  [second_degree, full]\n\
             public function runAlias(): void\n    {{\n        $svc = new SurveySvc();\n        \
             $svc->launch();\n    }}\n\
             \n\
             ── ReportService.php:28-31  App\\Reporting\\ReportService::runTyped  [second_degree, full]\n\
             public function runTyped(SurveyService $svc): void\n    {{\n        $svc->launch();\n    }}\n\
             \n\
             estimated_tokens: 152 / 4000  (tokenizer: utf8-bytes-v1; budget_scope: source)\n\
             {COVERAGE}"
        )
    );
}

#[test]
fn context_marks_a_name_match_segment_and_reports_omissions() {
    let temp = fixture_repo("context-marks");
    let output = human(temp.path(), &["context", LAUNCH]);
    assert!(
        output.contains(
            "── ReportService.php:34-37  App\\Reporting\\ReportService::runUnknown  \
             [caller, full] ?\n"
        ),
        "{output}"
    );
    assert!(!output.contains("omitted:"), "{output}");

    let output = human(temp.path(), &["context", LAUNCH, "--tokens", "60"]);
    assert!(
        output.contains(
            "── ReportService.php:21-25  App\\Reporting\\ReportService::runAlias  \
             [caller, signature]\npublic function runAlias(): void\n\n"
        ),
        "{output}"
    );
    assert!(
        output.ends_with(&format!(
            "omitted: 3 budget, 0 overlap, 0 limit\n\
             estimated_tokens: 57 / 60  (tokenizer: utf8-bytes-v1; budget_scope: source)\n\
             {COVERAGE}"
        )),
        "{output}"
    );
}

// ---------------------------------------------------------------------------
// (b) Context source verbatim.
// ---------------------------------------------------------------------------

#[test]
fn context_prints_every_segment_source_verbatim() {
    let temp = fixture_repo("context-source");
    for args in [
        vec!["context", LAUNCH],
        vec!["context", RELAUNCH, "--depth", "2"],
        vec!["context", LAUNCH, "--tokens", "60"],
        vec!["context", LAUNCH, "--collapse", "always"],
    ] {
        let text = human(temp.path(), &args);
        let mut json_args = args.clone();
        json_args.push("--json");
        let output = run(temp.path(), &json_args);
        assert_eq!(output.status.code(), Some(0));
        let value: Value = serde_json::from_slice(&output.stdout).expect("JSON");
        let segments = value["segments"].as_array().expect("segments");
        assert!(!segments.is_empty());
        let mut cursor = 0;
        for segment in segments {
            let symbol = &segment["symbol"];
            let header = format!(
                "── {}:{}-{}  {}  [{}, {}]",
                symbol["file"].as_str().unwrap(),
                symbol["start_line"],
                symbol["end_line"],
                symbol["qualified_name"].as_str().unwrap(),
                segment["reason"].as_str().unwrap(),
                segment["form"].as_str().unwrap(),
            );
            let source = segment["source"].as_str().expect("source");
            assert!(!source.is_empty());
            // The header, then the source directly beneath it, in order.
            let at = text[cursor..]
                .find(&header)
                .unwrap_or_else(|| panic!("{args:?}: missing {header}\n{text}"));
            let body_start = cursor + at + text[cursor + at..].find('\n').unwrap() + 1;
            assert!(
                text[body_start..].starts_with(source),
                "{args:?}: source of {header} not verbatim\n{text}"
            );
            cursor = body_start + source.len();
        }
    }
}

// ---------------------------------------------------------------------------
// (c) Pagination lines.
// ---------------------------------------------------------------------------

#[test]
fn truncated_refs_show_the_slice_and_next_offset() {
    let temp = fixture_repo("refs-page");
    let first = human(temp.path(), &["refs", LAUNCH, "--limit", "2"]);
    assert!(
        first.contains("\nshowing 1-2 of 6; next: --offset 2\n"),
        "{first}"
    );
    let middle = human(
        temp.path(),
        &["refs", LAUNCH, "--limit", "2", "--offset", "2"],
    );
    assert!(
        middle.contains(
            "\nReportService.php:36:13  App\\Reporting\\ReportService::runUnknown  call  \
             name_match ?\nSurveyService.php:26:16  App\\Services\\SurveyService::relaunch     \
             call  scoped\nshowing 3-4 of 6; next: --offset 4\n"
        ),
        "{middle}"
    );
    let last = human(
        temp.path(),
        &["refs", LAUNCH, "--limit", "2", "--offset", "4"],
    );
    assert!(
        last.contains("\nshowing 5-6 of 6; this is the last page\n"),
        "{last}"
    );
    let past = human(temp.path(), &["refs", LAUNCH, "--offset", "99"]);
    assert!(
        past.contains(
            "6 references  (5 scoped, 1 name_match)\n\
             showing none of 6: --offset is past the end; start again at --offset 0\n"
        ),
        "{past}"
    );
    // An untruncated page prints no pagination line.
    let whole = human(temp.path(), &["refs", LAUNCH]);
    assert!(!whole.contains("showing"), "{whole}");
}

#[test]
fn truncated_call_lists_show_the_slice_and_next_offset() {
    let temp = fixture_repo("symbol-page");
    // SY1: the page counts the five listed callers; the hidden count is the
    // same on every page.
    let output = human(temp.path(), &["symbol", LAUNCH, "--limit", "2"]);
    assert!(
        output.contains(
            "called by: (+1 name-only not listed)\n\
             \x20 ReportService.php:24:15  App\\Reporting\\ReportService::runAlias  scoped\n\
             \x20 ReportService.php:30:15  App\\Reporting\\ReportService::runTyped  scoped\n\
             \x20 showing 1-2 of 5; next: --offset 2\n"
        ),
        "{output}"
    );
    let output = human(
        temp.path(),
        &["symbol", LAUNCH, "--limit", "2", "--offset", "4"],
    );
    assert!(
        output.contains(
            "called by: (+1 name-only not listed)\n\
             \x20 boot.php:22:17  (file scope)  scoped\n\
             \x20 showing 5-5 of 5; this is the last page\n"
        ),
        "{output}"
    );
    let output = human(
        temp.path(),
        &[
            "symbol",
            LAUNCH,
            "--limit",
            "2",
            "--offset",
            "4",
            "--min-resolution",
            "name_match",
        ],
    );
    assert!(
        output.contains("called by:\n")
            && output.contains("  showing 5-6 of 6; this is the last page\n"),
        "{output}"
    );
    // The `calls` list of `relaunch` has one item; paging past it says so.
    let output = human(temp.path(), &["symbol", RELAUNCH, "--offset", "1"]);
    assert!(
        output.contains(
            "calls:\n  showing none of 1: --offset is past the end; start again at --offset 0\n"
        ),
        "{output}"
    );
}

#[test]
fn ambiguity_candidates_are_listed_and_paged_on_stderr() {
    let temp = fixture_repo("ambiguous");
    let output = run(temp.path(), &["symbol", "launch", "--limit", "2"]);
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty(), "stdout stays empty on failure");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "rivet symbol: query 'launch' matched 3 symbols\n\
             hint: Re-run with one of the returned canonical IDs.\n\
             candidates:\n\
             \x20 ReportService.php#App\\Reporting\\ReportService::launch  method  ReportService.php:16-18\n\
             \x20 SurveyService.php#App\\Services\\SurveyService::launch   method  SurveyService.php:18-21\n\
             showing 1-2 of 3; next: --offset 2\n\
             {COVERAGE}"
        )
    );

    // Context rejects `--offset`, so its next page points at `rivet symbol`.
    let output = run(temp.path(), &["context", "launch", "--limit", "1"]);
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("showing 1-1 of 3; next: rivet symbol <query> --offset 1\n"),
        "{stderr}"
    );
}

// ---------------------------------------------------------------------------
// (d) Coverage line.
// ---------------------------------------------------------------------------

#[test]
fn coverage_line_is_present_only_when_coverage_is_incomplete() {
    let partial = fixture_repo("coverage-partial");
    let complete = fixture_repo("coverage-complete");
    fs::remove_file(complete.path().join("README.md")).expect("remove README.md");

    let commands: [&[&str]; 5] = [
        &["index"],
        &["symbol", LAUNCH],
        &["refs", LAUNCH],
        &["refs", LAUNCH, "--min-resolution", "exact"],
        &["context", LAUNCH],
    ];
    for args in commands {
        let text = human(partial.path(), args);
        assert!(text.ends_with(COVERAGE), "{args:?}: {text}");
        let text = human(complete.path(), args);
        assert!(!text.contains("coverage"), "{args:?}: {text}");
    }

    // Index-dependent errors carry the snapshot, so they report it too.
    let output = run(partial.path(), &["symbol", "nosuch"]);
    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.ends_with(COVERAGE), "{stderr}");
    let output = run(complete.path(), &["symbol", "nosuch"]);
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("coverage"), "{stderr}");
}

#[test]
fn a_cached_snapshot_is_called_out() {
    let temp = fixture_repo("cached");
    human(temp.path(), &["index"]);
    let text = human(temp.path(), &["refs", LAUNCH, "--no-refresh"]);
    assert!(
        text.ends_with(&format!(
            "snapshot: cached (--no-refresh); it may not match the working tree\n{COVERAGE}"
        )),
        "{text}"
    );
}

#[test]
fn index_human_follows_the_spec_shape() {
    let temp = fixture_repo("index");
    let text = human(temp.path(), &["index"]);
    let elapsed = text
        .lines()
        .find(|line| line.starts_with("Elapsed: "))
        .expect("elapsed line")
        .to_string();
    assert_eq!(
        text,
        format!(
            "Indexed 9 files\n50 symbols\n14 relationships\n\nUpdated: 10\nUnchanged: 0\n\
             Deleted: 0\n\n{elapsed}\n\n{COVERAGE}"
        )
    );
}

/// The fixture plus three files that fail to index: two parse errors and one
/// binary file. Their contract sort order (file bytes: `B.php` < `a.php` <
/// `z/Broken.php`) is unlike their write order and unlike the by-code order.
fn diagnostics_repo(label: &str) -> TempDir {
    let temp = fixture_repo(label);
    fs::create_dir_all(temp.path().join("z")).expect("create z/");
    fs::write(
        temp.path().join("z/Broken.php"),
        "<?php\nclass {{{ broken\n",
    )
    .expect("write");
    fs::write(temp.path().join("a.php"), "<?php\nclass A {{{\n").expect("write");
    fs::write(temp.path().join("B.php"), b"<?php\n\0\0binary\n").expect("write");
    temp
}

/// The commands whose human output ends with the coverage line.
const COVERED_COMMANDS: [&[&str]; 4] = [
    &["index"],
    &["symbol", LAUNCH],
    &["refs", LAUNCH],
    &["context", LAUNCH],
];

/// Asserts every covered command, and an index-dependent error, ends with
/// `line` and never points to `--json`.
fn assert_coverage_line(dir: &Path, line: &str) {
    for args in COVERED_COMMANDS {
        let text = human(dir, args);
        assert!(text.ends_with(line), "{args:?}: {text}");
        assert!(!text.contains("--json"), "{args:?}: {text}");
    }
    let output = run(dir, &["symbol", "nosuch"]);
    assert_eq!(output.status.code(), Some(4));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.ends_with(line), "{stderr}");
    assert!(!stderr.contains("--json"), "{stderr}");
}

#[test]
fn coverage_line_names_one_diagnostic_inline() {
    let temp = fixture_repo("coverage-one");
    fs::write(temp.path().join("Broken.php"), "<?php\nclass {{{ broken\n").expect("write");
    assert_coverage_line(
        temp.path(),
        "coverage incomplete: 9/11 files indexed; skipped 1 unsupported, 1 parse_error; \
         1 diagnostic: Broken.php (parse_error)\n",
    );
}

#[test]
fn coverage_line_names_two_diagnostics_in_sort_order() {
    // Written in the reverse of the contract's order (`B.php` < `z/...`).
    let temp = fixture_repo("coverage-two");
    fs::create_dir_all(temp.path().join("z")).expect("create z/");
    fs::write(
        temp.path().join("z/Broken.php"),
        "<?php\nclass {{{ broken\n",
    )
    .expect("write");
    fs::write(temp.path().join("B.php"), b"<?php\n\0\0binary\n").expect("write");
    assert_coverage_line(
        temp.path(),
        "coverage incomplete: 9/12 files indexed; skipped 1 unsupported, 1 binary, \
         1 parse_error; 2 diagnostics: B.php (binary_file), z/Broken.php (parse_error)\n",
    );
}

#[test]
fn coverage_line_counts_more_than_two_diagnostics_by_code() {
    let temp = diagnostics_repo("coverage-three");
    let line = "coverage incomplete: 9/13 files indexed; skipped 1 unsupported, 1 binary, \
                2 parse_error; 3 diagnostics (2 parse_error, 1 binary_file)\n";
    assert_coverage_line(temp.path(), line);
    // Addressing a failed file directly is exit 6; its error ends the same way.
    let output = run(temp.path(), &["symbol", "a.php:2"]);
    assert_eq!(output.status.code(), Some(6));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.ends_with(line), "{stderr}");
}

#[test]
fn a_capped_diagnostic_list_counts_only_its_first_items() {
    // 55 binary PHP files (`binary_file`) sort before 5 broken PHP files, so
    // the 50 listed items are all binary; the parse errors stay in the skip
    // counts and the total. (Before T43 this used 55 `unsupported_language`
    // TypeScript files; TypeScript is indexed now.)
    let temp = fixture_repo("coverage-capped");
    fs::create_dir_all(temp.path().join("a")).expect("create a/");
    fs::create_dir_all(temp.path().join("z")).expect("create z/");
    for index in 0..55 {
        fs::write(
            temp.path().join(format!("a/f{index:02}.php")),
            b"<?php\n\0\0binary\n",
        )
        .expect("write");
    }
    for index in 0..5 {
        fs::write(
            temp.path().join(format!("z/Broken{index}.php")),
            "<?php\nclass {{{\n",
        )
        .expect("write");
    }
    let output = run(temp.path(), &["index", "--json"]);
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(value["index"]["diagnostics"]["total"], 60);
    assert_eq!(value["index"]["diagnostics"]["truncated"], true);
    assert_coverage_line(
        temp.path(),
        "coverage incomplete: 9/70 files indexed; skipped 1 unsupported, 55 binary, \
         5 parse_error; 60 diagnostics (first 50: 50 binary_file)\n",
    );
}

#[test]
fn coverage_line_counts_an_indexed_typescript_file() {
    // Since T43 a TypeScript file is indexed like a PHP file: counted, with
    // no diagnostic. README.md is an ordinary unsupported file, counted and
    // never named. (An enabled-language file with no adapter, which used to
    // make "only unsupported skips, with a diagnostic", no longer exists; a
    // non-UTF-8 path, below, still does.)
    let temp = fixture_repo("coverage-unsupported");
    fs::create_dir_all(temp.path().join("src")).expect("create src/");
    fs::write(temp.path().join("src/ok.ts"), "export const x = 1;\n").expect("write");
    assert_coverage_line(
        temp.path(),
        "coverage incomplete: 10/11 files indexed; skipped 1 unsupported; 0 diagnostics\n",
    );
}

#[cfg(unix)]
#[test]
fn coverage_line_keeps_a_non_utf8_path_escaped() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let temp = fixture_repo("coverage-non-utf8");
    let bad = temp.path().join(OsStr::from_bytes(b"bad-\xff.php"));
    if fs::write(&bad, "<?php\n").is_err() {
        // APFS (macOS) rejects a filename that is not valid UTF-8; the unit
        // test in `human.rs` covers the rendering there.
        return;
    }
    // The path is outside the scan domain: in no count, yet incomplete.
    assert_coverage_line(
        temp.path(),
        "coverage incomplete: 9/10 files indexed; skipped 1 unsupported; \
         1 diagnostic: bad-\\xff.php (non_utf8_path)\n",
    );
}

/// `--json` invocations against [`diagnostics_repo`], captured from the
/// binary built at commit 2581e00 (before CV1 changed the human coverage
/// line), in this order against one fresh copy. Each golden is `exit N`,
/// then `--- stdout` and the stdout bytes, then `--- stderr` and the stderr
/// bytes.
const CV1_JSON_GOLDENS: [(&str, &[&str]); 6] = [
    ("01-index", &["index", "--json"]),
    ("02-symbol", &["symbol", LAUNCH, "--json"]),
    ("03-refs", &["refs", LAUNCH, "--json"]),
    ("04-context", &["context", LAUNCH, "--json"]),
    ("05-symbol-not-found", &["symbol", "nosuch", "--json"]),
    ("06-symbol-parse-failure", &["symbol", "a.php:2", "--json"]),
];

#[test]
fn json_with_diagnostics_is_byte_identical_to_the_pre_cv1_binary() {
    let temp = diagnostics_repo("coverage-json");
    let goldens = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cv1-head");
    for (name, args) in CV1_JSON_GOLDENS {
        let output = run(temp.path(), args);
        let actual = format!(
            "exit {}\n--- stdout\n{}--- stderr\n{}",
            output.status.code().expect("exit code"),
            String::from_utf8(output.stdout).expect("UTF-8"),
            String::from_utf8(output.stderr).expect("UTF-8"),
        );
        let expected =
            fs::read_to_string(goldens.join(format!("{name}.out"))).expect("read golden");
        assert_eq!(actual, expected, "{name}: {args:?}");
    }
}

// ---------------------------------------------------------------------------
// Human errors: message and hint on stderr, nothing on stdout, same codes.
// ---------------------------------------------------------------------------

#[test]
fn human_errors_print_message_and_hint_on_stderr() {
    let temp = fixture_repo("errors");
    let cases: [(&[&str], i32, &str); 4] = [
        (
            &["refs", LAUNCH, "--mode", "everything"],
            2,
            "rivet refs: invalid value for `--mode`: \"everything\" (expected \"references\" \
             or \"candidates\")\nhint: Pass `--mode references` or `--mode candidates`.\n",
        ),
        (
            &["context", LAUNCH, "--offset", "1"],
            2,
            "rivet context: `--offset` is not supported by `rivet context`\nhint: Drop \
             `--offset`; context returns segments in rank order and uses `--limit` for the \
             segment count.\n",
        ),
        (
            &["index", "--no-refresh"],
            2,
            "rivet index: `index` cannot be combined with `--no-refresh`\nhint: Run `rivet \
             index` to refresh, or use `--no-refresh` with a query command.\n",
        ),
        (
            &["context", LAUNCH, "--tokens", "9"],
            8,
            "rivet context: the context target needs at least 10 estimated tokens, but the \
             budget is 9\nhint: Re-run with `--tokens 10` or higher.\nrequired_tokens: 10 \
             (budget_tokens: 9)\n",
        ),
    ];
    for (args, exit, expected) in cases {
        let output = run(temp.path(), args);
        assert_eq!(output.status.code(), Some(exit), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}: stdout stays empty");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let expected = if exit == 8 {
            format!("{expected}{COVERAGE}")
        } else {
            expected.to_string()
        };
        assert_eq!(stderr, expected, "{args:?}");
    }

    // A directly addressed unsupported file names its parse status (exit 7).
    let output = run(temp.path(), &["symbol", "README.md:1"]);
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("rivet symbol: README.md is not in a supported language\nhint: "),
        "{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Determinism: no dependence on the terminal environment.
// ---------------------------------------------------------------------------

#[test]
fn human_output_ignores_terminal_width_colour_and_locale() {
    let temp = fixture_repo("env");
    for args in [
        vec!["refs", LAUNCH],
        vec!["symbol", LAUNCH],
        vec!["context", LAUNCH],
        vec!["--help"],
        vec!["help", "context"],
    ] {
        let plain = run(temp.path(), &args);
        let styled = Command::new(RIVET)
            .args(&args)
            .current_dir(temp.path())
            .env("COLUMNS", "20")
            .env("CLICOLOR_FORCE", "1")
            .env("FORCE_COLOR", "1")
            .env("TERM", "xterm-256color")
            .env("LANG", "de_DE.UTF-8")
            .env("LC_ALL", "de_DE.UTF-8")
            .output()
            .expect("run");
        assert_eq!(plain.stdout, styled.stdout, "{args:?}");
        assert!(!plain.stdout.contains(&0x1b), "{args:?}: no escape codes");
    }
}

// ---------------------------------------------------------------------------
// (e) Help texts.
// ---------------------------------------------------------------------------

/// Every way of asking for help, with the text each prints.
fn help_texts(dir: &Path) -> Vec<(String, String)> {
    let mut invocations: Vec<Vec<String>> = vec![
        vec!["--help".to_string()],
        vec!["-h".to_string()],
        vec!["help".to_string()],
    ];
    for command in COMMANDS {
        invocations.push(vec!["help".to_string(), command.to_string()]);
        invocations.push(vec![command.to_string(), "--help".to_string()]);
        invocations.push(vec![command.to_string(), "-h".to_string()]);
    }
    invocations
        .into_iter()
        .map(|args| {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let output = run(dir, &refs);
            assert_eq!(output.status.code(), Some(0), "{args:?}");
            assert!(output.stderr.is_empty(), "{args:?}");
            (
                args.join(" "),
                String::from_utf8(output.stdout).expect("UTF-8"),
            )
        })
        .collect()
}

#[test]
fn every_help_text_is_short_starts_with_examples_and_names_rg() {
    let temp = fixture_repo("help");
    for (invocation, text) in help_texts(temp.path()) {
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines.len() < 40,
            "{invocation}: {} lines\n{text}",
            lines.len()
        );
        assert_eq!(lines[0], "Examples:", "{invocation}");
        assert!(lines[1].starts_with("  rivet "), "{invocation}");
        // Every help but `snippet`'s (which prints fixed text and touches no
        // index) says that queries refresh on their own.
        if !invocation.contains("snippet") {
            assert!(
                text.contains("refresh"),
                "{invocation}: help says queries refresh automatically"
            );
        }
        let command = invocation
            .split_whitespace()
            .find(|word| COMMANDS.contains(word));
        match command {
            Some(command) => {
                assert!(
                    lines
                        .iter()
                        .any(|line| line.starts_with("Use this instead of `rg` when")),
                    "{invocation}: missing the rg line"
                );
                assert!(
                    lines[1].starts_with(&format!("  rivet {command}")),
                    "{invocation}: examples are for this command"
                );
            }
            None => {
                for command in COMMANDS {
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.starts_with(&format!("  {command} "))
                                && line.contains("instead of `rg`")),
                        "top-level help: no rg line for {command}\n{text}"
                    );
                }
            }
        }
    }

    // `rivet help <command>` and `rivet <command> --help` agree.
    for command in COMMANDS {
        assert_eq!(
            run(temp.path(), &["help", command]).stdout,
            run(temp.path(), &[command, "--help"]).stdout,
            "{command}"
        );
    }
}

// ---------------------------------------------------------------------------
// (f) Every help example runs.
// ---------------------------------------------------------------------------

/// Splits an example line like a POSIX shell for the forms help uses: words
/// separated by spaces, with single quotes grouping a word literally.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in line.chars() {
        match character {
            '\'' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            other => {
                word.push(other);
                started = true;
            }
        }
    }
    assert!(!quoted, "unbalanced quote in {line:?}");
    if started {
        words.push(word);
    }
    words
}

/// The example command lines of one help text: the indented `rivet ...`
/// lines of its leading `Examples:` block.
fn examples(text: &str) -> Vec<String> {
    text.lines()
        .skip(1)
        .take_while(|line| line.starts_with("  rivet "))
        .map(|line| line.trim().to_string())
        .collect()
}

/// Adapts an example's generic names to the authored fixture. The only
/// generic name is the Laravel-style path `app/Services/SurveyService.php`;
/// the fixture keeps that file at its root. Every symbol name in the help
/// (`SurveyService.launch`, `App\Services\SurveyService::launch`) exists in
/// the fixture unchanged.
fn adapt(word: &str) -> String {
    word.replace("app/Services/SurveyService.php", "SurveyService.php")
}

#[test]
fn every_help_example_runs_against_the_fixture() {
    let temp = fixture_repo("examples-help");
    let mut seen = 0;
    for (invocation, text) in help_texts(temp.path()) {
        let lines = examples(&text);
        assert!(!lines.is_empty(), "{invocation}: no examples");
        // Each help text runs in its own fresh copy, since `init` examples
        // write files.
        let repo = fixture_repo("examples-run");
        for line in lines {
            let words: Vec<String> = shell_words(&line).iter().map(|w| adapt(w)).collect();
            assert_eq!(words[0], "rivet", "{line}");
            let args: Vec<&str> = words[1..].iter().map(String::as_str).collect();
            let output = run(repo.path(), &args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("invalid_arguments"),
                "{invocation}: `{line}` failed with invalid_arguments: {stderr}"
            );
            assert_eq!(
                output.status.code(),
                Some(0),
                "{invocation}: `{line}` (run as {args:?}) failed: {stderr}"
            );
            assert!(!output.stdout.is_empty(), "{line}: prints a result");
            if args.contains(&"--json") {
                let value: Value = serde_json::from_slice(&output.stdout).expect("JSON");
                assert_eq!(value["schema_version"], 1, "{line}");
            }
            seen += 1;
        }
    }
    assert!(seen >= 20, "only {seen} example lines were found");
}

#[test]
fn shell_words_handles_single_quotes() {
    assert_eq!(
        shell_words("rivet symbol 'App\\Services\\SurveyService::launch' --source"),
        vec![
            "rivet",
            "symbol",
            "App\\Services\\SurveyService::launch",
            "--source"
        ]
    );
}

// ---------------------------------------------------------------------------
// (g) JSON byte identity with the pre-T34 binary.
// ---------------------------------------------------------------------------

/// The `--json` invocations whose output was captured from the pre-T34
/// binary, in the order they were run against one fixture copy. Each golden
/// file is `exit N`, then `--- stdout` and the exact stdout bytes, then
/// `--- stderr` and the exact stderr bytes, with the repository root (only
/// present in `init` output, if at all) replaced by `<ROOT>`.
///
/// SY1 re-captured `03-symbol`, `04-symbol-source`, and `05-symbol-page`:
/// the call lists now default to `scoped`, so `called_by` drops its one
/// name-only row, and each list gains `hidden_name_match`. SN1 and SN2
/// re-captured `13-snippet` for each new managed block. Every other golden
/// is unchanged; `symbol_tiers.rs` checks the pre-SY1 bytes under
/// `--min-resolution name_match`.
const JSON_GOLDENS: [(&str, &[&str]); 16] = [
    ("01-index", &["index", "--json"]),
    ("02-index-force", &["index", "--json", "--force"]),
    ("03-symbol", &["symbol", LAUNCH, "--json"]),
    (
        "04-symbol-source",
        &["symbol", LAUNCH, "--json", "--source"],
    ),
    (
        "05-symbol-page",
        &["symbol", LAUNCH, "--json", "--limit", "2", "--offset", "2"],
    ),
    ("06-symbol-ambiguous", &["symbol", "launch", "--json"]),
    ("07-refs", &["refs", LAUNCH, "--json"]),
    (
        "08-refs-candidates",
        &[
            "refs",
            LAUNCH,
            "--json",
            "--mode",
            "candidates",
            "--limit",
            "3",
        ],
    ),
    (
        "09-refs-ambiguous",
        &["refs", "launch", "--json", "--limit", "1"],
    ),
    ("10-context", &["context", RELAUNCH, "--json"]),
    (
        "11-context-budget",
        &["context", LAUNCH, "--json", "--tokens", "60"],
    ),
    (
        "12-context-too-small",
        &["context", LAUNCH, "--json", "--tokens", "9"],
    ),
    ("13-snippet", &["snippet", "--json"]),
    ("14-init", &["init", "--json"]),
    ("15-init-snippet", &["init", "--json", "--write-snippet"]),
    ("16-init-again", &["init", "--json"]),
];

#[test]
fn json_output_is_byte_identical_to_the_pre_t34_binary() {
    let temp = fixture_repo("json-identity");
    let root = temp.path().to_string_lossy().into_owned();
    let real = fs::canonicalize(temp.path())
        .expect("canonical root")
        .to_string_lossy()
        .into_owned();
    let normalize = |bytes: &[u8]| {
        String::from_utf8(bytes.to_vec())
            .expect("UTF-8")
            .replace(&real, "<ROOT>")
            .replace(&root, "<ROOT>")
    };
    let goldens = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/t34-json");
    for (name, args) in JSON_GOLDENS {
        let output = run(temp.path(), args);
        let actual = format!(
            "exit {}\n--- stdout\n{}--- stderr\n{}",
            output.status.code().expect("exit code"),
            normalize(&output.stdout),
            normalize(&output.stderr)
        );
        let expected =
            fs::read_to_string(goldens.join(format!("{name}.out"))).expect("read golden");
        assert_eq!(actual, expected, "{name}: {args:?}");
    }
}
