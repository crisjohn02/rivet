//! Integration tests for the T27 context traversal: depth two, relationship
//! flags, and the fixed work caps (spec §16.3, §16.4 item 4).
//!
//! Like `context_rank.rs` (T26), these index a purpose-built PHP fixture with
//! the real `rivet index` binary and call [`rivet_cli::context::collect_ranked`]
//! directly against the committed store, because `rivet context` is not wired
//! until T30. Every test asserts the full ordered `(id, reason, resolution)`
//! sequence, not counts alone.

#![cfg(feature = "lang-php")]

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_cli::context::{
    Collection, ContextOptions, MAX_CANDIDATES, MAX_EXAMINED_USES, collect_candidates,
    collect_ranked,
};
use rivet_core::ContextConfig;
use rivet_store::{Store, SymbolRow};
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
            "rivet-traversal-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(path.join(".git")).expect("create temporary Git root");
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
fn authored_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    for entry in fs::read_dir(&source).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), temp.path().join(entry.file_name())).expect("copy fixture file");
        }
    }
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

/// A repository holding `files` as `(path, source)` pairs, indexed and opened.
fn indexed(label: &str, files: &[(&str, &str)]) -> (TempDir, Store) {
    let temp = TempDir::new(label);
    for (name, source) in files {
        write_file(temp.path(), name, source);
    }
    let store = index_and_open(temp.path());
    (temp, store)
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

/// The configuration-default options (depth 2, everything included).
fn defaults() -> ContextOptions {
    ContextOptions::from_config(&ContextConfig::default())
}

/// Default options at `depth`.
fn at_depth(depth: u8) -> ContextOptions {
    ContextOptions {
        depth,
        ..defaults()
    }
}

/// Runs the T27 collection for `id`.
fn collect(store: &Store, id: &str, options: &ContextOptions) -> Collection {
    let target = symbol(store, id);
    collect_ranked(store, &target, &ContextConfig::default(), options)
        .unwrap_or_else(|error| panic!("collecting {id}: {}", error.message))
}

/// The `(id, reason, resolution)` of each candidate in rank order.
fn sequence(collection: &Collection) -> Vec<(String, &'static str, &'static str)> {
    collection
        .candidates
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

/// Builds an expected sequence from string slices.
fn expect(
    rows: &[(&str, &'static str, &'static str)],
) -> Vec<(String, &'static str, &'static str)> {
    rows.iter()
        .map(|(id, reason, resolution)| (id.to_string(), *reason, *resolution))
        .collect()
}

/// How many times `id` appears in the collection.
fn occurrences(collection: &Collection, id: &str) -> usize {
    collection
        .candidates
        .iter()
        .filter(|candidate| candidate.id() == id)
        .count()
}

// ---------------------------------------------------------------------------
// T26 equivalence
// ---------------------------------------------------------------------------

#[test]
fn depth_one_with_everything_included_equals_the_t26_list_for_every_fixture_symbol() {
    let temp = authored_repo("t26-equality");
    let store = index_and_open(temp.path());
    let config = ContextConfig::default();
    let options = at_depth(1);

    let symbols = store.list_symbols().expect("list symbols");
    assert!(symbols.len() > 10, "the authored fixture has many symbols");
    for target in &symbols {
        let t26 = collect_candidates(&store, target, &config).expect("T26 collection");
        let t27 = collect_ranked(&store, target, &config, &options).expect("T27 collection");
        assert_eq!(
            t27.candidates, t26,
            "depth-1 T27 must equal T26 for {}",
            target.id
        );
        assert!(
            !t27.candidate_limit_reached,
            "no cap is near on the authored fixture ({})",
            target.id
        );
    }
}

// ---------------------------------------------------------------------------
// Depth two
// ---------------------------------------------------------------------------

const DEPTH_TWO_APP: &str = "<?php
declare(strict_types=1);
namespace App;
class Svc
{
    public function a(): void
    {
        helper();
    }
    public function b(): void {}
}
function helper(): void
{
    leaf();
}
function leaf(): void {}
function useSvc(Svc $s): void
{
    $s->a();
    other();
    helper();
}
function other(): void {}
";

const A: &str = "App.php#App\\Svc::a";
const B: &str = "App.php#App\\Svc::b";
const SVC: &str = "App.php#App\\Svc";
const HELPER: &str = "App.php#App\\helper";
const LEAF: &str = "App.php#App\\leaf";
const USE_SVC: &str = "App.php#App\\useSvc";
const OTHER: &str = "App.php#App\\other";

#[test]
fn depth_two_candidates_are_second_degree_with_the_weakest_link_resolution() {
    let (_temp, store) = indexed("depth-two", &[("App.php", DEPTH_TWO_APP)]);

    // Depth 1: `a` calls `helper` (exact), `useSvc` calls `a` through a typed
    // receiver (scoped), and `Svc` is the parent. Depth 2 from `helper`
    // reaches `leaf` over exact/exact, so `exact`; from `useSvc` reaches
    // `other` over scoped/exact, so the weakest link is `scoped`.
    let collection = collect(&store, A, &defaults());
    assert_eq!(
        sequence(&collection),
        expect(&[
            (A, "target", "exact"),
            (HELPER, "callee", "exact"),
            (USE_SVC, "caller", "scoped"),
            (SVC, "parent", "exact"),
            (LEAF, "second_degree", "exact"),
            (OTHER, "second_degree", "scoped"),
        ])
    );
    assert!(!collection.candidate_limit_reached);

    // Depth 1 omits every second-degree candidate.
    assert_eq!(
        sequence(&collect(&store, A, &at_depth(1))),
        expect(&[
            (A, "target", "exact"),
            (HELPER, "callee", "exact"),
            (USE_SVC, "caller", "scoped"),
            (SVC, "parent", "exact"),
        ])
    );
}

#[test]
fn a_symbol_reachable_at_both_depths_appears_once_at_depth_one_and_the_target_never_reappears() {
    let (_temp, store) = indexed("both-depths", &[("App.php", DEPTH_TWO_APP)]);
    let collection = collect(&store, A, &defaults());

    // `helper` is a direct callee of `a` and also a callee of the depth-1
    // caller `useSvc`; `Svc` is the parent and also `useSvc`'s parameter type;
    // `a` itself is `useSvc`'s callee and `helper`'s caller.
    for (id, reason) in [(HELPER, "callee"), (SVC, "parent"), (A, "target")] {
        assert_eq!(occurrences(&collection, id), 1, "{id}: {collection:?}");
        let found = collection
            .candidates
            .iter()
            .find(|candidate| candidate.id() == id)
            .expect("present");
        assert_eq!(found.reason.as_str(), reason, "{id}");
    }
    assert_eq!(collection.candidates[0].id(), A, "the target stays first");
    // The sibling `b` is never reached through containment.
    assert_eq!(occurrences(&collection, B), 0);
}

#[test]
fn the_best_path_wins_even_when_a_weaker_path_is_traversed_first() {
    // Frontier order is tuple order, so the scoped callee `Svc::m` (reason 2)
    // is expanded before the exact caller `y` (reason 4). `z` is first found
    // over scoped/exact, then over exact/exact; the retained tuple is exact.
    let (_temp, store) = indexed(
        "best-path",
        &[(
            "P.php",
            "<?php
declare(strict_types=1);
namespace P;
class Svc
{
    public function m(): void
    {
        z();
    }
}
function t(Svc $s): void
{
    $s->m();
}
function y(): void
{
    t();
    z();
}
function z(): void {}
",
        )],
    );
    let collection = collect(&store, "P.php#P\\t", &defaults());
    assert_eq!(
        sequence(&collection),
        expect(&[
            ("P.php#P\\t", "target", "exact"),
            ("P.php#P\\Svc", "type", "exact"),
            ("P.php#P\\Svc::m", "callee", "scoped"),
            ("P.php#P\\y", "caller", "exact"),
            ("P.php#P\\z", "second_degree", "exact"),
        ])
    );
}

// ---------------------------------------------------------------------------
// Relationship flags
// ---------------------------------------------------------------------------

const FLAGS_APP: &str = "<?php
declare(strict_types=1);
namespace App;
function target(): void
{
    callee();
}
function callee(): void
{
    deep();
}
function deep(): void {}
function caller(): void
{
    target();
    callee();
}
function outer(): void
{
    caller();
}
function sideCaller(): void
{
    callee();
}
";
const TARGET_TEST: &str = "<?php
declare(strict_types=1);
namespace Tests;
use function App\\target;
function testTarget(): void
{
    target();
}
";
const CALLER_TEST: &str = "<?php
declare(strict_types=1);
namespace Tests;
use function App\\caller;
function testCaller(): void
{
    caller();
}
";
const CALLEE_TEST: &str = "<?php
declare(strict_types=1);
namespace Tests;
use function App\\callee;
function testCallee(): void
{
    callee();
}
";

const TARGET: &str = "App.php#App\\target";
const CALLEE: &str = "App.php#App\\callee";
const DEEP: &str = "App.php#App\\deep";
const CALLER: &str = "App.php#App\\caller";
const OUTER: &str = "App.php#App\\outer";
const SIDE_CALLER: &str = "App.php#App\\sideCaller";
const TEST_TARGET: &str = "tests/TargetTest.php#Tests\\testTarget";
const TEST_CALLER: &str = "tests/CallerTest.php#Tests\\testCaller";
const TEST_CALLEE: &str = "tests/CalleeTest.php#Tests\\testCallee";

fn flags_store(label: &str) -> (TempDir, Store) {
    indexed(
        label,
        &[
            ("App.php", FLAGS_APP),
            ("tests/TargetTest.php", TARGET_TEST),
            ("tests/CallerTest.php", CALLER_TEST),
            ("tests/CalleeTest.php", CALLEE_TEST),
        ],
    )
}

#[test]
fn every_relationship_is_included_by_default() {
    let (_temp, store) = flags_store("flags-default");
    let collection = collect(&store, TARGET, &defaults());
    assert_eq!(
        sequence(&collection),
        expect(&[
            (TARGET, "target", "exact"),
            (CALLEE, "callee", "exact"),
            (TEST_TARGET, "test", "exact"),
            (CALLER, "caller", "exact"),
            (DEEP, "second_degree", "exact"),
            (OUTER, "second_degree", "exact"),
            (SIDE_CALLER, "second_degree", "exact"),
            (TEST_CALLEE, "second_degree", "exact"),
            (TEST_CALLER, "second_degree", "exact"),
        ])
    );
}

#[test]
fn excluding_callers_removes_caller_links_at_both_depths() {
    let (_temp, store) = flags_store("flags-callers");
    let options = ContextOptions {
        include_callers: false,
        ..defaults()
    };
    // `caller` calls both `target` and `callee`: it is absent even though the
    // second hop `target -> callee <- caller` would reach it, as are the
    // depth-2 callers `sideCaller` and `outer` and every test caller.
    assert_eq!(
        sequence(&collect(&store, TARGET, &options)),
        expect(&[
            (TARGET, "target", "exact"),
            (CALLEE, "callee", "exact"),
            (DEEP, "second_degree", "exact"),
        ])
    );
}

#[test]
fn excluding_callees_removes_callee_links_at_both_depths() {
    let (_temp, store) = flags_store("flags-callees");
    let options = ContextOptions {
        include_callees: false,
        ..defaults()
    };
    // `callee` is absent even though the second hop `target <- caller ->
    // callee` would reach it, and so is its own callee `deep`.
    assert_eq!(
        sequence(&collect(&store, TARGET, &options)),
        expect(&[
            (TARGET, "target", "exact"),
            (TEST_TARGET, "test", "exact"),
            (CALLER, "caller", "exact"),
            (OUTER, "second_degree", "exact"),
            (TEST_CALLER, "second_degree", "exact"),
        ])
    );
}

#[test]
fn excluding_tests_removes_test_file_caller_links_at_both_depths() {
    let (_temp, store) = flags_store("flags-tests");
    let options = ContextOptions {
        include_tests: false,
        ..defaults()
    };
    // The depth-1 `test` caller and both depth-2 test callers are gone.
    assert_eq!(
        sequence(&collect(&store, TARGET, &options)),
        expect(&[
            (TARGET, "target", "exact"),
            (CALLEE, "callee", "exact"),
            (CALLER, "caller", "exact"),
            (DEEP, "second_degree", "exact"),
            (OUTER, "second_degree", "exact"),
            (SIDE_CALLER, "second_degree", "exact"),
        ])
    );

    // The default comes from configuration.
    let config = ContextConfig {
        include_tests: false,
        ..ContextConfig::default()
    };
    assert!(!ContextOptions::from_config(&config).include_tests);
}

// ---------------------------------------------------------------------------
// What does not expand
// ---------------------------------------------------------------------------

#[test]
fn name_only_links_never_seed_or_produce_depth_two_candidates() {
    let (_temp, store) = indexed(
        "name-only",
        &[(
            "N.php",
            "<?php
declare(strict_types=1);
namespace N;
function target(): void
{
    mid();
}
function mid(): void {}
function unknownCaller($x): void
{
    $x->target();
    onlyFromUnknown();
}
function onlyFromUnknown(): void {}
function nameOnlyMidCaller($y): void
{
    $y->mid();
}
",
        )],
    );

    // The links exist: `unknownCaller` calls `onlyFromUnknown` exactly, and
    // `nameOnlyMidCaller` is a name-only caller of `mid`.
    assert_eq!(
        sequence(&collect(&store, "N.php#N\\unknownCaller", &at_depth(1))),
        expect(&[
            ("N.php#N\\unknownCaller", "target", "exact"),
            ("N.php#N\\onlyFromUnknown", "callee", "exact"),
        ])
    );
    assert_eq!(
        sequence(&collect(&store, "N.php#N\\mid", &at_depth(1))),
        expect(&[
            ("N.php#N\\mid", "target", "exact"),
            ("N.php#N\\target", "caller", "exact"),
            ("N.php#N\\nameOnlyMidCaller", "caller", "name_match"),
        ])
    );

    // From `target`: the name-only caller is a depth-1 candidate but opens no
    // hop (no `onlyFromUnknown`), and the name-only second hop from `mid`
    // produces no candidate (no `nameOnlyMidCaller`).
    assert_eq!(
        sequence(&collect(&store, "N.php#N\\target", &defaults())),
        expect(&[
            ("N.php#N\\target", "target", "exact"),
            ("N.php#N\\mid", "callee", "exact"),
            ("N.php#N\\unknownCaller", "caller", "name_match"),
        ])
    );
}

const CONTAINMENT: &str = "<?php
declare(strict_types=1);
namespace C;
class Dep {}
function t(): void {}
class Box
{
    private ?Dep $dep = null;
    public function a(): void
    {
        t();
    }
    public function sibling(): void {}
}
";

#[test]
fn containment_neither_expands_a_parent_nor_adds_a_parent_at_depth_two() {
    let (_temp, store) = indexed("containment", &[("C.php", CONTAINMENT)]);

    // `Box` has its own exact `type` link to `Dep` through the property.
    assert_eq!(
        sequence(&collect(&store, "C.php#C\\Box", &at_depth(1))),
        expect(&[
            ("C.php#C\\Box", "target", "exact"),
            ("C.php#C\\Dep", "type", "exact"),
        ])
    );

    // Target `Box::a`: the parent `Box` is a depth-1 candidate but does not
    // seed depth 2, so neither `Dep` nor the sibling appears.
    assert_eq!(
        sequence(&collect(&store, "C.php#C\\Box::a", &defaults())),
        expect(&[
            ("C.php#C\\Box::a", "target", "exact"),
            ("C.php#C\\t", "callee", "exact"),
            ("C.php#C\\Box", "parent", "exact"),
        ])
    );

    // Target `t`: the depth-1 caller `Box::a` is expanded, but its parent
    // `Box` is not added at depth 2 (nor, therefore, `Dep` or the sibling).
    assert_eq!(
        sequence(&collect(&store, "C.php#C\\t", &defaults())),
        expect(&[
            ("C.php#C\\t", "target", "exact"),
            ("C.php#C\\Box::a", "caller", "exact"),
        ])
    );
}

// ---------------------------------------------------------------------------
// Termination
// ---------------------------------------------------------------------------

#[test]
fn call_cycles_terminate_with_each_symbol_once() {
    let (_temp, store) = indexed(
        "cycle",
        &[(
            "Cycle.php",
            "<?php
declare(strict_types=1);
namespace Cycle;
function a(): void
{
    b();
}
function b(): void
{
    c();
}
function c(): void
{
    a();
}
function alpha(): void
{
    beta();
}
function beta(): void
{
    alpha();
}
",
        )],
    );

    // Three-cycle a -> b -> c -> a: `b` is a callee, `c` a caller, and every
    // second hop leads back to a symbol already present.
    let three = collect(&store, "Cycle.php#Cycle\\a", &defaults());
    assert_eq!(
        sequence(&three),
        expect(&[
            ("Cycle.php#Cycle\\a", "target", "exact"),
            ("Cycle.php#Cycle\\b", "callee", "exact"),
            ("Cycle.php#Cycle\\c", "caller", "exact"),
        ])
    );
    assert!(!three.candidate_limit_reached);

    // Two-cycle alpha <-> beta.
    let two = collect(&store, "Cycle.php#Cycle\\alpha", &defaults());
    assert_eq!(
        sequence(&two),
        expect(&[
            ("Cycle.php#Cycle\\alpha", "target", "exact"),
            ("Cycle.php#Cycle\\beta", "callee", "exact"),
        ])
    );
    assert!(!two.candidate_limit_reached);
}

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// `hub` calls f1, f2, f3, f4, then f1 again. The functions are declared in
/// reverse, so rank order (declaration start byte) is the reverse of
/// traversal order (use start byte): survivors of a cap reveal which one
/// decided them.
const HUB: &str = "<?php
declare(strict_types=1);
namespace H;
function f4(): void {}
function f3(): void {}
function f2(): void {}
function f1(): void {}
function hub(): void
{
    f1();
    f2();
    f3();
    f4();
    f1();
}
";
const HUB_ID: &str = "H.php#H\\hub";
const F1: &str = "H.php#H\\f1";
const F2: &str = "H.php#H\\f2";
const F3: &str = "H.php#H\\f3";
const F4: &str = "H.php#H\\f4";

fn hub_all() -> Vec<(String, &'static str, &'static str)> {
    expect(&[
        (HUB_ID, "target", "exact"),
        (F4, "callee", "exact"),
        (F3, "callee", "exact"),
        (F2, "callee", "exact"),
        (F1, "callee", "exact"),
    ])
}

#[test]
fn the_caps_default_to_the_spec_values() {
    assert_eq!(MAX_CANDIDATES, 1_000);
    assert_eq!(MAX_EXAMINED_USES, 10_000);
    let options = defaults();
    assert_eq!(options.max_candidates, 1_000);
    assert_eq!(options.max_examined_uses, 10_000);
    assert_eq!(options.depth, 2, "configured max_depth defaults to 2");
    assert!(options.include_tests && options.include_callers && options.include_callees);
}

#[test]
fn the_candidate_cap_is_exact_at_the_boundary() {
    let (_temp, store) = indexed("candidate-cap", &[("H.php", HUB)]);

    // Exactly five unique candidates (target included), and depth 2 finds
    // nothing new: the repeated `f1()` and every second hop only revisit
    // present IDs, so the flag is false.
    let exact = collect(
        &store,
        HUB_ID,
        &ContextOptions {
            max_candidates: 5,
            ..defaults()
        },
    );
    assert_eq!(sequence(&exact), hub_all());
    assert!(
        !exact.candidate_limit_reached,
        "exactly the cap is not a cut"
    );

    // One fewer: the fourth use (`f4()`) would be the fifth unique candidate,
    // so traversal stops. The survivors are the first three by use order,
    // listed in rank order.
    let cut = collect(
        &store,
        HUB_ID,
        &ContextOptions {
            max_candidates: 4,
            ..defaults()
        },
    );
    assert_eq!(
        sequence(&cut),
        expect(&[
            (HUB_ID, "target", "exact"),
            (F3, "callee", "exact"),
            (F2, "callee", "exact"),
            (F1, "callee", "exact"),
        ])
    );
    assert!(cut.candidate_limit_reached);
    assert!(cut.candidates.len() <= 4);

    // A cap of one admits only the target.
    let one = collect(
        &store,
        HUB_ID,
        &ContextOptions {
            max_candidates: 1,
            ..defaults()
        },
    );
    assert_eq!(sequence(&one), expect(&[(HUB_ID, "target", "exact")]));
    assert!(one.candidate_limit_reached);
}

#[test]
fn the_candidate_cap_boundary_holds_at_depth_two() {
    // The target has one callee, which has one further callee.
    let (_temp, store) = indexed(
        "candidate-cap-depth-two",
        &[(
            "D.php",
            "<?php
declare(strict_types=1);
namespace D;
function t(): void
{
    m();
}
function m(): void
{
    n();
}
function n(): void {}
",
        )],
    );
    let full = expect(&[
        ("D.php#D\\t", "target", "exact"),
        ("D.php#D\\m", "callee", "exact"),
        ("D.php#D\\n", "second_degree", "exact"),
    ]);
    let exact = collect(
        &store,
        "D.php#D\\t",
        &ContextOptions {
            max_candidates: 3,
            ..defaults()
        },
    );
    assert_eq!(sequence(&exact), full);
    assert!(!exact.candidate_limit_reached);

    let cut = collect(
        &store,
        "D.php#D\\t",
        &ContextOptions {
            max_candidates: 2,
            ..defaults()
        },
    );
    assert_eq!(sequence(&cut), full[..2].to_vec());
    assert!(cut.candidate_limit_reached);
}

#[test]
fn the_examined_use_cap_is_exact_at_the_boundary() {
    let (_temp, store) = indexed("use-cap", &[("H.php", HUB)]);
    let with = |depth: u8, max_examined_uses: usize| {
        collect(
            &store,
            HUB_ID,
            &ContextOptions {
                depth,
                max_examined_uses,
                ..defaults()
            },
        )
    };

    // Depth 1 examines exactly the five contained calls of `hub` (it has no
    // callers).
    let exact = with(1, 5);
    assert_eq!(sequence(&exact), hub_all());
    assert!(!exact.candidate_limit_reached);

    // Four: every candidate was found, but the fifth use was left unexamined,
    // so work remained and the flag is true.
    let four = with(1, 4);
    assert_eq!(sequence(&four), hub_all());
    assert!(four.candidate_limit_reached);

    // Three: `f4()` is never examined.
    let three = with(1, 3);
    assert_eq!(
        sequence(&three),
        expect(&[
            (HUB_ID, "target", "exact"),
            (F3, "callee", "exact"),
            (F2, "callee", "exact"),
            (F1, "callee", "exact"),
        ])
    );
    assert!(three.candidate_limit_reached);

    // Depth 2 adds each callee's caller uses: f4, f3, f2 one each and f1 two,
    // for ten in total. Ten is exact; nine is a cut.
    let exact_two = with(2, 10);
    assert_eq!(sequence(&exact_two), hub_all());
    assert!(!exact_two.candidate_limit_reached);
    let cut_two = with(2, 9);
    assert_eq!(sequence(&cut_two), hub_all());
    assert!(cut_two.candidate_limit_reached);
}

#[test]
fn a_capped_collection_is_deterministic() {
    let (_temp, store) = indexed("determinism", &[("H.php", HUB)]);
    let options = ContextOptions {
        max_candidates: 3,
        ..defaults()
    };
    let first = collect(&store, HUB_ID, &options);
    let second = collect(&store, HUB_ID, &options);
    assert_eq!(first, second);
    assert_eq!(
        sequence(&first),
        expect(&[
            (HUB_ID, "target", "exact"),
            (F2, "callee", "exact"),
            (F1, "callee", "exact"),
        ])
    );
    assert!(first.candidate_limit_reached);
}

#[test]
fn an_invalid_depth_or_zero_candidate_cap_is_rejected() {
    let (_temp, store) = indexed("invalid", &[("H.php", HUB)]);
    let target = symbol(&store, HUB_ID);
    for options in [
        at_depth(0),
        at_depth(3),
        ContextOptions {
            max_candidates: 0,
            ..defaults()
        },
    ] {
        let error = collect_ranked(&store, &target, &ContextConfig::default(), &options)
            .expect_err("invalid options must fail");
        assert_eq!(error.code, "invalid_arguments", "{options:?}");
    }
}

/// A hub function calling `count` distinct functions, declared in call order.
fn generated_hub(count: usize) -> String {
    let mut source = String::from("<?php\ndeclare(strict_types=1);\nnamespace G;\n");
    for index in 0..count {
        writeln!(source, "function f{index:04}(): void {{}}").expect("write");
    }
    source.push_str("function hub(): void\n{\n");
    for index in 0..count {
        writeln!(source, "    f{index:04}();").expect("write");
    }
    source.push_str("}\n");
    source
}

/// The expected ranked sequence for a generated hub keeping `kept` callees.
fn generated_expectation(kept: usize) -> Vec<(String, &'static str, &'static str)> {
    let mut rows = vec![("G.php#G\\hub".to_string(), "target", "exact")];
    for index in 0..kept {
        rows.push((format!("G.php#G\\f{index:04}"), "callee", "exact"));
    }
    rows
}

#[test]
fn the_default_candidate_cap_holds_at_scale() {
    // 999 callees plus the target is exactly the default cap of 1,000.
    let (_exact_temp, exact_store) = indexed("scale-exact", &[("G.php", &generated_hub(999))]);
    let exact = collect(&exact_store, "G.php#G\\hub", &defaults());
    assert_eq!(exact.candidates.len(), MAX_CANDIDATES);
    assert_eq!(sequence(&exact), generated_expectation(999));
    assert!(!exact.candidate_limit_reached);

    // 1,100 callees: the first 999 by use order survive, never more than the
    // cap, and the flag is true.
    let (_over_temp, over_store) = indexed("scale-over", &[("G.php", &generated_hub(1_100))]);
    let over = collect(&over_store, "G.php#G\\hub", &defaults());
    assert_eq!(over.candidates.len(), MAX_CANDIDATES);
    assert_eq!(sequence(&over), generated_expectation(999));
    assert!(over.candidate_limit_reached);
}

#[test]
fn the_default_examined_use_cap_holds_at_scale() {
    // One callee called `count` times: two candidates, `count` examined uses
    // at depth 1.
    let repeated = |count: usize| {
        let mut source = String::from(
            "<?php\ndeclare(strict_types=1);\nnamespace R;\nfunction f(): void {}\nfunction hub(): void\n{\n",
        );
        for _ in 0..count {
            source.push_str("    f();\n");
        }
        source.push_str("}\n");
        source
    };
    let expected = expect(&[
        ("R.php#R\\hub", "target", "exact"),
        ("R.php#R\\f", "callee", "exact"),
    ]);

    let (_exact_temp, exact_store) = indexed("uses-exact", &[("R.php", &repeated(10_000))]);
    let exact = collect(&exact_store, "R.php#R\\hub", &at_depth(1));
    assert_eq!(sequence(&exact), expected);
    assert!(!exact.candidate_limit_reached);

    let (_over_temp, over_store) = indexed("uses-over", &[("R.php", &repeated(10_001))]);
    let over = collect(&over_store, "R.php#R\\hub", &at_depth(1));
    assert_eq!(sequence(&over), expected);
    assert!(over.candidate_limit_reached);
}
