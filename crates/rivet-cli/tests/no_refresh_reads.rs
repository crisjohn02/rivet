//! PF2 integration tests: `--no-refresh` reads without writing, and a cached
//! answer keeps the parser's diagnostic detail.
//!
//! OUTPUT-CONTRACT "Flag applicability": `--no-refresh` answers from the
//! committed snapshot and never refreshes, so on an unchanged tree it must
//! write nothing to `.rivet/` (every file's bytes and modification time are
//! unchanged, and no journal is left behind) and must return exactly the bytes
//! a refreshed query returns, apart from `freshness: cached`. This holds on
//! both authored fixtures, whose TypeScript half includes a file that fails to
//! parse.
//!
//! PF2 also persists each failed file's parser diagnostic in the store's
//! `diagnostics` table, so the `detail` a `--no-refresh` answer (or a
//! metadata-mode refresh, which reuses the stored status) reports is the one
//! the producing refresh reported, not a generic stored-status text. A snapshot
//! written before PF2 persisted no detail and still falls back to the generic
//! text.

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;
use support::{copy_fixture, failure, git_repo, run, success, write};

/// The authored TypeScript fixture.
fn typescript_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/typescript/authored")
}

/// Copies every regular file under `source` into `dest`, keeping relative
/// paths.
fn copy_tree(source: &Path, dest: &Path) {
    fn visit(root: &Path, dir: &Path, dest: &Path) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .expect("read directory")
            .map(|entry| entry.expect("entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                visit(root, &path, dest);
            } else {
                let rel = path.strip_prefix(root).expect("under root");
                let rel = rel.to_str().expect("UTF-8 path").replace('\\', "/");
                write(dest, &rel, &fs::read(&path).expect("read fixture file"));
            }
        }
    }
    visit(source, source, dest);
}

/// Every entry directly under `.rivet/`, with its bytes and modification time.
fn cache_state(root: &Path) -> BTreeMap<String, (Vec<u8>, SystemTime)> {
    fs::read_dir(root.join(".rivet"))
        .expect("read .rivet")
        .map(|entry| {
            let entry = entry.expect("entry");
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let bytes = fs::read(&path).expect("read cache file");
            let mtime = fs::metadata(&path)
                .expect("cache metadata")
                .modified()
                .expect("modification time");
            (name, (bytes, mtime))
        })
        .collect()
}

/// The human-output line that labels a cached snapshot.
const CACHED_LINE: &str = "snapshot: cached (--no-refresh); it may not match the working tree\n";

/// Runs each query refreshed and then with `--no-refresh`, and asserts the
/// cached run wrote nothing and differs from the refreshed run only in the
/// freshness label, successes and failures alike.
fn assert_cached_reads_match(root: &Path, queries: &[Vec<&str>]) {
    let indexed = run(root, &["index", "--json"]);
    assert_eq!(indexed.status.code(), Some(0), "index failed");
    let refreshed: Vec<_> = queries.iter().map(|args| run(root, args)).collect();

    let before = cache_state(root);
    assert!(before.contains_key("index.db"), "{:?}", before.keys());
    for (args, fresh) in queries.iter().zip(&refreshed) {
        let mut cached_args = args.clone();
        cached_args.insert(cached_args.len() - 1, "--no-refresh");
        let cached = run(root, &cached_args);
        assert_eq!(cached.status.code(), fresh.status.code(), "{cached_args:?}");
        // JSON carries the label in `freshness`; human output adds one line
        // naming the cached snapshot (OUTPUT-CONTRACT "Common index metadata").
        let relabel = |bytes: &[u8]| {
            String::from_utf8_lossy(bytes)
                .replace("\"freshness\":\"cached\"", "\"freshness\":\"content\"")
                .replace(CACHED_LINE, "")
        };
        assert_eq!(
            relabel(&cached.stdout),
            String::from_utf8_lossy(&fresh.stdout),
            "{cached_args:?}"
        );
        assert_eq!(
            relabel(&cached.stderr),
            String::from_utf8_lossy(&fresh.stderr),
            "{cached_args:?}"
        );
        let text = String::from_utf8_lossy(&cached.stdout).into_owned()
            + &String::from_utf8_lossy(&cached.stderr);
        assert!(
            text.contains("\"freshness\":\"cached\"") || text.contains(CACHED_LINE),
            "{cached_args:?} is not labeled cached"
        );
    }
    let after = cache_state(root);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "--no-refresh created or removed a cache file"
    );
    for (name, (bytes, mtime)) in &before {
        let (after_bytes, after_mtime) = &after[name];
        assert!(
            after_bytes == bytes,
            "--no-refresh changed the bytes of {name}"
        );
        assert_eq!(after_mtime, mtime, "--no-refresh touched {name}");
    }
}

