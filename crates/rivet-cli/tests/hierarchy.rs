//! T36d integration tests: class-header uses, scope-expression uses, and the
//! declared class hierarchy.
//!
//! Each case writes a temporary Git root with a small PHP fixture, indexes it
//! with the real binary, and checks `rivet refs --json` output and the
//! [`rivet_index::Hierarchy`] read back from the committed store:
//!
//! - `refs` on a base class lists every subclass header with the tier the
//!   import/namespace rules give it: imported by `use`, same namespace, and
//!   fully qualified are `exact`; an unimported name in another namespace does
//!   not bind and stays `name_match` with no target;
//! - an anonymous class's `extends`/`implements` are listed but it is still
//!   not a symbol;
//! - a use inside an expression scope (`$this->make()::$count`) is listed;
//! - direct and transitive supertypes, a vendor ancestor kept as a name, a
//!   cycle that terminates, and byte-identical output across refreshes and
//!   file creation orders.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_core::extract::SupertypeRelation;
use rivet_index::{Hierarchy, Supertype};
use rivet_store::Store;
use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

const LIB: &str = "<?php
declare(strict_types=1);

namespace Lib;

abstract class Base {}
interface I1 {}
interface I2 extends I1 {}
interface Both extends I1, I2 {}
class Mid extends Base implements I2 {}
class Leaf extends Mid implements Both, \\Vendor\\Contract, I1, I1 {}
class SameNs extends Base {}
class CycA extends CycB {}
class CycB extends CycA {}
enum Kind: string implements I1 { case A = 'a'; }
";

const APP: &str = "<?php
declare(strict_types=1);

namespace App;

use Lib\\Base;
use Vendor\\Thing;

class Imported extends Base {}
class Qualified extends \\Lib\\Base {}
class FromVendor extends Thing {}
class Factory
{
    public static int $count = 0;
    public function make(): self { return $this; }
    public function run(): void { $this->make()::$count; }
}
";

const OTHER: &str = "<?php
declare(strict_types=1);

namespace Other;

class Unimported extends Base {}
$x = new class extends \\Lib\\Base implements \\Lib\\I1 {};
";

const FILES: [(&str, &str); 3] = [("Lib.php", LIB), ("App.php", APP), ("Other.php", OTHER)];

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
            "rivet-hierarchy-{label}-{}-{nanos}-{unique}",
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

/// Writes `files` in the given order into a new temporary Git root.
fn repo(label: &str, files: &[(&str, &str)]) -> TempDir {
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

fn json(root: &Path, args: &[&str]) -> Value {
    serde_json::from_slice(&ok(root, args)).expect("stdout is JSON")
}

/// `(file, line, ref_kind, resolution, resolved_target, container)` of one
/// reference.
type RefRow = (String, u64, String, String, Option<String>, Option<String>);

/// A [`RefRow`] for each reference of one `refs --json` page.
fn reference_rows(value: &Value) -> Vec<RefRow> {
    value["references"]
        .as_array()
        .expect("references array")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().expect("file").to_string(),
                item["line"].as_u64().expect("line"),
                item["ref_kind"].as_str().expect("ref_kind").to_string(),
                item["resolution"].as_str().expect("resolution").to_string(),
                item["resolved_target"].as_str().map(str::to_string),
                item["containing_symbol"]["qualified_name"]
                    .as_str()
                    .map(str::to_string),
            )
        })
        .collect()
}

fn row(
    file: &str,
    line: u64,
    kind: &str,
    resolution: &str,
    target: Option<&str>,
    container: Option<&str>,
) -> RefRow {
    (
        file.to_string(),
        line,
        kind.to_string(),
        resolution.to_string(),
        target.map(str::to_string),
        container.map(str::to_string),
    )
}

/// Loads the committed hierarchy of an indexed repository.
fn hierarchy(root: &Path) -> Hierarchy {
    let store = Store::open(&root.join(".rivet")).expect("open committed store");
    Hierarchy::load(&store).expect("load hierarchy")
}

fn resolved(relation: SupertypeRelation, spelling: &str, id: &str) -> Supertype {
    let qualified_name = id.split_once('#').expect("canonical ID").1.to_string();
    Supertype {
        relation,
        spelling: spelling.to_string(),
        qualified_name: Some(qualified_name),
        resolved: Some(id.to_string()),
    }
}

fn unresolved(relation: SupertypeRelation, spelling: &str, qname: &str) -> Supertype {
    Supertype {
        relation,
        spelling: spelling.to_string(),
        qualified_name: Some(qname.to_string()),
        resolved: None,
    }
}

const BASE: &str = "Lib.php#Lib\\Base";
const I1: &str = "Lib.php#Lib\\I1";
const I2: &str = "Lib.php#Lib\\I2";
const BOTH: &str = "Lib.php#Lib\\Both";
const MID: &str = "Lib.php#Lib\\Mid";

