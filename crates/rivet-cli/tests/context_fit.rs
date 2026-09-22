//! Integration tests for T28 context source estimation and greedy budget
//! fitting (spec §16.2, §16.4 items 1 and 2; OUTPUT-CONTRACT `rivet context`).
//!
//! `rivet context` is not wired yet (T30), so, like `context_rank.rs`, these
//! tests index a copy of the authored fixture (or a purpose-built repository)
//! with the real binary, then call [`rivet_cli::budget`] against the
//! committed [`rivet_store::Store`]. Every form is asserted as an exact
//! string and every estimate as an exact integer.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_cli::budget::{
    Fitted, Form, Forms, candidate_forms, estimate_tokens, fit, fit_candidates,
};
use rivet_cli::context::{Candidate, ContextOptions, Reason, collect_ranked};
use rivet_core::{Collapse, ContextConfig, Resolution};
use rivet_store::{Store, SymbolRow};
use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const SURVEY_CLASS: &str = "SurveyService.php#App\\Services\\SurveyService";
const RUN_ALIAS: &str = "ReportService.php#App\\Reporting\\ReportService::runAlias";
const RUN_TYPED: &str = "ReportService.php#App\\Reporting\\ReportService::runTyped";
const RUN_UNKNOWN: &str = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
const DOCUMENTED: &str = "Documented.php#App\\Documented\\Documented";
const DOCUMENTED_RUN: &str = "Documented.php#App\\Documented\\Documented::run";
const ABSTRACT_THING: &str = "Members.php#App\\Members\\AbstractThing";

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
            "rivet-context-fit-{label}-{}-{nanos}-{unique}",
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

