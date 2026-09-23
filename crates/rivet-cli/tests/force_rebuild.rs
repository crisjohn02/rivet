//! Integration tests for AF6: `rivet index --force` genuinely rebuilds.
//!
//! `--force` is the recovery path for a disposable cache whose stored facts are
//! suspect although no fingerprint changed (ARCHITECTURE "Concurrency and
//! source consistency"). It must reread and reparse every eligible file,
//! reusing no stored symbol, use, scope, or binding, and its `updated` count
//! must be true: "all current rows for `--force`" (OUTPUT-CONTRACT
//! "Administrative commands").
//!
//! Every test drives the built binary against a temporary repository. Stored
//! facts are tampered with and dumped directly through SQLite, bypassing the
//! binary, so a `--force` that merely reinserts the stored rows cannot pass.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};

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
            "rivet-af6-{label}-{}-{nanos}-{unique}",
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

/// A temporary Git root holding a byte-for-byte copy of the authored fixture.
fn fixture_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    for entry in fs::read_dir(&source).expect("read authored fixture") {
        let entry = entry.expect("fixture entry");
        if entry.path().is_file() {
            fs::copy(entry.path(), temp.path().join(entry.file_name())).expect("copy fixture file");
        }
    }
    temp
}

/// The authored fixture's eligible PHP files, in path-byte order.
fn fixture_php_files() -> Vec<String> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/php/authored");
    let mut names: Vec<String> = fs::read_dir(&source)
        .expect("read authored fixture")
        .map(|entry| entry.expect("fixture entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".php"))
        .collect();
    names.sort();
    names
}

/// Writes `contents` at `rel` under `root`, creating parent directories.
fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

/// Runs the binary in `dir` with the given extra environment. Debug hooks are
/// never inherited from the environment running the tests.
fn run_with(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(RIVET);
    command
        .args(args)
        .current_dir(dir)
        .env_remove("RIVET_DEBUG_MAX_NODES")
        .env_remove("RIVET_DEBUG_MAX_USES")
        .env_remove("RIVET_DEBUG_REPARSED")
        .env_remove("RIVET_DEBUG_PAUSE_DIR");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run the rivet binary")
}

/// Runs the binary in `dir` with no debug hooks.
fn run(dir: &Path, args: &[&str]) -> Output {
    run_with(dir, args, &[])
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

/// Parses an error object, requiring `exit`, empty stdout, and JSON stderr.
fn parse_error(output: &Output, exit: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "an error must not write stdout");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON object")
}

/// The index database of the repository at `root`.
fn db_path(root: &Path) -> PathBuf {
    root.join(".rivet").join("index.db")
}

/// Opens the index database directly, with foreign keys on so a deleted use
/// cascades to its binding exactly as it would inside rivet.
fn open_db(root: &Path) -> Connection {
    let conn = Connection::open(db_path(root)).expect("open index.db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");
    conn
}

/// Every row of `table` in `order`, each column rendered with its SQLite type,
/// so two dumps are equal only when every stored value is equal.
fn dump_table(conn: &Connection, table: &str, order: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order}"))
        .expect("prepare dump");
    let columns = stmt.column_count();
    let rows = stmt
        .query_map([], |row| {
            let mut fields = Vec::with_capacity(columns);
            for index in 0..columns {
                let value: SqlValue = row.get(index)?;
                fields.push(format!("{value:?}"));
            }
            Ok(fields.join(" | "))
        })
        .expect("query dump");
    rows.collect::<rusqlite::Result<Vec<String>>>()
        .expect("read dump rows")
}

/// The stored facts of one snapshot: symbols, uses, bindings, scopes,
/// diagnostics, and file rows without their mtime. mtime is excluded only
/// because it is not a fact; no test here edits a file.
#[derive(Debug, PartialEq, Eq)]
struct Facts {
    symbols: Vec<String>,
    uses: Vec<String>,
    bindings: Vec<String>,
    scopes: Vec<String>,
    diagnostics: Vec<String>,
    files: Vec<String>,
}

