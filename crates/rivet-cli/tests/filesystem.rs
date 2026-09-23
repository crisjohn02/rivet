//! T36 acceptance matrix, "Filesystem": root boundary and exclusion rules
//! (spec §§25–27; OUTPUT-CONTRACT "Coordinates and symbol objects" and
//! "Common index metadata").
//!
//! Every test drives the built binary against temporary directories it
//! creates, and every run is bounded by a deadline (see `support`), so a
//! regression that opens a FIFO fails instead of hanging.

mod support;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use support::{RIVET, TempDir, failure, git, git_repo, run, run_guarded, success, write};

const SHARED_PHP: &[u8] = b"<?php\nnamespace App;\nfunction shared(): void {}\n";
const EXTRA_PHP: &[u8] = b"<?php\nnamespace App;\nfunction extra(): void {}\n";

/// The three query commands for `query`, each ending in `--json`.
fn queries(query: &str) -> Vec<Vec<&str>> {
    vec![
        vec!["symbol", query, "--json"],
        vec!["refs", query, "--json"],
        vec!["context", query, "--tokens", "4000", "--json"],
    ]
}

// ---------------------------------------------------------------------------
// Worktree `.git` file
// ---------------------------------------------------------------------------

/// A real `git worktree add` checkout, whose `.git` is a file, is its own
/// root: a query from a subdirectory creates `.rivet/` at the worktree root,
/// sees only the worktree's files, and does not count the `.git` file as a
/// repository file. The worktree's snapshot equals a plain clone of the same
/// tree. Before T36 the `.git` file was admitted as an unsupported file, so the
/// worktree reported one more file and a different snapshot.
#[test]
fn a_worktree_whose_git_is_a_file_is_the_root_and_its_git_file_is_not_indexed() {
    let home = TempDir::new("git-home");
    let base = TempDir::new("worktree");
    let main = base.path().join("main");
    fs::create_dir_all(&main).expect("create main");
    write(&main, "a.php", SHARED_PHP);
    git(&main, home.path(), &["init", "-q"]);
    git(&main, home.path(), &["add", "a.php"]);
    git(&main, home.path(), &["commit", "-q", "-m", "initial"]);
    git(
        &main,
        home.path(),
        &["worktree", "add", "-q", "../wt", "-b", "feature"],
    );
    let wt = base.path().join("wt");
    assert!(
        fs::symlink_metadata(wt.join(".git"))
            .expect("worktree .git")
            .is_file(),
        "git worktree add must create a .git file"
    );
    write(&wt, "sub/extra.php", EXTRA_PHP);

    // A query from a subdirectory with no marker of its own.
    let found = success(&run(&wt.join("sub"), &["symbol", "App\\extra", "--json"]));
    assert_eq!(found["symbol"]["id"], "sub/extra.php#App\\extra");
    assert!(wt.join(".rivet").is_dir(), "cache at the worktree root");
    assert!(!wt.join("sub/.rivet").exists());
    assert!(
        !main.join(".rivet").exists(),
        "the main checkout is untouched"
    );
    assert_eq!(
        found["index"]["coverage"],
        json!({
            "complete": true,
            "files_seen": 2,
            "files_indexed": 2,
            "skipped": {
                "unsupported": 0, "binary": 0, "size": 0, "encoding": 0,
                "parse_error": 0, "resource_limit": 0,
            },
        }),
        "the .git file is Git metadata, not a repository file"
    );

    // The same tree in an ordinary checkout has the same snapshot.
    let plain = git_repo("worktree-plain");
    write(plain.path(), "a.php", SHARED_PHP);
    write(plain.path(), "sub/extra.php", EXTRA_PHP);
    let plain_index = success(&run(plain.path(), &["index", "--json"]));
    let wt_index = success(&run(&wt, &["index", "--json"]));
    assert_eq!(wt_index["index"], plain_index["index"]);

    // The main checkout is a separate root that never sees the worktree.
    let missing = failure(&run(&main, &["symbol", "App\\extra", "--json"]), 4);
    assert_eq!(missing["error"], "symbol_not_found");
    let main_index = success(&run(&main, &["index", "--json"]));
    assert_eq!(main_index["index"]["coverage"]["files_seen"], 1);
}

