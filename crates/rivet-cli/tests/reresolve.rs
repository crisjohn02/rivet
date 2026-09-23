//! T22 integration tests: cross-file invalidation and re-resolution.
//!
//! `bindings.rs` covers the resolution *rules* on the authored fixture; every
//! edit there touches the file that declares the use. T22's contract is the
//! cross-file case: a change to a *different* file (a new duplicate
//! declaration, a changed import, or a deleted target) must update the binding
//! of an unchanged file's use while the use row survives with its `use_id`.
//! These tests live in a new `reresolve.rs` rather than extending `bindings.rs`
//! because they drive index history across refreshes, not one extraction.
//!
//! The final test is the T21 invalidation regression: a snapshot written in the
//! pre-T21 fact shape must be reparsed when the extractor fingerprint's
//! fact-schema component differs, so a conditionally assigned `new` receiver
//! cannot inherit a stale unconditional meaning.
//!
//! PHP `use` aliases are file-scoped (the parser records them into the use's own
//! file scope), so a use in file A can never be resolved by an import written in
//! a different file. Scenario (b) therefore proves the nearest real cross-file
//! guarantee: changing an import in file A moves A's own bindings while
//! unrelated bindings in untouched file B stay byte-identical.

#![cfg(feature = "lang-php")]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_languages::EXTRACTOR_FINGERPRINT;
use rivet_store::{BindingRow, InventoryInput, Store, UseRow};
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
            "rivet-reresolve-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root with no `.rivet/` yet.
fn git_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// Writes one PHP file under `root`.
fn write_php(root: &Path, name: &str, source: &str) {
    fs::write(root.join(name), source.as_bytes()).expect("write PHP fixture");
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs the binary with extra environment variables.
fn run_with_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(RIVET);
    command.args(args).current_dir(dir);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run the rivet binary")
}

/// Parses a success object, requiring exit 0 and empty stderr.
fn parse_success(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "success must not write stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

/// Runs `index --json`, requiring a clean exit.
fn index_json(dir: &Path) -> Value {
    let output = run(dir, &["index", "--json"]);
    parse_success(&output)
}

/// Opens the committed cache created by a successful `index`.
fn open_store(root: &Path) -> Store {
    Store::open(&root.join(".rivet")).expect("open committed store")
}

/// Every persisted use of one file, in store order.
fn uses_for_file(store: &Store, file: &str) -> Vec<UseRow> {
    store.list_uses_for_file(file).expect("list uses")
}

/// The use IDs of one file, in store order.
fn use_ids(store: &Store, file: &str) -> Vec<i64> {
    uses_for_file(store, file)
        .iter()
        .filter_map(|row| row.use_id)
        .collect()
}

/// The binding for `use_row`, if any.
fn binding_for<'a>(bindings: &'a [BindingRow], use_row: &UseRow) -> Option<&'a BindingRow> {
    let use_id = use_row.use_id.expect("persisted uses carry an ID");
    bindings.iter().find(|binding| binding.use_id == use_id)
}

/// The non-receiver `call` use of `spelling` in `file`.
fn call_use<'a>(uses: &'a [UseRow], file: &str, spelling: &str) -> &'a UseRow {
    uses.iter()
        .find(|row| {
            row.file == file
                && row.ref_kind.as_str() == "call"
                && row.spelling == spelling
                && row.receiver.is_none()
        })
        .unwrap_or_else(|| panic!("missing {file} {spelling} call"))
}

/// The `call` use of `spelling` on `receiver` in `file`.
fn receiver_call<'a>(uses: &'a [UseRow], file: &str, spelling: &str, receiver: &str) -> &'a UseRow {
    uses.iter()
        .find(|row| {
            row.file == file
                && row.ref_kind.as_str() == "call"
                && row.spelling == spelling
                && row.receiver.as_deref() == Some(receiver)
        })
        .unwrap_or_else(|| panic!("missing {file} {spelling} call on {receiver}"))
}

/// Asserts a no-edit re-index reproduces byte-identical binding rows.
///
/// This also re-affirms the T22 requirement that the same content yields the
/// same bindings regardless of how many times it is re-indexed.
fn assert_deterministic(root: &Path) {
    let _ = index_json(root);
    let first = open_store(root).list_bindings().expect("list bindings");
    let _ = index_json(root);
    let second = open_store(root).list_bindings().expect("list bindings");
    assert_eq!(
        first, second,
        "a no-edit re-index must keep identical bindings"
    );
}

