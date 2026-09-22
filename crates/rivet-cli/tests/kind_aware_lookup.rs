//! AF2 integration tests: kind-aware declaration lookup, ASCII-only case
//! folding, and the honest global function fallback.
//!
//! Each case writes a temporary Git root, indexes it with the real binary, and
//! inspects the committed `.rivet/index.db` through [`rivet_store::Store`].
//! They cover audit findings 4, 5 and 10 of
//! `orchestration/review-notes/audit-2026-09-22.md`, plus the fallback defect
//! confirmed during the AF1 review:
//!
//! - a fully qualified call binds only a function, never a same-name class;
//! - a method call binds only a method, a property access only a property, and
//!   a class-constant read only a constant;
//! - PHP identifiers fold case by ASCII only, in resolution and in `rivet
//!   symbol`; and
//! - an unqualified namespaced call does not fall back to the global function
//!   while a PHP file that could declare the namespaced one is unindexed.

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
            "rivet-kind-lookup-{label}-{}-{nanos}-{unique}",
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

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// The committed rows of one indexed temporary repository.
struct Indexed {
    temp: TempDir,
    index_json: Value,
    uses: Vec<UseRow>,
    bindings: Vec<BindingRow>,
}

impl Indexed {
    /// Writes `files`, runs `rivet index --json`, and loads the snapshot.
    fn new(label: &str, files: &[(&str, &[u8])]) -> Indexed {
        let temp = TempDir::new(label);
        for (name, source) in files {
            fs::write(temp.path().join(name), source).expect("write source file");
        }
        Indexed::load(temp, label)
    }

