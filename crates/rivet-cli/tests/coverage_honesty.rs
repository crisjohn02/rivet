//! Integration tests for AF5: contract and coverage honesty.
//!
//! Each test drives the built binary against a temporary repository and
//! asserts exact JSON values for one audit item: a file is reported as indexed
//! only when an extractor produced its facts (finding 14; since T43 TypeScript
//! has one, so a valid `.ts`/`.tsx` file is indexed and a broken one is a
//! parse error), index-dependent errors carry `index` once a snapshot was
//! acquired (15), `updated` counts files whose facts were regenerated (16),
//! the non-UTF-8 path diagnostic is repository-relative (17), and
//! `--no-refresh` refuses a cache built by other extractor or resolver rules
//! (the audit's second unverified suspicion). T41 added that indexing the
//! authored TypeScript fixture changed nothing; T43 replaces that with PHP
//! answers staying unchanged while the TypeScript files are indexed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

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
            "rivet-af5-{label}-{}-{nanos}-{unique}",
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

/// Writes `contents` at `rel` under `root`, creating parent directories.
fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write fixture");
}

/// Runs the binary in `dir` and returns the captured output.
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs the binary in `dir` with one extra environment variable.
fn run_env(dir: &Path, args: &[&str], key: &str, value: &Path) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .env(key, value)
        .output()
        .expect("run the rivet binary")
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
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "stdout must stay empty on error");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON object")
}

/// The top-level keys of a JSON object, in order.
fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect()
}

/// Sets one `meta` value in the repository's committed cache.
fn set_meta(root: &Path, key: &str, value: &str) {
    let store = rivet_store::Store::open(&root.join(".rivet")).expect("open store");
    store.set_meta(key, value).expect("set meta");
}

/// A Git root holding one valid PHP file; each test adds its other files.
fn typescript_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    write(temp.path(), "a.php", b"<?php\nfunction one(): void {}\n");
    temp
}

// ---------------------------------------------------------------------------
// Finding 14: a file counts as indexed only when an extractor produced its
// facts. Since T43 TypeScript has an extractor.
// ---------------------------------------------------------------------------

#[test]
fn invalid_typescript_file_is_a_parse_error() {
    let temp = typescript_repo("ts-invalid");
    write(
        temp.path(),
        "src/bad.ts",
        b"class {{{ this is not typescript\n",
    );

    let value = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(
        value["index"]["coverage"],
        json!({
            "complete": false,
            "files_seen": 2,
            "files_indexed": 1,
            "skipped": {
                "unsupported": 0,
                "binary": 0,
                "size": 0,
                "encoding": 0,
                "parse_error": 1,
                "resource_limit": 0,
            },
        })
    );
    let items = value["index"]["diagnostics"]["items"]
        .as_array()
        .expect("items");
    assert_eq!(items.len(), 1, "{value}");
    assert_eq!(items[0]["file"], "src/bad.ts");
    assert_eq!(items[0]["code"], "parse_error");
    assert!(
        items[0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(" at byte ")),
        "{value}"
    );
    assert_eq!(value["symbols"], 1);

    // A query's refreshed metadata and a cached read report the same coverage.
    let refreshed = parse_success(&run(temp.path(), &["symbol", "one", "--json"]));
    assert_eq!(refreshed["index"], value["index"]);
    let cached = parse_success(&run(
        temp.path(),
        &["symbol", "one", "--no-refresh", "--json"],
    ));
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["coverage"], value["index"]["coverage"]);
    assert_eq!(
        cached["index"]["diagnostics"]["items"][0]["code"],
        "parse_error"
    );

    // Addressing the broken file is a parse failure (exit 6), never
    // `unsupported_language`.
    let error = parse_error(&run(temp.path(), &["symbol", "src/bad.ts:1", "--json"]), 6);
    assert_eq!(error["error"], "parse_failure");
}

