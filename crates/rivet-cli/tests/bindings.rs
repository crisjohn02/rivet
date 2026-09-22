//! T19/T20/T21 integration tests: persisted resolution bindings.
//!
//! The authored PHP fixture is copied into a temporary Git root, indexed with
//! the real binary, and inspected by opening the committed `.rivet/index.db`
//! directly with [`rivet_store::Store::open`]. T19 resolves direct imports and
//! lexically bound functions as `exact`; T20 adds `$this`/`self` member uses
//! and explicit receiver types as `scoped`; T21 adds preceding `new` receiver
//! hints as `scoped`:
//!
//! - bind the `new SurveySvc()` alias type use in `ReportService::runAlias` to
//!   `App\Services\SurveyService` as `exact`;
//! - bind the top-level `launch();` in `boot.php` to `App\Boot\launch` as
//!   `exact`;
//! - bind `$this->launch()` and `self::DEFAULT_LABEL` inside `SurveyService`,
//!   the typed-parameter `$svc->launch()` in `ReportService::runTyped`, and the
//!   `new`-receiver calls in `ReportService::runAlias` and `boot.php` as
//!   `scoped`;
//! - leave the untyped `$x->launch()` unresolved;
//! - report the real `bindings` count in `index --json`; and
//! - reproduce byte-identical binding rows on a no-edit re-index.
//!
//! A separate temporary fixture checks the `use function` alias rule and the
//! PHP namespaced-to-global fallback, and another checks that a reassigned or
//! conditionally assigned `new` receiver stays unbound.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_store::{BindingRow, Store, UseRow};
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
            "rivet-bindings-{label}-{}-{nanos}-{unique}",
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

/// The `index --json` object, requiring a clean exit.
fn index_json(dir: &Path) -> Value {
    let output = run(dir, &["index", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Opens the committed cache created by a successful `index`.
fn open_store(root: &Path) -> Store {
    Store::open(&root.join(".rivet")).expect("open committed store")
}

/// Every use row for the authored fixture files.
fn all_use_rows(store: &Store) -> Vec<UseRow> {
    let mut rows = Vec::new();
    for file in store.list_files().expect("list files") {
        rows.extend(store.list_uses_for_file(&file.path).expect("list uses"));
    }
    rows
}

/// The use at `[start, end)` in `file` with the given ref kind.
fn use_at<'a>(uses: &'a [UseRow], file: &str, start: u32, end: u32, kind: &str) -> &'a UseRow {
    uses.iter()
        .find(|row| {
            row.file == file
                && row.start_byte == start
                && row.end_byte == end
                && row.ref_kind.as_str() == kind
        })
        .unwrap_or_else(|| panic!("missing {kind} use {file} [{start}..{end}]"))
}

/// The binding for `use_row`, if any.
fn binding_for<'a>(bindings: &'a [BindingRow], use_row: &UseRow) -> Option<&'a BindingRow> {
    let use_id = use_row.use_id.expect("persisted uses carry an ID");
    bindings.iter().find(|binding| binding.use_id == use_id)
}

