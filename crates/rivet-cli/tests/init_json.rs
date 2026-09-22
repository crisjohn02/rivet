//! Integration tests for `rivet init` (T33a).
//!
//! These drive the built binary and assert exact JSON bytes and exact file
//! contents: root selection (spec §25), the default config and its
//! equivalence to having no config (spec §22), idempotence, `.gitignore`
//! handling, and guarded writes that reject symlinks and leave a rejected
//! tree untouched (spec §27).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rivet_core::Config;
use rusqlite::Connection;
use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The spec §22 template, copied here literally so a drift in either the
/// spec-derived constant or the file `init` writes is caught.
const SPEC_TEMPLATE: &str = r#"[index]
respect_gitignore = true
exclude = ["vendor/**", "node_modules/**", "dist/**", "build/**"]
max_file_size_kb = 1024
freshness = "content"                 # content | metadata

[languages]
enabled = ["php", "typescript"]

[context]
default_token_budget = 4000
include_tests = true
test_globs = ["tests/**", "**/*.test.ts", "**/*.spec.ts"]
max_depth = 2
collapse = "auto"

[output]
default_limit = 50
"#;

/// The success object of a first run in a repository without `.gitignore`.
const FIRST_RUN_CREATES_ALL: &str = "{\"schema_version\":1,\"created\":[\".gitignore\",\".rivet/\",\".rivet/config.toml\"],\"modified\":[],\"snippet_file\":null}\n";

/// The success object of a run that changed nothing.
const NOTHING_CHANGED: &str =
    "{\"schema_version\":1,\"created\":[],\"modified\":[],\"snippet_file\":null}\n";

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
            "rivet-cli-init-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        TempDir {
            path: path
                .canonicalize()
                .expect("canonicalize temporary directory"),
        }
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

/// A temporary directory that is a Git root (has `.git` but no `.rivet/`).
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

