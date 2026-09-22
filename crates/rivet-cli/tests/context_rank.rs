//! Integration tests for the T26 depth-1 context candidate collection and the
//! fixed integer ranking (spec §16.3).
//!
//! The `rivet context` command is not wired yet (T30), so no `context` output
//! exists to assert. Instead these tests follow the `bindings.rs` /
//! `reresolve.rs` pattern: the authored PHP fixture is copied into a temporary
//! Git root and indexed with the real `rivet index` binary, then
//! [`rivet_cli::context::collect_candidates`] is called directly against the
//! committed [`rivet_store::Store`] (the binary-only crate now has a library
//! target precisely so shared pipelines can be tested this way). Each test
//! asserts concrete canonical IDs, the full ordered reason sequence, and the
//! link resolution rather than only counts.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_cli::context::{Candidate, collect_candidates};
use rivet_core::ContextConfig;
use rivet_store::{Store, SymbolRow};
use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// Authored-fixture canonical IDs used as collection targets.
const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const SURVEY_CLASS: &str = "SurveyService.php#App\\Services\\SurveyService";
const RUN_ALIAS: &str = "ReportService.php#App\\Reporting\\ReportService::runAlias";
const RUN_TYPED: &str = "ReportService.php#App\\Reporting\\ReportService::runTyped";
const RUN_UNKNOWN: &str = "ReportService.php#App\\Reporting\\ReportService::runUnknown";
const REPORT_CLASS: &str = "ReportService.php#App\\Reporting\\ReportService";
const FIRST: &str = "Constants.php#App\\Constants\\FIRST";

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

/// Collects candidates for `id` with the default context configuration.
fn collect(store: &Store, id: &str) -> Vec<Candidate> {
    let target = symbol(store, id);
    collect_candidates(store, &target, &ContextConfig::default())
        .unwrap_or_else(|error| panic!("collecting {id}: {}", error.message))
}

/// The `(id, reason, resolution)` of each candidate in rank order.
fn sequence(candidates: &[Candidate]) -> Vec<(String, &'static str, &'static str)> {
    candidates
        .iter()
        .map(|candidate| {
            (
                candidate.id().to_string(),
                candidate.reason.as_str(),
                candidate.resolution.as_str(),
            )
        })
        .collect()
}

/// The candidate with `id`, if present.
fn candidate<'a>(candidates: &'a [Candidate], id: &str) -> Option<&'a Candidate> {
    candidates.iter().find(|candidate| candidate.id() == id)
}

#[test]
fn launch_lists_callers_and_parent_in_the_fixed_order() {
    let temp = fixture_repo("launch");
    let store = index_and_open(temp.path());
    let candidates = collect(&store, LAUNCH);

    // (depth, reason_priority, resolution_priority, file bytes, start_byte,
    // id): the four callers sort before the parent, the three `scoped` callers
    // sort before the `name_match` one by resolution, and file bytes then start
    // byte order the ties. The two `boot.php` top-level calls have no
    // containing symbol and contribute nothing.
    assert_eq!(
        sequence(&candidates),
        vec![
            (LAUNCH.to_string(), "target", "exact"),
            (RUN_ALIAS.to_string(), "caller", "scoped"),
            (RUN_TYPED.to_string(), "caller", "scoped"),
            (RELAUNCH.to_string(), "caller", "scoped"),
            (RUN_UNKNOWN.to_string(), "caller", "name_match"),
            (SURVEY_CLASS.to_string(), "parent", "exact"),
        ]
    );

    // No top-level caller invented a container.
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.symbol.file != "boot.php"),
        "top-level callers must be skipped, not guessed: {candidates:?}"
    );

    // The parent is the lexical container, not another caller.
    let parent = candidate(&candidates, SURVEY_CLASS).expect("parent candidate");
    assert_eq!(parent.reason.as_str(), "parent");
    assert_eq!(parent.resolution.as_str(), "exact");
}

#[test]
fn run_typed_has_a_callee_and_a_type_and_dedupes_the_import() {
    let temp = fixture_repo("run-typed");
    let store = index_and_open(temp.path());
    let candidates = collect(&store, RUN_TYPED);

    assert_eq!(
        sequence(&candidates),
        vec![
            (RUN_TYPED.to_string(), "target", "exact"),
            (SURVEY_CLASS.to_string(), "type", "exact"),
            (LAUNCH.to_string(), "callee", "scoped"),
            (REPORT_CLASS.to_string(), "parent", "exact"),
        ]
    );

    // The imported class is reached twice: as the parameter type and as the
    // file-level import target. Deduplication by ID keeps the better `type`
    // tuple, so the survey class appears exactly once.
    let survey = candidates
        .iter()
        .filter(|candidate| candidate.id() == SURVEY_CLASS)
        .count();
    assert_eq!(
        survey, 1,
        "type and import must deduplicate to one candidate: {candidates:?}"
    );
}

#[test]
fn run_alias_consumes_the_aliased_import_once() {
    let temp = fixture_repo("run-alias");
    let store = index_and_open(temp.path());
    let candidates = collect(&store, RUN_ALIAS);

    // `new SurveySvc()` resolves the aliased `use ... as SurveySvc;` import, so
    // the survey class is a `type` candidate. ReportService.php imports the
    // class twice (the alias and the plain name); both bind to the same
    // declaration, so the class is emitted once.
    assert_eq!(
        sequence(&candidates),
        vec![
            (RUN_ALIAS.to_string(), "target", "exact"),
            (SURVEY_CLASS.to_string(), "type", "exact"),
            (LAUNCH.to_string(), "callee", "scoped"),
            (REPORT_CLASS.to_string(), "parent", "exact"),
        ]
    );
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.id() == SURVEY_CLASS)
            .count(),
        1,
        "the twice-imported class must appear once: {candidates:?}"
    );
}

