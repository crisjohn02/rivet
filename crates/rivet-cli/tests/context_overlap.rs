//! Integration tests for T29 context overlap suppression, the segment limit,
//! and the omission counts (spec §16.4 items 3 and 4; OUTPUT-CONTRACT
//! `rivet context`).
//!
//! `rivet context` is not wired yet (T30), so, like `context_fit.rs`, these
//! tests index a repository with the real binary and call
//! [`rivet_cli::budget::fit_context`] against the committed store. Segments,
//! forms, sources, and counts are asserted exactly.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_cli::budget::{
    ContextEntry, ContextFit, DEFAULT_SEGMENT_LIMIT, Form, Omitted, context_entries,
    estimate_tokens, fit, fit_collection, fit_context,
};
use rivet_cli::context::{Candidate, ContextOptions, Reason, collect_ranked};
use rivet_core::{Collapse, ContextConfig, Resolution};
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
            "rivet-context-overlap-{label}-{}-{nanos}-{unique}",
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

const HELPER_PHP: &str = "\
<?php
namespace App;

final class Helper
{
    public const LIMIT = 3;

    public static function make(): Helper
    {
        return new Helper();
    }

    public function small(): int
    {
        return self::LIMIT;
    }

    public function big(): int
    {
        $total = 0;
        for ($i = 0; $i < 10; $i++) {
            $total += $this->small();
        }
        return $total;
    }
}
";

const CLIENT_PHP: &str = "\
<?php
namespace App;

final class Client
{
    public function useIt(Helper $helper): int
    {
        return $helper->small();
    }

    public function useBoth(Helper $helper): int
    {
        return $helper->small() + $helper->big();
    }
}
";

const MULTI_PHP: &str = "<?php\nnamespace App;\nfinal class Multi\n{\n    public const ZED = 'A = 1', A = 1, AB = 2;\n    public int $zeta, $alpha = 2,\n        $al;\n}\n";

const NESTED_PHP: &str =
    "<?php\nfunction outer(): void\n{\n    function inner(): void\n    {\n    }\n    inner();\n}\n";

/// The purpose-built fixture: a class, its methods, callers of those methods
/// in another class, a multi-name declaration, and a function declared in
/// another function's body.
fn helper_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::write(temp.path().join("Helper.php"), HELPER_PHP).expect("write Helper.php");
    fs::write(temp.path().join("Client.php"), CLIENT_PHP).expect("write Client.php");
    fs::write(temp.path().join("Multi.php"), MULTI_PHP).expect("write Multi.php");
    fs::write(temp.path().join("Nested.php"), NESTED_PHP).expect("write Nested.php");
    temp
}

const HELPER: &str = "Helper.php#App\\Helper";
const MAKE: &str = "Helper.php#App\\Helper::make";
const SMALL: &str = "Helper.php#App\\Helper::small";
const BIG: &str = "Helper.php#App\\Helper::big";
const CLIENT: &str = "Client.php#App\\Client";
const USE_IT: &str = "Client.php#App\\Client::useIt";
const USE_BOTH: &str = "Client.php#App\\Client::useBoth";
const MULTI: &str = "Multi.php#App\\Multi";
const ZETA: &str = "Multi.php#App\\Multi::$zeta";
const ALPHA: &str = "Multi.php#App\\Multi::$alpha";
const OUTER: &str = "Nested.php#outer";
const INNER: &str = "Nested.php#inner";

const LAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::launch";
const RELAUNCH: &str = "SurveyService.php#App\\Services\\SurveyService::relaunch";
const SURVEY_CLASS: &str = "SurveyService.php#App\\Services\\SurveyService";
const RUN_ALIAS: &str = "ReportService.php#App\\Reporting\\ReportService::runAlias";
const RUN_TYPED: &str = "ReportService.php#App\\Reporting\\ReportService::runTyped";
const CONSTRUCT: &str = "Members.php#App\\Members\\AbstractThing::__construct";
const ABSTRACT_THING: &str = "Members.php#App\\Members\\AbstractThing";
const DESCRIBE: &str = "Members.php#App\\Members\\AbstractThing::describe";
const SEED: &str = "Members.php#App\\Members\\AbstractThing::$seed";

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

fn symbol(store: &Store, id: &str) -> SymbolRow {
    store
        .get_symbol(id)
        .expect("read symbol")
        .unwrap_or_else(|| panic!("missing symbol {id}"))
}

fn options(depth: u8) -> ContextOptions {
    ContextOptions {
        depth,
        ..ContextOptions::from_config(&ContextConfig::default())
    }
}

/// The ranked candidates for `id` at `depth` under the default configuration.
fn ranked(store: &Store, id: &str, depth: u8) -> Vec<Candidate> {
    collect_ranked(
        store,
        &symbol(store, id),
        &ContextConfig::default(),
        &options(depth),
    )
    .unwrap_or_else(|error| panic!("collecting {id}: {}", error.message))
    .candidates
}