#[test]
fn resolves_exact_imports_and_scoped_receivers_but_not_new_hints() {
    let temp = fixture_repo("fixture");
    let root = temp.path();

    let value = index_json(root);
    // Thirteen bindings: six `exact` from T19, four `scoped` from T20, and
    // three `scoped` from T21.
    //   T19 (exact):
    //   1. ReportService.php import alias `SurveySvc`
    //   2. ReportService.php import `SurveyService`
    //   3. ReportService.php `new SurveySvc()` type use (the gold alias case)
    //   4. ReportService.php `runTyped(SurveyService $svc)` parameter type
    //   5. boot.php top-level `launch()` call (the gold top-level case)
    //   6. boot.php `new \App\Services\SurveyService()` fully qualified type
    //   T20 (scoped):
    //   7. SurveyService.php `$this->label` property write
    //   8. SurveyService.php `self::DEFAULT_LABEL` constant read
    //   9. SurveyService.php `$this->launch()` call
    //  10. ReportService.php `$svc->launch()` typed-parameter call
    //   T21 (scoped):
    //  11. ReportService.php `$svc->launch()` after `new SurveySvc()`
    //  12. boot.php `$svc->launch()` after `new \App\Services\SurveyService()`
    //  13. boot.php interpolated `{$svc->launch()}`
    assert_eq!(value["bindings"], 13, "T21 binding count: {value}");

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");

    // The gold (b) alias use binds to the aliased class as `exact`.
    let alias = use_at(&uses, "ReportService.php", 564, 573, "type");
    let alias_binding = binding_for(&bindings, alias).expect("alias use must bind");
    assert_eq!(
        alias_binding.target_id, "SurveyService.php#App\\Services\\SurveyService",
        "alias must resolve through its import"
    );
    assert_eq!(alias_binding.resolution.as_str(), "exact");

    // The gold (c) top-level call binds to the namespaced function as `exact`.
    let top_level = use_at(&uses, "boot.php", 179, 185, "call");
    let call_binding = binding_for(&bindings, top_level).expect("top-level call must bind");
    assert_eq!(call_binding.target_id, "boot.php#App\\Boot\\launch");
    assert_eq!(call_binding.resolution.as_str(), "exact");

    // Gold (d): `$this->launch()` binds to the enclosing class member, scoped.
    let this_call = use_at(&uses, "SurveyService.php", 640, 646, "call");
    let this_binding = binding_for(&bindings, this_call).expect("$this->launch() must bind");
    assert_eq!(
        this_binding.target_id,
        "SurveyService.php#App\\Services\\SurveyService::launch"
    );
    assert_eq!(this_binding.resolution.as_str(), "scoped");

    // `$this->label` (write) binds to the declared property, not a method.
    let property_write = use_at(&uses, "SurveyService.php", 512, 517, "write");
    let property_binding =
        binding_for(&bindings, property_write).expect("$this->label write must bind");
    assert_eq!(
        property_binding.target_id,
        "SurveyService.php#App\\Services\\SurveyService::$label"
    );
    assert_eq!(property_binding.resolution.as_str(), "scoped");

    // `self::DEFAULT_LABEL` (read) binds to the class constant, case-sensitively.
    let const_read = use_at(&uses, "SurveyService.php", 526, 539, "read");
    let const_binding =
        binding_for(&bindings, const_read).expect("self::DEFAULT_LABEL read must bind");
    assert_eq!(
        const_binding.target_id,
        "SurveyService.php#App\\Services\\SurveyService::DEFAULT_LABEL"
    );
    assert_eq!(const_binding.resolution.as_str(), "scoped");

    // Gold (f): a typed parameter resolves the receiver to its class, scoped.
    let typed_call = use_at(&uses, "ReportService.php", 736, 742, "call");
    let typed_binding = binding_for(&bindings, typed_call).expect("typed $svc->launch() must bind");
    assert_eq!(
        typed_binding.target_id,
        "SurveyService.php#App\\Services\\SurveyService::launch"
    );
    assert_eq!(typed_binding.resolution.as_str(), "scoped");

    // T21: a preceding direct `new` binds the call as `scoped`.
    for (file, start, end, reason) in [
        ("ReportService.php", 591, 597, "aliased new SurveySvc()"),
        ("boot.php", 267, 273, "fully qualified new"),
        ("boot.php", 430, 436, "interpolated new receiver"),
    ] {
        let row = use_at(&uses, file, start, end, "call");
        let binding = binding_for(&bindings, row).expect(reason);
        assert_eq!(
            binding.target_id, "SurveyService.php#App\\Services\\SurveyService::launch",
            "{reason} must resolve to the new class's member"
        );
        assert_eq!(binding.resolution.as_str(), "scoped", "{reason}");
    }

    // Gold (g): an untyped receiver stays unbound.
    let untyped = use_at(&uses, "ReportService.php", 862, 868, "call");
    assert!(
        binding_for(&bindings, untyped).is_none(),
        "untyped $x receiver must stay unbound: {untyped:?}"
    );

    // No receiver-based call may claim `exact`: receiver evidence is `scoped`.
    for binding in &bindings {
        let Some(row) = uses.iter().find(|row| row.use_id == Some(binding.use_id)) else {
            continue;
        };
        if row.ref_kind.as_str() == "call" && row.receiver.is_some() {
            assert_ne!(
                binding.resolution.as_str(),
                "exact",
                "a receiver call must never be exact: {row:?}"
            );
        }
    }

    // A no-edit re-index reproduces byte-identical binding rows.
    drop(store);
    let _ = index_json(root);
    let store = open_store(root);
    assert_eq!(
        store.list_bindings().expect("list bindings"),
        bindings,
        "a no-edit re-index must keep identical bindings"
    );
}

