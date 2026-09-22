//! AF3 integration tests: typed-receiver conservatism and the remaining
//! rebinding forms, through the real binary.
//!
//! Each case writes a small PHP tree into a temporary Git root, runs
//! `rivet index --json`, and reads the committed bindings back with
//! [`rivet_store::Store`]. Every audit reproduction (findings 6-9 of
//! `orchestration/review-notes/audit-2026-09-22.md`) must record no binding,
//! and each safe counterpart must still bind `scoped`:
//!
//! - a typed parameter is suppressed by a by-reference argument, a reference
//!   alias, `extract`, `preg_match` into it, and a reassignment later in a
//!   loop, but binds when never rebound (finding 6);
//! - a typed property keeps its binding after reassignment;
//! - `A|B`, `A&B`, and `A|null` bind nothing, `?A` binds `A` (finding 7);
//! - a reference alias, a by-reference constructor argument, and a file-scope
//!   variable rebound through `global` or `$GLOBALS` by a called function
//!   suppress a `new` receiver (finding 8), while a `global` for another name
//!   does not;
//! - attribute, default-value, and comment text cannot mislead by-reference
//!   parameter detection (finding 9); and
//! - a method-call argument's receiver class is used only when the receiver
//!   itself is trustworthy.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_store::{BindingRow, Store, UseRow};

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
            "rivet-af3-{label}-{}-{nanos}-{unique}",
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

/// Two unrelated classes with a same-name method, so a wrong binding is
/// visible as the wrong class.
const AB_PHP: &str =
    "<?php\nclass A { public function m(): void {} }\nclass B { public function m(): void {} }\n";

/// The indexed rows of one case.
struct Indexed {
    _temp: TempDir,
    uses: Vec<UseRow>,
    bindings: Vec<BindingRow>,
}

impl Indexed {
    /// The call use of `spelling` on `receiver` on 1-based `line` of `file`.
    fn call(&self, file: &str, line: u32, receiver: &str, spelling: &str) -> &UseRow {
        self.uses
            .iter()
            .find(|row| {
                row.file == file
                    && row.line == line
                    && row.ref_kind.as_str() == "call"
                    && row.spelling == spelling
                    && row.receiver.as_deref() == Some(receiver)
            })
            .unwrap_or_else(|| panic!("missing {file}:{line} {receiver}->{spelling}()"))
    }

    /// The binding target and tier of that call, if any.
    fn target(
        &self,
        file: &str,
        line: u32,
        receiver: &str,
        spelling: &str,
    ) -> Option<(&str, &str)> {
        let use_id = self
            .call(file, line, receiver, spelling)
            .use_id
            .expect("persisted uses carry an ID");
        self.bindings
            .iter()
            .find(|binding| binding.use_id == use_id)
            .map(|binding| (binding.target_id.as_str(), binding.resolution.as_str()))
    }

    /// Asserts the call records no binding.
    fn assert_unbound(&self, file: &str, line: u32, receiver: &str, spelling: &str) {
        assert_eq!(
            self.target(file, line, receiver, spelling),
            None,
            "{file}:{line} {receiver}->{spelling}() must record no binding"
        );
    }

    /// Asserts the call binds `target` as `scoped`.
    fn assert_scoped(&self, file: &str, line: u32, receiver: &str, spelling: &str, target: &str) {
        assert_eq!(
            self.target(file, line, receiver, spelling),
            Some((target, "scoped")),
            "{file}:{line} {receiver}->{spelling}()"
        );
    }
}

