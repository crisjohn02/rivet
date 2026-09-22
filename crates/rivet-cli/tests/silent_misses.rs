//! AF4 integration tests: close the silent misses.
//!
//! Each case writes a temporary Git root, indexes it with the real binary, and
//! asserts on `rivet refs --json` output: spans, `resolution`,
//! `resolved_target`, and `containing_symbol`, not counts alone. They cover
//! audit findings 11, 12 and 13 of
//! `orchestration/review-notes/audit-2026-09-22.md`, the property `$`
//! lookup-name mismatch, and the `instanceof` binding withdrawn by AF2:
//!
//! - uses inside an anonymous class body are recorded with the nearest named
//!   container, while `$this`, `self`, and `static` there bind nothing even
//!   when the enclosing class declares a same-name member;
//! - an unresolved qualified class spelling and a bare constant name-match
//!   their declarations by normalized short name, with the declaration's case
//!   rule;
//! - an unresolved property use name-matches its `$`-prefixed declaration;
//! - `Foo::make()`, `Foo::BAR`, `Foo::$prop`, and `Foo::class` are type uses
//!   of `Foo`, and their members bind `scoped` only when `Foo` declares them;
//! - `static::` and `parent::` bind nothing; and
//! - `$x instanceof \App\I` binds the interface `exact`, as does a `catch`
//!   type.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named temporary Git root removed when dropped.
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
            "rivet-silent-misses-{label}-{}-{nanos}-{unique}",
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

/// One reference as `(file, start_byte, end_byte, ref_kind, resolution,
/// resolved_target, containing_symbol id)`.
type Ref = (
    String,
    u64,
    u64,
    String,
    String,
    Option<String>,
    Option<String>,
);

/// An indexed temporary repository.
struct Repo {
    temp: TempDir,
    files: Vec<(String, String)>,
}

