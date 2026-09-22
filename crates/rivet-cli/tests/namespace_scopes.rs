//! AF1 integration tests: PHP namespace and scope structure.
//!
//! Each case writes a temporary Git root, indexes it with the real binary, and
//! inspects the committed `.rivet/index.db` through [`rivet_store::Store`].
//! They cover the audit's reproductions (findings 1, 2 and 3 of
//! `orchestration/review-notes/audit-2026-09-22.md`) and the namespace forms
//! around them:
//!
//! - a top-level closure or arrow function sees its file's namespace and
//!   imports: an unqualified function call may still fall back to the global
//!   function (PHP's runtime rule), but a class never does;
//! - each block of a multi-namespace file, unbraced or braced, resolves against
//!   its own namespace and only its own `use` imports, including a closure
//!   nested in a method in a later block;
//! - a global `namespace { }` block declares bare global names; and
//! - a use that cannot be attributed to exactly one namespace block records no
//!   binding.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_store::{BindingRow, Store, SymbolRow, UseRow};

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
            "rivet-ns-scopes-{label}-{}-{nanos}-{unique}",
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

/// The committed rows of one indexed temporary repository.
struct Indexed {
    _temp: TempDir,
    symbols: Vec<SymbolRow>,
    uses: Vec<UseRow>,
    bindings: Vec<BindingRow>,
}

impl Indexed {
    /// Writes `files`, runs `rivet index --json`, and loads the snapshot.
    fn new(label: &str, files: &[(&str, &str)]) -> Indexed {
        let temp = TempDir::new(label);
        for (name, source) in files {
            fs::write(temp.path().join(name), source.as_bytes()).expect("write PHP file");
        }
        let output = Command::new(RIVET)
            .args(["index", "--json"])
            .current_dir(temp.path())
            .output()
            .expect("run the rivet binary");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
        let symbols = store.list_symbols().expect("list symbols");
        let mut uses = Vec::new();
        for file in store.list_files().expect("list files") {
            uses.extend(store.list_uses_for_file(&file.path).expect("list uses"));
        }
        let bindings = store.list_bindings().expect("list bindings");
        drop(store);
        Indexed {
            _temp: temp,
            symbols,
            uses,
            bindings,
        }
    }

    /// The `nth` (0-based, source order) use of `spelling` with `kind` in
    /// `file`.
    fn use_of(&self, file: &str, spelling: &str, kind: &str, nth: usize) -> &UseRow {
        let mut matching: Vec<&UseRow> = self
            .uses
            .iter()
            .filter(|row| {
                row.file == file && row.spelling == spelling && row.ref_kind.as_str() == kind
            })
            .collect();
        matching.sort_by_key(|row| row.start_byte);
        matching.get(nth).copied().unwrap_or_else(|| {
            panic!(
                "missing {kind} use #{nth} of {spelling} in {file}: {:?}",
                self.uses
            )
        })
    }

    /// The `(target_id, resolution)` bound to a use, if any.
    fn target(&self, use_row: &UseRow) -> Option<(String, String)> {
        let use_id = use_row.use_id.expect("persisted uses carry an ID");
        self.bindings
            .iter()
            .find(|binding| binding.use_id == use_id)
            .map(|binding| {
                (
                    binding.target_id.clone(),
                    binding.resolution.as_str().to_string(),
                )
            })
    }

    /// Asserts that the `nth` use binds `target` with `resolution`.
    fn assert_bound(
        &self,
        file: &str,
        spelling: &str,
        kind: &str,
        nth: usize,
        target: &str,
        resolution: &str,
    ) {
        let use_row = self.use_of(file, spelling, kind, nth);
        assert_eq!(
            self.target(use_row),
            Some((target.to_string(), resolution.to_string())),
            "{kind} use #{nth} of {spelling} in {file}: {use_row:?}"
        );
    }