#[test]
fn valid_typescript_files_are_indexed() {
    let temp = typescript_repo("ts-valid");
    write(
        temp.path(),
        "src/ok.ts",
        b"export class Ok {\n  run(): void {}\n}\n",
    );
    write(
        temp.path(),
        "src/view.tsx",
        b"export const View = () => <div />;\n",
    );

    let value = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(
        value["index"]["coverage"],
        json!({
            "complete": true,
            "files_seen": 3,
            "files_indexed": 3,
            "skipped": {
                "unsupported": 0,
                "binary": 0,
                "size": 0,
                "encoding": 0,
                "parse_error": 0,
                "resource_limit": 0,
            },
        })
    );
    assert_eq!(
        value["index"]["diagnostics"],
        json!({"total": 0, "truncated": false, "items": []})
    );
    // `one`, and `Ok`, `Ok.run`, and `View`.
    assert_eq!(value["symbols"], 4);

    // Metadata mode reports the same coverage on a no-change refresh.
    let metadata = parse_success(&run(
        temp.path(),
        &["index", "--freshness", "metadata", "--json"],
    ));
    assert_eq!(metadata["index"]["coverage"], value["index"]["coverage"]);
    assert_eq!(
        metadata["index"]["diagnostics"],
        value["index"]["diagnostics"]
    );
    assert_eq!(
        (&metadata["updated"], &metadata["unchanged"]),
        (&json!(0), &json!(3))
    );
    // Both grammar variants are addressable.
    for query in ["src/ok.ts#Ok.run", "src/view.tsx:1"] {
        let found = parse_success(&run(temp.path(), &["symbol", query, "--json"]));
        assert_eq!(found["symbol"]["language"], "typescript", "{query}");
    }
}

#[test]
fn disabled_typescript_is_an_ordinary_unsupported_file() {
    let temp = typescript_repo("ts-disabled");
    write(temp.path(), "src/ok.ts", b"export const x = 1;\n");
    write(
        temp.path(),
        ".rivet/config.toml",
        b"[languages]\nenabled = [\"php\"]\n",
    );

    let value = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(value["index"]["coverage"]["complete"], false);
    assert_eq!(value["index"]["coverage"]["files_indexed"], 1);
    assert_eq!(value["index"]["coverage"]["skipped"]["unsupported"], 1);
    // Ordinary unsupported files are counted only (OUTPUT-CONTRACT).
    assert_eq!(
        value["index"]["diagnostics"],
        json!({"total": 0, "truncated": false, "items": []})
    );
}

#[test]
fn php_only_fixture_coverage_is_unchanged() {
    let temp = fixture_repo("php-only");
    let value = parse_success(&run(temp.path(), &["index", "--json"]));
    // README.md alone makes the fixture incomplete, exactly as before AF5.
    assert_eq!(
        value["index"]["coverage"],
        json!({
            "complete": false,
            "files_seen": 10,
            "files_indexed": 9,
            "skipped": {
                "unsupported": 1,
                "binary": 0,
                "size": 0,
                "encoding": 0,
                "parse_error": 0,
                "resource_limit": 0,
            },
        })
    );
    assert_eq!(value["index"]["diagnostics"]["total"], 0);
    assert_eq!(
        (&value["symbols"], &value["uses"], &value["bindings"]),
        (&json!(50), &json!(14), &json!(13))
    );
}

/// Copies every file under `source` into `dest`, keeping relative paths.
fn copy_tree(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create destination");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy fixture file");
        }
    }
}

/// The PHP fixture at the root and the authored TypeScript fixture (T41)
/// under `typescript/`.
fn mixed_fixture_repo(label: &str) -> TempDir {
    let temp = fixture_repo(label);
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/typescript/authored");
    copy_tree(&source, &temp.path().join("typescript"));
    temp
}

/// A success object without its `index` metadata, whose snapshot and
/// coverage differ between the two repositories by construction.
fn without_index(output: &Output) -> Value {
    let mut value = parse_success(output);
    value.as_object_mut().expect("an object").remove("index");
    value
}