/// A hand-built ranked list: `ids[0]` as the target, then each `(id, reason)`.
fn hand_built(store: &Store, target: &str, related: &[(&str, Reason)]) -> Vec<Candidate> {
    let mut candidates = vec![Candidate {
        symbol: symbol(store, target),
        reason: Reason::Target,
        resolution: Resolution::Exact,
    }];
    for (id, reason) in related {
        candidates.push(Candidate {
            symbol: symbol(store, id),
            reason: *reason,
            resolution: Resolution::Scoped,
        });
    }
    candidates
}

fn entries(store: &Store, candidates: &[Candidate]) -> Vec<ContextEntry> {
    context_entries(store, candidates).expect("entries")
}

fn fitted(
    store: &Store,
    candidates: &[Candidate],
    collapse: Collapse,
    budget: u64,
    limit: usize,
) -> ContextFit {
    fit_context(&entries(store, candidates), collapse, budget, limit, false)
        .unwrap_or_else(|error| panic!("{collapse:?} {budget} {limit}: {}", error.message))
}

/// `(id, form, estimate)` per segment.
fn shape(fit: &ContextFit) -> Vec<(String, Form, u64)> {
    fit.segments
        .iter()
        .map(|s| (s.candidate.symbol.id.clone(), s.form, s.estimated_tokens))
        .collect()
}

fn seg(id: &str, form: Form, estimate: u64) -> (String, Form, u64) {
    (id.to_string(), form, estimate)
}

fn omitted(budget: u64, overlap: u64, limit: u64) -> Omitted {
    Omitted {
        budget,
        overlap,
        limit,
    }
}

/// The number of non-overlapping occurrences of `needle` across every
/// emitted source.
fn occurrences(fit: &ContextFit, needle: &str) -> usize {
    fit.segments
        .iter()
        .map(|s| s.source.matches(needle).count())
        .sum()
}

const HELPER_FULL_SIGNATURE: &str = "\
final class Helper
{
    public const LIMIT = 3;
    public static function make(): Helper { … }
    public function small(): int { … }
    public function big(): int { … }
}";

// (a) A class target in full suppresses its own members arriving as callers
// or callees. A PHP class has no directly contained uses, so collection never
// yields its members; the ranked list is built by hand from stored rows.
#[test]
fn a_full_class_target_suppresses_its_members_as_overlap() {
    let temp = helper_repo("class-target");
    let store = index_and_open(temp.path());
    let candidates = hand_built(
        &store,
        HELPER,
        &[
            (SMALL, Reason::Callee),
            (BIG, Reason::Caller),
            (USE_IT, Reason::Caller),
        ],
    );
    for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
        let fit = fitted(&store, &candidates, collapse, 1_000, DEFAULT_SEGMENT_LIMIT);
        let caller_form = if collapse == Collapse::Always {
            (Form::Signature, 14)
        } else {
            (Form::Full, 29)
        };
        assert_eq!(
            shape(&fit),
            vec![
                seg(HELPER, Form::Full, 127),
                seg(USE_IT, caller_form.0, caller_form.1),
            ],
            "{collapse:?}"
        );
        assert_eq!(
            fit.segments[0].source,
            &HELPER_PHP[22..HELPER_PHP.len() - 1]
        );
        assert_eq!(fit.omitted, omitted(0, 2, 0), "{collapse:?}");
        assert_eq!(fit.estimated_tokens, 127 + caller_form.1);
        // The target's `small` declaration appears once, inside its body.
        assert_eq!(occurrences(&fit, "public function small(): int"), 1);
    }
}