fn dump_facts(root: &Path) -> Facts {
    let conn = open_db(root);
    let files = {
        let mut stmt = conn
            .prepare(
                "SELECT path, language, size, content_hash, source, parse_status
                 FROM files ORDER BY path",
            )
            .expect("prepare files dump");
        stmt.query_map([], |row| {
            let mut fields = Vec::new();
            for index in 0..6 {
                let value: SqlValue = row.get(index)?;
                fields.push(format!("{value:?}"));
            }
            Ok(fields.join(" | "))
        })
        .expect("query files")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("read files")
    };
    Facts {
        symbols: dump_table(&conn, "symbols", "id"),
        uses: dump_table(&conn, "uses", "use_id"),
        bindings: dump_table(&conn, "bindings", "use_id"),
        scopes: dump_table(&conn, "scopes", "file, scope_key"),
        diagnostics: dump_table(&conn, "diagnostics", "rowid"),
        files,
    }
}

/// The lines of the reparse log, or none when the hook wrote nothing.
fn reparsed_lines(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

// ---------------------------------------------------------------------------
// (a) The reparse hook: `--force` reparses every eligible file.
// ---------------------------------------------------------------------------

#[test]
#[cfg(debug_assertions)]
fn force_reparses_every_eligible_file_and_normal_refresh_none() {
    let temp = fixture_repo("hook");
    let root = temp.path();
    let log_dir = TempDir::new("hook-log");
    let log = log_dir.path().join("reparsed.txt");
    let log_env = log.to_str().expect("UTF-8 log path");

    let first = parse_success(&run(root, &["index", "--json"]));
    let files_seen = first["index"]["coverage"]["files_seen"]
        .as_u64()
        .expect("files_seen");

    // A no-change normal refresh reparses nothing and updates nothing.
    let normal = parse_success(&run_with(
        root,
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_env)],
    ));
    assert_eq!(
        reparsed_lines(&log),
        Vec::<String>::new(),
        "a no-change refresh must reuse every file"
    );
    assert_eq!(
        (normal["updated"].clone(), normal["unchanged"].clone()),
        (json!(0), json!(files_seen))
    );

    // `--force` reparses every eligible file exactly once. The README is an
    // ordinary unsupported file: counted, never parsed.
    let forced = parse_success(&run_with(
        root,
        &["index", "--force", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_env)],
    ));
    let mut lines = reparsed_lines(&log);
    lines.sort();
    assert_eq!(lines, fixture_php_files());
    assert_eq!(
        (
            forced["updated"].clone(),
            forced["unchanged"].clone(),
            forced["deleted"].clone()
        ),
        (json!(files_seen), json!(0), json!(0)),
        "`updated` counts all current rows for `--force`"
    );

    // Forcing again still reparses everything: nothing forced is ever reused.
    fs::write(&log, b"").expect("clear reparse log");
    parse_success(&run_with(
        root,
        &["index", "--force", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_env)],
    ));
    let mut again = reparsed_lines(&log);
    again.sort();
    assert_eq!(again, fixture_php_files());
}

// ---------------------------------------------------------------------------
// (b) Recovery: tampered facts survive a normal refresh; `--force` repairs them.
// ---------------------------------------------------------------------------