// T43: adding the TypeScript fixture indexes its twelve `.ts`/`.d.ts`/`.tsx`
// files and reports its broken one as a parse error, and changes nothing rivet
// reports for PHP.
#[test]
fn typescript_fixture_is_indexed_beside_unchanged_php() {
    let php_only = fixture_repo("t43-php");
    let mixed = mixed_fixture_repo("t43-mixed");
    let before = parse_success(&run(php_only.path(), &["index", "--json"]));
    let value = parse_success(&run(mixed.path(), &["index", "--json"]));

    // README.md (twice), tsconfig.json, `.js`, and `.mts` are ordinary
    // unsupported files, counted only; `src/broken.ts` is the one parse error.
    assert_eq!(
        value["index"]["coverage"],
        json!({
            "complete": false,
            "files_seen": 27,
            "files_indexed": 21,
            "skipped": {
                "unsupported": 5,
                "binary": 0,
                "size": 0,
                "encoding": 0,
                "parse_error": 1,
                "resource_limit": 0,
            },
        })
    );
    let items = value["index"]["diagnostics"]["items"]
        .as_array()
        .expect("items");
    assert_eq!(value["index"]["diagnostics"]["total"], 1, "{value}");
    assert_eq!(items[0]["file"], "typescript/src/broken.ts");
    assert_eq!(items[0]["code"], "parse_error");

    // The TypeScript fixture adds its 82 gold declarations and 138 uses,
    // T44's 51 exact TypeScript bindings (direct relative imports, namespace
    // members, same-file declarations), and T45's 16 scoped receiver
    // bindings; the 13 PHP bindings are unchanged.
    assert_eq!(
        (&before["symbols"], &before["uses"], &before["bindings"]),
        (&json!(50), &json!(14), &json!(13))
    );
    assert_eq!(
        (&value["symbols"], &value["uses"], &value["bindings"]),
        (&json!(132), &json!(152), &json!(80))
    );

    // PHP answers are the PHP-only repository's.
    for args in [
        &["refs", "App\\Services\\SurveyService::launch", "--json"][..],
        &[
            "refs",
            "App\\Services\\SurveyService::launch",
            "--mode",
            "candidates",
            "--json",
        ][..],
        &["symbol", "App\\Reporting\\ReportService", "--json"][..],
        &["context", "App\\Services\\SurveyService::launch", "--json"][..],
    ] {
        assert_eq!(
            without_index(&run(mixed.path(), args)),
            without_index(&run(php_only.path(), args)),
            "{args:?}"
        );
    }

    // The broken file is a parse failure (exit 6); an indexed one resolves.
    let error = parse_error(
        &run(
            mixed.path(),
            &["symbol", "typescript/src/broken.ts:2", "--json"],
        ),
        6,
    );
    assert_eq!(error["error"], "parse_failure");
    let found = parse_success(&run(
        mixed.path(),
        &["symbol", "typescript/src/util.ts#double", "--json"],
    ));
    assert_eq!(found["symbol"]["kind"], "function");
    assert_eq!(found["symbol"]["language"], "typescript");

    // The human coverage line names the one diagnostic (CV1).
    let human = run(mixed.path(), &["index"]);
    assert_eq!(human.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&human.stdout).ends_with(
            "coverage incomplete: 21/27 files indexed; skipped 5 unsupported, 1 parse_error; \
             1 diagnostic: typescript/src/broken.ts (parse_error)\n"
        ),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );
}

// ---------------------------------------------------------------------------
// Finding 15: index-dependent errors carry `index` after acquisition.
// ---------------------------------------------------------------------------