/// Writes `files` into a fresh Git root, indexes it, and returns the rows.
fn index(label: &str, files: &[(&str, &str)]) -> Indexed {
    let temp = TempDir::new(label);
    for (name, source) in files {
        fs::write(temp.path().join(name), source.as_bytes()).expect("write PHP fixture");
    }
    let output = Command::new(RIVET)
        .args(["index", "--json"])
        .current_dir(temp.path())
        .output()
        .expect("run the rivet binary");
    assert_eq!(
        output.status.code(),
        Some(0),
        "index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
    let mut uses = Vec::new();
    for file in store.list_files().expect("list files") {
        uses.extend(store.list_uses_for_file(&file.path).expect("list uses"));
    }
    let bindings = store.list_bindings().expect("list bindings");
    Indexed {
        _temp: temp,
        uses,
        bindings,
    }
}

/// Finding 6: every rebinding form that suppresses a `new` receiver also
/// suppresses a typed parameter, while an untouched typed parameter binds.
#[test]
fn typed_parameter_rebinding_forms_record_no_binding() {
    let case = index(
        "typed-parameter",
        &[
            ("AB.php", AB_PHP),
            (
                "F.php",
                "<?php\n\
                 function takes_ref(&$v) { $v = new B(); }\n\
                 function by_ref(A $x) { takes_ref($x); $x->m(); }\n\
                 function alias(A $x) { $y = &$x; $y = new B(); $x->m(); }\n\
                 function extracted(A $x, array $vars) { extract($vars); $x->m(); }\n\
                 function matched(A $x) { preg_match('/a/', 'a', $x); $x->m(); }\n\
                 function looped(A $x) { for ($i = 0; $i < 2; $i++) { $x->m(); $x = new B(); } }\n\
                 function safe(A $x) { $x->m(); }\n\
                 function by_value_arg(A $x) { takes_value($x); $x->m(); }\n\
                 function takes_value($v) {}\n",
            ),
        ],
    );
    for line in 3..=7 {
        case.assert_unbound("F.php", line, "$x", "m");
    }
    case.assert_scoped("F.php", 8, "$x", "m", "AB.php#A::m");
    // An indexed by-value parameter proves the argument is not rebound.
    case.assert_scoped("F.php", 9, "$x", "m", "AB.php#A::m");
}

/// Finding 6: a promoted constructor parameter used as a local variable is a
/// parameter, while the promoted property read through `$this` is a property.
/// A typed property keeps its binding however often it is reassigned.
#[test]
fn typed_property_survives_reassignment_but_the_promoted_parameter_does_not() {
    let case = index(
        "typed-property",
        &[
            ("AB.php", AB_PHP),
            (
                "C.php",
                "<?php\n\
                 class C {\n\
                     private A $p;\n\
                     public function __construct(private A $q) {\n\
                         $q = new B();\n\
                         $q->m();\n\
                         $this->q->m();\n\
                     }\n\
                     public function run(): void {\n\
                         $this->p = new A();\n\
                         $this->p = make();\n\
                         $this->p->m();\n\
                     }\n\
                 }\n",
            ),
        ],
    );
    // `$q = new B()` makes this a safe `new` receiver of `B`, not of `A`.
    case.assert_scoped("C.php", 6, "$q", "m", "AB.php#B::m");
    case.assert_scoped("C.php", 7, "$this->q", "m", "AB.php#A::m");
    case.assert_scoped("C.php", 12, "$this->p", "m", "AB.php#A::m");

    let promoted = index(
        "promoted-parameter",
        &[
            ("AB.php", AB_PHP),
            (
                "C.php",
                "<?php\n\
                 class C {\n\
                     public function __construct(private A $q) {\n\
                         if (rand()) { $q = new B(); }\n\
                         $q->m();\n\
                         $this->q->m();\n\
                     }\n\
                 }\n",
            ),
        ],
    );
    promoted.assert_unbound("C.php", 5, "$q", "m");
    promoted.assert_scoped("C.php", 6, "$this->q", "m", "AB.php#A::m");
}

/// Finding 7: only a single class type or a nullable one names a receiver
/// class, for parameters and properties alike.
#[test]
fn union_intersection_and_dnf_types_bind_nothing() {
    let case = index(
        "union-types",
        &[
            ("AB.php", AB_PHP),
            (
                "F.php",
                "<?php\n\
                 function u(A|B $x) { $x->m(); }\n\
                 function i(A&B $x) { $x->m(); }\n\
                 function d((A&B)|null $x) { $x->m(); }\n\
                 function n(?A $x) { $x->m(); }\n\
                 function un(A|null $x) { $x->m(); }\n\
                 class P {\n\
                     private A|B $u;\n\
                     private ?A $n;\n\
                     public function run(): void { $this->u->m(); $this->n->m(); }\n\
                 }\n",
            ),
        ],
    );
    case.assert_unbound("F.php", 2, "$x", "m");
    case.assert_unbound("F.php", 3, "$x", "m");
    case.assert_unbound("F.php", 4, "$x", "m");
    case.assert_scoped("F.php", 5, "$x", "m", "AB.php#A::m");
    case.assert_unbound("F.php", 6, "$x", "m");
    case.assert_unbound("F.php", 10, "$this->u", "m");
    case.assert_scoped("F.php", 10, "$this->n", "m", "AB.php#A::m");
}

/// Finding 8 and requirement 3: a reference taken to a variable makes it
/// untrustworthy for the rest of its scope.
#[test]
fn a_reference_to_a_new_receiver_records_no_binding() {
    let case = index(
        "reference-alias",
        &[
            ("AB.php", AB_PHP),
            (
                "main.php",
                "<?php\n\
                 $x = new A(); $y = &$x; $y = new B(); $x->m();\n\
                 $r = new A(); foreach ($r as &$e) {} $r->m();\n\
                 $k = new A(); $arr = [&$k]; $k->m();\n\
                 $s = new A(); $s->m();\n",
            ),
        ],
    );
    case.assert_unbound("main.php", 2, "$x", "m");
    case.assert_unbound("main.php", 3, "$r", "m");
    case.assert_unbound("main.php", 4, "$k", "m");
    case.assert_scoped("main.php", 5, "$s", "m", "AB.php#A::m");
}

/// Finding 8 and requirement 4: constructor arguments are call arguments of
/// `Class::__construct`.
#[test]
fn constructor_arguments_are_adjudicated_like_method_arguments() {
    let case = index(
        "constructor-arguments",
        &[
            ("AB.php", AB_PHP),
            (
                "H.php",
                "<?php\n\
                 class Holder { public function __construct(&$v) { $v = new B(); } }\n\
                 class ValueHolder { public function __construct($v) {} }\n\
                 class NoConstructor {}\n\
                 $s = new A(); new Holder($s); $s->m();\n\
                 $t = new A(); new ValueHolder($t); $t->m();\n\
                 $w = new A(); new NoConstructor($w); $w->m();\n\
                 $z = new A(); new Unindexed($z); $z->m();\n\
                 $q = new A(); new class($q) {}; $q->m();\n\
                 $c = new A(); new $cls($c); $c->m();\n\
                 $a = new A(); new ValueHolder($a = new B()); $a->m();\n",
            ),
        ],
    );
    case.assert_unbound("H.php", 5, "$s", "m");
    case.assert_scoped("H.php", 6, "$t", "m", "AB.php#A::m");
    case.assert_unbound("H.php", 7, "$w", "m");
    case.assert_unbound("H.php", 8, "$z", "m");
    case.assert_unbound("H.php", 9, "$q", "m");
    case.assert_unbound("H.php", 10, "$c", "m");
    // A reassignment inside constructor arguments is code the walker must see.
    case.assert_unbound("H.php", 11, "$a", "m");
}

/// Finding 8 and requirement 5: a file-scope variable that a called function
/// can rebind through `global` or `$GLOBALS` records no binding.
#[test]
fn a_global_rebound_by_a_called_function_records_no_binding() {
    let main = "<?php\n\
                $x = new A(); reset_it(); $x->m();\n\
                $y = new A(); reset_it(); $y->m();\n\
                $z = new A(); $z->m(); reset_it();\n\
                $w = new A(make()); $w->m();\n";
    let by_name = index(
        "global-by-name",
        &[
            ("AB.php", AB_PHP),
            (
                "R.php",
                "<?php\nfunction reset_it() { global $x, $w; $x = new B(); }\n",
            ),
            ("main.php", main),
        ],
    );
    by_name.assert_unbound("main.php", 2, "$x", "m");
    // `global $x` does not reach `$y`.
    by_name.assert_scoped("main.php", 3, "$y", "m", "AB.php#A::m");
    // Calls after the use, or inside the assignment's own `new`, cannot rebind
    // the variable before the use.
    by_name.assert_scoped("main.php", 4, "$z", "m", "AB.php#A::m");
    by_name.assert_scoped("main.php", 5, "$w", "m", "AB.php#A::m");

    let other_name = index(
        "global-other-name",
        &[
            ("AB.php", AB_PHP),
            (
                "R.php",
                "<?php\nfunction reset_it() { global $other; $other = new B(); }\n",
            ),
            ("main.php", main),
        ],
    );
    other_name.assert_scoped("main.php", 2, "$x", "m", "AB.php#A::m");

    let globals_key = index(
        "globals-key",
        &[
            ("AB.php", AB_PHP),
            (
                "R.php",
                "<?php\nfunction reset_it() { $GLOBALS['x'] = new B(); }\n",
            ),
            ("main.php", main),
        ],
    );
    globals_key.assert_unbound("main.php", 2, "$x", "m");
    globals_key.assert_scoped("main.php", 3, "$y", "m", "AB.php#A::m");

    let globals_dynamic = index(
        "globals-dynamic",
        &[
            ("AB.php", AB_PHP),
            (
                "R.php",
                "<?php\nfunction reset_it() {}\nfunction set($k) { $GLOBALS[$k] = new B(); }\n",
            ),
            ("main.php", main),
        ],
    );
    globals_dynamic.assert_unbound("main.php", 2, "$x", "m");
    globals_dynamic.assert_unbound("main.php", 3, "$y", "m");
    globals_dynamic.assert_scoped("main.php", 4, "$z", "m", "AB.php#A::m");
}

/// Requirement 5: an unindexed PHP file could hold a function that declares
/// the variable `global`, as the AF2 fallback rule assumes.
#[test]
fn an_unindexed_php_file_suppresses_a_global_across_a_call() {
    let case = index(
        "global-unindexed",
        &[
            ("AB.php", AB_PHP),
            (
                "main.php",
                "<?php\n$x = new A(); reset_it(); $x->m();\n$v = new A(); $v->m();\n",
            ),
            ("broken.php", "<?php function (\n"),
        ],
    );
    case.assert_unbound("main.php", 2, "$x", "m");
    case.assert_scoped("main.php", 3, "$v", "m", "AB.php#A::m");
}

/// Requirement 5: a backward `goto` can run a call written after the use
/// before it.
#[test]
fn a_goto_widens_the_global_call_window() {
    let reset = "<?php\nfunction reset_it() { global $x; $x = new B(); }\n";
    let with_goto = index(
        "global-goto",
        &[
            ("AB.php", AB_PHP),
            ("R.php", reset),
            (
                "main.php",
                "<?php\n$x = new A();\nL:\n$x->m();\nreset_it();\ngoto L;\n",
            ),
        ],
    );
    with_goto.assert_unbound("main.php", 4, "$x", "m");
    let without = index(
        "global-no-goto",
        &[
            ("AB.php", AB_PHP),
            ("R.php", reset),
            (
                "main.php",
                "<?php\n$x = new A();\nL:\n$x->m();\nreset_it();\n",
            ),
        ],
    );
    without.assert_scoped("main.php", 4, "$x", "m", "AB.php#A::m");
}

/// Finding 9 and requirement 6: by-reference positions come from the tree, so
/// attribute, default-value, and comment text cannot shift or fake them.
#[test]
fn declaration_text_cannot_mislead_by_reference_detection() {
    let case = index(
        "by-ref-parsing",
        &[
            ("AB.php", AB_PHP),
            (
                "F.php",
                "<?php\n\
                 #[Deprecated(\"use function other\")] #[Pure(1)] function rebind(&$v) { $v = new B(); }\n\
                 function dflt($a = \"function (\", &$v = null) {}\n\
                 function cmt(/* ) , */ $a, /* & */ &$v) {}\n\
                 function fake(#[Attr('&$v')] $a, $v = '&$x', /* &$c */ $c = null) {}\n\
                 function variadic($a, &...$rest) {}\n\
                 $a = new A(); rebind($a); $a->m();\n\
                 $b = new A(); dflt(1, $b); $b->m();\n\
                 $c = new A(); cmt(1, $c); $c->m();\n\
                 $d = new A(); fake($d, $d, $d); $d->m();\n\
                 $e = new A(); variadic($e); $e->m();\n\
                 $f = new A(); variadic(1, $f); $f->m();\n",
            ),
        ],
    );
    case.assert_unbound("F.php", 7, "$a", "m");
    case.assert_unbound("F.php", 8, "$b", "m");
    case.assert_unbound("F.php", 9, "$c", "m");
    case.assert_scoped("F.php", 10, "$d", "m", "AB.php#A::m");
    case.assert_scoped("F.php", 11, "$e", "m", "AB.php#A::m");
    case.assert_unbound("F.php", 12, "$f", "m");
}

/// A method-call argument is adjudicated against its receiver's class only
/// when that receiver is itself trustworthy.
#[test]
fn an_untrustworthy_call_receiver_cannot_prove_an_argument_by_value() {
    let case = index(
        "call-receiver-trust",
        &[(
            "K.php",
            "<?php\n\
                 class Svc { public function go(): void {} }\n\
                 class ByRef { public function takes(&$v): void {} }\n\
                 class ByVal { public function takes($v): void {} }\n\
                 function g(&$v) { $v = new ByRef(); }\n\
                 function typed(ByVal $r, Svc $s) { g($r); $r->takes($s); $s->go(); }\n\
                 function branch(bool $c) { $r = new ByRef(); if ($c) { $r = new ByVal(); } $s = new Svc(); $r->takes($s); $s->go(); }\n\
                 function typed_safe(ByVal $r, Svc $s) { $r->takes($s); $s->go(); }\n\
                 function new_safe() { $r = new ByVal(); $s = new Svc(); $r->takes($s); $s->go(); }\n",
        )],
    );
    case.assert_unbound("K.php", 6, "$s", "go");
    case.assert_unbound("K.php", 7, "$s", "go");
    case.assert_scoped("K.php", 8, "$s", "go", "K.php#Svc::go");
    case.assert_scoped("K.php", 9, "$s", "go", "K.php#Svc::go");
}