#[test]
fn a_top_level_constant_has_no_parent_and_is_not_an_error() {
    let temp = fixture_repo("no-parent");
    let store = index_and_open(temp.path());
    let candidates = collect(&store, FIRST);

    // `App\Constants\FIRST` is declared at file scope: the absence of a parent
    // is simply an absent candidate, never an error.
    assert_eq!(
        sequence(&candidates),
        vec![(FIRST.to_string(), "target", "exact")]
    );
}

#[test]
fn a_caller_in_a_test_glob_file_uses_the_test_reason() {
    let temp = git_repo("test-glob");
    let root = temp.path();
    write_file(
        root,
        "App.php",
        "<?php\ndeclare(strict_types=1);\nnamespace App;\nfunction target(): void {}\n",
    );
    write_file(
        root,
        "tests/TargetTest.php",
        "<?php\ndeclare(strict_types=1);\nnamespace Tests;\nuse function App\\target;\nfunction run(): void\n{\n    target();\n}\n",
    );
    let store = index_and_open(root);

    let candidates = collect(&store, "App.php#App\\target");
    assert_eq!(
        sequence(&candidates),
        vec![
            ("App.php#App\\target".to_string(), "target", "exact"),
            (
                "tests/TargetTest.php#Tests\\run".to_string(),
                "test",
                "exact"
            ),
        ],
        "a caller in tests/** is a source `test` relationship"
    );
}

#[test]
fn a_consumed_file_level_import_is_an_import_candidate() {
    let temp = git_repo("import");
    let root = temp.path();
    write_file(
        root,
        "Config.php",
        "<?php\ndeclare(strict_types=1);\nnamespace App\\Config;\nconst LIMIT = 10;\n",
    );
    write_file(
        root,
        "Use.php",
        "<?php\ndeclare(strict_types=1);\nnamespace App;\nuse const App\\Config\\LIMIT;\nfunction readLimit(): int\n{\n    return LIMIT;\n}\n",
    );
    let store = index_and_open(root);

    // The `return LIMIT;` use inside `readLimit` binds through the file-level
    // `use const App\Config\LIMIT;`, so the imported constant is a depth-1
    // `import` candidate. Nothing else links the two declarations.
    let candidates = collect(&store, "Use.php#App\\readLimit");
    assert_eq!(
        sequence(&candidates),
        vec![
            ("Use.php#App\\readLimit".to_string(), "target", "exact"),
            (
                "Config.php#App\\Config\\LIMIT".to_string(),
                "import",
                "exact"
            ),
        ]
    );
}

#[test]
fn a_name_only_caller_is_a_candidate_and_does_not_produce_a_callee() {
    let temp = fixture_repo("name-only");
    let store = index_and_open(temp.path());

    // `ReportService::runUnknown` calls `$x->launch()` with no stored binding.
    // The caller direction still yields a `name_match` candidate, confirming
    // the reading that a name-only link produces a depth-1 candidate and only
    // fails to open a further hop.
    let launch = collect(&store, LAUNCH);
    let unknown = candidate(&launch, RUN_UNKNOWN).expect("the name-only caller");
    assert_eq!(unknown.reason.as_str(), "caller");
    assert_eq!(unknown.resolution.as_str(), "name_match");

    // The callee direction has no declaration target to follow, so the
    // unresolved call adds nothing.
    let run_unknown = collect(&store, RUN_UNKNOWN);
    assert_eq!(
        sequence(&run_unknown),
        vec![
            (RUN_UNKNOWN.to_string(), "target", "exact"),
            (REPORT_CLASS.to_string(), "parent", "exact"),
        ],
        "an unresolved call must not invent a callee"
    );
}

#[test]
fn a_symbol_reachable_as_both_callee_and_caller_keeps_the_callee_ranking() {
    let temp = git_repo("callee-caller");
    let root = temp.path();
    write_file(
        root,
        "Mutual.php",
        "<?php\ndeclare(strict_types=1);\nnamespace Mutual;\nfunction alpha(): void\n{\n    beta();\n}\nfunction beta(): void\n{\n    alpha();\n}\n",
    );
    let store = index_and_open(root);

    // `alpha` calls `beta` and is called by it. Both relationships reach the
    // same ID, so deduplication must keep the better `callee` tuple (2) over the
    // `caller` tuple (4), and `beta` must appear exactly once.
    let candidates = collect(&store, "Mutual.php#Mutual\\alpha");
    assert_eq!(
        sequence(&candidates),
        vec![
            ("Mutual.php#Mutual\\alpha".to_string(), "target", "exact"),
            ("Mutual.php#Mutual\\beta".to_string(), "callee", "exact"),
        ]
    );
    assert_eq!(
        candidates
            .iter()
            .filter(|candidate| candidate.id() == "Mutual.php#Mutual\\beta")
            .count(),
        1,
        "the mutually recursive symbol must deduplicate: {candidates:?}"
    );
}

#[test]
fn collection_is_deterministic() {
    let temp = fixture_repo("determinism");
    let store = index_and_open(temp.path());

    let first = collect(&store, LAUNCH);
    let second = collect(&store, LAUNCH);
    assert_eq!(
        first, second,
        "the same snapshot must produce the identical candidate list"
    );
    assert!(!first.is_empty());
}