// ---------------------------------------------------------------------------
// Nested repository and submodule
// ---------------------------------------------------------------------------

/// A nested repository (`.git` directory) and a submodule checkout (`.git`
/// file) are outside the outer root's scan domain; a query started inside the
/// nested repository selects it as its own root.
#[test]
fn a_nested_repository_and_a_submodule_are_excluded_from_the_outer_root() {
    let temp = git_repo("nested");
    let root = temp.path();
    write(
        root,
        "a.php",
        b"<?php\nnamespace App;\nfunction outer(): void { inner(); subFn(); }\n",
    );
    fs::create_dir_all(root.join("lib/nested/.git")).expect("nested .git dir");
    write(
        root,
        "lib/nested/n.php",
        b"<?php\nnamespace App;\nfunction inner(): void {}\n",
    );
    write(root, "lib/sub/.git", b"gitdir: ../../.git/modules/sub\n");
    write(
        root,
        "lib/sub/s.php",
        b"<?php\nnamespace App;\nfunction subFn(): void {}\n",
    );

    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["files_seen"], 1, "{index}");
    assert_eq!(index["index"]["coverage"]["complete"], true);
    for name in ["App\\inner", "App\\subFn"] {
        for args in queries(name) {
            let error = failure(&run(root, &args), 4);
            assert_eq!(error["error"], "symbol_not_found", "{args:?}");
        }
    }
    // The calls into the excluded files are uses with nothing to bind to, so
    // the default call list hides them as name-only rows (SY1).
    let outer = success(&run(root, &["symbol", "App\\outer", "--json"]));
    assert_eq!(outer["calls"]["total"], 0, "{outer}");
    assert_eq!(outer["calls"]["hidden_name_match"], 2, "{outer}");
    let outer = success(&run(
        root,
        &[
            "symbol",
            "App\\outer",
            "--json",
            "--min-resolution",
            "name_match",
        ],
    ));
    let calls = outer["calls"]["items"].as_array().expect("calls");
    assert_eq!(calls.len(), 2, "{outer}");
    assert!(calls.iter().all(|call| call["resolved_target"].is_null()));

    // Inside the nested repository, it is the root.
    let nested = root.join("lib/nested");
    let inner = success(&run(&nested, &["symbol", "App\\inner", "--json"]));
    assert_eq!(inner["symbol"]["id"], "n.php#App\\inner");
    assert_eq!(inner["index"]["coverage"]["files_seen"], 1);
    assert!(nested.join(".rivet").is_dir());
    let error = failure(&run(&nested, &["symbol", "App\\outer", "--json"]), 4);
    assert_eq!(error["error"], "symbol_not_found");

    // The outer root is unchanged by the nested cache.
    let again = success(&run(root, &["index", "--json"]));
    assert_eq!(again["index"], index["index"]);
}

// ---------------------------------------------------------------------------
// Symlinked cache
// ---------------------------------------------------------------------------

