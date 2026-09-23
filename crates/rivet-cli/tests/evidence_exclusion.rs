//! LR2 integration tests: evidence-based exclusion in `refs --mode references`
//! (spec §11.5).
//!
//! Each case writes a temporary Git root with a small PHP fixture, indexes it
//! with the real binary, and checks which same-name unresolved uses reference
//! mode keeps, which it excludes, and how it counts them:
//!
//! - rule 1, use form: a bare call against a method, a receiver call against
//!   a function, a call against a property, and a property read against a
//!   method are excluded;
//! - rule 2, receiver class: a receiver whose class a receiver rule determined
//!   (a typed parameter, `new`, `$this`, a class before `::`) is excluded when
//!   the class is unrelated to the target's; a subclass, the parent, an
//!   implemented interface, and an unindexed vendor ancestor are kept;
//! - never excluded: an untyped receiver, a type name that has no qualified
//!   name, a receiver the rules refuse (rebinding, conflicting `new`, union), a
//!   trait target, a target whose ancestry has an unknown link, and any use
//!   bound to the target;
//! - `by_exclusion` counts are page-independent and zero in candidate mode,
//!   candidate mode still lists every excluded use, output is deterministic
//!   across `--force`, and `context`/`symbol.called_by` inherit the rule;
//! - EX1: the human line states each non-zero reason with the target's kind
//!   word, never points at candidate mode, and is absent when nothing is
//!   excluded; `refs --json` is byte-identical to the pre-EX1 binary.

#![cfg(feature = "lang-php")]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

const MODEL: &str = "<?php
declare(strict_types=1);

namespace App;

use Illuminate\\Database\\Eloquent\\Model;
use Illuminate\\Http\\Request;

interface Savable {}
class Base extends Model {}
class M extends Base implements Savable
{
    public array $items = [];
    public function save(): void {}
    public function items(): array { return []; }
    public function posts(): array { return []; }
    public function again(): void { $this->save(); } // SELF_BOUND
    public function statically(): void { self::save(); } // SELF_STATIC
}
class Sub extends M {}
class Child extends M
{
    public function run(): void { $this->save(); } // CHILD_THIS
}
class Other
{
    private Request $req;
    public function run(): void { $this->save(); } // OTHER_THIS
    public function viaProperty(): void { $this->req->save(); } // PROP_REQ
}
trait Tr
{
    public function persist(): void {}
    public function go(): void { $this->save(); } // TRAIT_THIS
}
class UsesTr
{
    use Tr;
    public function run(): void { $this->persist(); } // TRAIT_USER
}
class Unrelated
{
    public function run(): void { $this->persist(); } // TRAIT_UNREL
}
function helper(): void {}
";

const USES: &str = "<?php
declare(strict_types=1);

namespace App;

use Illuminate\\Http\\Request;
use Illuminate\\Database\\Eloquent\\Model;
use One\\Clash;
use Two\\Clash;

function typed(Request $req, Sub $sub, Base $base, Savable $sav, Model $model, $untyped, Clash $clash): void
{
    $req->save(); // REQ
    $sub->save(); // SUB
    $base->save(); // BASE
    $sav->save(); // SAV
    $model->save(); // MODEL
    $untyped->save(); // UNTYPED
    $clash->save(); // CLASH
    $req->persist(); // REQ_PERSIST
}
function rebound(Request $again): void
{
    $again = make();
    $again->save(); // REBOUND
}
function branchy(bool $c): void
{
    $obj = new Request();
    if ($c) { $obj = new Other(); }
    $obj->save(); // CONFLICT
}
function union(Request|Other $u): void
{
    $u->save(); // UNION
}
function news(): void
{
    $q = new Request();
    $q->save(); // NEWREQ
    $p = new Sub();
    $p->save(); // NEWSUB
    $m = new M();
    $m->save(); // BOUND_NEW
    Request::save(); // STATIC_REQ
    Sub::save(); // STATIC_SUB
    M::save(); // BOUND_STATIC
    save(); // BARE
}
function forms($any): void
{
    $any->helper(); // RECV_HELPER
    helper(); // FN_HELPER
    $any->items(); // CALL_ITEMS
    $any->items; // READ_ITEMS
    $any->items = 1; // WRITE_ITEMS
}
function relations(M $m): void
{
    $m->posts; // READ_POSTS
    $m->posts = []; // WRITE_POSTS
}
";