#[test]
fn no_refresh_writes_nothing_and_matches_a_refreshed_query_on_the_php_fixture() {
    let temp = git_repo("pf2-php");
    copy_fixture(temp.path());
    let queries: Vec<Vec<&str>> = vec![
        vec![
            "symbol",
            "App\\Services\\SurveyService::launch",
            "--source",
            "--json",
        ],
        vec!["symbol", "launch", "--json"],
        vec!["symbol", "SurveyService.php:20", "--json"],
        vec!["symbol", "App\\Nope", "--json"],
        vec!["refs", "App\\Services\\SurveyService::launch", "--json"],
        vec![
            "refs",
            "App\\Services\\SurveyService::launch",
            "--mode",
            "candidates",
            "--json",
        ],
        vec!["context", "App\\Services\\SurveyService::launch", "--json"],
        vec!["symbol", "App\\Services\\SurveyService::launch"],
    ];
    assert_cached_reads_match(temp.path(), &queries);
}

#[test]
fn no_refresh_writes_nothing_and_matches_a_refreshed_query_on_the_typescript_fixture() {
    let temp = git_repo("pf2-ts");
    copy_tree(&typescript_fixture(), temp.path());
    let launch = "src/services/survey.ts#SurveyService.launch";
    let run_typed = "src/report.ts#ReportService.runTyped";
    let queries: Vec<Vec<&str>> = vec![
        vec!["symbol", launch, "--source", "--json"],
        vec!["symbol", run_typed, "--json"],
        vec!["refs", launch, "--json"],
        vec!["refs", launch, "--mode", "candidates", "--json"],
        vec!["context", run_typed, "--json"],
        vec!["context", launch, "--tokens", "120", "--json"],
        // A directly addressed file that failed to parse: the error carries
        // the parser's detail in both modes.
        vec!["symbol", "src/broken.ts:2", "--json"],
        vec!["refs", "launch", "--json"],
        vec!["symbol", launch],
    ];
    assert_cached_reads_match(temp.path(), &queries);
}

const BROKEN_PHP: &[u8] = b"<?php\nfunction broken( {\n";
const BROKEN_TS: &[u8] = b"export function broken(value: number {\n  return value;\n}\n";

/// The diagnostics object of a success or of an index-dependent failure (both
/// carry `index` at the top level).
fn diagnostics_of(value: &Value) -> Value {
    let index = value
        .get("index")
        .unwrap_or_else(|| panic!("no index metadata: {value}"));
    index["diagnostics"].clone()
}

/// A tree with a broken PHP file and a broken TypeScript file beside the PHP
/// fixture.
fn tree_with_parse_failures(label: &str) -> support::TempDir {
    let temp = git_repo(label);
    copy_fixture(temp.path());
    write(temp.path(), "Broken.php", BROKEN_PHP);
    write(temp.path(), "src/broken.ts", BROKEN_TS);
    temp
}

#[test]
fn cached_and_metadata_mode_diagnostics_keep_the_parser_detail() {
    let temp = tree_with_parse_failures("pf2-diagnostics");
    let root = temp.path();
    let query = "App\\Services\\SurveyService::launch";

    let indexed = success(&run(root, &["index", "--json"]));
    let fresh = diagnostics_of(&indexed);
    let items = fresh["items"].as_array().expect("items");
    let files: Vec<&str> = items
        .iter()
        .map(|item| item["file"].as_str().expect("file"))
        .collect();
    assert_eq!(files, ["Broken.php", "src/broken.ts"], "{fresh}");
    for item in items {
        assert_eq!(item["code"], "parse_error", "{item}");
        let detail = item["detail"].as_str().expect("detail");
        assert!(detail.contains(" at byte "), "not a parser detail: {item}");
    }

    // A refreshed query, a cached query, and a metadata-mode refresh that
    // reuses both files' stored status all report the same items.
    let refreshed = success(&run(root, &["symbol", query, "--json"]));
    assert_eq!(diagnostics_of(&refreshed), fresh);
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(cached["index"]["freshness"], "cached");
    assert_eq!(diagnostics_of(&cached), fresh);
    let metadata = success(&run(
        root,
        &["symbol", query, "--freshness", "metadata", "--json"],
    ));
    assert_eq!(metadata["index"]["freshness"], "metadata");
    assert_eq!(diagnostics_of(&metadata), fresh);
    // A second metadata-mode refresh carries the persisted detail forward.
    let metadata = success(&run(root, &["index", "--freshness", "metadata", "--json"]));
    assert_eq!(diagnostics_of(&metadata), fresh);
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(diagnostics_of(&cached), fresh);

    // The parse-failure error for the directly addressed file carries the same
    // detail as a cached answer and as a refreshed one.
    let ts_detail = items[1]["detail"].clone();
    for extra in [None, Some("--no-refresh")] {
        let mut args = vec!["symbol", "src/broken.ts:1"];
        args.extend(extra);
        args.push("--json");
        let error = failure(&run(root, &args), 6);
        assert_eq!(error["error"], "parse_failure", "{error}");
        assert_eq!(error["detail"], ts_detail, "{args:?}: {error}");
    }

    // Fixing a file drops its persisted diagnostic; breaking it differently
    // replaces it.
    write(root, "Broken.php", b"<?php\nfunction fixed(): void {}\n");
    write(root, "src/broken.ts", b"export const x = ;\n");
    let indexed = success(&run(root, &["index", "--json"]));
    let fresh = diagnostics_of(&indexed);
    assert_eq!(fresh["total"], 1, "{fresh}");
    assert_eq!(fresh["items"][0]["file"], "src/broken.ts");
    assert_ne!(fresh["items"][0]["detail"], ts_detail, "{fresh}");
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(diagnostics_of(&cached), fresh);
}