/// An empty temporary Git root for a purpose-built fixture.
fn git_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// Writes one file under `root`, creating parent directories.
fn write_file(root: &Path, name: &str, source: &str) {
    let path = root.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture directory");
    }
    fs::write(path, source.as_bytes()).expect("write fixture file");
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs `index --json`, requiring a clean exit, and opens the committed cache.
fn index_and_open(root: &Path) -> Store {
    let output = run(root, &["index", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).expect("stdout is one JSON object");
    Store::open(&root.join(".rivet")).expect("open committed store")
}

/// The symbol row for `id`, which must exist in the snapshot.
fn symbol(store: &Store, id: &str) -> SymbolRow {
    store
        .get_symbol(id)
        .expect("read symbol")
        .unwrap_or_else(|| panic!("missing symbol {id}"))
}

/// The ranked depth-1 candidates for `id` under the default configuration.
fn ranked(store: &Store, id: &str) -> Vec<Candidate> {
    let target = symbol(store, id);
    let config = ContextConfig::default();
    let options = ContextOptions {
        depth: 1,
        ..ContextOptions::from_config(&config)
    };
    collect_ranked(store, &target, &config, &options)
        .unwrap_or_else(|error| panic!("collecting {id}: {}", error.message))
        .candidates
}

/// `id` as the lone target candidate.
fn target_only(store: &Store, id: &str) -> Vec<Candidate> {
    vec![Candidate {
        symbol: symbol(store, id),
        reason: Reason::Target,
        resolution: Resolution::Exact,
    }]
}

/// The forms of `id` as a lone target.
fn forms_of(store: &Store, id: &str) -> Forms {
    candidate_forms(store, &target_only(store, id))
        .expect("forms")
        .remove(0)
}

/// `(id, form, estimate)` per fitted segment.
fn shape(fitted: &Fitted) -> Vec<(String, Form, u64)> {
    fitted
        .segments
        .iter()
        .map(|segment| {
            (
                segment.candidate.symbol.id.clone(),
                segment.form,
                segment.estimated_tokens,
            )
        })
        .collect()
}

const LAUNCH_FULL: &str =
    "public function launch(): void\n    {\n        $this->label = self::DEFAULT_LABEL;\n    }";
const SURVEY_CLASS_SIGNATURE: &str = "\
final class SurveyService
{
    public const DEFAULT_LABEL = 'survey';
    private string $label = 'survey';
    public function launch(): void { … }
    public function relaunch(): void { … }
}";

#[test]
fn full_and_signature_forms_are_exact_with_exact_estimates() {
    let temp = fixture_repo("forms");
    let store = index_and_open(temp.path());

    let launch = forms_of(&store, LAUNCH);
    assert_eq!(launch.full, LAUNCH_FULL);
    assert_eq!(
        launch.signature.as_deref(),
        Some("public function launch(): void")
    );
    assert_eq!(estimate_tokens(&launch.full), 29); // 86 bytes
    assert_eq!(estimate_tokens(launch.signature.as_deref().unwrap()), 10); // 30 bytes

    // The container form is the T25a summary; `…` counts as three bytes.
    let class = forms_of(&store, SURVEY_CLASS);
    assert_eq!(class.signature.as_deref(), Some(SURVEY_CLASS_SIGNATURE));
    assert_eq!(SURVEY_CLASS_SIGNATURE.len(), 198);
    assert_eq!(estimate_tokens(SURVEY_CLASS_SIGNATURE), 66);
    let fixture = fs::read_to_string(temp.path().join("SurveyService.php")).expect("read fixture");
    let start = fixture
        .find("final class SurveyService")
        .expect("class start");
    let end = fixture.rfind('}').expect("class end") + 1;
    assert_eq!(class.full, &fixture[start..end]);
    assert_eq!(estimate_tokens(&class.full), 152); // 455 bytes
}

/// Spec §16.2 puts the doc comment in the signature form; the adapter's
/// summary does not carry it, so the context form prepends it.
#[test]
fn the_signature_form_includes_the_doc_comment() {
    let temp = fixture_repo("doc");
    let store = index_and_open(temp.path());

    let class = forms_of(&store, DOCUMENTED);
    let expected = "\
/**
 * A documented service.
 */
final class Documented
{
    public function run(): void { … }
    public function plain(): void { … }
    public function separated(): void { … }
}";
    assert_eq!(class.signature.as_deref(), Some(expected));
    // The full form is the declaration span, which starts at `final`.
    assert!(
        class.full.starts_with("final class Documented\n{"),
        "{:?}",
        class.full
    );

    let run = forms_of(&store, DOCUMENTED_RUN);
    assert_eq!(
        run.signature.as_deref(),
        Some("/**\n * Runs the documented work.\n */\npublic function run(): void")
    );
    assert_eq!(estimate_tokens(run.signature.as_deref().unwrap()), 22); // 64 bytes

    // An undocumented symbol's form has no leading comment.
    let launch = forms_of(&store, LAUNCH);
    assert_eq!(
        launch.signature.as_deref(),
        Some("public function launch(): void")
    );
}

/// Members rebuilt from stored rows render exactly the T25a summary, including
/// the multi-name declarations and the promoted property rule.
#[test]
fn stored_members_render_the_t25a_container_form() {
    let temp = fixture_repo("members");
    let store = index_and_open(temp.path());
    let expected = "\
abstract class AbstractThing
{
    public const FIRST = 1;
    public const SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left;
    public int $right = 2;
    abstract public function describe(): string;
    public function __construct(private int $seed, public string $tag = 'x') { … }
}";
    assert_eq!(
        forms_of(&store, ABSTRACT_THING).signature.as_deref(),
        Some(expected)
    );
}

/// Members of one multi-name declaration share a span, so their stored rows
/// sort by ID; the form must still list them in source order, which here is
/// the reverse of alphabetical order.
#[test]
fn multi_name_members_keep_source_order_not_id_order() {
    let temp = git_repo("multi-name");
    write_file(
        temp.path(),
        "Multi.php",
        "<?php\nnamespace App;\nfinal class Multi\n{\n    public const ZED = 'A = 1', A = 1, AB = 2;\n    public int $zeta, $alpha = 2,\n        $al;\n}\n",
    );
    let store = index_and_open(temp.path());
    let expected = "\
final class Multi
{
    public const ZED = 'A = 1';
    public const A = 1;
    public const AB = 2;
    public int $zeta;
    public int $alpha = 2;
    public int $al;
}";
    assert_eq!(
        forms_of(&store, "Multi.php#App\\Multi")
            .signature
            .as_deref(),
        Some(expected)
    );
}

/// The full form comes from the snapshot's stored bytes, never the file on
/// disk: editing the file without a refresh changes nothing.
#[test]
fn the_full_form_reads_stored_bytes_not_the_disk() {
    let temp = fixture_repo("stored");
    let store = index_and_open(temp.path());
    fs::write(
        temp.path().join("SurveyService.php"),
        "<?php\n// rewritten on disk after indexing\n",
    )
    .expect("overwrite fixture");
    let launch = forms_of(&store, LAUNCH);
    assert_eq!(launch.full, LAUNCH_FULL);
}

/// Estimates for the `launch` candidate set (target, three scoped callers,
/// one name-match caller, parent), in tokens:
///
/// | candidate  | full | signature |
/// |------------|------|-----------|
/// | launch     | 29   | 10        |
/// | runAlias   | 34   | 11        |
/// | runTyped   | 29   | 17        |
/// | relaunch   | 23   | 11        |
/// | runUnknown | 23   | 12        |
/// | class      | 152  | 66        |
fn launch_candidates(store: &Store) -> Vec<Candidate> {
    let candidates = ranked(store, LAUNCH);
    let ids: Vec<&str> = candidates.iter().map(|c| c.symbol.id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            LAUNCH,
            RUN_ALIAS,
            RUN_TYPED,
            RELAUNCH,
            RUN_UNKNOWN,
            SURVEY_CLASS
        ]
    );
    let forms = candidate_forms(store, &candidates).expect("forms");
    let estimates: Vec<(u64, u64)> = forms
        .iter()
        .map(|forms| {
            (
                estimate_tokens(&forms.full),
                estimate_tokens(forms.signature.as_deref().expect("signature")),
            )
        })
        .collect();
    assert_eq!(
        estimates,
        vec![(29, 10), (34, 11), (29, 17), (23, 11), (23, 12), (152, 66)]
    );
    candidates
}