#[test]
fn force_restores_tampered_facts_that_a_normal_refresh_keeps() {
    let temp = fixture_repo("recover");
    let root = temp.path();

    let fresh_report = parse_success(&run(root, &["index", "--json"]));
    let fresh = dump_facts(root);
    assert!(!fresh.uses.is_empty() && !fresh.bindings.is_empty());
    parse_success(&run(root, &["symbol", "App\\Boot\\launch", "--json"]));

    // Tamper with stored facts without touching any fingerprint, file row, or
    // source byte: rename a symbol and delete one use (its binding cascades).
    let meta_before = dump_table(&open_db(root), "meta", "key");
    {
        let conn = open_db(root);
        let renamed = conn
            .execute(
                "UPDATE symbols
                 SET name = 'tampered', lookup_name = 'tampered',
                     qualified_name = 'App\\Boot\\tampered'
                 WHERE qualified_name = 'App\\Boot\\launch'",
                [],
            )
            .expect("rename symbol");
        assert_eq!(renamed, 1);
        let deleted = conn
            .execute(
                "DELETE FROM uses WHERE use_id =
                     (SELECT MIN(use_id) FROM uses WHERE file = 'ReportService.php')",
                [],
            )
            .expect("delete use");
        assert_eq!(deleted, 1);
    }
    assert_eq!(dump_table(&open_db(root), "meta", "key"), meta_before);
    let tampered = dump_facts(root);
    assert_ne!(tampered.symbols, fresh.symbols);
    assert_eq!(tampered.uses.len() + 1, fresh.uses.len());
    assert_eq!(tampered.files, fresh.files, "no file row was touched");

    // A normal refresh sees unchanged fingerprints and hashes, so it reuses
    // the tampered rows: the tamper survives, proving reuse.
    let normal = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(normal["updated"], 0);
    assert_eq!(normal["uses"], json!(tampered.uses.len()));
    let after_normal = dump_facts(root);
    assert_eq!(after_normal.symbols, tampered.symbols);
    assert_eq!(after_normal.uses, tampered.uses);
    parse_error(&run(root, &["symbol", "App\\Boot\\launch", "--json"]), 4);

    // `--force` rebuilds from the bytes on disk and restores exactly the facts
    // of a fresh index, row for row, including use IDs and bindings.
    let forced = parse_success(&run(root, &["index", "--force", "--json"]));
    assert_eq!(dump_facts(root), fresh);
    assert_eq!(forced["symbols"], fresh_report["symbols"]);
    assert_eq!(forced["uses"], fresh_report["uses"]);
    assert_eq!(forced["bindings"], fresh_report["bindings"]);
    assert_eq!(forced["index"], fresh_report["index"]);
    parse_success(&run(root, &["symbol", "App\\Boot\\launch", "--json"]));
}

#[test]
fn force_restores_a_corrupted_source_blob_and_deleted_scopes() {
    let temp = fixture_repo("recover-row");
    let root = temp.path();
    parse_success(&run(root, &["index", "--json"]));
    let fresh = dump_facts(root);

    // Corrupt a stored source blob and the facts' spans while leaving the
    // content hash intact: a normal refresh trusts the hash and keeps them.
    {
        let conn = open_db(root);
        conn.execute(
            "UPDATE files SET source = X'00' WHERE path = 'SurveyService.php'",
            [],
        )
        .expect("corrupt source");
        conn.execute("DELETE FROM scopes WHERE file = 'SurveyService.php'", [])
            .expect("delete scopes");
    }
    parse_success(&run(root, &["index", "--json"]));
    assert_ne!(dump_facts(root), fresh, "a normal refresh keeps the tamper");

    parse_success(&run(root, &["index", "--force", "--json"]));
    assert_eq!(dump_facts(root), fresh);
}

// ---------------------------------------------------------------------------
// (c) The T32 case: a file indexed under the defaults, then forced under a
//     lower use limit, loses its facts.
// ---------------------------------------------------------------------------

const THREE_USES_PHP: &[u8] = b"<?php\nnamespace App;\nfunction three(): void { f(); g(); h(); }\n";
const GOOD_PHP: &[u8] = b"<?php\nnamespace App;\nfunction good(): void {}\n";