/// A class whose only supertype has no qualified name: two imports claim
/// its alias, so its ancestry has an unknown link.
const WEIRD: &str = "<?php
declare(strict_types=1);

namespace W;

use A\\X;
use B\\X;

class Weird extends X
{
    public function save(): void {}
}
";

/// The reviewer's repros and the vendor-free cases: every subtype of `T`,
/// `I`, and `K` is indexed and has no unindexed ancestor.
const POLY: &str = "<?php
declare(strict_types=1);

namespace P;

use Illuminate\\Http\\Request;

interface R {}
class T { public function m(): void {} }
class S extends T implements R {}
interface I { public function run(): void; }
class C {}
class D extends C implements I { public function run(): void {} }
class U {}
class K { public function kick(): void {} }
interface J {}
interface J2 {}
$anon = new class extends K implements J {};

function a(R $r): void { $r->m(); } // A_INTERFACE_RECEIVER
function b(C $c): void { $c->run(); } // B_PARENT_OF_IMPLEMENTER
function u(U $u): void { $u->m(); } // UNRELATED_INDEXED
function v(Request $q): void { $q->m(); } // UNINDEXED_RECEIVER
function w(J $j): void { $j->kick(); } // ANON_COMMON_SUBTYPE
function w2(J2 $j): void { $j->kick(); } // NO_ANON_SUBTYPE
";

/// The main fixture: `M` reaches the vendor class `Model`.
const FILES: [(&str, &str); 2] = [("Model.php", MODEL), ("Uses.php", USES)];

/// Every file a marker may live in.
const ALL_FILES: [(&str, &str); 4] = [
    ("Model.php", MODEL),
    ("Uses.php", USES),
    ("Weird.php", WEIRD),
    ("Poly.php", POLY),
];

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
            "rivet-exclusion-{label}-{}-{nanos}-{unique}",
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

fn repo(label: &str) -> TempDir {
    repo_with(label, &FILES)
}

fn repo_with(label: &str, files: &[(&str, &str)]) -> TempDir {
    let temp = TempDir::new(label);
    for (name, source) in files {
        fs::write(temp.path().join(name), source.as_bytes()).expect("write PHP file");
    }
    temp
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(root)
        .output()
        .expect("run the rivet binary")
}

/// Runs a command that must succeed and returns its stdout bytes.
fn ok(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = run(root, args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn json_of(root: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&ok(root, args)).expect("stdout is JSON")
}

/// The 1-based line of the fixture line carrying `// MARKER`.
fn line_of(marker: &str) -> (String, u64) {
    let needle = format!("// {marker}\n");
    for (name, source) in ALL_FILES {
        if let Some(position) = source.find(&needle) {
            let line = source[..position].matches('\n').count() as u64 + 1;
            return (name.to_string(), line);
        }
    }
    panic!("no marker {marker}");
}

/// The fixture markers of every reference on one page, sorted.
fn markers(value: &Value, all: &[&str]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for item in value["references"].as_array().expect("references") {
        let site = (
            item["file"].as_str().expect("file").to_string(),
            item["line"].as_u64().expect("line"),
        );
        let marker = all
            .iter()
            .find(|marker| line_of(marker) == site)
            .unwrap_or_else(|| panic!("unmarked reference {item}"));
        found.insert(marker.to_string());
    }
    found
}

fn set(markers: &[&str]) -> BTreeSet<String> {
    markers.iter().map(|marker| marker.to_string()).collect()
}

/// The human `refs` line that reports exclusions (EX1), which must directly
/// follow the count header; `None` when no line reports any.
fn exclusion_line(text: &str) -> Option<&str> {
    let (index, line) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("ruled out"))?;
    assert_eq!(index, 2, "the line follows the count header: {text}");
    Some(line)
}