#[test]
fn target_fits_exactly_at_the_budget_and_fails_over_one_under() {
    let temp = fixture_repo("exact");
    let store = index_and_open(temp.path());
    let candidates = launch_candidates(&store);

    let exact = fit_candidates(&store, &candidates, Collapse::Auto, 29).expect("fits");
    assert_eq!(shape(&exact), vec![(LAUNCH.to_string(), Form::Full, 29)]);
    assert_eq!(exact.segments[0].source, LAUNCH_FULL);
    assert_eq!(exact.estimated_tokens, 29);

    // One under: the target collapses to 10, leaving 18 for runAlias's
    // signature (11); nothing else fits in the remaining 7.
    let under = fit_candidates(&store, &candidates, Collapse::Auto, 28).expect("fits");
    assert_eq!(
        shape(&under),
        vec![
            (LAUNCH.to_string(), Form::Signature, 10),
            (RUN_ALIAS.to_string(), Form::Signature, 11),
        ]
    );
    assert_eq!(under.segments[0].source, "public function launch(): void");
    assert_eq!(under.estimated_tokens, 21);
}

#[test]
fn the_three_modes_differ_on_the_fixture_candidates() {
    let temp = fixture_repo("modes");
    let store = index_and_open(temp.path());
    let candidates = launch_candidates(&store);

    let auto = fit_candidates(&store, &candidates, Collapse::Auto, 80).expect("auto");
    assert_eq!(
        shape(&auto),
        vec![
            (LAUNCH.to_string(), Form::Full, 29),
            (RUN_ALIAS.to_string(), Form::Full, 34),
            (RUN_TYPED.to_string(), Form::Signature, 17),
        ]
    );
    assert_eq!(auto.estimated_tokens, 80);
    assert_eq!(
        auto.segments[2].source,
        "public function runTyped(SurveyService $svc): void"
    );

    // `always` still takes the target in full because it fits.
    let always = fit_candidates(&store, &candidates, Collapse::Always, 80).expect("always");
    assert_eq!(
        shape(&always),
        vec![
            (LAUNCH.to_string(), Form::Full, 29),
            (RUN_ALIAS.to_string(), Form::Signature, 11),
            (RUN_TYPED.to_string(), Form::Signature, 17),
            (RELAUNCH.to_string(), Form::Signature, 11),
            (RUN_UNKNOWN.to_string(), Form::Signature, 12),
        ]
    );
    assert_eq!(always.estimated_tokens, 80);

    let never = fit_candidates(&store, &candidates, Collapse::Never, 80).expect("never");
    assert_eq!(
        shape(&never),
        vec![
            (LAUNCH.to_string(), Form::Full, 29),
            (RUN_ALIAS.to_string(), Form::Full, 34),
        ]
    );
    assert_eq!(never.estimated_tokens, 63);
}