    /// Re-indexes `temp` and loads its snapshot.
    fn load(temp: TempDir, label: &str) -> Indexed {
        let output = run(temp.path(), &["index", "--json"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let index_json: Value =
            serde_json::from_slice(&output.stdout).expect("index stdout is one JSON object");
        let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
        let mut uses = Vec::new();
        for file in store.list_files().expect("list files") {
            uses.extend(store.list_uses_for_file(&file.path).expect("list uses"));
        }
        let bindings = store.list_bindings().expect("list bindings");
        drop(store);
        Indexed {
            temp,
            index_json,
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
}

// ---------------------------------------------------------------------------
// Finding 4: a fully qualified call binds only a function.
// ---------------------------------------------------------------------------

#[test]
fn fully_qualified_call_to_a_class_name_records_nothing() {
    let indexed = Indexed::new(
        "fq-class",
        &[("a.php", b"<?php\nclass Maker {}\n\\Maker();\n")],
    );
    indexed.assert_unbound("a.php", "\\Maker", "call", 0);
}

#[test]
fn fully_qualified_call_to_a_function_still_binds_exact() {
    // A class and a function may share a name in PHP; the call is the
    // function's.
    let indexed = Indexed::new(
        "fq-function",
        &[(
            "a.php",
            b"<?php\nnamespace App;\nclass Maker {}\nfunction maker() {}\n\\App\\Maker();\n",
        )],
    );
    indexed.assert_bound(
        "a.php",
        "\\App\\Maker",
        "call",
        0,
        "a.php#App\\maker",
        "exact",
    );
}

// ---------------------------------------------------------------------------
// Finding 5: member lookup respects the use kind.
// ---------------------------------------------------------------------------

#[test]
fn method_call_with_only_a_same_name_property_records_nothing() {
    let indexed = Indexed::new(
        "method-vs-property",
        &[(
            "c.php",
            b"<?php\nclass C { public $items; public function run() { $this->items(); } }\n",
        )],
    );
    indexed.assert_unbound("c.php", "items", "call", 0);
}

#[test]
fn property_read_with_only_a_same_name_method_records_nothing() {
    let indexed = Indexed::new(
        "property-vs-method",
        &[(
            "c.php",
            b"<?php\nclass C {\n  public function items() {}\n  public function run() { return [$this->items, self::$items]; }\n}\n",
        )],
    );
    indexed.assert_unbound("c.php", "items", "read", 0);
    indexed.assert_unbound("c.php", "$items", "read", 0);
}

#[test]
fn class_constant_read_binds_only_a_constant() {
    // `self::MAX` and `$this::MAX` are constant reads; `$this->MAX` is a
    // property read. The class declares a constant `MAX` and a property
    // `$MAX`, so each binds its own kind.
    let indexed = Indexed::new(
        "constant-vs-property",
        &[(
            "c.php",
            b"<?php\nclass C {\n  const MAX = 1;\n  public $MAX;\n  public function run() { return self::MAX + $this::MAX + $this->MAX; }\n}\n",
        )],
    );
    indexed.assert_bound("c.php", "MAX", "read", 0, "c.php#C::MAX", "scoped");
    indexed.assert_bound("c.php", "MAX", "read", 1, "c.php#C::MAX", "scoped");
    indexed.assert_bound("c.php", "MAX", "read", 2, "c.php#C::$MAX", "scoped");
}

#[test]
fn class_constant_read_with_only_a_same_name_property_records_nothing() {
    let indexed = Indexed::new(
        "constant-without-constant",
        &[(
            "c.php",
            b"<?php\nclass C {\n  public $MAX;\n  public function items() {}\n  public function run() { return self::MAX + $this::items; }\n}\n",
        )],
    );
    indexed.assert_unbound("c.php", "MAX", "read", 0);
    indexed.assert_unbound("c.php", "items", "read", 0);
}

#[test]
fn method_and_property_of_one_name_each_bind_their_own_kind() {
    let indexed = Indexed::new(
        "both-kinds",
        &[(
            "c.php",
            b"<?php\nclass C {\n  public $items;\n  public function items() {}\n  public function run() { $this->items(); $this->items = 1; return $this->items; }\n}\n",
        )],
    );
    indexed.assert_bound("c.php", "items", "call", 0, "c.php#C::items", "scoped");
    indexed.assert_bound("c.php", "items", "write", 0, "c.php#C::$items", "scoped");
    indexed.assert_bound("c.php", "items", "read", 0, "c.php#C::$items", "scoped");
}

#[test]
fn new_receiver_member_lookup_is_kind_aware() {
    // The `new`-receiver rule shares the member lookup.
    let indexed = Indexed::new(
        "new-receiver",
        &[(
            "c.php",
            b"<?php\nclass W { public $size; public function grow() {} }\nfunction f() { $w = new W(); $w->size(); $w->grow; $w->grow(); }\n",
        )],
    );
    indexed.assert_unbound("c.php", "size", "call", 0);
    indexed.assert_unbound("c.php", "grow", "read", 0);
    indexed.assert_bound("c.php", "grow", "call", 0, "c.php#W::grow", "scoped");
}

// ---------------------------------------------------------------------------
// Finding 10: case folds by ASCII only.
// ---------------------------------------------------------------------------

#[test]
fn non_ascii_class_names_do_not_fold_but_ascii_ones_do() {
    let indexed = Indexed::new(
        "ascii-fold",
        &[(
            "a.php",
            "<?php\nclass Ä {}\nclass Foo {}\nnew ä();\nnew Ä();\nnew foo();\n".as_bytes(),
        )],
    );
    indexed.assert_unbound("a.php", "ä", "type", 0);
    indexed.assert_bound("a.php", "Ä", "type", 0, "a.php#Ä", "exact");
    indexed.assert_bound("a.php", "foo", "type", 0, "a.php#Foo", "exact");
}

#[test]
fn symbol_query_folds_ascii_case_only() {
    let indexed = Indexed::new(
        "symbol-fold",
        &[(
            "a.php",
            "<?php\nnamespace App;\nclass SurveyService {}\nclass Ärger {}\n".as_bytes(),
        )],
    );
    let dir = indexed.temp.path();
    for query in ["surveyservice", "SURVEYSERVICE", "app\\surveyservice"] {
        let output = run(dir, &["symbol", query, "--json"]);
        assert_eq!(output.status.code(), Some(0), "{query}");
        let value: Value = serde_json::from_slice(&output.stdout).expect("symbol JSON");
        assert_eq!(value["symbol"]["id"], "a.php#App\\SurveyService", "{query}");
    }
    for query in ["Ärger", "ÄRGER"] {
        let output = run(dir, &["symbol", query, "--json"]);
        assert_eq!(output.status.code(), Some(0), "{query}");
        let value: Value = serde_json::from_slice(&output.stdout).expect("symbol JSON");
        assert_eq!(value["symbol"]["id"], "a.php#App\\Ärger", "{query}");
    }
    // `ä` is a different PHP name from `Ä`: not found.
    let output = run(dir, &["symbol", "ärger", "--json"]);
    assert_eq!(output.status.code(), Some(4));
}

// ---------------------------------------------------------------------------
// The global function fallback under partial PHP coverage.
// ---------------------------------------------------------------------------

/// The caller: an unqualified `launch()` in `namespace App`.
const CALLER_PHP: &[u8] = b"<?php\nnamespace App;\nfunction run() { launch(); }\n";

/// The global `launch` the fallback would reach.
const GLOBAL_PHP: &[u8] = b"<?php\nfunction launch() {}\n";

/// `App\launch`, valid.
const NAMESPACED_PHP: &[u8] = b"<?php\nnamespace App;\nfunction launch() {}\n";

/// `App\launch` with a syntax error, so the file is not indexed.
const NAMESPACED_BROKEN_PHP: &[u8] = b"<?php\nnamespace App;\nfunction launch() {}\n{\n";

#[test]
fn parse_failed_namespaced_declaration_suppresses_the_global_fallback() {
    let indexed = Indexed::new(
        "fallback-broken",
        &[
            ("c.php", CALLER_PHP),
            ("fns.php", NAMESPACED_BROKEN_PHP),
            ("util.php", GLOBAL_PHP),
        ],
    );
    assert_eq!(
        indexed.index_json["index"]["coverage"]["skipped"]["parse_error"],
        1
    );
    indexed.assert_unbound("c.php", "launch", "call", 0);

    // The same repository with the file fixed binds the namespaced function.
    fs::write(indexed.temp.path().join("fns.php"), NAMESPACED_PHP).expect("fix fns.php");
    let fixed = Indexed::load(indexed.temp, "fallback-fixed");
    fixed.assert_bound("c.php", "launch", "call", 0, "fns.php#App\\launch", "exact");
}

#[test]
fn invalid_utf8_php_file_suppresses_the_global_fallback() {
    let indexed = Indexed::new(
        "fallback-encoding",
        &[
            ("c.php", CALLER_PHP),
            (
                "fns.php",
                b"<?php\nnamespace App;\nfunction launch() {} // \xff\n",
            ),
            ("util.php", GLOBAL_PHP),
        ],
    );
    assert_eq!(
        indexed.index_json["index"]["coverage"]["skipped"]["encoding"],
        1
    );
    indexed.assert_unbound("c.php", "launch", "call", 0);
}

#[test]
fn indexed_namespaced_function_wins_while_another_php_file_is_unindexed() {
    let indexed = Indexed::new(
        "fallback-namespaced-wins",
        &[
            ("broken.php", b"<?php\nfunction {\n"),
            ("c.php", CALLER_PHP),
            ("fns.php", NAMESPACED_PHP),
            ("util.php", GLOBAL_PHP),
        ],
    );
    indexed.assert_bound("c.php", "launch", "call", 0, "fns.php#App\\launch", "exact");
}

#[test]
fn a_readme_does_not_suppress_the_fallback() {
    // A non-PHP file makes coverage incomplete, but every PHP file is indexed,
    // so the global fallback behaves exactly as before.
    let indexed = Indexed::new(
        "fallback-readme",
        &[
            ("README.md", b"# readme\n"),
            ("c.php", CALLER_PHP),
            ("util.php", GLOBAL_PHP),
        ],
    );
    assert_eq!(indexed.index_json["index"]["coverage"]["complete"], false);
    indexed.assert_bound("c.php", "launch", "call", 0, "util.php#launch", "exact");
}

#[test]
fn by_reference_adjudication_does_not_use_the_fallback_while_a_php_file_is_unindexed() {
    // `take($w)` would fall back to the global by-value `take`, which keeps
    // the `new` receiver trustworthy. While a PHP file that may declare a
    // by-reference `App\take` is unindexed, neither the call nor the receiver
    // binds.
    let caller: &[u8] = b"<?php\nnamespace App;\nclass W { public function go() {} }\nfunction f() { $w = new W(); take($w); $w->go(); }\n";
    let global: &[u8] = b"<?php\nfunction take($v) {}\n";
    let complete = Indexed::new(
        "callee-complete",
        &[("a.php", caller), ("util.php", global)],
    );
    complete.assert_bound("a.php", "take", "call", 0, "util.php#take", "exact");
    complete.assert_bound("a.php", "go", "call", 0, "a.php#App\\W::go", "scoped");

    let partial = Indexed::new(
        "callee-partial",
        &[
            ("a.php", caller),
            ("broken.php", b"<?php\nfunction {\n"),
            ("util.php", global),
        ],
    );
    partial.assert_unbound("a.php", "take", "call", 0);
    partial.assert_unbound("a.php", "go", "call", 0);
}
