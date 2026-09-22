//! PF1 integration tests: what a refresh writes.
//!
//! A refresh that changes nothing must write no row and re-resolve nothing
//! (ARCHITECTURE "Refresh and invalidation": facts are replaced only for
//! changed files, and bindings are cleared and recomputed only "if content,
//! membership, or resolver fingerprint changed"). When something does change,
//! every persisted use is still re-resolved (T22), so a change in one file can
//! move a binding in a file that was not rewritten.
//!
//! Writes are counted without timing: SQLite's `total_changes` over the
//! refresh's own connection, and temporary triggers on that connection that
//! log every inserted, updated, or deleted fact row with its owning file.

#![cfg(feature = "lang-php")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_cli::refresh::{RefreshMode, refresh};
use rivet_core::Config;
use rivet_store::Store;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named temporary Git root removed when dropped.
struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(label: &str) -> TempRepo {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "rivet-refresh-writes-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(path.join(".git")).expect("create .git");
        fs::create_dir_all(path.join(".rivet")).expect("create .rivet");
        TempRepo { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, name: &str, source: &str) {
        fs::write(self.path.join(name), source.as_bytes()).expect("write fixture file");
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const TARGET: &str = "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\nfinal class Widget\n{\n    public function launch(): void\n    {\n    }\n}\n";
const USER: &str = "<?php\ndeclare(strict_types=1);\nnamespace C;\nuse function A\\launch;\nuse A\\Widget;\nfinal class Runner\n{\n    public function run(Widget $w): void\n    {\n        launch();\n        $w->launch();\n    }\n}\n";
const HELPER: &str = "<?php\nnamespace B;\nfunction helper(int $n): int { return $n + 1; }\nfunction twice(int $n): int { return helper(helper($n)); }\n";
const SERVICE: &str = "<?php\nnamespace B;\nclass Service\n{\n    public function go(): int { return twice(2); }\n}\n";
const BROKEN: &str = "<?php\nfunction broken( {\n";

/// A repository of several PHP files, one unsupported file, and one file that
/// fails to parse (and is therefore reparsed on every refresh).
fn several_files(label: &str) -> TempRepo {
    let repo = TempRepo::new(label);
    repo.write("Target.php", TARGET);
    repo.write("Use.php", USER);
    repo.write("Helper.php", HELPER);
    repo.write("Service.php", SERVICE);
    repo.write("Broken.php", BROKEN);
    repo.write("README.md", "# notes\n");
    repo
}

fn open(repo: &TempRepo) -> Store {
    Store::open(&repo.path().join(".rivet")).expect("open store")
}

fn config(repo: &TempRepo) -> Config {
    Config::load(repo.path()).expect("load config")
}

/// Runs one refresh and returns `(snapshot digest, rows changed by it)`.
fn refresh_counting(repo: &TempRepo, store: &mut Store, mode: RefreshMode) -> (String, u64) {
    let before = store.total_changes();
    refresh(repo.path(), &config(repo), store, mode, false).expect("refresh");
    let changes = store.total_changes() - before;
    let digest = store
        .get_meta("snapshot_digest")
        .expect("read digest")
        .expect("a committed digest");
    (digest, changes)
}

/// Every stored fact, normalized so it compares across refreshes: symbols and
/// scopes as stored, uses with their IDs, and bindings keyed by use ID.
fn facts_dump(store: &Store) -> String {
    let mut out = String::new();
    for symbol in store.list_symbols().expect("symbols") {
        out.push_str(&format!("{symbol:?}\n"));
    }
    for row in store.list_uses().expect("uses") {
        out.push_str(&format!("{row:?}\n"));
    }
    for row in store.list_scopes().expect("scopes") {
        out.push_str(&format!("{row:?}\n"));
    }
    for binding in store.list_bindings().expect("bindings") {
        out.push_str(&format!("{binding:?}\n"));
    }
    for file in store.list_files().expect("files") {
        out.push_str(&format!(
            "{} {:?} {:?} {:?}\n",
            file.path, file.language, file.content_hash, file.parse_status
        ));
    }
    out
}

/// Installs temporary triggers on the store's connection that log each fact
/// row written, as `(table, owning file, operation)`, into `temp.write_log`.
/// Binding rows carry no file, so they log the bound use's ID instead.
fn install_write_log(store: &Store) {
    let mut sql = String::from("CREATE TEMP TABLE write_log (tbl TEXT, file TEXT, op TEXT);\n");
    for table in ["files", "symbols", "uses", "scopes"] {
        let column = if table == "files" { "path" } else { "file" };
        for (op, row) in [("insert", "NEW"), ("update", "NEW"), ("delete", "OLD")] {
            sql.push_str(&format!(
                "CREATE TEMP TRIGGER log_{table}_{op} AFTER {} ON main.{table} BEGIN \
                 INSERT INTO write_log VALUES ('{table}', {row}.{column}, '{op}'); END;\n",
                op.to_uppercase()
            ));
        }
    }
    for (op, row) in [("insert", "NEW"), ("update", "NEW"), ("delete", "OLD")] {
        sql.push_str(&format!(
            "CREATE TEMP TRIGGER log_bindings_{op} AFTER {} ON main.bindings BEGIN \
             INSERT INTO write_log VALUES ('bindings', {row}.use_id, '{op}'); END;\n",
            op.to_uppercase()
        ));
    }
    sql.push_str(
        "CREATE TEMP TRIGGER log_meta_update AFTER UPDATE ON main.meta BEGIN \
         INSERT INTO write_log VALUES ('meta', NEW.key, 'update'); END;\n\
         CREATE TEMP TRIGGER log_meta_insert AFTER INSERT ON main.meta BEGIN \
         INSERT INTO write_log VALUES ('meta', NEW.key, 'insert'); END;\n",
    );
    store
        .connection()
        .execute_batch(&sql)
        .expect("install write log");
}

/// Drains `temp.write_log` in a deterministic order.
fn take_write_log(store: &Store) -> Vec<(String, String, String)> {
    let conn = store.connection();
    let rows = {
        let mut stmt = conn
            .prepare("SELECT tbl, file, op FROM write_log ORDER BY tbl, file, op")
            .expect("prepare log read");
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("read log")
            .collect::<Result<Vec<(String, String, String)>, _>>()
            .expect("log rows")
    };
    conn.execute_batch("DELETE FROM write_log")
        .expect("clear log");
    rows
}

#[test]
fn a_no_change_refresh_writes_no_row_in_either_mode() {
    let repo = several_files("nochange");
    let mut store = open(&repo);
    let (first, first_changes) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    assert!(first_changes > 0, "the first index writes the inventory");
    let dump = facts_dump(&store);

    for mode in [
        RefreshMode::Content,
        RefreshMode::Metadata,
        RefreshMode::Content,
    ] {
        let (digest, changes) = refresh_counting(&repo, &mut store, mode);
        assert_eq!(
            changes, 0,
            "a {mode:?} refresh with nothing changed must modify no row"
        );
        assert_eq!(digest, first, "the digest of a no-change refresh is stable");
        assert_eq!(facts_dump(&store), dump, "no stored fact may move");
    }

    // A second process (a fresh connection) sees the same: zero rows changed.
    drop(store);
    let mut reopened = open(&repo);
    let (digest, changes) = refresh_counting(&repo, &mut reopened, RefreshMode::Content);
    assert_eq!(changes, 0);
    assert_eq!(digest, first);
}

#[test]
fn an_mtime_only_touch_updates_only_that_files_metadata() {
    let repo = several_files("touch");
    let mut store = open(&repo);
    let (first, _) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let dump = facts_dump(&store);

    // Rewrite identical bytes after a pause long enough to move the mtime on
    // any filesystem's timestamp granularity.
    let path = repo.path().join("Helper.php");
    let before = fs::metadata(&path)
        .expect("stat")
        .modified()
        .expect("mtime");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open for touch");
    file.set_modified(before + std::time::Duration::from_secs(5))
        .expect("set mtime");
    drop(file);

    install_write_log(&store);
    let (digest, changes) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let log = take_write_log(&store);
    assert_eq!(
        log,
        vec![(
            "files".to_string(),
            "Helper.php".to_string(),
            "update".to_string()
        )],
        "only the touched file's row is updated; no fact, binding, or meta row"
    );
    // The logged row plus the log insert itself.
    assert_eq!(changes, 2);
    assert_eq!(digest, first, "an mtime-only change keeps the digest");
    assert_eq!(facts_dump(&store), dump);
}

#[test]
fn a_one_file_edit_rewrites_only_that_files_facts_and_the_bindings() {
    let repo = several_files("edit");
    let mut store = open(&repo);
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let use_ids_before: Vec<(String, Option<i64>)> = store
        .list_uses()
        .expect("uses")
        .into_iter()
        .map(|row| (row.file, row.use_id))
        .collect();

    repo.write(
        "Helper.php",
        "<?php\nnamespace B;\nfunction helper(int $n): int { return $n + 2; }\nfunction twice(int $n): int { return helper(helper($n)); }\n",
    );
    install_write_log(&store);
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let log = take_write_log(&store);

    let mut other_bindings = 0;
    for (table, file, op) in &log {
        match table.as_str() {
            // Every binding is cleared and re-resolved (T22).
            "bindings" => {
                assert!(op == "insert" || op == "delete", "binding {op}");
                other_bindings += 1;
            }
            // The digest moved, so meta records it.
            "meta" => assert_eq!(file, "snapshot_digest", "only the digest changes"),
            _ => assert_eq!(
                file, "Helper.php",
                "{table} {op} touched a file whose content did not change"
            ),
        }
    }
    assert!(other_bindings > 0, "the edit re-resolves the bindings");
    for table in ["files", "symbols", "uses", "scopes"] {
        assert!(
            log.iter().any(|(t, f, _)| t == table && f == "Helper.php"),
            "the edited file's {table} rows are rewritten"
        );
    }

    // Unchanged files keep their use rows, IDs included.
    let use_ids_after: Vec<(String, Option<i64>)> = store
        .list_uses()
        .expect("uses")
        .into_iter()
        .map(|row| (row.file, row.use_id))
        .collect();
    let keep = |ids: &[(String, Option<i64>)]| -> Vec<(String, Option<i64>)> {
        ids.iter()
            .filter(|(file, _)| file != "Helper.php")
            .cloned()
            .collect()
    };
    assert_eq!(keep(&use_ids_after), keep(&use_ids_before));
}

#[test]
fn a_competing_declaration_in_a_new_file_moves_a_binding_in_an_unwritten_file() {
    let repo = several_files("crossfile");
    let mut store = open(&repo);
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let use_row = |store: &Store| {
        store
            .list_uses()
            .expect("uses")
            .into_iter()
            .find(|row| row.file == "Use.php" && row.spelling == "launch" && row.receiver.is_none())
            .expect("the imported function call")
    };
    let call = use_row(&store);
    let bound = |store: &Store, use_id: i64| {
        store
            .list_bindings()
            .expect("bindings")
            .into_iter()
            .find(|binding| binding.use_id == use_id)
    };
    let call_id = call.use_id.expect("use id");
    let before = bound(&store, call_id).expect("a unique target binds");
    assert_eq!(before.target_id, "Target.php#A\\launch");
    assert_eq!(before.resolution.as_str(), "exact");

    // A second declaration of the same function in a new file makes the
    // import ambiguous (T22).
    repo.write(
        "Dup.php",
        "<?php\ndeclare(strict_types=1);\nnamespace A;\nfunction launch(): void {}\n",
    );
    install_write_log(&store);
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let log = take_write_log(&store);
    assert!(
        log.iter()
            .all(|(table, file, _)| table == "bindings" || table == "meta" || file == "Dup.php"),
        "only the new file's facts are written: {log:?}"
    );
    assert_eq!(
        use_row(&store),
        call,
        "the untouched use row survives as is"
    );
    assert_eq!(
        bound(&store, call_id),
        None,
        "an ambiguous import must no longer bind"
    );

    // Deleting the duplicate restores the unique binding, again without
    // rewriting the untouched file.
    fs::remove_file(repo.path().join("Dup.php")).expect("remove duplicate");
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let log = take_write_log(&store);
    assert!(
        log.iter()
            .all(|(table, file, _)| table == "bindings" || table == "meta" || file == "Dup.php"),
        "only the deleted file's facts are removed: {log:?}"
    );
    assert_eq!(bound(&store, call_id), Some(before));
}

#[test]
fn a_no_change_refresh_matches_a_fresh_rebuild_of_the_same_tree() {
    let repo = several_files("rebuild");
    let mut store = open(&repo);
    let (first, _) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    // Edit and revert, then refresh with nothing changed.
    repo.write("Service.php", "<?php\nnamespace B;\nclass Service {}\n");
    let _ = refresh_counting(&repo, &mut store, RefreshMode::Content);
    repo.write("Service.php", SERVICE);
    let (reverted, _) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    let (settled, changes) = refresh_counting(&repo, &mut store, RefreshMode::Content);
    assert_eq!(changes, 0);
    assert_eq!(reverted, first);
    assert_eq!(settled, first);
    let bindings = normalized_bindings(&store);
    drop(store);

    // A cold build of the same tree yields the same digest and bindings.
    fs::remove_dir_all(repo.path().join(".rivet")).expect("remove cache");
    fs::create_dir_all(repo.path().join(".rivet")).expect("recreate cache dir");
    let mut cold = open(&repo);
    let (cold_digest, _) = refresh_counting(&repo, &mut cold, RefreshMode::Content);
    assert_eq!(cold_digest, first);
    assert_eq!(normalized_bindings(&cold), bindings);
}

/// Bindings keyed by the use's position rather than its surrogate ID.
fn normalized_bindings(store: &Store) -> Vec<(String, u32, u32, String, String, String)> {
    let uses = store.list_uses().expect("uses");
    let mut out: Vec<_> = store
        .list_bindings()
        .expect("bindings")
        .into_iter()
        .map(|binding| {
            let row = uses
                .iter()
                .find(|row| row.use_id == Some(binding.use_id))
                .expect("bound use exists");
            (
                row.file.clone(),
                row.start_byte,
                row.end_byte,
                row.ref_kind.as_str().to_string(),
                binding.target_id,
                binding.resolution.as_str().to_string(),
            )
        })
        .collect();
    out.sort();
    out
}