#[test]
fn refs_on_a_base_class_list_every_subclass_with_an_honest_tier() {
    let temp = repo("refs-base", &FILES);
    let refs = json(temp.path(), &["refs", "Lib\\Base", "--json"]);
    assert_eq!(
        reference_rows(&refs),
        vec![
            // The `use` import itself, as before T36d.
            row("App.php", 6, "import", "exact", Some(BASE), None),
            // Imported by `use`.
            row(
                "App.php",
                9,
                "type",
                "exact",
                Some(BASE),
                Some("App\\Imported")
            ),
            // Fully qualified.
            row(
                "App.php",
                10,
                "type",
                "exact",
                Some(BASE),
                Some("App\\Qualified")
            ),
            row("Lib.php", 10, "type", "exact", Some(BASE), Some("Lib\\Mid")),
            // Same namespace.
            row(
                "Lib.php",
                12,
                "type",
                "exact",
                Some(BASE),
                Some("Lib\\SameNs")
            ),
            // `Base` in `Other` means `Other\Base`, which is not indexed: the
            // use matches by name only and is never upgraded.
            row(
                "Other.php",
                6,
                "type",
                "name_match",
                None,
                Some("Other\\Unimported")
            ),
            // An anonymous class's `extends` at file scope.
            row("Other.php", 7, "type", "exact", Some(BASE), None),
        ]
    );
    assert_eq!(
        refs["by_resolution"],
        serde_json::json!({"exact": 6, "scoped": 0, "name_match": 1})
    );
}

#[test]
fn implements_lists_interface_extends_and_enums_are_references() {
    let temp = repo("refs-interfaces", &FILES);
    let refs = json(temp.path(), &["refs", "Lib\\I1", "--json"]);
    assert_eq!(
        reference_rows(&refs),
        vec![
            row("Lib.php", 8, "type", "exact", Some(I1), Some("Lib\\I2")),
            row("Lib.php", 9, "type", "exact", Some(I1), Some("Lib\\Both")),
            // `implements ..., I1, I1` records both names as written.
            row("Lib.php", 11, "type", "exact", Some(I1), Some("Lib\\Leaf")),
            row("Lib.php", 11, "type", "exact", Some(I1), Some("Lib\\Leaf")),
            row("Lib.php", 15, "type", "exact", Some(I1), Some("Lib\\Kind")),
            row("Other.php", 7, "type", "exact", Some(I1), None),
        ]
    );
    let both = json(temp.path(), &["refs", "Lib\\I2", "--json"]);
    assert_eq!(
        reference_rows(&both),
        vec![
            row("Lib.php", 9, "type", "exact", Some(I2), Some("Lib\\Both")),
            row("Lib.php", 10, "type", "exact", Some(I2), Some("Lib\\Mid")),
        ]
    );
}

#[test]
fn an_anonymous_class_header_creates_no_symbol() {
    let temp = repo("anonymous", &FILES);
    ok(temp.path(), &["index", "--json"]);
    let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
    let other: Vec<String> = store
        .list_symbols()
        .expect("list symbols")
        .into_iter()
        .filter(|symbol| symbol.file == "Other.php")
        .map(|symbol| symbol.qualified_name)
        .collect();
    assert_eq!(other, vec!["Other", "Other\\Unimported"]);
}

#[test]
fn a_call_inside_an_expression_scope_is_a_reference() {
    let temp = repo("scope-expression", &FILES);
    let refs = json(temp.path(), &["refs", "App\\Factory::make", "--json"]);
    assert_eq!(
        reference_rows(&refs),
        vec![row(
            "App.php",
            16,
            "call",
            "scoped",
            Some("App.php#App\\Factory::make"),
            Some("App\\Factory::run")
        )]
    );
    // The static property after `::` is read through an expression, which
    // names no class: it is a use, but not a type use of anything.
    let count = json(temp.path(), &["refs", "App\\Factory::$count", "--json"]);
    let rows = reference_rows(&count);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].2, "read");
    assert_eq!(rows[0].3, "name_match");
}

#[test]
fn direct_supertypes_are_resolved_through_the_same_bindings() {
    let temp = repo("direct", &FILES);
    ok(temp.path(), &["index", "--json"]);
    let hierarchy = hierarchy(temp.path());
    use SupertypeRelation::{Extends, Implements};
    assert_eq!(
        hierarchy.direct_supertypes("Lib.php#Lib\\Leaf"),
        vec![
            resolved(Extends, "Mid", MID),
            resolved(Implements, "Both", BOTH),
            // `I1, I1` deduplicates to one entry.
            resolved(Implements, "I1", I1),
            // Not indexed: kept as its qualified name, never guessed.
            unresolved(Implements, "\\Vendor\\Contract", "Vendor\\Contract"),
        ]
    );
    assert_eq!(
        hierarchy.direct_supertypes("App.php#App\\Imported"),
        vec![resolved(Extends, "Base", BASE)]
    );
    assert_eq!(
        hierarchy.direct_supertypes("App.php#App\\Qualified"),
        vec![resolved(Extends, "\\Lib\\Base", BASE)]
    );
    // An import whose target is not indexed qualifies through the import.
    assert_eq!(
        hierarchy.direct_supertypes("App.php#App\\FromVendor"),
        vec![unresolved(Extends, "Thing", "Vendor\\Thing")]
    );
    // An unimported name qualifies against its own namespace and stays
    // unresolved, as its `type` use does.
    assert_eq!(
        hierarchy.direct_supertypes("Other.php#Other\\Unimported"),
        vec![unresolved(Extends, "Base", "Other\\Base")]
    );
    assert_eq!(
        hierarchy.direct_supertypes("Lib.php#Lib\\Both"),
        vec![resolved(Extends, "I1", I1), resolved(Extends, "I2", I2)]
    );
    assert_eq!(
        hierarchy.direct_supertypes("Lib.php#Lib\\Kind"),
        vec![resolved(Implements, "I1", I1)]
    );
    assert!(hierarchy.direct_supertypes(BASE).is_empty());
    assert!(hierarchy.direct_supertypes("no-such-id").is_empty());
    // The free function reads the same facts from the store.
    let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
    assert_eq!(
        rivet_index::direct_supertypes(&store, "Lib.php#Lib\\Leaf").expect("direct"),
        hierarchy.direct_supertypes("Lib.php#Lib\\Leaf")
    );
}