// (b) A method target in full with its class as the parent: the parent is a
// signature omitting the target's declaration, or skipped under `never`.
#[test]
fn b_the_parent_of_a_full_method_target_is_a_reduced_signature() {
    let temp = helper_repo("parent");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, MAKE, 1);
    let ids: Vec<&str> = candidates.iter().map(|c| c.id()).collect();
    assert_eq!(ids, vec![MAKE, HELPER]);
    assert_eq!(candidates[1].reason, Reason::Type);

    let reduced = "\
final class Helper
{
    public const LIMIT = 3;
    public function small(): int { … }
    public function big(): int { … }
}";
    for collapse in [Collapse::Auto, Collapse::Always] {
        let fit = fitted(&store, &candidates, collapse, 1_000, DEFAULT_SEGMENT_LIMIT);
        assert_eq!(
            shape(&fit),
            vec![seg(MAKE, Form::Full, 26), seg(HELPER, Form::Signature, 44)]
        );
        assert_eq!(fit.segments[1].source, reduced);
        assert_eq!(estimate_tokens(reduced), 44);
        assert_eq!(fit.omitted, omitted(0, 0, 0));
        assert_eq!(fit.estimated_tokens, 70);
        assert_eq!(occurrences(&fit, "function make()"), 1);
    }
    // `never` has no signature form for the ancestor: skipped as overlap, at
    // every budget, never as budget.
    for budget in [26, 70, 127, 1_000] {
        let fit = fitted(&store, &candidates, Collapse::Never, budget, 50);
        assert_eq!(shape(&fit), vec![seg(MAKE, Form::Full, 26)]);
        assert_eq!(fit.omitted, omitted(0, 1, 0));
    }

    // The collected parent, with every other member also emitted as a caller:
    // the summary omits all three methods.
    let candidates = ranked(&store, SMALL, 1);
    let ids: Vec<&str> = candidates.iter().map(|c| c.id()).collect();
    assert_eq!(ids, vec![SMALL, USE_IT, USE_BOTH, BIG, HELPER]);
    assert_eq!(candidates[4].reason, Reason::Parent);
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(SMALL, Form::Full, 23),
            seg(USE_IT, Form::Full, 29),
            seg(USE_BOTH, Form::Full, 36),
            seg(BIG, Form::Full, 56),
            seg(HELPER, Form::Signature, 34),
        ]
    );
    assert_eq!(
        fit.segments[4].source,
        "final class Helper\n{\n    public const LIMIT = 3;\n    public static function make(): Helper { … }\n}"
    );
    assert_eq!(fit.estimated_tokens, 178);

    // The authored fixture's parent example.
    let temp = authored_repo("authored-parent");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, LAUNCH, 1);
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    let class = fit
        .segments
        .iter()
        .find(|s| s.candidate.symbol.id == SURVEY_CLASS)
        .expect("parent emitted");
    // relaunch is emitted as its own caller segment, so it is omitted too.
    assert!(
        fit.segments
            .iter()
            .any(|s| s.candidate.symbol.id == RELAUNCH)
    );
    assert_eq!(class.form, Form::Signature);
    assert_eq!(
        class.source,
        "final class SurveyService\n{\n    public const DEFAULT_LABEL = 'survey';\n    private string $label = 'survey';\n}"
    );
    assert_eq!(class.estimated_tokens, estimate_tokens(&class.source));
    assert_eq!(occurrences(&fit, "public function launch(): void"), 1);
}

// (c) The ordering trap: `Helper` (type, priority 1) is emitted as a
// signature before its method `small` (callee, priority 2) is visited.
#[test]
fn c_a_container_emitted_before_its_member_is_rebuilt_without_it() {
    let temp = helper_repo("trap");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, USE_IT, 1);
    let ids: Vec<&str> = candidates.iter().map(|c| c.id()).collect();
    assert_eq!(ids, vec![USE_IT, HELPER, SMALL, CLIENT]);
    let forms = entries(&store, &candidates);
    assert_eq!(
        forms[1].forms.signature.as_deref(),
        Some(HELPER_FULL_SIGNATURE)
    );
    assert_eq!(estimate_tokens(HELPER_FULL_SIGNATURE), 60);

    let rebuilt = "\
final class Helper
{
    public const LIMIT = 3;
    public static function make(): Helper { … }
    public function big(): int { … }
}";
    assert_eq!(estimate_tokens(rebuilt), 47);

    // auto at 120: useIt full (29); Helper full (127) does not fit, its
    // signature (60) does; small full (23) fits only on the net total
    // 29 + 47 + 23 = 99, after the rebuild; Client's reduced signature (27)
    // does not fit in the remaining 21.
    let fit = fitted(&store, &candidates, Collapse::Auto, 120, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_IT, Form::Full, 29),
            seg(HELPER, Form::Signature, 47),
            seg(SMALL, Form::Full, 23),
        ]
    );
    assert_eq!(fit.segments[1].source, rebuilt);
    assert_eq!(
        fit.segments[1].estimated_tokens,
        (rebuilt.len() as u64).div_ceil(3)
    );
    assert_eq!(
        fit.segments[2].source,
        "public function small(): int\n    {\n        return self::LIMIT;\n    }"
    );
    assert_eq!(occurrences(&fit, "public function small(): int"), 1);
    assert_eq!(fit.estimated_tokens, 99);
    assert_eq!(fit.omitted, omitted(1, 0, 0));

    // At 90 the rebuild still happens; small's full net total (99) is over,
    // so it takes its signature on the net total 29 + 47 + 10 = 86.
    let fit = fitted(&store, &candidates, Collapse::Auto, 90, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_IT, Form::Full, 29),
            seg(HELPER, Form::Signature, 47),
            seg(SMALL, Form::Signature, 10),
        ]
    );
    assert_eq!(fit.segments[1].source, rebuilt);
    assert_eq!(occurrences(&fit, "public function small(): int"), 1);

    // `always`: the same rebuild with small as a signature, then Client's
    // signature, which omits the target `useIt`.
    let fit = fitted(&store, &candidates, Collapse::Always, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_IT, Form::Full, 29),
            seg(HELPER, Form::Signature, 47),
            seg(SMALL, Form::Signature, 10),
            seg(CLIENT, Form::Signature, 27),
        ]
    );
    assert_eq!(fit.segments[1].source, rebuilt);
    assert_eq!(
        fit.segments[3].source,
        "final class Client\n{\n    public function useBoth(Helper $helper): int { … }\n}"
    );
    assert_eq!(occurrences(&fit, "public function small(): int"), 1);
    assert_eq!(occurrences(&fit, "public function useIt("), 1);
    assert_eq!(fit.estimated_tokens, 113);

    // With room for Helper in full, small is contained and suppressed.
    let fit = fitted(&store, &candidates, Collapse::Auto, 200, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_IT, Form::Full, 29),
            seg(HELPER, Form::Full, 127),
            seg(CLIENT, Form::Signature, 27),
        ]
    );
    assert_eq!(fit.omitted, omitted(0, 1, 0));
    assert_eq!(occurrences(&fit, "public function small(): int"), 1);
}