#[test]
fn symbol_not_found_carries_the_acquired_index() {
    let temp = fixture_repo("not-found");
    let indexed = parse_success(&run(temp.path(), &["index", "--json"]));

    for command in ["symbol", "refs"] {
        let error = parse_error(&run(temp.path(), &[command, "nosuch", "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found");
        assert_eq!(
            keys(&error),
            vec![
                "schema_version",
                "error",
                "message",
                "hint",
                "suggestions",
                "index"
            ],
            "{command}"
        );
        assert_eq!(error["index"], indexed["index"], "{command}");
    }

    // A cached read acquires a snapshot too, labeled `cached`.
    let cached = parse_error(
        &run(temp.path(), &["symbol", "nosuch", "--no-refresh", "--json"]),
        4,
    );
    assert_eq!(cached["index"]["snapshot"], indexed["index"]["snapshot"]);
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["coverage"], indexed["index"]["coverage"]);
}

#[test]
fn ambiguous_symbol_carries_the_acquired_index() {
    let temp = git_repo("ambiguous");
    write(
        temp.path(),
        "a.php",
        b"<?php\nnamespace A;\nfunction launch(): void {}\n",
    );
    write(
        temp.path(),
        "b.php",
        b"<?php\nnamespace B;\nfunction launch(): void {}\n",
    );
    let indexed = parse_success(&run(temp.path(), &["index", "--json"]));

    for command in ["symbol", "refs"] {
        let error = parse_error(&run(temp.path(), &[command, "launch", "--json"]), 5);
        assert_eq!(error["error"], "ambiguous_symbol");
        assert_eq!(
            keys(&error),
            vec![
                "schema_version",
                "error",
                "message",
                "hint",
                "total",
                "truncated",
                "next_offset",
                "candidates",
                "index"
            ],
            "{command}"
        );
        assert_eq!(error["total"], 2);
        assert_eq!(error["index"], indexed["index"], "{command}");
    }
}

#[test]
fn errors_before_acquisition_carry_no_index() {
    let temp = fixture_repo("no-index");
    parse_success(&run(temp.path(), &["index", "--json"]));

    // Argument errors precede any filesystem work.
    for args in [
        &["symbol", "nosuch", "--limit", "0", "--json"][..],
        &["refs", "nosuch", "--mode", "bogus", "--json"][..],
        &[
            "symbol",
            "nosuch",
            "--no-refresh",
            "--freshness",
            "content",
            "--json",
        ][..],
    ] {
        let error = parse_error(&run(temp.path(), args), 2);
        assert_eq!(error["error"], "invalid_arguments", "{args:?}");
        assert!(error.get("index").is_none(), "{args:?}: {error}");
    }

    // A cache that cannot be acquired has no snapshot to report.
    let bare = git_repo("no-cache");
    let error = parse_error(
        &run(bare.path(), &["symbol", "nosuch", "--no-refresh", "--json"]),
        3,
    );
    assert_eq!(error["error"], "repository_unavailable");
    assert!(error.get("index").is_none(), "{error}");
}

// ---------------------------------------------------------------------------
// Finding 16: `updated` counts regenerated facts.
// ---------------------------------------------------------------------------

#[test]
fn updated_counts_files_reparsed_after_an_extractor_change() {
    let temp = fixture_repo("updated");
    let first = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(
        (&first["updated"], &first["unchanged"]),
        (&json!(10), &json!(0))
    );

    // A no-change refresh regenerates nothing.
    let log_dir = TempDir::new("updated-log");
    let quiet_log = log_dir.path().join("quiet.log");
    let quiet = parse_success(&run_env(
        temp.path(),
        &["index", "--json"],
        "RIVET_DEBUG_REPARSED",
        &quiet_log,
    ));
    assert_eq!(
        (&quiet["updated"], &quiet["unchanged"], &quiet["deleted"]),
        (&json!(0), &json!(10), &json!(0))
    );
    assert!(!quiet_log.exists(), "a no-change refresh reparses nothing");

    // A different stored extractor fingerprint reparses every PHP file with
    // unchanged content; each is `updated`, and README.md stays unchanged.
    set_meta(temp.path(), "extractor_fingerprint", "older-extractor");
    let log = log_dir.path().join("reparsed.log");
    let after = parse_success(&run_env(
        temp.path(),
        &["index", "--json"],
        "RIVET_DEBUG_REPARSED",
        &log,
    ));
    let reparsed = fs::read_to_string(&log).expect("reparsed log");
    let reparsed_count = reparsed.lines().count() as u64;
    assert_eq!(reparsed_count, 9, "{reparsed}");
    assert_eq!(after["updated"], json!(reparsed_count));
    assert_eq!(after["unchanged"], json!(1));
    assert_eq!(after["deleted"], json!(0));
    assert_eq!(after["index"]["coverage"]["files_seen"], json!(10));
    // Facts, bindings, and snapshot are the same as before.
    assert_eq!(after["index"], first["index"]);
    assert_eq!(
        (&after["symbols"], &after["uses"], &after["bindings"]),
        (&first["symbols"], &first["uses"], &first["bindings"])
    );

    let again = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(
        (&again["updated"], &again["unchanged"]),
        (&json!(0), &json!(10))
    );
}

// ---------------------------------------------------------------------------
// Finding 17: the non-UTF-8 diagnostic is repository-relative.
// ---------------------------------------------------------------------------

/// Runs end to end only where the filesystem accepts a non-UTF-8 name. APFS
/// (macOS) rejects one, so there the test returns early and the path form is
/// covered by the `rivet-core` unit test `escaped_relative_path_is_relative_and_escaped`.
#[cfg(unix)]
#[test]
fn non_utf8_path_diagnostic_is_repository_relative() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let temp = typescript_repo("non-utf8");
    let bad = temp
        .path()
        .join("sub")
        .join(OsStr::from_bytes(b"bad-\xff.txt"));
    fs::create_dir_all(bad.parent().expect("parent")).expect("create sub");
    if fs::write(&bad, "bad").is_err() {
        eprintln!("skipping: this filesystem rejects non-UTF-8 file names");
        return;
    }

    let value = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(value["index"]["coverage"]["complete"], false);
    assert_eq!(
        value["index"]["diagnostics"]["items"],
        json!([{
            "file": "sub/bad-\\xff.txt",
            "code": "non_utf8_path",
            "detail": "path is not valid UTF-8",
        }])
    );
}