/// Under `never` at 52, runAlias (34) and runTyped (29) are skipped, and the
/// later, smaller relaunch (23) still fits exactly.
#[test]
fn skipped_candidates_do_not_stop_a_later_smaller_one() {
    let temp = fixture_repo("skip");
    let store = index_and_open(temp.path());
    let candidates = launch_candidates(&store);
    let fitted = fit_candidates(&store, &candidates, Collapse::Never, 52).expect("fits");
    assert_eq!(
        shape(&fitted),
        vec![
            (LAUNCH.to_string(), Form::Full, 29),
            (RELAUNCH.to_string(), Form::Full, 23),
        ]
    );
    assert_eq!(fitted.estimated_tokens, 52);
}

#[test]
fn a_target_that_cannot_fit_is_error_8_with_the_allowed_minimum() {
    let temp = fixture_repo("error-8");
    let store = index_and_open(temp.path());
    let candidates = launch_candidates(&store);

    let never = fit_candidates(&store, &candidates, Collapse::Never, 28).unwrap_err();
    assert_eq!((never.code, never.exit), ("budget_too_small", 8));
    assert_eq!(never.extra.get("budget_tokens"), Some(&Value::from(28)));
    assert_eq!(never.extra.get("required_tokens"), Some(&Value::from(29)));

    for collapse in [Collapse::Auto, Collapse::Always] {
        let error = fit_candidates(&store, &candidates, collapse, 9).unwrap_err();
        assert_eq!((error.code, error.exit), ("budget_too_small", 8));
        assert_eq!(error.extra.get("budget_tokens"), Some(&Value::from(9)));
        assert_eq!(error.extra.get("required_tokens"), Some(&Value::from(10)));
        // One more token and the collapsed target fits.
        let fitted = fit_candidates(&store, &candidates, collapse, 10).expect("fits");
        assert_eq!(
            shape(&fitted),
            vec![(LAUNCH.to_string(), Form::Signature, 10)]
        );
    }
}