/// A constructor's promoted properties are declared in its header: once the
/// constructor is emitted, in either form, the parent summary lists neither
/// the constructor nor its promoted properties; once a promoted property is
/// emitted, the constructor's header would repeat it, so the parent omits the
/// constructor too and a constructor candidate is overlap.
#[test]
fn promoted_properties_live_in_the_constructor_header() {
    let temp = authored_repo("promoted");
    let store = index_and_open(temp.path());
    let with_describe = "\
abstract class AbstractThing
{
    public const FIRST = 1;
    public const SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left;
    public int $right = 2;
    abstract public function describe(): string;
}";
    let without_describe = "\
abstract class AbstractThing
{
    public const FIRST = 1;
    public const SECOND = 2;
    public static int $count = 0;
    public readonly string $title;
    public int $left;
    public int $right = 2;
}";

    // The constructor in full, then the reduced parent.
    let candidates = hand_built(&store, CONSTRUCT, &[(ABSTRACT_THING, Reason::Parent)]);
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(CONSTRUCT, Form::Full, 65),
            seg(
                ABSTRACT_THING,
                Form::Signature,
                estimate_tokens(with_describe)
            ),
        ]
    );
    assert_eq!(fit.segments[1].source, with_describe);

    // The constructor as a signature: the parent omits it by its own line
    // and its promoted properties by containment; a promoted property then
    // arriving is already shown by the constructor's header.
    let candidates = hand_built(
        &store,
        DESCRIBE,
        &[
            (CONSTRUCT, Reason::Caller),
            (SEED, Reason::Caller),
            (ABSTRACT_THING, Reason::Parent),
        ],
    );
    let fit = fitted(&store, &candidates, Collapse::Always, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(DESCRIBE, Form::Full, 15),
            seg(CONSTRUCT, Form::Signature, 24),
            seg(
                ABSTRACT_THING,
                Form::Signature,
                estimate_tokens(without_describe)
            ),
        ]
    );
    assert_eq!(fit.segments[2].source, without_describe);
    assert_eq!(fit.omitted, omitted(0, 1, 0));
    assert_eq!(occurrences(&fit, "private int $seed"), 1);

    // A promoted property target: the constructor candidate is overlap in
    // every mode, and the parent omits the property, the constructor whose
    // header shows it, and the other promoted property with it.
    let candidates = hand_built(
        &store,
        SEED,
        &[
            (CONSTRUCT, Reason::Caller),
            (ABSTRACT_THING, Reason::Parent),
        ],
    );
    for collapse in [Collapse::Auto, Collapse::Always] {
        let fit = fitted(&store, &candidates, collapse, 1_000, 50);
        assert_eq!(
            shape(&fit),
            vec![
                seg(SEED, Form::Full, 6),
                seg(
                    ABSTRACT_THING,
                    Form::Signature,
                    estimate_tokens(with_describe)
                ),
            ]
        );
        assert_eq!(fit.segments[1].source, with_describe);
        assert_eq!(fit.omitted, omitted(0, 1, 0));
        assert_eq!(occurrences(&fit, "private int $seed"), 1);
    }
    let fit = fitted(&store, &candidates, Collapse::Never, 1_000, 50);
    assert_eq!(shape(&fit), vec![seg(SEED, Form::Full, 6)]);
    assert_eq!(fit.omitted, omitted(0, 2, 0));
}

/// A function declared in another function's body lies outside that
/// function's header, so the outer signature and the inner declaration never
/// repeat each other; only the outer full body contains it.
#[test]
fn a_nested_declaration_is_not_in_its_ancestors_header() {
    let temp = helper_repo("nested");
    let store = index_and_open(temp.path());

    // The collected caller of `inner` is its ancestor `outer`: signature only.
    let candidates = ranked(&store, INNER, 1);
    let ids: Vec<&str> = candidates.iter().map(|c| c.id()).collect();
    assert_eq!(ids, vec![INNER, OUTER]);
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![seg(INNER, Form::Full, 12), seg(OUTER, Form::Signature, 8)]
    );
    assert_eq!(fit.segments[1].source, "function outer(): void");
    let fit = fitted(&store, &candidates, Collapse::Never, 1_000, 50);
    assert_eq!(fit.omitted, omitted(0, 1, 0));

    // `outer` as a signature first, then `inner`: still emitted.
    let candidates = hand_built(
        &store,
        USE_IT,
        &[(OUTER, Reason::Caller), (INNER, Reason::Callee)],
    );
    let fit = fitted(&store, &candidates, Collapse::Always, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_IT, Form::Full, 29),
            seg(OUTER, Form::Signature, 8),
            seg(INNER, Form::Signature, 8),
        ]
    );
    // `outer` in full contains `inner`: overlap.
    let fit = fitted(&store, &candidates, Collapse::Never, 1_000, 50);
    assert_eq!(shape(&fit)[1].0, OUTER);
    assert_eq!(fit.segments.len(), 2);
    assert_eq!(fit.omitted, omitted(0, 1, 0));
}