/// Every command refuses a symlinked `.rivet`, a symlinked `index.db`, and a
/// symlinked `config.toml` with exit 3 and writes nothing through the link.
#[cfg(unix)]
#[test]
fn a_symlinked_cache_directory_database_or_config_is_refused_without_writing() {
    use std::os::unix::fs::symlink;

    let outside = TempDir::new("cache-target");
    let temp = git_repo("symlinked-cache");
    let root = temp.path();
    write(root, "a.php", SHARED_PHP);

    let mut commands: Vec<Vec<&str>> = vec![vec!["index", "--json"]];
    commands.extend(queries("App\\shared"));
    commands.push(vec!["symbol", "App\\shared", "--no-refresh", "--json"]);

    symlink(outside.path(), root.join(".rivet")).expect("symlink .rivet");
    for args in &commands {
        let error = failure(&run(root, args), 3);
        assert_eq!(error["error"], "repository_unavailable", "{args:?}");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("symlinked .rivet"),
            "{args:?}: {error}"
        );
    }
    assert_eq!(
        fs::read_dir(outside.path()).expect("read target").count(),
        0,
        "nothing was written through the .rivet link"
    );
    fs::remove_file(root.join(".rivet")).expect("remove link");

    // A real cache directory whose database is a dangling link.
    fs::create_dir_all(root.join(".rivet")).expect("create .rivet");
    symlink(
        outside.path().join("index.db"),
        root.join(".rivet/index.db"),
    )
    .expect("db link");
    for args in &commands[..4] {
        let error = failure(&run(root, args), 3);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("index database is a symbolic link"),
            "{args:?}: {error}"
        );
    }
    assert!(!outside.path().join("index.db").exists());
    fs::remove_file(root.join(".rivet/index.db")).expect("remove db link");

    // A symlinked configuration file is never read or rewritten.
    write(
        outside.path(),
        "config.toml",
        b"[index]\nexclude = [\"a.php\"]\n",
    );
    symlink(
        outside.path().join("config.toml"),
        root.join(".rivet/config.toml"),
    )
    .expect("config link");
    for args in &commands[..4] {
        let error = failure(&run(root, args), 3);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("refusing to read symlinked config"),
            "{args:?}: {error}"
        );
    }
    assert_eq!(
        fs::read(outside.path().join("config.toml")).expect("read config"),
        b"[index]\nexclude = [\"a.php\"]\n"
    );
}

// ---------------------------------------------------------------------------
// Symlinks pointing outside the root
// ---------------------------------------------------------------------------