impl Repo {
    /// Writes `files` and runs `rivet index --json`, requiring success.
    fn new(label: &str, files: &[(&str, &str)]) -> Repo {
        let temp = TempDir::new(label);
        for (name, source) in files {
            fs::write(temp.path().join(name), source).expect("write source file");
        }
        let output = run(temp.path(), &["index", "--json"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Repo {
            temp,
            files: files
                .iter()
                .map(|(name, source)| (name.to_string(), source.to_string()))
                .collect(),
        }
    }

    /// The `[start, end)` span of the `nth` occurrence of `needle` in `file`,
    /// narrowed to `inner` within it.
    fn span(&self, file: &str, needle: &str, nth: usize, inner: &str) -> (u64, u64) {
        let source = &self
            .files
            .iter()
            .find(|(name, _)| name == file)
            .expect("known file")
            .1;
        let start = source
            .match_indices(needle)
            .nth(nth)
            .unwrap_or_else(|| panic!("missing occurrence #{nth} of {needle:?} in {file}"))
            .0;
        let offset = needle
            .find(inner)
            .unwrap_or_else(|| panic!("{inner:?} is not inside {needle:?}"));
        let start = (start + offset) as u64;
        (start, start + inner.len() as u64)
    }

    /// `rivet refs <query> --json` in `mode`, as comparable tuples.
    fn refs(&self, query: &str, mode: &str) -> Vec<Ref> {
        let output = run(
            self.temp.path(),
            &["refs", query, "--mode", mode, "--limit", "1000", "--json"],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "refs {query}: stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let json: Value = serde_json::from_slice(&output.stdout).expect("refs JSON");
        json["references"]
            .as_array()
            .expect("references array")
            .iter()
            .map(|reference| {
                (
                    reference["file"].as_str().expect("file").to_string(),
                    reference["start_byte"].as_u64().expect("start_byte"),
                    reference["end_byte"].as_u64().expect("end_byte"),
                    reference["ref_kind"]
                        .as_str()
                        .expect("ref_kind")
                        .to_string(),
                    reference["resolution"]
                        .as_str()
                        .expect("resolution")
                        .to_string(),
                    reference["resolved_target"].as_str().map(str::to_string),
                    reference["containing_symbol"]["id"]
                        .as_str()
                        .map(str::to_string),
                )
            })
            .collect()
    }
}

/// Builds one expected reference tuple.
fn reference(
    file: &str,
    (start, end): (u64, u64),
    kind: &str,
    resolution: &str,
    target: Option<&str>,
    container: Option<&str>,
) -> Ref {
    (
        file.to_string(),
        start,
        end,
        kind.to_string(),
        resolution.to_string(),
        target.map(str::to_string),
        container.map(str::to_string),
    )
}

const DECLS: &str = "<?php
namespace App;

const LIMIT = 10;

function launch(): void {}

interface I {}

class Dep {}

class Foo
{
    public const BAR = 1;
    public static $prop = 2;
    public $items;
    public static function make(): void {}
}

class MyError extends \\Exception {}
";

const OUTER: &str = "<?php
namespace App;

class Outer
{
    public function run(): void {}

    public function host(): void
    {
        $a = new class {
            public function run(Dep $d): void
            {
                launch();
                new Dep();
                $this->run();
                self::run();
                static::run();
            }
        };
        $this->run();
    }
}
";

/// Finding 11: uses inside an anonymous class body appear, attached to the
/// nearest named container.
#[test]
fn anonymous_class_body_uses_appear_with_the_nearest_named_container() {
    let repo = Repo::new("anon-body", &[("decls.php", DECLS), ("outer.php", OUTER)]);
    let host = Some("outer.php#App\\Outer::host");
    assert_eq!(
        repo.refs("App\\launch", "references"),
        vec![reference(
            "outer.php",
            repo.span("outer.php", "launch();", 0, "launch"),
            "call",
            "exact",
            Some("decls.php#App\\launch"),
            host,
        )]
    );
    assert_eq!(
        repo.refs("App\\Dep", "references"),
        vec![
            reference(
                "outer.php",
                repo.span("outer.php", "Dep $d", 0, "Dep"),
                "type",
                "exact",
                Some("decls.php#App\\Dep"),
                host,
            ),
            reference(
                "outer.php",
                repo.span("outer.php", "new Dep()", 0, "Dep"),
                "type",
                "exact",
                Some("decls.php#App\\Dep"),
                host,
            ),
        ]
    );
}

/// The anonymous-class trap: `$this`, `self`, and `static` inside the
/// anonymous class name the anonymous class, so they bind nothing even though
/// the enclosing class declares `run`. Only the outer `$this->run()` binds.
#[test]
fn this_inside_an_anonymous_class_never_binds_the_outer_class() {
    let repo = Repo::new("anon-this", &[("decls.php", DECLS), ("outer.php", OUTER)]);
    let host = Some("outer.php#App\\Outer::host");
    let run = Some("outer.php#App\\Outer::run");
    let expected = vec![
        reference(
            "outer.php",
            repo.span("outer.php", "$this->run();", 0, "run"),
            "call",
            "name_match",
            None,
            host,
        ),
        reference(
            "outer.php",
            repo.span("outer.php", "self::run();", 0, "run"),
            "call",
            "name_match",
            None,
            host,
        ),
        reference(
            "outer.php",
            repo.span("outer.php", "static::run();", 0, "run"),
            "call",
            "name_match",
            None,
            host,
        ),
        reference(
            "outer.php",
            repo.span("outer.php", "$this->run();", 1, "run"),
            "call",
            "scoped",
            run,
            host,
        ),
    ];
    assert_eq!(repo.refs("App\\Outer::run", "candidates"), expected);
    // Unbound uses are identical in reference mode.
    assert_eq!(repo.refs("App\\Outer::run", "references"), expected);
}

/// Finding 12: an unresolved qualified class spelling name-matches by its
/// short name, and a bare constant by its exact, case-sensitive spelling.
#[test]
fn unresolved_qualified_new_and_bare_constant_appear_in_candidate_mode() {
    let source = "<?php
namespace App;

function f(): void
{
    $x = new Sub\\Missing\\Foo();
    $l = LIMIT;
    $m = limit;
}
";
    let repo = Repo::new("short-names", &[("decls.php", DECLS), ("use.php", source)]);
    let f = Some("use.php#App\\f");
    assert_eq!(
        repo.refs("App\\Foo", "candidates"),
        vec![reference(
            "use.php",
            repo.span("use.php", "Sub\\Missing\\Foo", 0, "Sub\\Missing\\Foo"),
            "type",
            "name_match",
            None,
            f,
        )]
    );
    // Constants are case-sensitive: `limit` is not a use of `LIMIT`.
    assert_eq!(
        repo.refs("App\\LIMIT", "candidates"),
        vec![reference(
            "use.php",
            repo.span("use.php", "LIMIT;", 0, "LIMIT"),
            "unknown",
            "name_match",
            None,
            f,
        )]
    );
    // `rivet symbol` agrees: the lowercase spelling names no constant.
    let output = run(repo.temp.path(), &["symbol", "limit", "--json"]);
    assert_ne!(
        output.status.code(),
        Some(0),
        "`limit` must not find `LIMIT`"
    );
}

/// The first "Already known" item: an unresolved property use name-matches
/// its declaration, whose name keeps the leading `$`.
#[test]
fn an_unresolved_property_use_name_matches_its_declaration() {
    let source = "<?php
namespace App;

function g($q): void
{
    $q->items;
    $q->Items;
}
";
    let repo = Repo::new("property", &[("decls.php", DECLS), ("use.php", source)]);
    assert_eq!(
        repo.refs("App\\Foo::$items", "references"),
        vec![reference(
            "use.php",
            repo.span("use.php", "$q->items", 0, "items"),
            "read",
            "name_match",
            None,
            Some("use.php#App\\g"),
        )]
    );
}

/// Finding 13: a class named explicitly before `::` is a type use of that
/// class, and the member binds `scoped` when the class declares it.
#[test]
fn explicit_class_references_appear_in_refs() {
    let source = "<?php
namespace App;

function h(): void
{
    Foo::make();
    $b = Foo::BAR;
    $p = Foo::$prop;
    $c = Foo::class;
    \\App\\Foo::missing();
}
";
    let repo = Repo::new("explicit", &[("decls.php", DECLS), ("use.php", source)]);
    let h = Some("use.php#App\\h");
    let foo = Some("decls.php#App\\Foo");
    let type_ref = |needle: &str, inner: &str| {
        reference(
            "use.php",
            repo.span("use.php", needle, 0, inner),
            "type",
            "exact",
            foo,
            h,
        )
    };
    assert_eq!(
        repo.refs("App\\Foo", "references"),
        vec![
            type_ref("Foo::make", "Foo"),
            type_ref("Foo::BAR", "Foo"),
            type_ref("Foo::$prop", "Foo"),
            type_ref("Foo::class", "Foo"),
            type_ref("\\App\\Foo::missing", "\\App\\Foo"),
        ]
    );
    assert_eq!(
        repo.refs("App\\Foo::make", "references"),
        vec![reference(
            "use.php",
            repo.span("use.php", "Foo::make", 0, "make"),
            "call",
            "scoped",
            Some("decls.php#App\\Foo::make"),
            h,
        )]
    );
    assert_eq!(
        repo.refs("App\\Foo::BAR", "references"),
        vec![reference(
            "use.php",
            repo.span("use.php", "Foo::BAR", 0, "BAR"),
            "read",
            "scoped",
            Some("decls.php#App\\Foo::BAR"),
            h,
        )]
    );
    assert_eq!(
        repo.refs("App\\Foo::$prop", "references"),
        vec![reference(
            "use.php",
            repo.span("use.php", "Foo::$prop", 0, "$prop"),
            "read",
            "scoped",
            Some("decls.php#App\\Foo::$prop"),
            h,
        )]
    );
}

/// `static::` is late static binding and `parent::` needs inheritance; both
/// bind nothing, even when the named member exists on the enclosing class or
/// its parent. `self::` keeps binding `scoped`.
#[test]
fn static_and_parent_scopes_bind_nothing() {
    let source = "<?php
namespace App;

class Child extends Foo
{
    public static function make(): void {}

    public function go(): void
    {
        static::make();
        parent::make();
        self::make();
    }
}
";
    let repo = Repo::new(
        "static-parent",
        &[("decls.php", DECLS), ("child.php", source)],
    );
    let go = Some("child.php#App\\Child::go");
    assert_eq!(
        repo.refs("App\\Child::make", "candidates"),
        vec![
            reference(
                "child.php",
                repo.span("child.php", "static::make", 0, "make"),
                "call",
                "name_match",
                None,
                go,
            ),
            reference(
                "child.php",
                repo.span("child.php", "parent::make", 0, "make"),
                "call",
                "name_match",
                None,
                go,
            ),
            reference(
                "child.php",
                repo.span("child.php", "self::make", 0, "make"),
                "call",
                "scoped",
                Some("child.php#App\\Child::make"),
                go,
            ),
        ]
    );
}

/// Carried from the AF2 review: an `instanceof` operand is a type use, so a
/// fully qualified interface binds `exact` again. A `catch` type already was.
#[test]
fn instanceof_and_catch_types_bind_the_class_exact() {
    let source = "<?php
namespace App;

function k($x): bool
{
    try {
    } catch (MyError $e) {
    }
    return $x instanceof \\App\\I || $x instanceof I;
}
";
    let repo = Repo::new("instanceof", &[("decls.php", DECLS), ("use.php", source)]);
    let k = Some("use.php#App\\k");
    assert_eq!(
        repo.refs("App\\I", "references"),
        vec![
            reference(
                "use.php",
                repo.span("use.php", "\\App\\I", 0, "\\App\\I"),
                "type",
                "exact",
                Some("decls.php#App\\I"),
                k,
            ),
            reference(
                "use.php",
                repo.span("use.php", "instanceof I", 0, "I"),
                "type",
                "exact",
                Some("decls.php#App\\I"),
                k,
            ),
        ]
    );
    assert_eq!(
        repo.refs("App\\MyError", "references"),
        vec![reference(
            "use.php",
            repo.span("use.php", "MyError $e", 0, "MyError"),
            "type",
            "exact",
            Some("decls.php#App\\MyError"),
            k,
        )]
    );
}