/// Members of one multi-name declaration share a span and byte-identical full
/// text, so an equal span counts as contained.
#[test]
fn an_equal_span_counts_as_contained() {
    let temp = helper_repo("equal-span");
    let store = index_and_open(temp.path());
    let candidates = hand_built(
        &store,
        ZETA,
        &[(ALPHA, Reason::Caller), (MULTI, Reason::Parent)],
    );
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![seg(ZETA, Form::Full, 14), seg(MULTI, Form::Signature, 34)]
    );
    assert_eq!(
        fit.segments[0].source,
        "public int $zeta, $alpha = 2,\n        $al;"
    );
    // The full segment covers all three names, so the summary lists none.
    assert_eq!(
        fit.segments[1].source,
        "final class Multi\n{\n    public const ZED = 'A = 1';\n    public const A = 1;\n    public const AB = 2;\n}"
    );
    assert_eq!(fit.omitted, omitted(0, 1, 0));

    // A signature suppresses nothing: with the target collapsed, `$alpha`
    // is an ancestor by its equal span, so it may only be a signature.
    let fit = fitted(&store, &candidates, Collapse::Auto, 13, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(ZETA, Form::Signature, 6),
            seg(ALPHA, Form::Signature, 7)
        ]
    );
    assert_eq!(fit.segments[1].source, "public int $alpha = 2");
    assert_eq!(fit.omitted, omitted(1, 0, 0));
}

// (d) The limit: exactly `limit` segments, the rest `limit`, except
// candidates that overlap, which count as `overlap` by precedence.
#[test]
fn d_the_limit_caps_segments_and_overlap_takes_precedence() {
    let temp = helper_repo("limit");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, USE_BOTH, 1);
    let ids: Vec<&str> = candidates.iter().map(|c| c.id()).collect();
    assert_eq!(ids, vec![USE_BOTH, HELPER, SMALL, BIG, CLIENT]);

    // Unlimited: Helper in full suppresses small and big.
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_BOTH, Form::Full, 36),
            seg(HELPER, Form::Full, 127),
            seg(CLIENT, Form::Signature, 26),
        ]
    );
    assert_eq!(fit.omitted, omitted(0, 2, 0));

    // Limit 2: small and big still overlap; Client counts as limit.
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 2);
    assert_eq!(
        shape(&fit),
        vec![seg(USE_BOTH, Form::Full, 36), seg(HELPER, Form::Full, 127)]
    );
    assert_eq!(fit.omitted, omitted(0, 2, 1));

    // Under `never` at limit 2, Client (an ancestor with no allowed form)
    // is overlap, not limit.
    let fit = fitted(&store, &candidates, Collapse::Never, 1_000, 2);
    assert_eq!(fit.segments.len(), 2);
    assert_eq!(fit.omitted, omitted(0, 3, 0));

    // `always` at limit 2: Helper is a signature, which suppresses nothing,
    // so every later candidate is limit.
    let fit = fitted(&store, &candidates, Collapse::Always, 1_000, 2);
    assert_eq!(
        shape(&fit),
        vec![
            seg(USE_BOTH, Form::Full, 36),
            seg(HELPER, Form::Signature, 60)
        ]
    );
    assert_eq!(fit.segments[1].source, HELPER_FULL_SIGNATURE);
    assert_eq!(fit.omitted, omitted(0, 0, 3));

    // Limit 1 emits only the target, in every mode.
    for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
        let fit = fitted(&store, &candidates, collapse, 1_000, 1);
        assert_eq!(shape(&fit), vec![seg(USE_BOTH, Form::Full, 36)]);
        let expected = if collapse == Collapse::Never {
            omitted(0, 1, 3)
        } else {
            omitted(0, 0, 4)
        };
        assert_eq!(fit.omitted, expected, "{collapse:?}");
    }

    // A limit of 0 is invalid.
    let error = fit_context(
        &entries(&store, &candidates),
        Collapse::Auto,
        1_000,
        0,
        false,
    )
    .unwrap_err();
    assert_eq!((error.code, error.exit), ("invalid_arguments", 2));

    // The authored fixture: launch's candidate set with limit 3.
    let temp = authored_repo("authored-limit");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, LAUNCH, 1);
    let fit = fitted(&store, &candidates, Collapse::Auto, 1_000, 3);
    assert_eq!(
        shape(&fit),
        vec![
            seg(LAUNCH, Form::Full, 29),
            seg(RUN_ALIAS, Form::Full, 34),
            seg(RUN_TYPED, Form::Full, 29),
        ]
    );
    assert_eq!(fit.omitted, omitted(0, 0, 3));
    let fit = fitted(&store, &candidates, Collapse::Never, 1_000, 3);
    assert_eq!(fit.omitted, omitted(0, 1, 2));
}