    /// Asserts that the `nth` use records no binding.
    fn assert_unbound(&self, file: &str, spelling: &str, kind: &str, nth: usize) {
        let use_row = self.use_of(file, spelling, kind, nth);
        assert_eq!(
            self.target(use_row),
            None,
            "{kind} use #{nth} of {spelling} in {file} must record no binding: {use_row:?}"
        );
    }

    /// Every symbol ID declared in `file`, sorted.
    fn symbol_ids(&self, file: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .symbols
            .iter()
            .filter(|row| row.file == file)
            .map(|row| row.id.clone())
            .collect();
        ids.sort();
        ids
    }
}

/// Global `launch` and `Thing`, declared outside any namespace.
const GLOBALS_PHP: &str = "<?php\nfunction launch(): void {}\nclass Thing {}\n";

/// `App\Lib\Tool`, the import target of the closure case.
const TOOL_PHP: &str = "<?php\nnamespace App\\Lib;\nclass Tool {}\n";

/// Audit finding 1: a top-level closure sees `namespace App` and its imports.
///
/// With the namespace visible, `launch()` still binds the global `launch`
/// through PHP's function fallback because no `App\launch` exists. PHP has no
/// such fallback for classes, so `new Thing()` (which means `App\Thing`) must
/// not bind the global `Thing`, and `new Tool()` binds `App\Lib\Tool` through
/// the import.
#[test]
fn top_level_closure_sees_the_file_namespace_and_imports() {
    let case = "<?php\nnamespace App;\nuse App\\Lib\\Tool;\n$f = function () { launch(); new Thing(); new Tool(); };\n";
    let indexed = Indexed::new(
        "closure",
        &[
            ("g.php", GLOBALS_PHP),
            ("lib.php", TOOL_PHP),
            ("a.php", case),
        ],
    );
    indexed.assert_bound("a.php", "launch", "call", 0, "g.php#launch", "exact");
    indexed.assert_unbound("a.php", "Thing", "type", 0);
    indexed.assert_bound(
        "a.php",
        "Tool",
        "type",
        0,
        "lib.php#App\\Lib\\Tool",
        "exact",
    );
}

/// Finding 1 for an arrow function, and a namespaced function that shadows the
/// global one from inside a closure.
#[test]
fn top_level_arrow_function_sees_the_file_namespace_and_imports() {
    let case = "<?php\nnamespace App;\nuse App\\Lib\\Tool;\nfunction launch(): void {}\n$g = fn () => new Tool();\n$h = fn () => new Thing();\n$f = function () { return fn () => launch(); };\n";
    let indexed = Indexed::new(
        "arrow",
        &[
            ("g.php", GLOBALS_PHP),
            ("lib.php", TOOL_PHP),
            ("a.php", case),
        ],
    );
    indexed.assert_bound(
        "a.php",
        "Tool",
        "type",
        0,
        "lib.php#App\\Lib\\Tool",
        "exact",
    );
    indexed.assert_unbound("a.php", "Thing", "type", 0);
    indexed.assert_bound("a.php", "launch", "call", 0, "a.php#App\\launch", "exact");
}

/// Finding 1 without a namespace: a top-level closure in a file with no
/// namespace still sees the file's `use` imports.
#[test]
fn top_level_closure_in_a_file_without_namespace_sees_imports() {
    let case = "<?php\nuse App\\Lib\\Tool;\n$f = function () { new Tool(); };\n";
    let indexed = Indexed::new("closure-global", &[("lib.php", TOOL_PHP), ("a.php", case)]);
    indexed.assert_bound(
        "a.php",
        "Tool",
        "type",
        0,
        "lib.php#App\\Lib\\Tool",
        "exact",
    );
}

/// Audit finding 2: the second unbraced block does not resolve against the
/// first block's namespace.
#[test]
fn a_later_unbraced_block_does_not_resolve_against_the_first() {
    let case = "<?php\nnamespace First;\nclass Widget {}\nfunction build() {}\nnamespace Second;\nfunction caller() { build(); new Widget(); }\n";
    let indexed = Indexed::new("unbraced-first", &[("m.php", case)]);
    indexed.assert_unbound("m.php", "build", "call", 0);
    indexed.assert_unbound("m.php", "Widget", "type", 0);
    assert_eq!(
        indexed.symbol_ids("m.php"),
        [
            "m.php#First",
            "m.php#First\\Widget",
            "m.php#First\\build",
            "m.php#Second",
            "m.php#Second\\caller",
        ]
    );
}