#[test]
fn ancestors_are_transitive_cycle_safe_and_keep_vendor_names() {
    let temp = repo("ancestors", &FILES);
    ok(temp.path(), &["index", "--json"]);
    let hierarchy = hierarchy(temp.path());
    use SupertypeRelation::{Extends, Implements};
    // Leaf -> Mid -> Base; Leaf -> Both -> I1, I2; Mid -> I2 -> I1; plus the
    // vendor interface, which is not traversed further. Each ancestor is
    // listed once, with the relation and spelling of the first edge that
    // reached it (shallowest level, then sorted order): `I2` is first reached
    // from `Both`'s `extends` list.
    assert_eq!(
        hierarchy.ancestors("Lib.php#Lib\\Leaf"),
        vec![
            resolved(Extends, "Base", BASE),
            resolved(Extends, "I2", I2),
            resolved(Extends, "Mid", MID),
            resolved(Implements, "Both", BOTH),
            resolved(Implements, "I1", I1),
            unresolved(Implements, "\\Vendor\\Contract", "Vendor\\Contract"),
        ]
    );
    // `CycA extends CycB`, `CycB extends CycA`: terminates, and the class
    // itself is never its own listed ancestor.
    assert_eq!(
        hierarchy.ancestors("Lib.php#Lib\\CycA"),
        vec![resolved(Extends, "CycB", "Lib.php#Lib\\CycB")]
    );
    assert_eq!(
        hierarchy.ancestors("Lib.php#Lib\\CycB"),
        vec![resolved(Extends, "CycA", "Lib.php#Lib\\CycA")]
    );
    assert_eq!(
        hierarchy.ancestors("App.php#App\\FromVendor"),
        vec![unresolved(Extends, "Thing", "Vendor\\Thing")]
    );
    let store = Store::open(&temp.path().join(".rivet")).expect("open committed store");
    assert_eq!(
        rivet_index::ancestors(&store, "Lib.php#Lib\\Leaf").expect("ancestors"),
        hierarchy.ancestors("Lib.php#Lib\\Leaf")
    );
}

#[test]
fn hierarchy_and_output_are_deterministic_across_refreshes_and_orders() {
    let forward = repo("order-forward", &FILES);
    let mut reversed_files = FILES;
    reversed_files.reverse();
    let reversed = repo("order-reversed", &reversed_files);

    let commands: [&[&str]; 4] = [
        &["index", "--json"],
        &["refs", "Lib\\Base", "--json"],
        &["refs", "Lib\\I1", "--json"],
        &["refs", "App\\Factory::make", "--json"],
    ];
    let first: Vec<Vec<u8>> = commands
        .iter()
        .map(|args| ok(forward.path(), args))
        .collect();
    // A forced rebuild reparses every file and must publish identical facts.
    ok(forward.path(), &["index", "--json", "--force"]);
    for (args, expected) in commands.iter().skip(1).zip(first.iter().skip(1)) {
        assert_eq!(
            &ok(forward.path(), args),
            expected,
            "{args:?} after --force"
        );
    }
    for (args, expected) in commands.iter().zip(&first) {
        assert_eq!(
            &ok(reversed.path(), args),
            expected,
            "{args:?} in a repo written in reverse order"
        );
    }
    assert_eq!(hierarchy(forward.path()), hierarchy(reversed.path()));

    // The hierarchy does not depend on the order of its input rows.
    let store = Store::open(&forward.path().join(".rivet")).expect("open committed store");
    let symbols = store.list_symbols().expect("symbols");
    let uses = store.list_uses().expect("uses");
    let scopes = store.list_scopes().expect("scopes");
    let bindings = store.list_bindings().expect("bindings");
    fn rev_rows<T: Clone>(rows: &[T]) -> Vec<T> {
        rows.iter().rev().cloned().collect()
    }
    assert_eq!(
        Hierarchy::from_rows(
            &rev_rows(&symbols),
            &rev_rows(&uses),
            &rev_rows(&scopes),
            &rev_rows(&bindings)
        ),
        Hierarchy::from_rows(&symbols, &uses, &scopes, &bindings)
    );
}