// (e) Budget omissions count as `budget` only when neither overlap nor limit
// applies.
#[test]
fn e_budget_omissions_are_budget_only_without_overlap_or_limit() {
    let temp = authored_repo("budget");
    let store = index_and_open(temp.path());
    let candidates = ranked(&store, LAUNCH, 1);

    // T28's auto result at 80, plus counts: relaunch, runUnknown, and the
    // parent's reduced signature do not fit.
    let fit = fitted(&store, &candidates, Collapse::Auto, 80, 50);
    assert_eq!(
        shape(&fit),
        vec![
            seg(LAUNCH, Form::Full, 29),
            seg(RUN_ALIAS, Form::Full, 34),
            seg(RUN_TYPED, Form::Signature, 17),
        ]
    );
    assert_eq!(fit.omitted, omitted(3, 0, 0));

    // The same budget under `never`: the parent is overlap, not budget.
    let fit = fitted(&store, &candidates, Collapse::Never, 80, 50);
    assert_eq!(
        shape(&fit),
        vec![seg(LAUNCH, Form::Full, 29), seg(RUN_ALIAS, Form::Full, 34)]
    );
    assert_eq!(fit.omitted, omitted(3, 1, 0));

    // The same budget at limit 2: the limit, reached, takes precedence.
    let fit = fitted(&store, &candidates, Collapse::Auto, 80, 2);
    assert_eq!(fit.omitted, omitted(0, 0, 4));
}

/// T27's `candidate_limit_reached` flows through unchanged.
#[test]
fn candidate_limit_reached_passes_through() {
    let temp = helper_repo("cap");
    let store = index_and_open(temp.path());
    let config = ContextConfig::default();
    for (cap, expected) in [(2, true), (1_000, false)] {
        let collection = collect_ranked(
            &store,
            &symbol(&store, USE_BOTH),
            &config,
            &ContextOptions {
                max_candidates: cap,
                ..options(1)
            },
        )
        .expect("collect");
        assert_eq!(collection.candidate_limit_reached, expected);
        let fit = fit_collection(&store, &collection, Collapse::Auto, 1_000, 50).expect("fits");
        assert_eq!(fit.candidate_limit_reached, expected);
        assert_eq!(
            fit.segments.len() as u64
                + fit.omitted.budget
                + fit.omitted.overlap
                + fit.omitted.limit,
            collection.candidates.len() as u64
        );
    }
}

/// True when `inner` lies within `outer` in the same file, bounds included.
fn within(inner: &SymbolRow, outer: &SymbolRow) -> bool {
    inner.file == outer.file
        && outer.start_byte <= inner.start_byte
        && inner.end_byte <= outer.end_byte
}