/// Writes one PHP file under `root`.
fn write_php(root: &Path, name: &str, source: &str) {
    fs::write(root.join(name), source.as_bytes()).expect("write PHP fixture");
}

#[test]
fn function_import_alias_binds_and_without_it_stays_unresolved() {
    let temp = TempDir::new("function-alias");
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

    // With `use function A\launch;` the call resolves to A\launch.
    write_php(
        root,
        "C.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nlaunch();\n",
    );
    let value = index_json(root);
    assert_eq!(
        value["bindings"], 2,
        "import use and call both bind: {value}"
    );

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");
    let call = uses
        .iter()
        .find(|row| {
            row.file == "C.php" && row.ref_kind.as_str() == "call" && row.spelling == "launch"
        })
        .expect("C.php has a launch call");
    let binding = binding_for(&bindings, call).expect("imported call must bind");
    assert_eq!(binding.target_id, "A.php#A\\launch");
    assert_eq!(binding.resolution.as_str(), "exact");
    drop(store);

    // Without the import, namespace C has no `launch`, so the call is
    // unresolved rather than name-matched to A or B.
    write_php(
        root,
        "C.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nlaunch();\n",
    );
    let value = index_json(root);
    assert_eq!(value["bindings"], 0, "no import means no binding: {value}");

    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");
    let call = uses
        .iter()
        .find(|row| {
            row.file == "C.php" && row.ref_kind.as_str() == "call" && row.spelling == "launch"
        })
        .expect("C.php has a launch call");
    assert!(
        binding_for(&bindings, call).is_none(),
        "an unimported call must stay unresolved"
    );
}

/// The `call` use of `spelling` on `receiver` in `file`.
fn call_use<'a>(uses: &'a [UseRow], file: &str, spelling: &str, receiver: &str) -> &'a UseRow {
    uses.iter()
        .find(|row| {
            row.file == file
                && row.ref_kind.as_str() == "call"
                && row.spelling == spelling
                && row.receiver.as_deref() == Some(receiver)
        })
        .unwrap_or_else(|| panic!("missing {file} {spelling} call on {receiver}"))
}

#[test]
fn typed_receiver_binds_only_its_class_and_inheritance_stays_unbound() {
    let temp = TempDir::new("typed-receiver");
    let root = temp.path();
    fs::create_dir_all(root.join(".git")).expect("create .git");

    // Two unrelated classes with the same short name both declare `launch`.
    write_php(
        root,
        "A.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfinal class Widget\n{\n    public function launch(): void\n    {\n    }\n}\n",
    );
    write_php(
        root,
        "B.php",
        "<?php\ndeclare(strict_types=1);\nnamespace B;\nfinal class Widget\n{\n    public function launch(): void\n    {\n    }\n}\n",
    );
    // The typed receiver imports A\Widget, so `$p->launch()` must never bind
    // to B\Widget::launch.
    write_php(
        root,
        "C.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse A\\Widget;\nfinal class Runner\n{\n    public function go(Widget $p): void\n    {\n        $p->launch();\n    }\n}\n",
    );
    // Inheritance traversal is future work in v0.1: `Child` does not declare
    // `launch` even though `Base` does, so a `Child` receiver stays unbound
    // rather than resolving to the parent's method.
    write_php(
        root,
        "Base.php",
        "<?php\ndeclare(strict_types=1);\nnamespace D;\nclass Base\n{\n    public function launch(): void\n    {\n    }\n}\n",
    );
    write_php(
        root,
        "Child.php",
        "<?php\ndeclare(strict_types=1);\nnamespace D;\nfinal class Child extends Base\n{\n}\n",
    );
    write_php(
        root,
        "Caller.php",
        "<?php\ndeclare(strict_types=1);\nnamespace D;\nfinal class Caller\n{\n    public function go(Child $c): void\n    {\n        $c->launch();\n    }\n}\n",
    );

    let _ = index_json(root);
    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");

    // The typed receiver binds to the imported class only, as `scoped`.
    let typed = call_use(&uses, "C.php", "launch", "$p");
    let binding = binding_for(&bindings, typed).expect("typed receiver must bind");
    assert_eq!(binding.target_id, "A.php#A\\Widget::launch");
    assert_eq!(binding.resolution.as_str(), "scoped");
    assert_ne!(
        binding.target_id, "B.php#B\\Widget::launch",
        "the other same-name class must not win"
    );

    // The child receiver does not reach the parent's member in v0.1.
    let inherited = call_use(&uses, "Caller.php", "launch", "$c");
    assert!(
        binding_for(&bindings, inherited).is_none(),
        "inheritance is not traversed in v0.1: {inherited:?}"
    );
}