#[test]
fn force_rebuild_persists_the_same_diagnostics() {
    let temp = tree_with_parse_failures("pf2-force");
    let root = temp.path();
    let query = "App\\Services\\SurveyService::launch";
    let indexed = success(&run(root, &["index", "--json"]));
    let forced = success(&run(root, &["index", "--force", "--json"]));
    assert_eq!(diagnostics_of(&forced), diagnostics_of(&indexed));
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(diagnostics_of(&cached), diagnostics_of(&indexed));
}

#[test]
fn a_snapshot_without_persisted_diagnostics_falls_back_to_the_generic_detail() {
    let temp = tree_with_parse_failures("pf2-legacy");
    let root = temp.path();
    let query = "App\\Services\\SurveyService::launch";
    success(&run(root, &["index", "--json"]));

    // A snapshot written before PF2 has an empty `diagnostics` table.
    let conn = rusqlite::Connection::open(root.join(".rivet/index.db")).expect("open store");
    conn.execute("DELETE FROM diagnostics", [])
        .expect("clear diagnostics");
    drop(conn);

    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    let items = diagnostics_of(&cached)["items"].clone();
    let details: Vec<(&str, &str, &str)> = items
        .as_array()
        .expect("items")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().expect("file"),
                item["code"].as_str().expect("code"),
                item["detail"].as_str().expect("detail"),
            )
        })
        .collect();
    assert_eq!(
        details,
        [
            ("Broken.php", "parse_error", "stored parse error"),
            ("src/broken.ts", "parse_error", "stored parse error"),
        ]
    );

    // The next content-mode refresh reparses the failed files and persists
    // their detail again.
    let refreshed = success(&run(root, &["index", "--json"]));
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(diagnostics_of(&cached), diagnostics_of(&refreshed));
}

#[test]
fn cached_skip_diagnostics_use_the_refresh_detail() {
    let temp = git_repo("pf2-skips");
    let root = temp.path();
    copy_fixture(root);
    write(
        root,
        ".rivet/config.toml",
        b"[index]\nmax_file_size_kb = 1\n",
    );
    let mut big = b"<?php\n".to_vec();
    big.extend(std::iter::repeat_n(b'/', 2048));
    big.push(b'\n');
    write(root, "Big.php", &big);
    write(root, "Binary.php", b"<?php\n\0\n");
    write(root, "Latin.php", b"<?php\n// caf\xe9\n");

    let query = "App\\Services\\SurveyService::launch";
    let indexed = success(&run(root, &["index", "--json"]));
    let fresh = diagnostics_of(&indexed);
    let codes: Vec<&str> = fresh["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| {
            ["Big.php", "Binary.php", "Latin.php"].contains(&item["file"].as_str().unwrap_or(""))
        })
        .map(|item| item["code"].as_str().expect("code"))
        .collect();
    assert_eq!(
        codes,
        ["file_too_large", "binary_file", "invalid_utf8"],
        "{fresh}"
    );
    let cached = success(&run(root, &["symbol", query, "--no-refresh", "--json"]));
    assert_eq!(diagnostics_of(&cached), fresh);
    let metadata = success(&run(root, &["index", "--freshness", "metadata", "--json"]));
    assert_eq!(diagnostics_of(&metadata), fresh);
}