const SAVE_SITES: &[&str] = &[
    "SELF_BOUND",
    "SELF_STATIC",
    "CHILD_THIS",
    "OTHER_THIS",
    "PROP_REQ",
    "TRAIT_THIS",
    "REQ",
    "SUB",
    "BASE",
    "SAV",
    "MODEL",
    "UNTYPED",
    "CLASH",
    "REBOUND",
    "CONFLICT",
    "UNION",
    "NEWREQ",
    "NEWSUB",
    "BOUND_NEW",
    "STATIC_REQ",
    "STATIC_SUB",
    "BOUND_STATIC",
    "BARE",
];

/// The `save` uses reference mode keeps for `App\M::save`.
const SAVE_KEPT: &[&str] = &[
    // Bound to the target: never excluded.
    "SELF_BOUND",
    "SELF_STATIC",
    "BOUND_NEW",
    "BOUND_STATIC",
    // A subclass, the parent, an implemented interface, and an unindexed
    // (vendor) ancestor of `M` are related receivers.
    "CHILD_THIS",
    "SUB",
    "BASE",
    "SAV",
    "MODEL",
    "NEWSUB",
    "STATIC_SUB",
    // `$this` inside a trait may be any class that uses it.
    "TRAIT_THIS",
    // Unindexed receivers: every subtype of `M` reaches the vendor class
    // `Model`, whose unknown ancestors could include them (rule 2 (b)).
    "REQ",
    "PROP_REQ",
    "NEWREQ",
    "STATIC_REQ",
    // No receiver class: untyped, no qualified name, rebound, conflicting
    // `new` assignments, a union type.
    "UNTYPED",
    "CLASH",
    "REBOUND",
    "CONFLICT",
    "UNION",
];

#[test]
fn reference_mode_excludes_uses_by_form_and_unrelated_receiver() {
    let temp = repo("save");
    let value = json_of(temp.path(), &["refs", "App\\M::save", "--json"]);
    assert_eq!(markers(&value, SAVE_SITES), set(SAVE_KEPT), "{value}");
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 1, "unrelated_receiver": 1}),
        "BARE is excluded by form; OTHER_THIS (indexed, no common subtype) by receiver"
    );
    assert_eq!(value["total"], SAVE_KEPT.len());

    // Bound uses keep their tier; every kept unbound use stays name_match.
    for item in value["references"].as_array().unwrap() {
        let bound = item["resolved_target"] == "Model.php#App\\M::save";
        let expected = if bound { "scoped" } else { "name_match" };
        assert_eq!(item["resolution"], expected, "{item}");
    }

    // The human rendering states the excluded count and why (EX1).
    let text = String::from_utf8(ok(temp.path(), &["refs", "App\\M::save"])).unwrap();
    assert_eq!(
        exclusion_line(&text),
        Some(
            "2 same-name uses ruled out (unrelated receiver class: 1; form cannot reference a method: 1)"
        ),
        "{text}"
    );
    assert!(!text.contains("--mode candidates"), "{text}");
}

#[test]
fn candidate_mode_still_lists_every_excluded_use_unchanged() {
    let temp = repo("candidates");
    let value = json_of(
        temp.path(),
        &["refs", "App\\M::save", "--json", "--mode", "candidates"],
    );
    assert_eq!(markers(&value, SAVE_SITES), set(SAVE_SITES));
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 0})
    );
    // An excluded use is listed exactly as an unresolved candidate: no tier
    // change, no target.
    let req = line_of("REQ");
    let item = value["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == req.0.as_str() && item["line"] == req.1)
        .expect("REQ candidate");
    assert_eq!(item["resolution"], "name_match");
    assert_eq!(item["resolved_target"], Value::Null);
    assert_eq!(item["receiver"], "$req");
    assert_eq!(item["ref_kind"], "call");

    // No exclusion line in the human rendering of candidate mode.
    let text = String::from_utf8(ok(
        temp.path(),
        &["refs", "App\\M::save", "--mode", "candidates"],
    ))
    .unwrap();
    assert!(!text.contains("ruled out"), "{text}");
}