/// Asserts no receiver-based call ever claims `exact`.
fn assert_no_receiver_call_is_exact(store: &Store) {
    let bindings = store.list_bindings().expect("list bindings");
    for file in store.list_files().expect("list files") {
        for row in uses_for_file(store, &file.path) {
            if row.ref_kind.as_str() != "call" || row.receiver.is_none() {
                continue;
            }
            if let Some(binding) = binding_for(&bindings, &row) {
                assert_ne!(
                    binding.resolution.as_str(),
                    "exact",
                    "a receiver call must never be exact: {row:?}"
                );
            }
        }
    }
}

/// The full binding set in a form independent of SQLite `use_id`s.
///
/// Each binding is mapped back to its use's `(file, start, end, ref_kind)` and
/// paired with its `(target_id, resolution)`, then sorted. Two stores with the
/// same content must produce equal vectors even when refresh history assigned
/// different surrogate `use_id`s.
fn normalized_bindings(store: &Store) -> Vec<(String, u32, u32, String, String, String)> {
    let mut contexts: HashMap<i64, (String, u32, u32, String)> = HashMap::new();
    for file in store.list_files().expect("list files") {
        for row in uses_for_file(store, &file.path) {
            if let Some(id) = row.use_id {
                contexts.insert(
                    id,
                    (
                        row.file,
                        row.start_byte,
                        row.end_byte,
                        row.ref_kind.as_str().to_string(),
                    ),
                );
            }
        }
    }
    let mut rows: Vec<_> = store
        .list_bindings()
        .expect("list bindings")
        .into_iter()
        .map(|binding| {
            let (file, start, end, kind) =
                contexts.get(&binding.use_id).cloned().unwrap_or_default();
            (
                file,
                start,
                end,
                kind,
                binding.target_id,
                binding.resolution.as_str().to_string(),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// The pre-T22 extractor fingerprint: the current value with the fact-schema
/// component removed. Under the T22 fix this differs from the current
/// fingerprint, so a snapshot stored with it must be reparsed. Under the
/// pre-T22 code the marker is absent, so this returns the current value and the
/// regression test below fails (the stale snapshot is reused).
fn pre_schema_extractor_fingerprint() -> String {
    EXTRACTOR_FINGERPRINT
        .split(";fact-schema")
        .next()
        .expect("a non-empty extractor fingerprint")
        .to_string()
}

/// Strips the T21 fields from a persisted scope's `facts_json`, producing the
/// pre-T21 shape (`new_bindings` entries without `direct_new` and `block`).
fn strip_new_binding_fields(facts_json: &str) -> String {
    let mut value: Value = serde_json::from_str(facts_json).expect("scope facts JSON");
    if let Some(bindings) = value.get_mut("new_bindings").and_then(Value::as_array_mut) {
        for binding in bindings {
            if let Some(object) = binding.as_object_mut() {
                object.remove("direct_new");
                object.remove("block");
            }
        }
    }
    value.to_string()
}

/// Strips `use_block` from a persisted `new_expr` hint, producing the pre-T21
/// hint shape.
fn strip_use_block(hint_json: &str) -> String {
    let mut value: Value = serde_json::from_str(hint_json).expect("use hint JSON");
    if value.get("kind").and_then(Value::as_str) == Some("new_expr")
        && let Some(object) = value.as_object_mut()
    {
        object.remove("use_block");
    }
    value.to_string()
}

#[test]
fn duplicate_declaration_in_a_new_file_invalidates_an_exact_binding() {
    let temp = git_repo("duplicate");
    let root = temp.path();
    write_php(
        root,
        "Target.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\nfinal class Widget\n{\n    public function launch(): void\n    {\n    }\n}\n",
    );
    write_php(
        root,
        "Use.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nuse A\\Widget;\nfinal class Runner\n{\n    public function run(Widget $w): void\n    {\n        launch();\n        $w->launch();\n    }\n}\n",
    );

    let _ = index_json(root);
    let store = open_store(root);
    let uses_before = uses_for_file(&store, "Use.php");
    let call_id = call_use(&uses_before, "Use.php", "launch")
        .use_id
        .expect("persisted use id");
    let receiver_id = receiver_call(&uses_before, "Use.php", "launch", "$w")
        .use_id
        .expect("persisted receiver use id");
    let ids_before = use_ids(&store, "Use.php");
    let bindings = store.list_bindings().expect("list bindings");
    let call_before = uses_before
        .iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("call use");
    let binding = binding_for(&bindings, call_before).expect("a unique target must bind");
    assert_eq!(binding.target_id, "Target.php#A\\launch");
    assert_eq!(binding.resolution.as_str(), "exact");

    // The typed receiver call is `scoped` before the duplicate appears.
    let receiver_before = uses_before
        .iter()
        .find(|row| row.use_id == Some(receiver_id))
        .expect("receiver use");
    let receiver_binding =
        binding_for(&bindings, receiver_before).expect("the typed receiver binds");
    assert_eq!(receiver_binding.target_id, "Target.php#A\\Widget::launch");
    assert_eq!(receiver_binding.resolution.as_str(), "scoped");
    drop(store);
    assert_deterministic(root);

    // A new file introduces a second `A\launch`, so the candidate set is no
    // longer unique and the untouched file's exact binding must disappear. The
    // unrelated receiver binding must not be weakened or upgraded.
    write_php(
        root,
        "Dup.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    let value = index_json(root);
    let store = open_store(root);
    assert_eq!(
        use_ids(&store, "Use.php"),
        ids_before,
        "an untouched file's use_ids must not be reassigned: {value}"
    );
    let uses_after = uses_for_file(&store, "Use.php");
    let call_after = uses_after
        .iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("the call use must survive the duplicate");
    let bindings = store.list_bindings().expect("list bindings");
    assert!(
        binding_for(&bindings, call_after).is_none(),
        "a duplicate declaration must remove the exact binding: {value}"
    );
    let receiver_after = uses_after
        .iter()
        .find(|row| row.use_id == Some(receiver_id))
        .expect("the receiver use must survive the duplicate");
    let receiver_binding =
        binding_for(&bindings, receiver_after).expect("the receiver binding survives");
    assert_eq!(receiver_binding.resolution.as_str(), "scoped");
    assert_no_receiver_call_is_exact(&store);
    drop(store);
    assert_deterministic(root);

    // Removing the duplicate makes the target unique again.
    fs::remove_file(root.join("Dup.php")).expect("remove Dup.php");
    let _ = index_json(root);
    let store = open_store(root);
    assert_eq!(
        use_ids(&store, "Use.php"),
        ids_before,
        "restoring uniqueness must not reassign use_ids"
    );
    let uses_restored = uses_for_file(&store, "Use.php");
    let call = uses_restored
        .iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("the call use survives");
    let bindings = store.list_bindings().expect("list bindings");
    let binding =
        binding_for(&bindings, call).expect("removing the duplicate restores the binding");
    assert_eq!(binding.target_id, "Target.php#A\\launch");
    assert_eq!(binding.resolution.as_str(), "exact");
    assert_no_receiver_call_is_exact(&store);
    drop(store);
    assert_deterministic(root);
}

#[test]
fn changed_import_in_one_file_leaves_another_files_bindings_identical() {
    let temp = git_repo("import-change");
    let root = temp.path();
    write_php(
        root,
        "Target.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    write_php(
        root,
        "Alt.php",
        "<?php\ndeclare(strict_types=1);\nnamespace X;\nfunction launch(): void {}\n",
    );
    write_php(
        root,
        "Other.php",
        "<?php\ndeclare(strict_types=1);\nnamespace B;\nfunction other(): void {}\n",
    );
    write_php(
        root,
        "A.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nlaunch();\n",
    );
    write_php(
        root,
        "B.php",
        "<?php\ndeclare(strict_types=1);\nnamespace D;\nuse function B\\other;\nother();\n",
    );

    let _ = index_json(root);
    let store = open_store(root);
    let b_ids = use_ids(&store, "B.php");
    let b_bindings_before: Vec<BindingRow> = store
        .list_bindings()
        .expect("list bindings")
        .into_iter()
        .filter(|binding| b_ids.contains(&binding.use_id))
        .collect();
    let a_uses = uses_for_file(&store, "A.php");
    let a_call = call_use(&a_uses, "A.php", "launch");
    let bindings = store.list_bindings().expect("list bindings");
    let binding = binding_for(&bindings, a_call).expect("A's call binds through its import");
    assert_eq!(binding.target_id, "Target.php#A\\launch");
    drop(store);
    assert_deterministic(root);

    // Changing A's own import moves A's call to the other file's declaration.
    // B's file-scoped import and call are untouched.
    write_php(
        root,
        "A.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function X\\launch;\nlaunch();\n",
    );
    let _ = index_json(root);
    let store = open_store(root);
    let a_uses = uses_for_file(&store, "A.php");
    let a_call = call_use(&a_uses, "A.php", "launch");
    let bindings = store.list_bindings().expect("list bindings");
    let binding =
        binding_for(&bindings, a_call).expect("A's call rebinds through the changed import");
    assert_eq!(binding.target_id, "Alt.php#X\\launch");
    assert_eq!(
        use_ids(&store, "B.php"),
        b_ids,
        "leaving B.php untouched must not reassign its use_ids"
    );
    let b_bindings_after: Vec<BindingRow> = store
        .list_bindings()
        .expect("list bindings")
        .into_iter()
        .filter(|binding| b_ids.contains(&binding.use_id))
        .collect();
    assert_eq!(
        b_bindings_after, b_bindings_before,
        "unrelated bindings in an untouched file must stay byte-identical"
    );
    assert_no_receiver_call_is_exact(&store);
    drop(store);
    assert_deterministic(root);
}

#[test]
fn deleting_a_target_file_unbinds_an_untouched_use_and_restoring_rebinds() {
    let temp = git_repo("deleted-target");
    let root = temp.path();
    write_php(
        root,
        "Target.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    write_php(
        root,
        "Use.php",
        "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nfunction caller(): void\n{\n    launch();\n}\n",
    );

    let _ = index_json(root);
    let store = open_store(root);
    let call_id = call_use(&uses_for_file(&store, "Use.php"), "Use.php", "launch")
        .use_id
        .expect("persisted use id");
    let ids_before = use_ids(&store, "Use.php");
    let call = uses_for_file(&store, "Use.php")
        .into_iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("call use");
    let bindings = store.list_bindings().expect("list bindings");
    let binding = binding_for(&bindings, &call).expect("the target binds");
    assert_eq!(binding.target_id, "Target.php#A\\launch");
    drop(store);
    assert_deterministic(root);

    // Deleting the target file must not delete the use or error the refresh.
    fs::remove_file(root.join("Target.php")).expect("delete Target.php");
    let value = index_json(root);
    let store = open_store(root);
    assert_eq!(
        use_ids(&store, "Use.php"),
        ids_before,
        "the use row must survive a deleted target with its use_id: {value}"
    );
    let call_after = uses_for_file(&store, "Use.php")
        .into_iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("the call use survives the deleted target");
    let bindings = store.list_bindings().expect("list bindings");
    assert!(
        binding_for(&bindings, &call_after).is_none(),
        "a deleted target must leave the use unresolved, not bound: {value}"
    );
    drop(store);
    assert_deterministic(root);

    // The query path answers the untouched file rather than erroring. (`refs`
    // would be the direct "use is unresolved" surface, but it is T23; the
    // declaration query is the available path.)
    let caller = parse_success(&run(root, &["symbol", "C\\caller", "--json"]));
    assert_eq!(caller["symbol"]["qualified_name"], "C\\caller");

    // Restoring the target file rebinds the untouched use.
    write_php(
        root,
        "Target.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    let _ = index_json(root);
    let store = open_store(root);
    assert_eq!(use_ids(&store, "Use.php"), ids_before);
    let call_restored = uses_for_file(&store, "Use.php")
        .into_iter()
        .find(|row| row.use_id == Some(call_id))
        .expect("the call use survives the restore");
    let bindings = store.list_bindings().expect("list bindings");
    let binding = binding_for(&bindings, &call_restored).expect("restoring the target rebinds");
    assert_eq!(binding.target_id, "Target.php#A\\launch");
    assert_eq!(binding.resolution.as_str(), "exact");
    assert_no_receiver_call_is_exact(&store);
    drop(store);
    assert_deterministic(root);
}

/// The T21 invalidation regression: a snapshot holding the pre-T21 fact shape
/// under the pre-T21 fingerprint must be reparsed, so a `new` assignment inside
/// an `if` body does not leak a `scoped` binding to a use outside it. The
/// resulting binding set must equal a from-scratch index of identical content.
#[test]
fn old_fact_schema_snapshot_is_reparsed_and_conditional_stays_unbound() {
    let a_php = "<?php\ndeclare(strict_types=1);\nnamespace A;\nfinal class Svc\n{\n    public function go(): void\n    {\n    }\n}\n";
    let conditional_php =
        "<?php\ndeclare(strict_types=1);\nif ($c) {\n    $s = new \\A\\Svc();\n}\n$s->go();\n";

    let temp = git_repo("fact-schema");
    let root = temp.path();
    write_php(root, "A.php", a_php);
    write_php(root, "Conditional.php", conditional_php);

    // Build a correct snapshot first, then rewrite the Conditional.php facts
    // into the pre-T21 shape under the pre-T21 fingerprint with no bindings.
    let _ = index_json(root);
    {
        let mut store = open_store(root);
        let files = store.list_files().expect("list files");
        let symbols = store.list_symbols().expect("list symbols");
        let mut uses = Vec::new();
        let mut scopes = Vec::new();
        for file in &files {
            let mut file_uses = store.list_uses_for_file(&file.path).expect("list uses");
            if file.path == "Conditional.php" {
                for row in &mut file_uses {
                    row.hint_json = strip_use_block(&row.hint_json);
                }
            }
            uses.extend(file_uses);
            let mut file_scopes = store.list_scopes_for_file(&file.path).expect("list scopes");
            if file.path == "Conditional.php" {
                for row in &mut file_scopes {
                    row.facts_json = strip_new_binding_fields(&row.facts_json);
                }
            }
            scopes.extend(file_scopes);
        }
        let effective_config = store
            .get_meta("effective_config_fingerprint")
            .expect("read meta")
            .expect("effective-config fingerprint present");
        // Keep this build's resolver fingerprint so only the fact schema is
        // stale; a literal here would go stale on every resolver bump.
        let resolver = store
            .get_meta("resolver_fingerprint")
            .expect("read meta")
            .expect("resolver fingerprint present");
        store
            .publish_inventory(InventoryInput {
                fingerprint: rivet_store::Fingerprint {
                    index_format_version: rivet_store::INDEX_FORMAT_VERSION.to_string(),
                    effective_config,
                    extractor: pre_schema_extractor_fingerprint(),
                    resolver,
                },
                files,
                symbols,
                uses,
                scopes,
                bindings: Vec::new(),
                receiver_classes: Vec::new(),
                force: false,
                regenerated: Vec::new(),
            })
            .expect("publish a pre-T21-shape snapshot");
    }

    // The schema mismatch must reparse every enabled-language file.
    let log_dir = TempDir::new("fact-schema-log");
    let log = log_dir.path().join("reparsed.txt");
    fs::write(&log, b"").expect("create reparse log");
    let output = run_with_env(
        root,
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log.as_path())],
    );
    let _ = parse_success(&output);
    let store = open_store(root);
    let go = uses_for_file(&store, "Conditional.php")
        .into_iter()
        .find(|row| row.spelling == "go")
        .expect("Conditional.php has a go call");
    let bindings = store.list_bindings().expect("list bindings");
    assert!(
        binding_for(&bindings, &go).is_none(),
        "a conditional `new` assignment must stay unbound after a schema invalidation"
    );
    assert_eq!(
        store
            .get_meta("extractor_fingerprint")
            .expect("read meta")
            .as_deref(),
        Some(EXTRACTOR_FINGERPRINT),
        "the refresh must commit the current fact-schema fingerprint"
    );
    let stale = normalized_bindings(&store);
    drop(store);
    assert_deterministic(root);

    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(
        lines.contains(&"Conditional.php".to_string()),
        "a stale fact-schema fingerprint must reparse stored facts: {lines:?}"
    );

    // The same bytes in a fresh repository index to exactly the same bindings.
    let fresh = git_repo("fact-schema-fresh");
    write_php(fresh.path(), "A.php", a_php);
    write_php(fresh.path(), "Conditional.php", conditional_php);
    let _ = index_json(fresh.path());
    let fresh_store = open_store(fresh.path());
    assert_eq!(
        stale,
        normalized_bindings(&fresh_store),
        "index history must not change the binding set"
    );
    assert_no_receiver_call_is_exact(&fresh_store);
}