#[test]
#[cfg(debug_assertions)]
fn force_applies_current_resource_limits_to_previously_indexed_files() {
    let temp = git_repo("limits");
    let root = temp.path();
    write(root, "three.php", THREE_USES_PHP);
    write(root, "good.php", GOOD_PHP);

    let default = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(default["index"]["coverage"]["skipped"]["resource_limit"], 0);
    assert_eq!(default["uses"], 3);

    // A normal refresh reuses the unchanged `ok` file, so the lowered limit
    // never sees it: this is the stale case only `--force` can repair.
    let normal = parse_success(&run_with(
        root,
        &["index", "--json"],
        &[("RIVET_DEBUG_MAX_USES", "2")],
    ));
    assert_eq!(normal["index"]["coverage"]["skipped"]["resource_limit"], 0);
    assert_eq!(normal["uses"], 3);

    let forced = parse_success(&run_with(
        root,
        &["index", "--force", "--json"],
        &[("RIVET_DEBUG_MAX_USES", "2")],
    ));
    assert_eq!(forced["index"]["coverage"]["skipped"]["resource_limit"], 1);
    assert_eq!(forced["index"]["coverage"]["files_indexed"], 1);
    assert_eq!(
        forced["index"]["diagnostics"]["items"],
        json!([{
            "file": "three.php",
            "code": "resource_limit",
            "detail": "extracted more than 2 uses",
        }])
    );
    assert_eq!(
        (forced["uses"].clone(), forced["symbols"].clone()),
        (json!(0), json!(2))
    );

    let conn = open_db(root);
    let status: String = conn
        .query_row(
            "SELECT parse_status FROM files WHERE path = 'three.php'",
            [],
            |row| row.get(0),
        )
        .expect("three.php row");
    assert_eq!(status, "resource_limit");
    for table in ["symbols", "uses", "scopes"] {
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE file = 'three.php'"),
                [],
                |row| row.get(0),
            )
            .expect("count facts");
        assert_eq!(count, 0, "{table} must hold no facts for three.php");
    }
    drop(conn);

    // Under the default limits the file is indexed again.
    let restored = parse_success(&run(root, &["index", "--json"]));
    assert_eq!(
        restored["index"]["coverage"]["skipped"]["resource_limit"],
        0
    );
    assert_eq!(restored["uses"], 3);
}

// ---------------------------------------------------------------------------
// (d) A forced digest equals a normal index's digest.
// ---------------------------------------------------------------------------

#[test]
fn forced_digest_equals_normal_digest_on_the_authored_fixture() {
    let normal_repo = fixture_repo("digest-normal");
    let normal = parse_success(&run(normal_repo.path(), &["index", "--json"]));

    // A forced first index, and a forced rebuild over an existing one.
    let forced_repo = fixture_repo("digest-forced");
    let first = parse_success(&run(forced_repo.path(), &["index", "--force", "--json"]));
    let again = parse_success(&run(forced_repo.path(), &["index", "--force", "--json"]));

    for forced in [&first, &again] {
        assert_eq!(forced["index"], normal["index"]);
        for key in ["symbols", "uses", "bindings"] {
            assert_eq!(forced[key], normal[key], "{key}");
        }
    }
    assert_eq!(
        dump_facts(forced_repo.path()),
        dump_facts(normal_repo.path())
    );
}

// ---------------------------------------------------------------------------
// (e) Format and destination handling around `--force`.
// ---------------------------------------------------------------------------

