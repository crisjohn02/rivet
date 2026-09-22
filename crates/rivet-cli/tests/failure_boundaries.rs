//! Integration tests for T32: parser resource limits, stale-fact removal, the
//! direct-target error codes, and argument precedence (spec §27; ARCHITECTURE
//! "Parse and coverage policy"; OUTPUT-CONTRACT "Errors").
//!
//! Every test drives the built binary against a temporary repository and
//! asserts exact exit codes and JSON fields. The parser resource bounds are
//! lowered with the debug-only `RIVET_DEBUG_MAX_NODES` / `RIVET_DEBUG_MAX_USES`
//! hooks, which the test binary (a debug build) honors; a release build always
//! uses the spec's bounds.

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
            "rivet-t32-{label}-{}-{nanos}-{unique}",
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

/// Writes `contents` at `rel` under `root`, creating parent directories.
fn write(root: &Path, rel: &str, contents: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

/// Runs the binary in `dir` with the given extra environment.
fn run_with(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(RIVET);
    command.args(args).current_dir(dir);
    // Never inherit a limit override from the environment running the tests.
    command
        .env_remove("RIVET_DEBUG_MAX_NODES")
        .env_remove("RIVET_DEBUG_MAX_USES");
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run the rivet binary")
}

/// Runs the binary in `dir` under the default limits.
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

/// The skip counts of a response's `index.coverage`.
fn skipped(value: &Value) -> &Value {
    &value["index"]["coverage"]["skipped"]
}

/// The three query commands, each with the arguments needed to reach the
/// query stage. `context` gets an explicit budget.
fn query_commands(query: &str) -> Vec<Vec<String>> {
    let query = query.to_string();
    vec![
        vec!["symbol".into(), query.clone(), "--json".into()],
        vec!["refs".into(), query.clone(), "--json".into()],
        vec![
            "context".into(),
            query,
            "--tokens".into(),
            "4000".into(),
            "--json".into(),
        ],
    ]
}

/// Runs one command given as owned strings.
fn run_owned(dir: &Path, args: &[String], env: &[(&str, &str)]) -> Output {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_with(dir, &args, env)
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// A tiny valid file: well under every lowered limit used below.
const GOOD_PHP: &[u8] = b"<?php\nnamespace App;\nfunction good(): void {}\n";

/// A lowered node bound between `GOOD_PHP` (about 20 nodes) and
/// [`big_php`] (over 200 nodes).
const LOW_NODES: &str = "100";

/// A valid file with 30 calls: over 200 Tree-sitter nodes and 30 uses.
fn big_php() -> Vec<u8> {
    let mut source = String::from("<?php\nnamespace App;\nfunction big(): void {\n");
    for index in 1..=30 {
        source.push_str(&format!("    f{index}();\n"));
    }
    source.push_str("}\n");
    source.into_bytes()
}

/// A file with exactly three uses (`f`, `g`, `h`) and one symbol.
const THREE_USES_PHP: &[u8] = b"<?php\nnamespace App;\nfunction three(): void { f(); g(); h(); }\n";

// ---------------------------------------------------------------------------
// (a) Resource limits are counts that bite only when exceeded.
// ---------------------------------------------------------------------------

/// A fresh repository holding `files`.
///
/// A limit override applies only to files a refresh actually parses, and a
/// normal refresh reuses an unchanged `ok` file without reparsing, so a test
/// that lowers a limit on a file already indexed under the defaults uses a
/// fresh repository (or `--force`, which reparses everything; AF6).
fn repo_with(label: &str, files: &[(&str, &[u8])]) -> TempDir {
    let temp = git_repo(label);
    for (rel, bytes) in files {
        write(temp.path(), rel, bytes);
    }
    temp
}

#[test]
fn lowered_node_limit_records_resource_limit_and_default_indexes_it() {
    let big = big_php();
    let files: &[(&str, &[u8])] = &[("good.php", GOOD_PHP), ("big.php", &big)];
    let temp = repo_with("nodes", files);

    let limited = parse_success(&run_with(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_MAX_NODES", LOW_NODES)],
    ));
    assert_eq!(
        limited["index"]["coverage"],
        json!({
            "complete": false,
            "files_seen": 2,
            "files_indexed": 1,
            "skipped": {
                "unsupported": 0, "binary": 0, "size": 0, "encoding": 0,
                "parse_error": 0, "resource_limit": 1,
            },
        })
    );
    assert_eq!(
        limited["index"]["diagnostics"],
        json!({"total": 1, "truncated": false, "items": [{
            "file": "big.php",
            "code": "resource_limit",
            "detail": "visited more than 100 Tree-sitter nodes",
        }]})
    );
    // Only good.php's two symbols (`App` and `App\good`); big.php contributes
    // no symbols or uses.
    assert_eq!(
        (limited["symbols"].clone(), limited["uses"].clone()),
        (json!(2), json!(0))
    );
    // Its symbol is not queryable while it is over the bound.
    let error = parse_error(
        &run_with(
            temp.path(),
            &["symbol", "App\\big", "--json"],
            &[("RIVET_DEBUG_MAX_NODES", LOW_NODES)],
        ),
        4,
    );
    assert_eq!(error["error"], "symbol_not_found");

    // Same bytes and same limits give the same answer, digest included.
    let again = repo_with("nodes-again", files);
    let repeated = parse_success(&run_with(
        again.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_MAX_NODES", LOW_NODES)],
    ));
    assert_eq!(repeated["index"], limited["index"]);

    // The same bytes under the default limits index normally: a failed file is
    // reparsed on every refresh, and the lowered bound is what made it fail.
    let default = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(skipped(&default)["resource_limit"], 0);
    assert_eq!(default["index"]["coverage"]["complete"], true);
    assert_eq!(
        (default["symbols"].clone(), default["uses"].clone()),
        (json!(4), json!(30))
    );
    parse_success(&run(temp.path(), &["symbol", "App\\big", "--json"]));
}

#[test]
fn lowered_use_limit_records_resource_limit_only_when_exceeded() {
    let files: &[(&str, &[u8])] = &[("three.php", THREE_USES_PHP), ("good.php", GOOD_PHP)];

    // Exactly at the bound: indexed. Each file declares `App` and one function.
    let at_repo = repo_with("uses-at", files);
    let at = parse_success(&run_with(
        at_repo.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_MAX_USES", "3")],
    ));
    assert_eq!(skipped(&at)["resource_limit"], 0);
    assert_eq!(
        (at["symbols"].clone(), at["uses"].clone()),
        (json!(4), json!(3))
    );

    // One below: resource_limit with no facts.
    let temp = repo_with("uses-below", files);
    let limited = parse_success(&run_with(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_MAX_USES", "2")],
    ));
    assert_eq!(skipped(&limited)["resource_limit"], 1);
    assert_eq!(limited["index"]["coverage"]["files_indexed"], 1);
    assert_eq!(
        limited["index"]["diagnostics"]["items"],
        json!([{
            "file": "three.php",
            "code": "resource_limit",
            "detail": "extracted more than 2 uses",
        }])
    );
    assert_eq!(
        (limited["symbols"].clone(), limited["uses"].clone()),
        (json!(2), json!(0))
    );

    // Default limits: the failed file is reparsed and indexed normally.
    let default = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(skipped(&default)["resource_limit"], 0);
    assert_eq!(
        (default["symbols"].clone(), default["uses"].clone()),
        (json!(4), json!(3))
    );
    assert_eq!(default["index"], at["index"]);
}

// ---------------------------------------------------------------------------
// (b) A formerly valid file loses its facts when it fails.
// ---------------------------------------------------------------------------

/// Declares `App\Svc::launch` and `App\helper`.
const DECL_PHP: &[u8] = b"<?php\nnamespace App;\nclass Svc\n{\n    public function launch(): void {}\n}\nfunction helper(): void {}\n";

/// Uses both declarations from `App\run`.
const USER_PHP: &[u8] =
    b"<?php\nnamespace App;\nfunction run(Svc $s): void\n{\n    $s->launch();\n    helper();\n}\n";

/// `USER_PHP` with its closing brace removed: a parse error.
const USER_BROKEN_PHP: &[u8] =
    b"<?php\nnamespace App;\nfunction run(Svc $s): void\n{\n    $s->launch();\n    helper();\n";

/// `USER_PHP` still valid, but padded with calls past [`LOW_NODES`].
fn user_big_php() -> Vec<u8> {
    let mut source = String::from(
        "<?php\nnamespace App;\nfunction run(Svc $s): void\n{\n    $s->launch();\n    helper();\n",
    );
    for index in 1..=30 {
        source.push_str(&format!("    f{index}();\n"));
    }
    source.push_str("}\n");
    source.into_bytes()
}

/// The reference files listed by `refs <query>`.
fn reference_files(dir: &Path, query: &str, env: &[(&str, &str)]) -> Vec<String> {
    let value = parse_success(&run_with(dir, &["refs", query, "--json"], env));
    value["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| item["file"].as_str().expect("file").to_string())
        .collect()
}

#[test]
fn a_file_that_becomes_a_parse_error_or_resource_limit_loses_its_facts() {
    let temp = git_repo("stale");
    write(temp.path(), "decl.php", DECL_PHP);
    write(temp.path(), "user.php", USER_PHP);

    let valid = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(valid["index"]["coverage"]["complete"], true);
    // decl.php: App, Svc, launch, helper. user.php: App, run.
    assert_eq!(valid["symbols"], 6);
    assert!(valid["uses"].as_u64().unwrap() > 0);
    assert!(valid["bindings"].as_u64().unwrap() > 0);
    assert_eq!(
        reference_files(temp.path(), "App\\helper", &[]),
        vec!["user.php"]
    );
    assert_eq!(
        reference_files(temp.path(), "App\\Svc::launch", &[]),
        vec!["user.php"]
    );
    parse_success(&run(
        temp.path(),
        &["symbol", "user.php#App\\run", "--json"],
    ));

    // The declaration-only baseline: what remains when user.php has no facts.
    let decl_only = git_repo("stale-baseline");
    write(decl_only.path(), "decl.php", DECL_PHP);
    let baseline = parse_success(&run(decl_only.path(), &["index", "--json"]));

    for (label, bytes, env) in [
        ("parse_error", USER_BROKEN_PHP.to_vec(), vec![]),
        (
            "resource_limit",
            user_big_php(),
            vec![("RIVET_DEBUG_MAX_NODES", LOW_NODES)],
        ),
    ] {
        // Restore the valid file first so each failure replaces live facts.
        write(temp.path(), "user.php", USER_PHP);
        let restored = parse_success(&run(temp.path(), &["index", "--json"]));
        assert_eq!(restored["symbols"], 6, "{label}");

        write(temp.path(), "user.php", &bytes);
        let failed = parse_success(&run_with(temp.path(), &["index", "--json"], &env));
        assert_eq!(skipped(&failed)[label], 1, "{label}");
        assert_eq!(failed["index"]["coverage"]["complete"], false, "{label}");
        assert_eq!(
            failed["index"]["diagnostics"]["items"][0]["file"], "user.php",
            "{label}"
        );
        assert_eq!(
            failed["index"]["diagnostics"]["items"][0]["code"], label,
            "{label}"
        );
        // Its symbols, uses, and bindings are gone: exactly the counts of a
        // repository that never had user.php.
        for key in ["symbols", "uses", "bindings"] {
            assert_eq!(failed[key], baseline[key], "{label}: {key}");
        }
        // `refs` for the symbols it used no longer lists its uses.
        assert!(
            reference_files(temp.path(), "App\\helper", &env).is_empty(),
            "{label}"
        );
        assert!(
            reference_files(temp.path(), "App\\Svc::launch", &env).is_empty(),
            "{label}"
        );
        // The symbol it declared is gone: by name it is not found, and by ID
        // the direct target reports the failure.
        let by_name = parse_error(
            &run_with(temp.path(), &["symbol", "App\\run", "--json"], &env),
            4,
        );
        assert_eq!(by_name["error"], "symbol_not_found", "{label}");
        let by_id = parse_error(
            &run_with(
                temp.path(),
                &["symbol", "user.php#App\\run", "--json"],
                &env,
            ),
            6,
        );
        assert_eq!(by_id["file"], "user.php", "{label}");
    }

    // The other direction: the declaring file fails, so a symbol it used to
    // declare cannot be queried and its uses elsewhere stay unbound.
    write(temp.path(), "user.php", USER_PHP);
    write(
        temp.path(),
        "decl.php",
        b"<?php\nnamespace App;\nclass Svc\n{\n    public function launch(): void {}\n\nfunction helper(): void {}\n",
    );
    let failed = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(skipped(&failed)["parse_error"], 1);
    assert_eq!(
        failed["symbols"], 2,
        "only user.php's App and App\\run remain"
    );
    assert_eq!(failed["bindings"], 0, "nothing is left to bind to");
    let refs = parse_error(&run(temp.path(), &["refs", "App\\helper", "--json"]), 4);
    assert_eq!(refs["error"], "symbol_not_found");
    let refs = parse_error(
        &run(temp.path(), &["refs", "decl.php#App\\helper", "--json"]),
        6,
    );
    assert_eq!(refs["file"], "decl.php");
}

// ---------------------------------------------------------------------------
// (c) Direct-target error codes in symbol, refs, and context.
// ---------------------------------------------------------------------------

/// A repository holding one file of every non-indexed kind plus a valid one.
fn direct_target_repo() -> TempDir {
    let temp = git_repo("direct");
    write(
        temp.path(),
        ".rivet/config.toml",
        b"[index]\nmax_file_size_kb = 1\n",
    );
    write(temp.path(), "good.php", GOOD_PHP);
    write(temp.path(), "notes.txt", b"plain text\nsecond line\n");
    write(
        temp.path(),
        "src/app.ts",
        b"export function app(): void {}\n",
    );
    write(
        temp.path(),
        "broken.php",
        b"<?php\nnamespace App;\nfunction broken( {\n",
    );
    write(temp.path(), "big.php", &big_php());
    write(temp.path(), "bin.php", b"<?php\nfunction b() {}\n\0\0\0\n");
    let mut large = b"<?php\nnamespace App;\nfunction large(): void {}\n".to_vec();
    large.extend(std::iter::repeat_n(b'/', 2048));
    large.push(b'\n');
    write(temp.path(), "large.php", &large);
    write(
        temp.path(),
        "enc.php",
        b"<?php\nnamespace App;\nfunction enc(): void {} // \xff\n",
    );
    temp
}

/// The lowered node bound: over big.php, under every other PHP file.
const DIRECT_ENV: &[(&str, &str)] = &[("RIVET_DEBUG_MAX_NODES", LOW_NODES)];

/// The coverage every direct-target query reports for [`direct_target_repo`].
fn direct_coverage() -> Value {
    json!({
        "complete": false,
        "files_seen": 8,
        "files_indexed": 1,
        "skipped": {
            "unsupported": 2, "binary": 1, "size": 1, "encoding": 1,
            "parse_error": 1, "resource_limit": 1,
        },
    })
}

/// Asserts `error` is the documented direct-target failure for every query
/// form in every command.
fn assert_direct(
    temp: &TempDir,
    queries: &[&str],
    exit: i32,
    code: &str,
    required: &[&str],
    check: impl Fn(&Value),
) {
    for query in queries {
        for args in query_commands(query) {
            let error = parse_error(&run_owned(temp.path(), &args, DIRECT_ENV), exit);
            let label = format!("{} {query}", args[0]);
            assert_eq!(error["error"], code, "{label}: {error}");
            let mut expected = vec!["schema_version", "error", "message", "hint"];
            expected.extend_from_slice(required);
            expected.push("index");
            assert_eq!(keys(&error), expected, "{label}: {error}");
            assert!(!error["hint"].as_str().unwrap().is_empty(), "{label}");
            assert_eq!(error["index"]["coverage"], direct_coverage(), "{label}");
            check(&error);
        }
    }
}

#[test]
fn an_unsupported_file_is_exit_7_with_its_nullable_language() {
    let temp = direct_target_repo();
    assert_direct(
        &temp,
        &["notes.txt:1", "notes.txt#Anything"],
        7,
        "unsupported_language",
        &["file", "language"],
        |error| {
            assert_eq!(error["file"], "notes.txt");
            assert_eq!(error["language"], Value::Null);
        },
    );
    assert_direct(
        &temp,
        &["src/app.ts:1", "src/app.ts#app"],
        7,
        "unsupported_language",
        &["file", "language"],
        |error| {
            assert_eq!(error["file"], "src/app.ts");
            assert_eq!(error["language"], "typescript");
        },
    );
}

#[test]
fn a_parse_error_or_resource_limit_file_is_exit_6_with_detail() {
    let temp = direct_target_repo();
    assert_direct(
        &temp,
        &["broken.php:3", "broken.php#App\\broken"],
        6,
        "parse_failure",
        &["file", "detail"],
        |error| {
            assert_eq!(error["file"], "broken.php");
            // The parser's own detail, as the refresh diagnostic reports it.
            let detail = error["detail"].as_str().unwrap();
            let diagnostic = error["index"]["diagnostics"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["file"] == "broken.php")
                .expect("broken.php diagnostic");
            assert_eq!(diagnostic["code"], "parse_error");
            assert_eq!(diagnostic["detail"], detail);
            assert!(detail.contains(" at byte "), "{detail}");
        },
    );
    assert_direct(
        &temp,
        &["big.php:4", "big.php#App\\big"],
        6,
        "parse_failure",
        &["file", "detail"],
        |error| {
            assert_eq!(error["file"], "big.php");
            assert_eq!(error["detail"], "visited more than 100 Tree-sitter nodes");
        },
    );
}

#[test]
fn a_binary_oversize_or_encoding_exclusion_is_exit_3_with_reason_and_hint() {
    let temp = direct_target_repo();
    for (file, reason, hint) in [
        ("bin.php", "binary", "Binary files are never indexed"),
        ("large.php", "max_file_size_kb", "index.max_file_size_kb"),
        ("enc.php", "UTF-8", "as UTF-8"),
    ] {
        let line = format!("{file}:1");
        let id = format!("{file}#App\\x");
        assert_direct(
            &temp,
            &[&line, &id],
            3,
            "repository_unavailable",
            &[],
            |error| {
                let message = error["message"].as_str().unwrap();
                assert!(
                    message.starts_with(&format!("{file} is not indexed: ")),
                    "{message}"
                );
                assert!(message.contains(reason), "{message}");
                assert!(error["hint"].as_str().unwrap().contains(hint), "{error}");
            },
        );
    }
}

#[test]
fn a_missing_name_in_an_indexed_file_is_exit_4_while_coverage_is_partial() {
    let temp = direct_target_repo();
    // A canonical ID into the indexed file, a line past its end, and a plain
    // name: each is `symbol_not_found`, never a file error.
    assert_direct(
        &temp,
        &["good.php#App\\missing", "good.php:99", "missing_everywhere"],
        4,
        "symbol_not_found",
        &["suggestions"],
        |_| {},
    );
    // An unrelated query in the same repository still succeeds (T32 (f)).
    for args in query_commands("good.php#App\\good") {
        let value = parse_success(&run_owned(temp.path(), &args, DIRECT_ENV));
        assert_eq!(value["index"]["coverage"], direct_coverage(), "{}", args[0]);
    }
}

// ---------------------------------------------------------------------------
// (d) Invalid `file:line` is rejected before any filesystem work.
// ---------------------------------------------------------------------------

#[test]
fn invalid_file_line_forms_are_exit_2_outside_any_repository() {
    // No `.git` or `.rivet` here, so any filesystem work would fail with 3.
    let temp = TempDir::new("not-a-repo");
    let control = parse_error(&run(temp.path(), &["symbol", "a.php:1", "--json"]), 3);
    assert_eq!(control["error"], "repository_unavailable");

    for (query, reason) in [
        ("a.php:0", "lines are numbered from 1"),
        ("a.php:+0", "lines are numbered from 1"),
        ("a.php:4294967296", "the line number is out of range"),
        (":7", "the path is empty"),
        (
            "/etc/passwd:1",
            "absolute paths are not repository-relative",
        ),
        ("../a.php:1", "`..` path components are not allowed"),
        ("src/../../a.php:2", "`..` path components are not allowed"),
    ] {
        for args in query_commands(query) {
            let error = parse_error(&run_owned(temp.path(), &args, &[]), 2);
            let label = format!("{} {query}", args[0]);
            assert_eq!(error["error"], "invalid_arguments", "{label}");
            assert_eq!(
                keys(&error),
                vec!["schema_version", "error", "message", "hint"],
                "{label}"
            );
            assert!(
                error["message"].as_str().unwrap().ends_with(reason),
                "{label}: {error}"
            );
        }
    }
    assert!(!temp.path().join(".rivet").exists(), "nothing was created");
}

// ---------------------------------------------------------------------------
// (e) An unreadable file fails refresh with exit 3.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn an_unreadable_file_fails_refresh_instead_of_keeping_old_facts() {
    use std::os::unix::fs::PermissionsExt;

    let temp = git_repo("unreadable");
    write(temp.path(), "decl.php", DECL_PHP);
    write(temp.path(), "user.php", USER_PHP);
    let before = parse_success(&run(temp.path(), &["index", "--json"]));

    // A content change the refresh must read, then made unreadable.
    let path = temp.path().join("user.php");
    fs::write(&path, USER_BROKEN_PHP).expect("edit user.php");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::read(&path).is_ok() {
        // Running as root: permissions do not stop the read, so the case
        // cannot be constructed here.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("restore");
        eprintln!("skipping: a mode-000 file is still readable (running as root?)");
        return;
    }

    let index = parse_error(&run(temp.path(), &["index", "--json"]), 3);
    assert_eq!(index["error"], "repository_unavailable");
    assert!(
        index["message"].as_str().unwrap().contains("user.php"),
        "{index}"
    );
    assert_eq!(
        keys(&index),
        vec!["schema_version", "error", "message", "hint"]
    );
    // A query refreshes too, so it fails the same way rather than answering
    // from the old facts.
    for args in query_commands("App\\helper") {
        let error = parse_error(&run_owned(temp.path(), &args, &[]), 3);
        assert_eq!(error["error"], "repository_unavailable", "{}", args[0]);
    }
    // Nothing was published: the committed snapshot is still the old one.
    let cached = parse_success(&run(
        temp.path(),
        &["symbol", "App\\helper", "--no-refresh", "--json"],
    ));
    assert_eq!(cached["index"]["snapshot"], before["index"]["snapshot"]);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("restore");
    let after = parse_success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(skipped(&after)["parse_error"], 1);
}

// ---------------------------------------------------------------------------
// (f) Unrelated queries keep working with partial coverage.
// ---------------------------------------------------------------------------

#[test]
fn an_unrelated_query_succeeds_with_partial_coverage_and_the_diagnostic() {
    let temp = git_repo("unrelated");
    write(temp.path(), "decl.php", DECL_PHP);
    write(temp.path(), "user.php", USER_BROKEN_PHP);

    for args in query_commands("App\\helper") {
        let value = parse_success(&run_owned(temp.path(), &args, &[]));
        let label = &args[0];
        assert_eq!(value["index"]["coverage"]["complete"], false, "{label}");
        assert_eq!(skipped(&value)["parse_error"], 1, "{label}");
        let items = value["index"]["diagnostics"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{label}");
        assert_eq!(items[0]["file"], "user.php", "{label}");
        assert_eq!(items[0]["code"], "parse_error", "{label}");
    }
    let symbol = parse_success(&run(temp.path(), &["symbol", "App\\helper", "--json"]));
    assert_eq!(symbol["symbol"]["id"], "decl.php#App\\helper");
}