#[test]
fn a_trait_target_and_an_unknown_ancestor_are_never_excluded_by_receiver() {
    let temp = repo("trait");
    // `$this` in a class using the trait, in an unrelated class, and a typed
    // unrelated receiver: trait use is not tracked, so all are kept.
    let sites = &["TRAIT_USER", "TRAIT_UNREL", "REQ_PERSIST"];
    let value = json_of(temp.path(), &["refs", "App\\Tr::persist", "--json"]);
    assert_eq!(markers(&value, sites), set(sites));
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 0})
    );

    // `Weird` extends a name two imports claim, which could be any class, so
    // once it is indexed no receiver class is known to be unrelated to any
    // target, not even `OTHER_THIS` for `App\M::save`.
    let temp = repo_with(
        "weird",
        &[
            ("Model.php", MODEL),
            ("Uses.php", USES),
            ("Weird.php", WEIRD),
        ],
    );
    let value = json_of(temp.path(), &["refs", "App\\M::save", "--json"]);
    assert!(
        markers(&value, SAVE_SITES).contains("OTHER_THIS"),
        "{value}"
    );
    assert_eq!(value["by_exclusion"]["unrelated_receiver"], 0);
    let value = json_of(temp.path(), &["refs", "W\\Weird::save", "--json"]);
    let kept = markers(&value, SAVE_SITES);
    for site in ["REQ", "NEWREQ", "STATIC_REQ", "OTHER_THIS", "SUB"] {
        assert!(kept.contains(site), "{site} must be kept: {value}");
    }
    assert!(
        !kept.contains("BARE"),
        "a bare call is still excluded by form"
    );
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 1, "unrelated_receiver": 0})
    );
}

#[test]
fn use_forms_incompatible_with_the_target_kind_are_excluded() {
    let temp = repo("forms");
    let sites = &[
        "RECV_HELPER",
        "FN_HELPER",
        "CALL_ITEMS",
        "READ_ITEMS",
        "WRITE_ITEMS",
    ];

    // A receiver call cannot name a function.
    let value = json_of(temp.path(), &["refs", "App\\helper", "--json"]);
    assert_eq!(markers(&value, sites), set(&["FN_HELPER"]));
    assert_eq!(value["by_exclusion"]["incompatible_form"], 1);

    // A call cannot name a property.
    let value = json_of(temp.path(), &["refs", "App\\M::$items", "--json"]);
    assert_eq!(markers(&value, sites), set(&["READ_ITEMS", "WRITE_ITEMS"]));
    assert_eq!(value["by_exclusion"]["incompatible_form"], 1);

    // A property read is kept for a method, since `__get` can dispatch it
    // there; a property write cannot reach a method and is excluded.
    let value = json_of(temp.path(), &["refs", "App\\M::items", "--json"]);
    assert_eq!(markers(&value, sites), set(&["CALL_ITEMS", "READ_ITEMS"]));
    assert_eq!(value["by_exclusion"]["incompatible_form"], 1);

    // `$m->posts` may reach the relation method `posts()` through `__get`;
    // `$m->posts = []` goes through `__set`, which never calls `posts()`.
    let posts = &["READ_POSTS", "WRITE_POSTS"];
    let value = json_of(temp.path(), &["refs", "App\\M::posts", "--json"]);
    assert_eq!(markers(&value, posts), set(&["READ_POSTS"]));
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 1, "unrelated_receiver": 0})
    );
    let value = json_of(
        temp.path(),
        &["refs", "App\\M::posts", "--json", "--mode", "candidates"],
    );
    assert_eq!(markers(&value, posts), set(posts));
}