/// Each unbraced block's `use` import resolves only within its own block, even
/// when two blocks import different classes under the same alias.
#[test]
fn unbraced_block_imports_stay_in_their_block() {
    let lib = "<?php\nnamespace Lib;\nclass Tool {}\nclass Other {}\n";
    let case = "<?php\nnamespace One;\nuse Lib\\Tool;\nnew Tool();\nnamespace Two;\nnew Tool();\nnamespace Three;\nuse Lib\\Other as Tool;\nnew Tool();\n";
    let indexed = Indexed::new("unbraced-imports", &[("lib.php", lib), ("m.php", case)]);
    indexed.assert_bound("m.php", "Tool", "import", 0, "lib.php#Lib\\Tool", "exact");
    indexed.assert_bound("m.php", "Tool", "type", 0, "lib.php#Lib\\Tool", "exact");
    // `Two` has no import, and `Two\Tool` does not exist.
    indexed.assert_unbound("m.php", "Tool", "type", 1);
    indexed.assert_bound("m.php", "Tool", "import", 1, "lib.php#Lib\\Other", "exact");
    indexed.assert_bound("m.php", "Tool", "type", 2, "lib.php#Lib\\Other", "exact");
}

/// Audit finding 2 for braced blocks: a `use` in one braced block does not
/// leak into another, and each block's declarations resolve in their own
/// namespace.
#[test]
fn braced_blocks_resolve_against_their_own_namespace_and_imports() {
    let lib = "<?php\nnamespace Lib;\nclass Tool {}\n";
    let case = "<?php\nnamespace One {\n    use Lib\\Tool;\n    class Widget {}\n    function build() {}\n    new Tool();\n    build();\n}\nnamespace Two {\n    new Tool();\n    build();\n    new Widget();\n    function build() {}\n    function caller() { build(); }\n}\n";
    let indexed = Indexed::new("braced", &[("lib.php", lib), ("m.php", case)]);
    indexed.assert_bound("m.php", "Tool", "type", 0, "lib.php#Lib\\Tool", "exact");
    indexed.assert_bound("m.php", "build", "call", 0, "m.php#One\\build", "exact");
    indexed.assert_unbound("m.php", "Tool", "type", 1);
    indexed.assert_bound("m.php", "build", "call", 1, "m.php#Two\\build", "exact");
    indexed.assert_unbound("m.php", "Widget", "type", 0);
    indexed.assert_bound("m.php", "build", "call", 2, "m.php#Two\\build", "exact");
}

/// A closure nested in a method in the second block resolves against the
/// second block's namespace and import, at every nesting depth.
#[test]
fn nested_closure_in_a_method_in_the_second_block_uses_that_block() {
    let lib = "<?php\nnamespace Lib;\nclass Tool {}\nclass Other {}\n";
    let case = "<?php\nnamespace One;\nuse Lib\\Tool;\nclass Helper {}\nnamespace Two;\nuse Lib\\Other as Tool;\nclass Svc\n{\n    public function run(): void\n    {\n        $f = function () {\n            new Tool();\n            return fn () => new Helper();\n        };\n        $g = fn () => new Svc();\n    }\n}\n";
    let indexed = Indexed::new("nested-closure", &[("lib.php", lib), ("m.php", case)]);
    indexed.assert_bound("m.php", "Tool", "type", 0, "lib.php#Lib\\Other", "exact");
    // `Helper` is `One\Helper`; in `Two` it means `Two\Helper`, which does not
    // exist.
    indexed.assert_unbound("m.php", "Helper", "type", 0);
    indexed.assert_bound("m.php", "Svc", "type", 0, "m.php#Two\\Svc", "exact");
}