/// A symlink to a file or directory outside the root, and a symlink to a file
/// inside it, are never followed: they are not files of the tree, their
/// declarations are not symbols, and addressing them by path finds nothing.
#[cfg(unix)]
#[test]
fn symlinks_are_never_followed_even_to_php_outside_the_root() {
    use std::os::unix::fs::symlink;

    let outside = TempDir::new("outside");
    write(
        outside.path(),
        "o.php",
        b"<?php\nnamespace App;\nfunction outsideFn(): void {}\n",
    );
    write(
        outside.path(),
        "lib/l.php",
        b"<?php\nnamespace App;\nfunction outsideDirFn(): void {}\n",
    );
    let temp = git_repo("outside-links");
    let root = temp.path();
    write(root, "a.php", SHARED_PHP);
    symlink(outside.path().join("o.php"), root.join("linkfile.php")).expect("file link");
    symlink(outside.path().join("lib"), root.join("linkdir")).expect("dir link");
    symlink(root.join("a.php"), root.join("inside.php")).expect("inside link");

    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["files_seen"], 1, "{index}");
    assert_eq!(index["index"]["coverage"]["complete"], true);
    assert_eq!(index["index"]["diagnostics"]["total"], 0);

    for name in ["App\\outsideFn", "App\\outsideDirFn"] {
        for args in queries(name) {
            failure(&run(root, &args), 4);
        }
    }
    // The in-root link does not duplicate `shared`: it is unambiguous.
    let shared = success(&run(root, &["symbol", "App\\shared", "--json"]));
    assert_eq!(shared["symbol"]["id"], "a.php#App\\shared");
    // Addressing a link by path names a path that is not indexed.
    for query in ["linkfile.php:3", "linkdir/l.php:3", "inside.php:3"] {
        let error = failure(&run(root, &["symbol", query, "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{query}");
    }
    // The outside tree gained nothing (no cache, no index).
    let mut names: Vec<String> = fs::read_dir(outside.path())
        .expect("read outside")
        .map(|entry| entry.expect("entry").file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["lib", "o.php"]);
}

// ---------------------------------------------------------------------------
// FIFO
// ---------------------------------------------------------------------------

/// Creates a FIFO at `path` with the system `mkfifo`.
#[cfg(unix)]
fn mkfifo(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo {}", path.display());
}

/// Runs the binary with a short deadline: opening a FIFO for reading blocks
/// forever with no writer, so finishing at all proves none was opened.
fn run_fast(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = Command::new(RIVET);
    command.args(args).current_dir(dir);
    run_guarded(
        command,
        Duration::from_secs(30),
        &format!("rivet {args:?} beside a FIFO"),
    )
}

/// A FIFO named like PHP source is never opened: refresh finishes, the FIFO is
/// not a file of the tree, and every query succeeds. A FIFO where a Git ignore
/// file would be read is refused with exit 3 instead of opened; before T36 the
/// walk blocked on it forever.
#[cfg(unix)]
#[test]
fn a_fifo_is_never_opened_and_the_walk_never_blocks() {
    let temp = git_repo("fifo");
    let root = temp.path();
    write(root, "a.php", SHARED_PHP);
    mkfifo(&root.join("pipe.php"));
    mkfifo(&root.join("sub/pipe2.php"));

    let index = success(&run_fast(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["files_seen"], 1, "{index}");
    assert_eq!(index["index"]["coverage"]["complete"], true);
    for args in queries("App\\shared") {
        success(&run_fast(root, &args));
    }
    let error = failure(&run_fast(root, &["symbol", "pipe.php:1", "--json"]), 4);
    assert_eq!(error["error"], "symbol_not_found");

    for rel in [".gitignore", "sub/.gitignore", ".git/info/exclude"] {
        mkfifo(&root.join(rel));
        let error = failure(&run_fast(root, &["index", "--json"]), 3);
        assert_eq!(error["error"], "repository_unavailable", "{rel}");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains(rel) && message.contains("not a regular file"),
            "{rel}: {message}"
        );
        for args in queries("App\\shared") {
            failure(&run_fast(root, &args), 3);
        }
        fs::remove_file(root.join(rel)).expect("remove FIFO");
    }

    // With the FIFOs gone the same tree indexes to the same snapshot.
    let again = success(&run_fast(root, &["index", "--json"]));
    assert_eq!(again["index"], index["index"]);
}

// ---------------------------------------------------------------------------
// CRLF and non-ASCII coordinates
// ---------------------------------------------------------------------------

/// CRLF stays two bytes, lines are one-based and inclusive, and columns are
/// one-based UTF-8 byte columns, for symbols, references, and `file:line`.
/// The expected numbers are counted by hand in the comments.
#[test]
fn crlf_and_non_ascii_source_report_utf8_byte_coordinates() {
    let temp = git_repo("coordinates");
    let root = temp.path();
    // Line 1 `<?php\r\n` bytes 0-6; line 2 `namespace App;\r\n` 7-22; line 3
    // `\r\n` 23-24; line 4 starts at 25: `function café(): void\r\n` (é is two
    // bytes, 24 bytes with CRLF); line 5 `{\r\n` 49-51; line 6 `}` at 52, so
    // the function is [25, 53); line 7 `\r\n` 55-56; line 8 starts at 57 with
    // `/* é日本 */ ` (3 + 2 + 6 + 4 = 15 bytes), so the call is at byte 72,
    // column 16, and `café` is 5 bytes: [72, 77).
    let crlf = "<?php\r\nnamespace App;\r\n\r\nfunction café(): void\r\n{\r\n}\r\n\r\n/* é日本 */ café();\r\n";
    write(root, "crlf.php", crlf.as_bytes());
    // Line 3 starts at byte 21: `$s = "日本語"; ` is 6 + 9 + 3 = 18 bytes, so
    // the call is at 39, column 19; line 4 starts at 46.
    let utf8 = "<?php\nnamespace App;\n$s = \"日本語\"; fn2();\nfunction fn2(): void {}\n";
    write(root, "u.php", utf8.as_bytes());
    // A non-ASCII path and name, kept as UTF-8 rather than escaped.
    let path_php = "<?php\nnamespace App;\nfunction ünï(): void {}\n";
    write(root, "src/日本/ü.php", path_php.as_bytes());

    let symbol = success(&run(root, &["symbol", "App\\café", "--source", "--json"]));
    let expected = [
        ("id", json!("crlf.php#App\\café")),
        ("start_byte", json!(25)),
        ("end_byte", json!(53)),
        ("start_line", json!(4)),
        ("end_line", json!(6)),
    ];
    for (key, value) in &expected {
        assert_eq!(&symbol["symbol"][key], value, "{key}: {symbol}");
    }
    assert_eq!(symbol["source"], "function café(): void\r\n{\r\n}");
    assert_eq!(
        &crlf.as_bytes()[25..53],
        "function café(): void\r\n{\r\n}".as_bytes()
    );

    let refs = success(&run(root, &["refs", "App\\café", "--json"]));
    assert_eq!(refs["total"], 1, "{refs}");
    let reference = &refs["references"][0];
    for (key, value) in [
        ("start_byte", json!(72)),
        ("end_byte", json!(77)),
        ("line", json!(8)),
        ("column", json!(16)),
        ("resolution", json!("exact")),
    ] {
        assert_eq!(reference[key], value, "{key}: {reference}");
    }
    assert_eq!(&crlf.as_bytes()[72..77], "café".as_bytes());

    // Every line of the CRLF body resolves to the function; line 7 to none.
    for line in 4..=6 {
        let found = success(&run(
            root,
            &["symbol", &format!("crlf.php:{line}"), "--json"],
        ));
        assert_eq!(found["symbol"]["id"], "crlf.php#App\\café", "line {line}");
    }

    let fn2 = success(&run(root, &["refs", "App\\fn2", "--json"]));
    assert_eq!(fn2["symbol"]["start_byte"], 46);
    assert_eq!(fn2["symbol"]["start_line"], 4);
    let reference = &fn2["references"][0];
    assert_eq!(
        (
            &reference["start_byte"],
            &reference["end_byte"],
            &reference["line"],
            &reference["column"]
        ),
        (&json!(39), &json!(42), &json!(3), &json!(19)),
        "{reference}"
    );
    assert_eq!(&utf8.as_bytes()[39..42], b"fn2");

    let unicode = success(&run(root, &["symbol", "App\\ünï", "--json"]));
    assert_eq!(unicode["symbol"]["id"], "src/日本/ü.php#App\\ünï");
    assert_eq!(unicode["symbol"]["file"], "src/日本/ü.php");
    let by_id = success(&run(root, &["symbol", "src/日本/ü.php#App\\ünï", "--json"]));
    assert_eq!(by_id["symbol"], unicode["symbol"]);
    let by_line = success(&run(root, &["symbol", "src/日本/ü.php:3", "--json"]));
    assert_eq!(by_line["symbol"], unicode["symbol"]);

    // Context source is the stored bytes, CRLF included.
    let context = success(&run(
        root,
        &["context", "App\\café", "--tokens", "4000", "--json"],
    ));
    assert_eq!(
        context["segments"][0]["source"],
        "function café(): void\r\n{\r\n}"
    );
}

// ---------------------------------------------------------------------------
// Permission denied
// ---------------------------------------------------------------------------

/// An unreadable directory fails refresh with exit 3 for every command rather
/// than silently dropping its files; the committed snapshot stays readable with
/// `--no-refresh`. (An unreadable *file* is covered by
/// `failure_boundaries::an_unreadable_file_fails_refresh_instead_of_keeping_old_facts`.)
#[cfg(unix)]
#[test]
fn an_unreadable_directory_fails_refresh_with_exit_3() {
    use std::os::unix::fs::PermissionsExt;

    let temp = git_repo("unreadable-dir");
    let root = temp.path();
    write(root, "a.php", SHARED_PHP);
    write(root, "locked/b.php", EXTRA_PHP);
    let before = success(&run(root, &["index", "--json"]));
    let locked = root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore");
        eprintln!("skipping: a mode-000 directory is still readable (running as root?)");
        return;
    }

    let mut commands: Vec<Vec<&str>> = vec![vec!["index", "--json"]];
    commands.extend(queries("App\\extra"));
    for args in &commands {
        let error = failure(&run(root, args), 3);
        assert_eq!(error["error"], "repository_unavailable", "{args:?}");
        assert!(
            error["message"].as_str().unwrap().contains("locked"),
            "{args:?}: {error}"
        );
    }
    let cached = success(&run(
        root,
        &["symbol", "App\\extra", "--no-refresh", "--json"],
    ));
    assert_eq!(cached["index"]["snapshot"], before["index"]["snapshot"]);
    assert_eq!(cached["index"]["freshness"], "cached");

    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("restore");
    let after = success(&run(root, &["index", "--json"]));
    assert_eq!(after["index"], before["index"]);
}