/// Every fixture symbol as the target, at depth 2, in every mode, at every
/// budget from 0 through the sum of all its candidates' forms plus slack: a
/// success never exceeds the budget, its total is the sum of its segments,
/// each segment is exactly one of its candidate's forms (never a partial
/// body), the target is first, segments keep rank order, and an error occurs
/// exactly when the target's smallest allowed form exceeds the budget.
#[test]
fn no_success_exceeds_the_budget_for_any_target_mode_or_budget() {
    let temp = fixture_repo("sweep");
    let store = index_and_open(temp.path());
    let config = ContextConfig::default();
    let options = ContextOptions {
        depth: 2,
        ..ContextOptions::from_config(&config)
    };
    let symbols = store.list_symbols().expect("list symbols");
    assert!(symbols.len() > 30, "sweep covers the whole fixture");
    let mut successes = 0u64;
    let mut failures = 0u64;
    for target in &symbols {
        let candidates = collect_ranked(&store, target, &config, &options)
            .expect("collect")
            .candidates;
        let forms = candidate_forms(&store, &candidates).expect("forms");
        let entries: Vec<(Candidate, Forms)> = candidates
            .iter()
            .cloned()
            .zip(forms.iter().cloned())
            .collect();
        let ceiling: u64 = forms
            .iter()
            .map(|f| estimate_tokens(&f.full) + f.signature.as_deref().map_or(0, estimate_tokens))
            .sum::<u64>()
            + 3;
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            let target_full = estimate_tokens(&forms[0].full);
            let target_min = match (collapse, forms[0].signature.as_deref()) {
                (Collapse::Never, _) | (_, None) => target_full,
                (_, Some(signature)) => target_full.min(estimate_tokens(signature)),
            };
            for budget in 0..=ceiling {
                match fit(&entries, collapse, budget) {
                    Ok(fitted) => {
                        successes += 1;
                        assert!(budget >= target_min);
                        assert!(
                            fitted.estimated_tokens <= budget,
                            "{} {collapse:?} {budget}: {}",
                            target.id,
                            fitted.estimated_tokens
                        );
                        assert_eq!(
                            fitted.estimated_tokens,
                            fitted
                                .segments
                                .iter()
                                .map(|s| s.estimated_tokens)
                                .sum::<u64>()
                        );
                        assert_eq!(fitted.segments[0].candidate.symbol.id, target.id);
                        let mut last_rank = 0;
                        for (position, segment) in fitted.segments.iter().enumerate() {
                            let rank = candidates
                                .iter()
                                .position(|c| c.symbol.id == segment.candidate.symbol.id)
                                .expect("segment is a candidate");
                            assert!(position == 0 || rank > last_rank, "rank order");
                            last_rank = rank;
                            let expected = match segment.form {
                                Form::Full => Some(forms[rank].full.as_str()),
                                Form::Signature => forms[rank].signature.as_deref(),
                            };
                            assert_eq!(Some(segment.source.as_str()), expected);
                            assert_eq!(segment.estimated_tokens, estimate_tokens(&segment.source));
                            if position > 0 {
                                match collapse {
                                    Collapse::Always => assert_eq!(segment.form, Form::Signature),
                                    Collapse::Never => assert_eq!(segment.form, Form::Full),
                                    Collapse::Auto => {}
                                }
                            }
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        assert_eq!((error.code, error.exit), ("budget_too_small", 8));
                        assert!(budget < target_min, "{} {collapse:?} {budget}", target.id);
                        assert_eq!(
                            error.extra.get("required_tokens"),
                            Some(&Value::from(target_min))
                        );
                        assert_eq!(error.extra.get("budget_tokens"), Some(&Value::from(budget)));
                    }
                }
            }
        }
    }
    assert!(successes > 0 && failures > 0, "{successes} {failures}");
}

/// Two independently indexed copies of the fixture fit identically.
#[test]
fn fitting_is_deterministic_across_snapshots() {
    let first = fixture_repo("det-a");
    let second = fixture_repo("det-b");
    let store_a = index_and_open(first.path());
    let store_b = index_and_open(second.path());
    for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
        for budget in [29, 52, 80, 200, 1_000] {
            let a = fit_candidates(&store_a, &ranked(&store_a, LAUNCH), collapse, budget)
                .expect("fits");
            let b = fit_candidates(&store_b, &ranked(&store_b, LAUNCH), collapse, budget)
                .expect("fits");
            assert_eq!(a, b, "{collapse:?} {budget}");
            let again = fit_candidates(&store_a, &ranked(&store_a, LAUNCH), collapse, budget)
                .expect("fits");
            assert_eq!(a, again);
        }
    }
}