/// Runs `rivet init --json` in `dir`, asserts success with an empty stderr,
/// and returns stdout as a string.
fn init_ok(dir: &Path) -> String {
    let output = run(dir, &["init", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "stderr must stay empty");
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs `rivet init --json` in `dir`, asserts the documented failure shape,
/// and returns the parsed error object.
fn init_err(dir: &Path, exit: i32) -> Value {
    let output = run(dir, &["init", "--json"]);
    assert_eq!(output.status.code(), Some(exit), "{output:?}");
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    serde_json::from_slice(&output.stderr).expect("stderr is one JSON object")
}

/// Runs `rivet index --json` in `dir` and returns the parsed success object.
fn index_ok(dir: &Path) -> Value {
    let output = run(dir, &["index", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("stdout is one JSON object")
}

fn read(root: &Path, rel: &str) -> Vec<u8> {
    fs::read(root.join(rel)).expect("read file")
}

/// Every entry under `root`, recursively, without following symlinks: its
/// kind, its bytes or link target, and its Unix mode.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).expect("stat");
            let mode = mode_of(&metadata);
            let description = if metadata.file_type().is_symlink() {
                format!(
                    "link {mode:o} -> {}",
                    fs::read_link(&path).expect("read link").display()
                )
            } else if metadata.is_dir() {
                visit(root, &path, out);
                format!("dir {mode:o}")
            } else {
                format!("file {mode:o} {:?}", fs::read(&path).expect("read"))
            };
            out.insert(rel, description);
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[cfg(unix)]
fn mode_of(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(_metadata: &fs::Metadata) -> u32 {
    0
}

/// Reads one `meta` value from a committed index.
fn meta(root: &Path, key: &str) -> String {
    let conn = Connection::open(root.join(".rivet/index.db")).expect("open index");
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .expect("read meta")
}

#[test]
fn first_run_in_git_repository_creates_everything() {
    let temp = git_repo("first-git");
    let root = temp.path();

    assert_eq!(init_ok(root), FIRST_RUN_CREATES_ALL);
    assert!(root.join(".rivet").is_dir());
    assert_eq!(read(root, ".rivet/config.toml"), SPEC_TEMPLATE.as_bytes());
    assert_eq!(read(root, ".gitignore"), b".rivet/\n");
}

#[test]
fn written_config_is_the_spec_template_literally() {
    assert_eq!(Config::default_toml(), SPEC_TEMPLATE);
}

#[test]
fn existing_gitignore_is_reported_modified() {
    let temp = git_repo("modified-gitignore");
    let root = temp.path();
    write(root, ".gitignore", b"vendor/\n");

    assert_eq!(
        init_ok(root),
        "{\"schema_version\":1,\"created\":[\".rivet/\",\".rivet/config.toml\"],\"modified\":[\".gitignore\"],\"snippet_file\":null}\n"
    );
    assert_eq!(read(root, ".gitignore"), b"vendor/\n.rivet/\n");
}

#[test]
fn first_run_outside_git_establishes_the_current_directory() {
    let outer = TempDir::new("outside-git");
    let cwd = outer.path().join("project");
    fs::create_dir_all(&cwd).unwrap();

    assert_eq!(init_ok(&cwd), FIRST_RUN_CREATES_ALL);
    assert!(cwd.join(".rivet").is_dir());
    assert_eq!(read(&cwd, ".rivet/config.toml"), SPEC_TEMPLATE.as_bytes());
    assert_eq!(read(&cwd, ".gitignore"), b".rivet/\n");
    assert!(!outer.path().join(".rivet").exists());

    // The established root is now discoverable by queries from below it.
    write(&cwd, "src/a.php", b"<?php\nfunction f() {}\n");
    let nested = cwd.join("src");
    let value = index_ok(&nested);
    assert_eq!(value["symbols"], 1);
    assert!(cwd.join(".rivet/index.db").is_file());
}

#[test]
fn nested_directory_resolves_to_the_repository_root() {
    let temp = git_repo("nested");
    let root = temp.path();
    let nested = root.join("src/deep");
    fs::create_dir_all(&nested).unwrap();

    assert_eq!(init_ok(&nested), FIRST_RUN_CREATES_ALL);
    assert!(root.join(".rivet/config.toml").is_file());
    assert!(root.join(".gitignore").is_file());
    assert!(!nested.join(".rivet").exists());
    assert!(!nested.join(".gitignore").exists());
    assert!(!root.join("src/.rivet").exists());
}

#[test]
fn inner_worktree_git_file_is_the_root() {
    let temp = git_repo("worktree");
    let inner = temp.path().join("worktrees/feature");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join(".git"), "gitdir: ../../.git/worktrees/feature\n").unwrap();

    assert_eq!(init_ok(&inner), FIRST_RUN_CREATES_ALL);
    assert!(inner.join(".rivet/config.toml").is_file());
    assert!(!temp.path().join(".rivet").exists());
    assert!(!temp.path().join(".gitignore").exists());
}

#[test]
fn repeated_runs_change_nothing() {
    let temp = git_repo("repeat");
    let root = temp.path();
    write(root, ".gitignore", b"node_modules/\r\nlogs");

    init_ok(root);
    let after_first = tree(root);
    assert_eq!(
        read(root, ".gitignore"),
        b"node_modules/\r\nlogs\r\n.rivet/\r\n"
    );

    for _ in 0..3 {
        assert_eq!(init_ok(root), NOTHING_CHANGED);
        assert_eq!(tree(root), after_first);
    }
}

#[test]
fn equivalent_gitignore_spellings_are_left_untouched() {
    // Each of these makes Git ignore the root `.rivet/` directory.
    for (label, contents) in [
        ("bare", &b".rivet\n"[..]),
        ("slash", b".rivet/\n"),
        ("anchored", b"/.rivet\n"),
        ("anchored-slash", b"/.rivet/\n"),
        ("crlf-no-final-newline", b"vendor/\r\n/.rivet/"),
        ("trailing-spaces", b".rivet/   \n"),
        ("broader-glob", b".*\n!.gitignore\n"),
    ] {
        let temp = git_repo(&format!("spelling-{label}"));
        let root = temp.path();
        write(root, ".gitignore", contents);

        assert_eq!(
            init_ok(root),
            "{\"schema_version\":1,\"created\":[\".rivet/\",\".rivet/config.toml\"],\"modified\":[],\"snippet_file\":null}\n",
            "{label}"
        );
        assert_eq!(read(root, ".gitignore"), contents, "{label}");
    }
}

#[test]
fn non_equivalent_gitignore_rules_get_the_entry() {
    for (label, contents, expected) in [
        ("comment", &b"# .rivet/\n"[..], &b"# .rivet/\n.rivet/\n"[..]),
        (
            "file-only",
            b".rivet/index.db\n",
            b".rivet/index.db\n.rivet/\n",
        ),
        ("other-dir", b"sub/.rivet/\n", b"sub/.rivet/\n.rivet/\n"),
        (
            "negated",
            b".rivet/\n!.rivet/\n",
            b".rivet/\n!.rivet/\n.rivet/\n",
        ),
        ("empty", b"", b".rivet/\n"),
        ("no-final-newline", b"dist", b"dist\n.rivet/\n"),
    ] {
        let temp = git_repo(&format!("append-{label}"));
        let root = temp.path();
        write(root, ".gitignore", contents);

        let stdout = init_ok(root);
        assert!(
            stdout.contains("\"modified\":[\".gitignore\"]"),
            "{label}: {stdout}"
        );
        assert_eq!(read(root, ".gitignore"), expected, "{label}");
        // And the appended file is stable on the next run.
        assert_eq!(init_ok(root), NOTHING_CHANGED, "{label}");
        assert_eq!(read(root, ".gitignore"), expected, "{label}");
    }
}

#[test]
fn edited_config_is_preserved() {
    let temp = git_repo("edited-config");
    let root = temp.path();
    init_ok(root);

    let edited = b"# mine\n[index]\nmax_file_size_kb = 2048   # bigger\n";
    fs::write(root.join(".rivet/config.toml"), edited).unwrap();
    assert_eq!(init_ok(root), NOTHING_CHANGED);
    assert_eq!(read(root, ".rivet/config.toml"), edited);
}

#[test]
fn invalid_existing_config_is_preserved_not_rewritten() {
    let temp = git_repo("invalid-config");
    let root = temp.path();
    write(root, ".gitignore", b".rivet/\n");
    write(root, ".rivet/config.toml", b"[nonsense\n");

    assert_eq!(init_ok(root), NOTHING_CHANGED);
    assert_eq!(read(root, ".rivet/config.toml"), b"[nonsense\n");
}

#[test]
fn existing_cache_dir_without_config_gets_only_the_config() {
    let temp = git_repo("cache-only");
    let root = temp.path();
    write(root, "a.php", b"<?php\nfunction f() {}\n");
    // `index` auto-creates `.rivet/` at a Git root and never edits .gitignore.
    index_ok(root);
    assert!(!root.join(".rivet/config.toml").exists());

    assert_eq!(
        init_ok(root),
        "{\"schema_version\":1,\"created\":[\".gitignore\",\".rivet/config.toml\"],\"modified\":[],\"snippet_file\":null}\n"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_rivet_dir_is_rejected_and_tree_unchanged() {
    let temp = git_repo("symlink-rivet");
    let root = temp.path();
    let outside = TempDir::new("symlink-rivet-target");
    std::os::unix::fs::symlink(outside.path(), root.join(".rivet")).unwrap();
    let before = tree(root);

    let error = init_err(root, 3);
    assert_eq!(error["error"], "repository_unavailable");
    assert!(
        error["message"].as_str().unwrap().contains(".rivet"),
        "{error}"
    );
    assert_eq!(tree(root), before);
    assert!(!root.join(".gitignore").exists());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn symlinked_config_is_rejected_and_tree_unchanged() {
    for dangling in [false, true] {
        let temp = git_repo("symlink-config");
        let root = temp.path();
        fs::create_dir_all(root.join(".rivet")).unwrap();
        let target = root.join("elsewhere.toml");
        if !dangling {
            fs::write(&target, b"[index]\n").unwrap();
        }
        std::os::unix::fs::symlink(&target, root.join(".rivet/config.toml")).unwrap();
        let before = tree(root);

        let error = init_err(root, 3);
        assert_eq!(error["error"], "repository_unavailable");
        assert!(
            error["message"].as_str().unwrap().contains("config.toml"),
            "{error}"
        );
        assert_eq!(tree(root), before, "dangling={dangling}");
        assert!(!root.join(".gitignore").exists());
        assert_eq!(target.exists(), !dangling);
    }
}

#[cfg(unix)]
#[test]
fn symlinked_gitignore_is_rejected_and_tree_unchanged() {
    for dangling in [false, true] {
        let temp = git_repo("symlink-gitignore");
        let root = temp.path();
        let outside = TempDir::new("symlink-gitignore-target");
        let target = outside.path().join("shared-gitignore");
        if !dangling {
            fs::write(&target, b"vendor/\n").unwrap();
        }
        std::os::unix::fs::symlink(&target, root.join(".gitignore")).unwrap();
        let before = tree(root);
        let target_before = tree(outside.path());

        let error = init_err(root, 3);
        assert_eq!(error["error"], "repository_unavailable");
        assert!(
            error["message"].as_str().unwrap().contains(".gitignore"),
            "{error}"
        );
        // Nothing was written, not even `.rivet/`, which is validated first
        // but written only after every destination passes.
        assert_eq!(tree(root), before, "dangling={dangling}");
        assert!(!root.join(".rivet").exists());
        assert_eq!(tree(outside.path()), target_before);
    }
}

#[test]
fn wrong_kind_destinations_are_rejected_and_tree_unchanged() {
    // `.rivet` as a regular file at a Git root.
    let temp = git_repo("rivet-file");
    let root = temp.path();
    write(root, ".rivet", b"not a directory");
    let before = tree(root);
    assert_eq!(init_err(root, 3)["error"], "repository_unavailable");
    assert_eq!(tree(root), before);

    // `config.toml` as a directory.
    let temp = git_repo("config-dir");
    let root = temp.path();
    fs::create_dir_all(root.join(".rivet/config.toml")).unwrap();
    let before = tree(root);
    assert_eq!(init_err(root, 3)["error"], "repository_unavailable");
    assert_eq!(tree(root), before);

    // `.gitignore` as a directory.
    let temp = git_repo("gitignore-dir");
    let root = temp.path();
    fs::create_dir_all(root.join(".gitignore")).unwrap();
    let before = tree(root);
    assert_eq!(init_err(root, 3)["error"], "repository_unavailable");
    assert_eq!(tree(root), before);
    assert!(!root.join(".rivet").exists());
}

#[cfg(unix)]
#[test]
fn gitignore_permissions_are_preserved() {
    use std::os::unix::fs::PermissionsExt;

    for mode in [0o600, 0o640, 0o664] {
        let temp = git_repo("gitignore-mode");
        let root = temp.path();
        write(root, ".gitignore", b"vendor/\n");
        fs::set_permissions(root.join(".gitignore"), fs::Permissions::from_mode(mode)).unwrap();

        let stdout = init_ok(root);
        assert!(stdout.contains("\"modified\":[\".gitignore\"]"), "{stdout}");
        let after = fs::metadata(root.join(".gitignore")).unwrap();
        assert_eq!(after.permissions().mode() & 0o7777, mode);
        assert_eq!(read(root, ".gitignore"), b"vendor/\n.rivet/\n");
    }
}

#[test]
fn written_config_is_equivalent_to_no_config() {
    let with_file = git_repo("equivalent-with");
    let without_file = git_repo("equivalent-without");
    init_ok(with_file.path());

    let loaded = Config::load(with_file.path()).expect("load written config");
    let absent = Config::load(without_file.path()).expect("load absent config");
    assert!(!without_file.path().join(".rivet/config.toml").exists());
    assert_eq!(loaded, absent);
    assert_eq!(loaded, Config::default());
    assert_eq!(loaded.fingerprint(), absent.fingerprint());
}

#[test]
fn index_after_init_matches_index_without_init() {
    let files: [(&str, &[u8]); 3] = [
        (
            "src/Service.php",
            b"<?php\nnamespace App;\nclass Service { public function run() { helper(); } }\n",
        ),
        ("src/helper.php", b"<?php\nfunction helper() {}\n"),
        ("README.md", b"# readme\n"),
    ];

    // `init` creates `.gitignore`, and an unsupported file's row carries its
    // path in the digest, so the comparison repository has the same bytes.
    let initialized = git_repo("digest-init");
    let plain = git_repo("digest-plain");
    for (rel, contents) in files {
        write(initialized.path(), rel, contents);
        write(plain.path(), rel, contents);
    }
    init_ok(initialized.path());
    write(
        plain.path(),
        ".gitignore",
        &read(initialized.path(), ".gitignore"),
    );
    assert!(!plain.path().join(".rivet").exists());

    let first = index_ok(initialized.path());
    let second = index_ok(plain.path());
    assert!(!plain.path().join(".rivet/config.toml").exists());
    assert_eq!(first["index"]["snapshot"], second["index"]["snapshot"]);
    assert_eq!(first["symbols"], second["symbols"]);
    assert_eq!(
        meta(initialized.path(), "effective_config_fingerprint"),
        meta(plain.path(), "effective_config_fingerprint")
    );
    assert_eq!(
        meta(initialized.path(), "effective_config_fingerprint"),
        Config::default().fingerprint()
    );

    // Running `init` after an index does not force a different snapshot.
    let late = git_repo("digest-late");
    for (rel, contents) in files {
        write(late.path(), rel, contents);
    }
    write(late.path(), ".gitignore", b".rivet/\n");
    let before = index_ok(late.path());
    assert_eq!(
        init_ok(late.path()),
        "{\"schema_version\":1,\"created\":[\".rivet/config.toml\"],\"modified\":[],\"snippet_file\":null}\n"
    );
    let after = index_ok(late.path());
    assert_eq!(before["index"]["snapshot"], after["index"]["snapshot"]);
    assert_eq!(after["updated"], 0);
    assert_eq!(before["index"]["snapshot"], first["index"]["snapshot"]);
}

#[test]
fn snippet_flags_fail_before_filesystem_work() {
    let temp = git_repo("snippet-flags");
    let root = temp.path();
    let before = tree(root);

    let output = run(root, &["init", "--snippet-file", "AGENTS.md", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"], "invalid_arguments");

    for args in [
        &["init", "--write-snippet", "--json"][..],
        &[
            "init",
            "--write-snippet",
            "--snippet-file",
            "CLAUDE.md",
            "--json",
        ],
    ] {
        let output = run(root, args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty());
        let value: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(value["error"], "general");
        assert!(
            value["message"].as_str().unwrap().contains("T33b"),
            "{value}"
        );
    }
    assert_eq!(tree(root), before);
}

#[test]
fn init_rejects_a_positional_argument() {
    let temp = git_repo("positional");
    let output = run(temp.path(), &["init", "extra", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(!temp.path().join(".rivet").exists());
}

#[test]
fn human_output_summarizes_the_work() {
    let temp = git_repo("human");
    let root = temp.path();

    let output = run(root, &["init"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "rivet root: {}\ncreated .gitignore\ncreated .rivet/\ncreated .rivet/config.toml\n",
            root.display()
        )
    );

    let output = run(root, &["init"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "rivet root: {}\nAlready initialized; nothing changed.\n",
            root.display()
        )
    );
}