#[test]
fn new_receiver_conservatism_rejects_reassignment_and_control_flow() {
    let temp = TempDir::new("new-receiver");
    let root = temp.path();
    fs::create_dir_all(root.join(".git")).expect("create .git");

    // Two same-name classes that each declare `go`.
    write_php(
        root,
        "A.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfinal class Svc\n{\n    public function go(): void\n    {\n    }\n}\n",
    );
    write_php(
        root,
        "B.php",
        "<?php\ndeclare(strict_types=1);\nnamespace B;\nfinal class Svc\n{\n    public function go(): void\n    {\n    }\n}\n",
    );
    // Reassignment: the variable holds a different `new` by the second call,
    // so neither call is trustworthy.
    write_php(
        root,
        "Reassign.php",
        "<?php\ndeclare(strict_types=1);\n$s = new \\A\\Svc();\n$s->go();\n$s = new \\B\\Svc();\n$s->go();\n",
    );
    // A conditional assignment must not leak to a use outside the block.
    write_php(
        root,
        "Conditional.php",
        "<?php\ndeclare(strict_types=1);\nif ($c) {\n    $s = new \\A\\Svc();\n}\n$s->go();\n",
    );
    // No reassignment and no conditional: the call binds.
    write_php(
        root,
        "Safe.php",
        "<?php\ndeclare(strict_types=1);\n$s = new \\A\\Svc();\n$s->go();\n",
    );

    let value = index_json(root);
    let store = open_store(root);
    let uses = all_use_rows(&store);
    let bindings = store.list_bindings().expect("list bindings");

    // Reassign.php: both `go` calls stay unbound.
    let reassigned: Vec<&UseRow> = uses
        .iter()
        .filter(|row| row.file == "Reassign.php" && row.spelling == "go")
        .collect();
    assert_eq!(
        reassigned.len(),
        2,
        "the fixture has two calls: {reassigned:?}"
    );
    for row in reassigned {
        assert!(
            binding_for(&bindings, row).is_none(),
            "a reassigned receiver must not bind: {row:?}"
        );
    }

    // Conditional.php: the use is outside the assignment's block.
    let conditional = uses
        .iter()
        .find(|row| row.file == "Conditional.php" && row.spelling == "go")
        .expect("Conditional.php has a go call");
    assert!(
        binding_for(&bindings, conditional).is_none(),
        "a conditional assignment must not bind a use outside it: {conditional:?}"
    );

    // Safe.php: exactly one `scoped` binding to A\Svc::go.
    let safe = uses
        .iter()
        .find(|row| row.file == "Safe.php" && row.spelling == "go")
        .expect("Safe.php has a go call");
    let safe_binding = binding_for(&bindings, safe).expect("the safe receiver must bind");
    assert_eq!(safe_binding.target_id, "A.php#A\\Svc::go");
    assert_eq!(safe_binding.resolution.as_str(), "scoped");

    // Only the safe call's `go` use produces a receiver binding; the four
    // `new \A\Svc()`/`new \B\Svc()` type uses bind `exact` separately.
    let bound_go = uses
        .iter()
        .filter(|row| row.spelling == "go" && binding_for(&bindings, row).is_some())
        .count();
    assert_eq!(bound_go, 1, "only the safe call binds: {value}");
}