/// Asserts every T29 property of one successful fit.
fn check_fit(
    label: &str,
    candidates: &[Candidate],
    entries: &[ContextEntry],
    fit: &ContextFit,
    budget: u64,
    limit: usize,
) {
    // The budget, and estimates on the final sources.
    assert!(fit.estimated_tokens <= budget, "{label}");
    assert_eq!(
        fit.estimated_tokens,
        fit.segments.iter().map(|s| s.estimated_tokens).sum::<u64>(),
        "{label}"
    );
    for segment in &fit.segments {
        assert_eq!(
            segment.estimated_tokens,
            estimate_tokens(&segment.source),
            "{label}"
        );
    }
    // The limit and the target.
    assert!(fit.segments.len() <= limit, "{label}");
    assert_eq!(fit.segments[0].candidate.reason, Reason::Target, "{label}");
    // (f) Every explored candidate is emitted or counted exactly once.
    assert_eq!(
        fit.segments.len() as u64 + fit.omitted.budget + fit.omitted.overlap + fit.omitted.limit,
        candidates.len() as u64,
        "{label}: {:?}",
        fit.omitted
    );
    // Segments are unique, in rank order, and full sources are exact.
    let mut last = None;
    for segment in &fit.segments {
        let rank = candidates
            .iter()
            .position(|c| c.symbol.id == segment.candidate.symbol.id)
            .expect("segment is a candidate");
        assert!(last.is_none_or(|last| rank > last), "{label}: rank order");
        last = Some(rank);
        let forms = &entries[rank].forms;
        match segment.form {
            Form::Full => assert_eq!(segment.source, forms.full, "{label}"),
            Form::Signature => {
                let original = forms.signature.as_deref().expect("a signature exists");
                assert!(segment.source.len() <= original.len(), "{label}");
                // A summary with no separately emitted member is unreduced.
                let reduced = fit.segments.iter().any(|other| {
                    other.candidate.symbol.id != segment.candidate.symbol.id
                        && (other.candidate.symbol.parent_id.as_deref()
                            == Some(segment.candidate.symbol.id.as_str())
                            || (other.form == Form::Full
                                && within(&other.candidate.symbol, &segment.candidate.symbol)))
                });
                if !reduced {
                    assert_eq!(segment.source, original, "{label}");
                }
            }
        }
    }
    // No full body is ever emitted twice: no other segment's source contains
    // a full segment's body, and no full segment's span holds another segment.
    for (i, full) in fit.segments.iter().enumerate() {
        if full.form != Form::Full {
            continue;
        }
        for (j, other) in fit.segments.iter().enumerate() {
            if i == j {
                continue;
            }
            assert!(
                !other.source.contains(&full.source),
                "{label}: {} repeats the body of {}",
                other.candidate.symbol.id,
                full.candidate.symbol.id
            );
            assert!(
                !within(&other.candidate.symbol, &full.candidate.symbol),
                "{label}: {} lies inside full {}",
                other.candidate.symbol.id,
                full.candidate.symbol.id
            );
            assert!(
                !within(&full.candidate.symbol, &other.candidate.symbol)
                    || other.form == Form::Signature,
                "{label}: ancestor {} emitted in full",
                other.candidate.symbol.id
            );
        }
    }
    // A summary never lists a separately emitted direct member.
    for summary in &fit.segments {
        if summary.form != Form::Signature {
            continue;
        }
        for member in &fit.segments {
            if member.candidate.symbol.parent_id.as_deref()
                != Some(summary.candidate.symbol.id.as_str())
            {
                continue;
            }
            let Some(signature) = member.candidate.symbol.signature.as_deref() else {
                continue;
            };
            let listed = summary.source.lines().any(|line| {
                let line = line.trim_start();
                line == format!("{signature} {{ … }}") || line == format!("{signature};")
            });
            assert!(
                !listed,
                "{label}: {} lists emitted member {}",
                summary.candidate.symbol.id, member.candidate.symbol.id
            );
        }
    }
}

/// True when two candidates overlap by span in either direction.
fn any_overlap(candidates: &[Candidate]) -> bool {
    candidates.iter().enumerate().any(|(i, a)| {
        candidates
            .iter()
            .enumerate()
            .any(|(j, b)| i != j && within(&a.symbol, &b.symbol))
    })
}