#[test]
fn exclusion_counts_are_page_independent_and_output_is_deterministic() {
    let temp = repo("pages");
    let root = temp.path();
    let full = json_of(root, &["refs", "App\\M::save", "--json"]);
    let page = json_of(
        root,
        &[
            "refs",
            "App\\M::save",
            "--json",
            "--limit",
            "1",
            "--offset",
            "1",
        ],
    );
    assert_eq!(page["by_exclusion"], full["by_exclusion"]);
    assert_eq!(page["total"], full["total"]);
    assert_eq!(page["references"].as_array().unwrap().len(), 1);
    assert_eq!(page["references"][0], full["references"][1]);

    // A kind filter that matches nothing excludes nothing.
    let types = json_of(root, &["refs", "App\\M::save", "--json", "--kind", "type"]);
    assert_eq!(
        types["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 0})
    );
    // A minimum tier above name_match filters the unresolved uses first.
    let scoped = json_of(
        root,
        &[
            "refs",
            "App\\M::save",
            "--json",
            "--min-resolution",
            "scoped",
        ],
    );
    assert_eq!(
        scoped["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 0})
    );

    let args = ["refs", "App\\M::save", "--json"];
    let first = ok(root, &args);
    assert_eq!(ok(root, &args), first, "a repeated query is byte-identical");
    ok(root, &["index", "--force", "--json"]);
    assert_eq!(ok(root, &args), first, "--force rebuilds the same answer");
    let candidates = ["refs", "App\\M::save", "--json", "--mode", "candidates"];
    let before = ok(root, &candidates);
    ok(root, &["index", "--force", "--json"]);
    assert_eq!(ok(root, &candidates), before);

    // An edit to the hierarchy is seen by the next query: once `Other`
    // extends `M`, its `$this->save()` is related and kept.
    fs::write(
        root.join("Model.php"),
        MODEL.replace("class Other\n", "class Other extends M\n"),
    )
    .expect("rewrite Model.php");
    let edited = json_of(root, &args);
    assert_eq!(
        edited["by_exclusion"],
        json!({"incompatible_form": 1, "unrelated_receiver": 0})
    );
    let mut kept = set(SAVE_KEPT);
    kept.insert("OTHER_THIS".to_string());
    assert_eq!(markers(&edited, SAVE_SITES), kept);
}

#[test]
fn context_and_called_by_inherit_reference_mode_exclusion() {
    let temp = repo("context");
    let root = temp.path();

    // `symbol.called_by` is reference-mode matching of call sites.
    let symbol = json_of(
        root,
        &[
            "symbol",
            "App\\M::save",
            "--json",
            "--limit",
            "100",
            "--min-resolution",
            "name_match",
        ],
    );
    assert_eq!(symbol["called_by"]["total"], SAVE_KEPT.len());
    assert_eq!(symbol["called_by"]["hidden_name_match"], 0);
    // SY1: the default lists the bound callers and counts the kept name-only
    // ones; a use excluded by evidence is in neither number.
    let name_only = symbol["called_by"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["resolution"] == "name_match")
        .count();
    assert!(name_only > 0, "{symbol}");
    let default = json_of(
        root,
        &["symbol", "App\\M::save", "--json", "--limit", "100"],
    );
    assert_eq!(
        default["called_by"]["total"],
        SAVE_KEPT.len() - name_only,
        "{default}"
    );
    assert_eq!(default["called_by"]["hidden_name_match"], name_only);

    let value = json_of(
        root,
        &["context", "App\\M::save", "--json", "--tokens", "100000"],
    );
    let segments = value["segments"].as_array().expect("segments");
    let ids: Vec<&str> = segments
        .iter()
        .map(|segment| segment["symbol"]["id"].as_str().expect("id"))
        .collect();
    // `Child::run` calls `save` on a subclass receiver; `Other::run` only
    // on an unrelated `$this`, which reference mode excludes.
    assert!(ids.contains(&"Model.php#App\\Child::run"), "{ids:?}");
    assert!(!ids.contains(&"Model.php#App\\Other::run"), "{ids:?}");
    // `news` still calls `save` through kept and bound uses.
    assert!(ids.contains(&"Uses.php#App\\news"), "{ids:?}");

    // Budget accounting is unchanged: the total is the sum of the segments
    // and fits the budget.
    let sum: u64 = segments
        .iter()
        .map(|segment| segment["estimated_tokens"].as_u64().expect("tokens"))
        .sum();
    assert_eq!(value["estimated_tokens"], sum);
    assert!(sum <= value["budget_tokens"].as_u64().unwrap());

    // A tight budget still fits and still omits the excluded caller.
    let tight = json_of(
        root,
        &["context", "App\\M::save", "--json", "--tokens", "120"],
    );
    let sum: u64 = tight["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| segment["estimated_tokens"].as_u64().unwrap())
        .sum();
    assert_eq!(tight["estimated_tokens"], sum);
    assert!(sum <= 120);
    assert!(
        tight["segments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|segment| segment["symbol"]["id"] != "Model.php#App\\Other::run")
    );
}

/// The sites of `POLY`.
const POLY_SITES: &[&str] = &[
    "A_INTERFACE_RECEIVER",
    "B_PARENT_OF_IMPLEMENTER",
    "UNRELATED_INDEXED",
    "UNINDEXED_RECEIVER",
    "ANON_COMMON_SUBTYPE",
    "NO_ANON_SUBTYPE",
];

/// Reference mode for `query` must list exactly `kept` among the `POLY`
/// sites with `unrelated` receiver exclusions, and candidate mode every
/// name-matching site with nothing excluded.
fn assert_poly(root: &Path, query: &str, kept: &[&str], all: &[&str], unrelated: u64) {
    let value = json_of(root, &["refs", query, "--json"]);
    assert_eq!(markers(&value, POLY_SITES), set(kept), "{query}: {value}");
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": unrelated}),
        "{query}"
    );
    let candidates = json_of(root, &["refs", query, "--json", "--mode", "candidates"]);
    assert_eq!(markers(&candidates, POLY_SITES), set(all), "{query}");
    assert_eq!(
        candidates["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 0})
    );
    for item in candidates["references"].as_array().unwrap() {
        assert_eq!(item["resolution"], "name_match", "{item}");
        assert_eq!(item["resolved_target"], Value::Null, "{item}");
    }
}

#[test]
fn only_receivers_with_no_possible_common_subtype_are_excluded() {
    let temp = repo_with("poly", &[("Poly.php", POLY)]);
    let root = temp.path();
    let m_sites = &[
        "A_INTERFACE_RECEIVER",
        "UNRELATED_INDEXED",
        "UNINDEXED_RECEIVER",
    ];
    // (A) `$r` may be an `S`, which is a `T` and an `R`: kept. `U` shares no
    // subtype with `T`, and every subtype of `T` is indexed and vendor-free,
    // so the unindexed `Request` cannot be one either: both excluded.
    assert_poly(root, "P\\T::m", &["A_INTERFACE_RECEIVER"], m_sites, 2);
    // (B) `$c` may be a `D`, which is a `C` and an `I`: kept.
    assert_poly(
        root,
        "P\\I::run",
        &["B_PARENT_OF_IMPLEMENTER"],
        &["B_PARENT_OF_IMPLEMENTER"],
        0,
    );
    // An anonymous class extending `K` and implementing `J` is a common
    // subtype of `K` and `J`, but nothing is both a `K` and a `J2`.
    assert_poly(
        root,
        "P\\K::kick",
        &["ANON_COMMON_SUBTYPE"],
        &["ANON_COMMON_SUBTYPE", "NO_ANON_SUBTYPE"],
        1,
    );
}

#[test]
fn a_vendor_rooted_target_keeps_an_unindexed_ancestor_receiver() {
    // `User` reaches the vendor `Authenticatable`, whose own ancestors are
    // unknown and may include the vendor `Model`, so `$m->save()` may
    // dispatch to `User::save` and is kept; an indexed unrelated receiver is
    // still excluded.
    let source = "<?php
declare(strict_types=1);

namespace App;

use Illuminate\\Foundation\\Auth\\User as Authenticatable;
use Illuminate\\Database\\Eloquent\\Model;

class User extends Authenticatable { public function save(): void {} }
class Plain {}
function f(Model $m, Plain $p): void
{
    $m->save();
    $p->save();
}
";
    let temp = repo_with("vendor", &[("User.php", source)]);
    let value = json_of(temp.path(), &["refs", "App\\User::save", "--json"]);
    let receivers: Vec<&str> = value["references"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receiver"].as_str().unwrap())
        .collect();
    assert_eq!(receivers, ["$m"], "{value}");
    assert_eq!(
        value["by_exclusion"],
        json!({"incompatible_form": 0, "unrelated_receiver": 1})
    );
    let candidates = json_of(
        temp.path(),
        &["refs", "App\\User::save", "--json", "--mode", "candidates"],
    );
    assert_eq!(candidates["total"], 2);
}

/// Same-name uses an interface target excludes by form: a receiver call and
/// a bare call.
const IFACE: &str = "<?php
declare(strict_types=1);

namespace App;

function z($x): void { $x->Savable(); Savable(); }
";

/// Asserts the human rendering of `refs query` in `root` reports exactly
/// `expected` as its exclusion line, and never points at candidate mode.
fn assert_line(root: &Path, query: &str, expected: Option<&str>) {
    let text = String::from_utf8(ok(root, &["refs", query])).expect("stdout is UTF-8");
    assert_eq!(exclusion_line(&text), expected, "{query}: {text}");
    assert!(!text.contains("--mode candidates"), "{query}: {text}");
}

#[test]
fn the_exclusion_line_states_each_nonzero_reason_in_a_fixed_order() {
    let temp = repo_with(
        "line",
        &[
            ("Model.php", MODEL),
            ("Uses.php", USES),
            ("Iface.php", IFACE),
        ],
    );
    let root = temp.path();
    // Form only, singular; the kind word is the header's, not hard-coded.
    assert_line(
        root,
        "App\\M::posts",
        Some("1 same-name use ruled out (form cannot reference a method: 1)"),
    );
    assert_line(
        root,
        "App\\helper",
        Some("1 same-name use ruled out (form cannot reference a function: 1)"),
    );
    assert_line(
        root,
        "App\\M::$items",
        Some("1 same-name use ruled out (form cannot reference a property: 1)"),
    );
    // A kind word starting with a vowel takes `an`.
    assert_line(
        root,
        "App\\Savable",
        Some("2 same-name uses ruled out (form cannot reference an interface: 2)"),
    );
    // Nothing excluded: no line at all.
    assert_line(root, "App\\Tr::persist", None);

    // Receiver only, plural and singular.
    let temp = repo_with("line-poly", &[("Poly.php", POLY)]);
    let root = temp.path();
    assert_line(
        root,
        "P\\T::m",
        Some("2 same-name uses ruled out (unrelated receiver class: 2)"),
    );
    assert_line(
        root,
        "P\\K::kick",
        Some("1 same-name use ruled out (unrelated receiver class: 1)"),
    );
    // Both, receiver first: a bare call adds a form exclusion.
    fs::write(root.join("Bare.php"), "<?php\nnamespace P;\n\nm();\n").expect("write Bare.php");
    assert_line(
        root,
        "P\\T::m",
        Some(
            "3 same-name uses ruled out (unrelated receiver class: 2; form cannot reference a method: 1)",
        ),
    );
}

#[test]
fn refs_json_is_byte_identical_to_the_pre_ex1_binary() {
    // Captured from a build of 82879eb, before EX1 changed the human line,
    // for a query with both kinds of exclusion (counts are page-independent).
    let golden =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/ex1-json/refs-save-page.json");
    let expected = fs::read_to_string(golden).expect("read golden");
    let temp = repo("json-identity");
    let output = run(
        temp.path(),
        &["refs", "App\\M::save", "--json", "--limit", "1"],
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}