// ---------------------------------------------------------------------------
// Suspicion 2: `--no-refresh` requires a cache from this build's rules.
// ---------------------------------------------------------------------------

#[test]
fn no_refresh_refuses_a_cache_from_another_extractor() {
    let temp = fixture_repo("cache-extractor");
    parse_success(&run(temp.path(), &["index", "--json"]));
    set_meta(temp.path(), "extractor_fingerprint", "older-extractor");

    for command in ["symbol", "refs"] {
        let error = parse_error(
            &run(
                temp.path(),
                &[command, "App\\Boot\\launch", "--no-refresh", "--json"],
            ),
            3,
        );
        assert_eq!(error["error"], "repository_unavailable", "{command}");
        assert_eq!(
            keys(&error),
            vec!["schema_version", "error", "message", "hint"],
            "{command}"
        );
        let message = error["message"].as_str().expect("message");
        assert!(message.contains("extractor_fingerprint"), "{message}");
        assert!(message.contains("older-extractor"), "{message}");
    }
}

#[test]
fn no_refresh_refuses_a_cache_from_another_resolver() {
    let temp = fixture_repo("cache-resolver");
    parse_success(&run(temp.path(), &["index", "--json"]));
    set_meta(temp.path(), "resolver_fingerprint", "php-rules-v0");

    let error = parse_error(
        &run(
            temp.path(),
            &["symbol", "App\\Boot\\launch", "--no-refresh", "--json"],
        ),
        3,
    );
    assert_eq!(error["error"], "repository_unavailable");
    assert!(error.get("index").is_none(), "{error}");
    let message = error["message"].as_str().expect("message");
    assert!(message.contains("resolver_fingerprint"), "{message}");
    assert!(message.contains("php-rules-v0"), "{message}");
}

#[test]
fn no_refresh_answers_from_a_compatible_cache() {
    let temp = fixture_repo("cache-compatible");
    let indexed = parse_success(&run(temp.path(), &["index", "--json"]));

    // An altered fingerprint is refused, and a refresh makes the cache
    // compatible again.
    set_meta(temp.path(), "extractor_fingerprint", "older-extractor");
    parse_error(
        &run(
            temp.path(),
            &["symbol", "App\\Boot\\launch", "--no-refresh", "--json"],
        ),
        3,
    );
    parse_success(&run(temp.path(), &["index", "--json"]));

    // A config edit alone does not make the cache incompatible: the cached
    // answer is labeled stale by design, and its facts are this build's.
    write(
        temp.path(),
        ".rivet/config.toml",
        b"[index]\nexclude = [\"boot.php\"]\n",
    );

    let cached = parse_success(&run(
        temp.path(),
        &["symbol", "App\\Boot\\launch", "--no-refresh", "--json"],
    ));
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(cached["index"]["snapshot"], indexed["index"]["snapshot"]);
    assert_eq!(cached["symbol"]["qualified_name"], "App\\Boot\\launch");
    let refs = parse_success(&run(
        temp.path(),
        &["refs", "App\\Boot\\launch", "--no-refresh", "--json"],
    ));
    assert_eq!(refs["index"]["freshness"], "cached");
}