/// (g) Every symbol as the target, depths 1 and 2, every mode, every budget
/// from 0 past the sum of all forms, and several limits.
fn sweep(store: &Store, label: &str) -> (u64, u64) {
    let config = ContextConfig::default();
    let symbols = store.list_symbols().expect("list symbols");
    let (mut successes, mut rebuilt) = (0u64, 0u64);
    for target in &symbols {
        for depth in [1, 2] {
            let candidates = collect_ranked(store, target, &config, &options(depth))
                .expect("collect")
                .candidates;
            let entries = entries(store, &candidates);
            let pairs: Vec<(Candidate, rivet_cli::budget::Forms)> = entries
                .iter()
                .map(|e| (e.candidate.clone(), e.forms.clone()))
                .collect();
            let overlapping = any_overlap(&candidates);
            let ceiling: u64 = entries
                .iter()
                .map(|e| {
                    estimate_tokens(&e.forms.full)
                        + e.forms.signature.as_deref().map_or(0, estimate_tokens)
                })
                .sum::<u64>()
                + 3;
            let target_full = estimate_tokens(&entries[0].forms.full);
            for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
                let target_min = match (collapse, entries[0].forms.signature.as_deref()) {
                    (Collapse::Never, _) | (_, None) => target_full,
                    (_, Some(signature)) => target_full.min(estimate_tokens(signature)),
                };
                for budget in 0..=ceiling {
                    for limit in [1, 2, 3, 5, DEFAULT_SEGMENT_LIMIT] {
                        let tag = format!(
                            "{label} {} d{depth} {collapse:?} {budget} {limit}",
                            target.id
                        );
                        match fit_context(&entries, collapse, budget, limit, false) {
                            Ok(fitted) => {
                                successes += 1;
                                assert!(budget >= target_min, "{tag}");
                                check_fit(&tag, &candidates, &entries, &fitted, budget, limit);
                                if fitted.segments.iter().any(|s| {
                                    s.form == Form::Signature
                                        && Some(s.source.as_str())
                                            != entries
                                                .iter()
                                                .find(|e| {
                                                    e.candidate.symbol.id == s.candidate.symbol.id
                                                })
                                                .and_then(|e| e.forms.signature.as_deref())
                                }) {
                                    rebuilt += 1;
                                }
                                // No overlap and a limit at least the candidate
                                // count: exactly T28's segments.
                                if !overlapping && limit >= candidates.len() {
                                    let t28 = fit(&pairs, collapse, budget).expect("T28 fits");
                                    assert_eq!(fitted.segments, t28.segments, "{tag}");
                                    assert_eq!(fitted.estimated_tokens, t28.estimated_tokens);
                                    assert_eq!(fitted.omitted.overlap, 0, "{tag}");
                                    assert_eq!(fitted.omitted.limit, 0, "{tag}");
                                }
                            }
                            Err(error) => {
                                assert_eq!((error.code, error.exit), ("budget_too_small", 8));
                                assert!(budget < target_min, "{tag}");
                                assert_eq!(
                                    error.extra.get("required_tokens"),
                                    Some(&Value::from(target_min))
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    (successes, rebuilt)
}

#[test]
fn g_sweep_the_authored_fixture() {
    let temp = authored_repo("sweep");
    let store = index_and_open(temp.path());
    let (successes, rebuilt) = sweep(&store, "authored");
    assert!(successes > 0 && rebuilt > 0, "{successes} {rebuilt}");
}

#[test]
fn g_sweep_the_helper_fixture() {
    let temp = helper_repo("sweep");
    let store = index_and_open(temp.path());
    let (successes, rebuilt) = sweep(&store, "helper");
    assert!(successes > 0 && rebuilt > 0, "{successes} {rebuilt}");
}

/// Hand-built lists the collector never produces: every symbol of a file as
/// candidates, in every rotation of source order and of its reverse, so a
/// container arrives before, between, and after its members, and nested
/// declarations (promoted properties, multi-name members) meet in both
/// orders.
fn sweep_hand_built(store: &Store, files: &[&str]) -> u64 {
    let symbols = store.list_symbols().expect("list symbols");
    let mut successes = 0;
    for file in files {
        let rows: Vec<&SymbolRow> = symbols.iter().filter(|s| s.file == *file).collect();
        assert!(rows.len() > 1, "{file}");
        let mut reversed = rows.clone();
        reversed.reverse();
        for base in [rows.clone(), reversed] {
            for shift in 0..base.len() {
                let mut order = base.clone();
                order.rotate_left(shift);
                let candidates: Vec<Candidate> = order
                    .iter()
                    .enumerate()
                    .map(|(i, row)| Candidate {
                        symbol: (*row).clone(),
                        reason: if i == 0 {
                            Reason::Target
                        } else {
                            Reason::Caller
                        },
                        resolution: Resolution::Exact,
                    })
                    .collect();
                let entries = entries(store, &candidates);
                let ceiling: u64 = entries
                    .iter()
                    .map(|e| {
                        estimate_tokens(&e.forms.full)
                            + e.forms.signature.as_deref().map_or(0, estimate_tokens)
                    })
                    .sum::<u64>()
                    + 3;
                for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
                    for budget in 0..=ceiling {
                        for limit in [1, 2, 4, DEFAULT_SEGMENT_LIMIT] {
                            let tag = format!("{file} {shift} {collapse:?} {budget} {limit}");
                            match fit_context(&entries, collapse, budget, limit, false) {
                                Ok(fitted) => {
                                    successes += 1;
                                    check_fit(&tag, &candidates, &entries, &fitted, budget, limit);
                                }
                                Err(error) => assert_eq!(error.code, "budget_too_small", "{tag}"),
                            }
                        }
                    }
                }
            }
        }
    }
    successes
}

#[test]
fn g_sweep_hand_built_orders_on_the_helper_fixture() {
    let temp = helper_repo("sweep-hand");
    let store = index_and_open(temp.path());
    assert!(
        sweep_hand_built(
            &store,
            &["Helper.php", "Multi.php", "Client.php", "Nested.php"]
        ) > 0
    );
}

#[test]
fn g_sweep_hand_built_orders_on_the_authored_fixture() {
    let temp = authored_repo("sweep-hand");
    let store = index_and_open(temp.path());
    assert!(
        sweep_hand_built(
            &store,
            &["Members.php", "SurveyService.php", "Documented.php"]
        ) > 0
    );
}

// (h) Determinism: the same input twice, and two independent snapshots.
#[test]
fn h_fitting_is_deterministic() {
    let first = helper_repo("det-a");
    let second = helper_repo("det-b");
    let store_a = index_and_open(first.path());
    let store_b = index_and_open(second.path());
    for target in [USE_IT, USE_BOTH, SMALL, MAKE] {
        for collapse in [Collapse::Auto, Collapse::Always, Collapse::Never] {
            for budget in [36, 90, 120, 200, 1_000] {
                for limit in [1, 3, 50] {
                    let run = |store: &Store| {
                        let collection = collect_ranked(
                            store,
                            &symbol(store, target),
                            &ContextConfig::default(),
                            &options(2),
                        )
                        .expect("collect");
                        fit_collection(store, &collection, collapse, budget, limit)
                            .map_err(|error| error.code)
                    };
                    let a = run(&store_a);
                    assert_eq!(a, run(&store_a), "{target} {collapse:?} {budget} {limit}");
                    assert_eq!(a, run(&store_b), "{target} {collapse:?} {budget} {limit}");
                }
            }
        }
    }
}