/// Audit finding 3: a global `namespace { }` block declares bare global names
/// rather than inheriting the previous block's namespace.
#[test]
fn a_global_namespace_block_declares_global_names() {
    let case = "<?php\nnamespace A {\n    class X {}\n    helper();\n    new Y();\n}\nnamespace {\n    class Y {}\n    function helper() {}\n    new X();\n}\n";
    let indexed = Indexed::new("global-block", &[("a.php", case)]);
    assert_eq!(
        indexed.symbol_ids("a.php"),
        ["a.php#A", "a.php#A\\X", "a.php#Y", "a.php#helper"]
    );
    // PHP falls back to the global function from inside `A`.
    indexed.assert_bound("a.php", "helper", "call", 0, "a.php#helper", "exact");
    // No class fallback: `Y` in `A` means `A\Y`.
    indexed.assert_unbound("a.php", "Y", "type", 0);
    // `X` in the global block means the global `X`, which does not exist.
    indexed.assert_unbound("a.php", "X", "type", 0);
}

/// A file that mixes the braced and unbraced forms cannot be attributed block
/// by block, so none of its uses binds, not even an otherwise resolvable
/// import.
#[test]
fn a_file_mixing_namespace_forms_records_no_binding() {
    let lib = "<?php\nnamespace Lib;\nclass Tool {}\n";
    let case = "<?php\nnamespace One;\nuse Lib\\Tool;\nnew Tool();\nnamespace Two { new \\Lib\\Tool(); }\n";
    let indexed = Indexed::new("mixed", &[("lib.php", lib), ("m.php", case)]);
    indexed.assert_unbound("m.php", "Tool", "import", 0);
    indexed.assert_unbound("m.php", "Tool", "type", 0);
    indexed.assert_unbound("m.php", "\\Lib\\Tool", "type", 0);
}

/// Code outside every braced block belongs to no namespace block and records
/// no binding, while the blocks around it still resolve.
#[test]
fn code_between_braced_blocks_records_no_binding() {
    let lib = "<?php\nnamespace Lib;\nclass Tool {}\nfunction helper() {}\n";
    let case = "<?php\nnamespace One { use Lib\\Tool; new Tool(); }\nnew \\Lib\\Tool();\n\\Lib\\helper();\nnamespace Two { new \\Lib\\Tool(); }\n";
    let indexed = Indexed::new("between", &[("lib.php", lib), ("m.php", case)]);
    indexed.assert_bound("m.php", "Tool", "type", 0, "lib.php#Lib\\Tool", "exact");
    indexed.assert_unbound("m.php", "\\Lib\\Tool", "type", 0);
    indexed.assert_unbound("m.php", "\\Lib\\helper", "call", 0);
    indexed.assert_bound(
        "m.php",
        "\\Lib\\Tool",
        "type",
        1,
        "lib.php#Lib\\Tool",
        "exact",
    );
}

/// PHP namespaces do not scope variables, so a top-level variable assigned in
/// two blocks is never a trustworthy `new` receiver, while a variable assigned
/// once still binds in its own block's namespace.
#[test]
fn top_level_variables_are_shared_across_namespace_blocks() {
    let lib = "<?php\nnamespace Lib;\nclass Tool { public function go(): void {} }\nclass Other { public function go(): void {} }\n";
    let case = "<?php\nnamespace One;\nuse Lib\\Tool;\n$s = new Tool();\n$t = new Tool();\n$t->go();\nnamespace Two;\nuse Lib\\Other as Tool;\n$s = new Tool();\n$s->go();\n";
    let indexed = Indexed::new("shared-vars", &[("lib.php", lib), ("m.php", case)]);
    // `$t` is assigned once, in `One`, so it binds `Lib\Tool::go`.
    indexed.assert_bound("m.php", "go", "call", 0, "lib.php#Lib\\Tool::go", "scoped");
    // `$s` is assigned in both blocks.
    indexed.assert_unbound("m.php", "go", "call", 1);
}