/// ARCHITECTURE "Concurrency and source consistency": never *silently*
/// overwrite a database from a newer unsupported format; "Return exit 3 with
/// an explicit rebuild hint. `index --force` may rebuild that cache after
/// validation of the destination, never user source/config." A plain refresh
/// refuses it untouched and names `--force`; `--force` is the explicit rebuild
/// and leaves source and configuration byte-for-byte unchanged.
#[test]
fn newer_format_is_refused_untouched_without_force_and_rebuilt_by_force() {
    let temp = fixture_repo("newer");
    let root = temp.path();
    write(
        root,
        ".rivet/config.toml",
        b"[index]\nfreshness = \"content\"\n",
    );
    let config_before = fs::read(root.join(".rivet/config.toml")).expect("config");
    let source_before = fs::read(root.join("boot.php")).expect("source");

    let normal = parse_success(&run(root, &["index", "--json"]));
    {
        let conn = open_db(root);
        conn.execute(
            "UPDATE meta SET value = '3' WHERE key = 'index_format_version'",
            [],
        )
        .expect("bump format");
        // Fold the WAL into the main file so the byte comparison is complete.
        conn.pragma_update(None, "journal_mode", "DELETE")
            .expect("checkpoint");
    }
    let db_before = fs::read(db_path(root)).expect("read db");

    for args in [
        &["index", "--json"][..],
        &["symbol", "App\\Boot\\launch", "--json"][..],
        &["symbol", "App\\Boot\\launch", "--no-refresh", "--json"][..],
    ] {
        let error = parse_error(&run(root, args), 3);
        assert_eq!(error["error"], "repository_unavailable", "{args:?}");
        assert_eq!(
            fs::read(db_path(root)).expect("read db"),
            db_before,
            "{args:?} must leave a newer-format database byte-for-byte unchanged"
        );
    }
    let hinted = parse_error(&run(root, &["index", "--json"]), 3);
    assert!(
        hinted.to_string().contains("rivet index --force"),
        "the refusal must carry an explicit rebuild hint: {hinted}"
    );

    let forced = parse_success(&run(root, &["index", "--force", "--json"]));
    assert_eq!(forced["index"], normal["index"]);
    assert_eq!(forced["symbols"], normal["symbols"]);
    let version: String = open_db(root)
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_format_version'",
            [],
            |row| row.get(0),
        )
        .expect("format version");
    assert_eq!(version, "2");
    assert_eq!(
        fs::read(root.join(".rivet/config.toml")).unwrap(),
        config_before
    );
    assert_eq!(fs::read(root.join("boot.php")).unwrap(), source_before);
}

/// `--force` validates the destination first: a symlinked `index.db` is
/// refused with exit 3 and neither link nor target is modified.
#[test]
#[cfg(unix)]
fn force_refuses_a_symlinked_database_without_touching_it() {
    let temp = fixture_repo("symlink");
    let root = temp.path();
    let outside = TempDir::new("symlink-target");
    let target = outside.path().join("elsewhere.db");
    fs::write(&target, b"not a database").expect("write target");
    fs::create_dir_all(root.join(".rivet")).expect("create .rivet");
    std::os::unix::fs::symlink(&target, db_path(root)).expect("symlink index.db");

    let error = parse_error(&run(root, &["index", "--force", "--json"]), 3);
    assert_eq!(error["error"], "repository_unavailable");
    assert_eq!(fs::read(&target).unwrap(), b"not a database");
    assert!(
        fs::symlink_metadata(db_path(root))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// RELEASING "Index format version": "supported older formats rebuild"
/// (LR2). A version-1 cache, which predates `receiver_classes`, is rebuilt by
/// an ordinary refresh with the same answers as a fresh index, while
/// `--no-refresh`, which must not write, refuses it untouched with a hint.
#[test]
fn older_supported_format_is_rebuilt_by_a_normal_refresh() {
    let temp = fixture_repo("older");
    let root = temp.path();
    let normal = parse_success(&run(root, &["refs", "App\\Boot\\launch", "--json"]));
    {
        let conn = open_db(root);
        conn.execute_batch(
            "DROP TABLE receiver_classes; \
             UPDATE meta SET value = '1' WHERE key = 'index_format_version';",
        )
        .expect("downgrade to format 1");
        conn.pragma_update(None, "journal_mode", "DELETE")
            .expect("checkpoint");
    }
    let db_before = fs::read(db_path(root)).expect("read db");

    let error = parse_error(
        &run(
            root,
            &["refs", "App\\Boot\\launch", "--no-refresh", "--json"],
        ),
        3,
    );
    assert_eq!(error["error"], "repository_unavailable");
    assert!(error.to_string().contains("rivet index"), "{error}");
    assert_eq!(
        fs::read(db_path(root)).expect("read db"),
        db_before,
        "--no-refresh must leave an older-format database unchanged"
    );

    let rebuilt = parse_success(&run(root, &["refs", "App\\Boot\\launch", "--json"]));
    assert_eq!(rebuilt, normal);
    let version: String = open_db(root)
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_format_version'",
            [],
            |row| row.get(0),
        )
        .expect("format version");
    assert_eq!(version, "2");
}
