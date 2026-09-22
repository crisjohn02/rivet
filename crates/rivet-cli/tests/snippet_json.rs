//! Integration tests for `rivet snippet` and `rivet init --write-snippet`
//! (T33b; spec §25, §27; OUTPUT-CONTRACT "Administrative commands";
//! docs/AGENT-SNIPPET.md).
//!
//! These drive the built binary and assert exact stdout bytes and exact file
//! contents. The shipped block is compared with the fenced block in
//! `docs/AGENT-SNIPPET.md` itself, so the document and the binary cannot
//! drift apart.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

/// The binary under test, supplied by Cargo for integration tests.
const RIVET: &str = env!("CARGO_BIN_EXE_rivet");

/// The canonical snippet document.
const AGENT_SNIPPET_DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/AGENT-SNIPPET.md");

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
            "rivet-cli-snippet-{label}-{}-{nanos}-{unique}",
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

/// A Git root that has already been through `rivet init`, so snippet runs
/// report only the instruction file.
fn initialized_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    let output = run(temp.path(), &["init", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    temp
}

/// A Git root with only `.git`.
fn bare_repo(label: &str) -> TempDir {
    let temp = TempDir::new(label);
    fs::create_dir_all(temp.path().join(".git")).expect("create .git");
    temp
}

/// The fenced ```markdown block of `docs/AGENT-SNIPPET.md`, from the line
/// after the opening fence through the LF before the closing fence.
fn doc_block() -> String {
    let doc = fs::read_to_string(AGENT_SNIPPET_DOC).expect("read docs/AGENT-SNIPPET.md");
    let opening = "\n```markdown\n";
    assert_eq!(
        doc.matches(opening).count(),
        1,
        "exactly one markdown fence"
    );
    let body = &doc[doc.find(opening).unwrap() + opening.len()..];
    let close = body.find("\n```\n").expect("closing fence");
    body[..=close].to_string()
}

fn snippet() -> String {
    doc_block()
}

/// The shipped block without its final LF (what replaces marker to marker).
fn snippet_body() -> String {
    snippet().strip_suffix('\n').unwrap().to_string()
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(RIVET)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run the rivet binary")
}

/// Runs `rivet init --json` with `extra` arguments, asserts success with an
/// empty stderr, and returns stdout.
fn init_ok(dir: &Path, extra: &[&str]) -> String {
    let mut args = vec!["init", "--json"];
    args.extend_from_slice(extra);
    let output = run(dir, &args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "stderr must stay empty");
    String::from_utf8(output.stdout).expect("stdout is UTF-8")
}

/// Runs `rivet init --json` with `extra` arguments, asserts the documented
/// failure shape, and returns the parsed error object.
fn init_err(dir: &Path, extra: &[&str], exit: i32) -> Value {
    let mut args = vec!["init", "--json"];
    args.extend_from_slice(extra);
    let output = run(dir, &args);
    assert_eq!(output.status.code(), Some(exit), "{args:?}: {output:?}");
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let value: Value = serde_json::from_slice(&output.stderr).expect("stderr is one JSON object");
    assert_eq!(value["schema_version"], 1);
    assert!(value["hint"].is_string());
    value
}

/// The success object for a run whose only change is the instruction file.
fn reported(created: &[&str], modified: &[&str], file: &str) -> String {
    let list = |items: &[&str]| {
        items
            .iter()
            .map(|item| format!("\"{item}\""))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "{{\"schema_version\":1,\"created\":[{}],\"modified\":[{}],\"snippet_file\":\"{file}\"}}\n",
        list(created),
        list(modified)
    )
}

fn write(root: &Path, rel: &str, contents: &[u8]) {
    fs::write(root.join(rel), contents).expect("write fixture");
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

/// The root's entry names, sorted.
fn names(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn shipped_snippet_is_the_agent_snippet_doc_block_byte_for_byte() {
    let block = doc_block();
    assert!(block.starts_with("<!-- rivet:start -->\n"), "{block:?}");
    assert!(block.ends_with("\n<!-- rivet:end -->\n"), "{block:?}");
    assert_eq!(rivet_cli::snippet::SNIPPET, block);

    let temp = TempDir::new("doc-equality");
    let output = run(temp.path(), &["snippet"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, block.as_bytes());
}

#[test]
fn snippet_works_outside_a_repository_and_creates_nothing() {
    let temp = TempDir::new("outside");
    let root = temp.path();
    assert!(!root.join(".git").exists());

    let output = run(root, &["snippet"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(output.stdout, snippet().as_bytes());

    let output = run(root, &["snippet", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let expected = format!(
        "{{\"schema_version\":1,\"snippet\":{}}}\n",
        serde_json::to_string(&snippet()).unwrap()
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);

    assert!(names(root).is_empty(), "{:?}", names(root));
}

#[test]
fn snippet_rejects_a_positional_argument() {
    let temp = TempDir::new("snippet-positional");
    let output = run(temp.path(), &["snippet", "extra", "--json"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"], "invalid_arguments");
}

#[test]
fn neither_file_creates_agents_md() {
    let temp = bare_repo("neither");
    let root = temp.path();

    assert_eq!(
        init_ok(root, &["--write-snippet"]),
        reported(
            &[".gitignore", ".rivet/", ".rivet/config.toml", "AGENTS.md"],
            &[],
            "AGENTS.md"
        )
    );
    assert_eq!(read(root, "AGENTS.md"), snippet().as_bytes());
    assert!(!root.join("CLAUDE.md").exists());
}

#[test]
fn sole_existing_file_is_selected() {
    for (existing, other) in [("AGENTS.md", "CLAUDE.md"), ("CLAUDE.md", "AGENTS.md")] {
        let temp = initialized_repo("sole");
        let root = temp.path();
        write(root, existing, b"# Project\n");

        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &[existing], existing)
        );
        assert_eq!(
            read(root, existing),
            format!("# Project\n\n{}", snippet()).as_bytes()
        );
        assert!(!root.join(other).exists(), "{other} must not be created");
    }
}

#[test]
fn both_files_without_snippet_file_is_invalid_arguments() {
    let temp = bare_repo("both");
    let root = temp.path();
    write(root, "AGENTS.md", b"agents\n");
    write(root, "CLAUDE.md", b"claude\n");
    let before = tree(root);

    let error = init_err(root, &["--write-snippet"], 2);
    assert_eq!(error["error"], "invalid_arguments");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("AGENTS.md") && message.contains("CLAUDE.md"));
    assert!(error["hint"].as_str().unwrap().contains("--snippet-file"));
    // Nothing at all was written, not even `.rivet/`.
    assert_eq!(tree(root), before);

    // Human mode also exits 2 and writes nothing.
    let output = run(root, &["init", "--write-snippet"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(tree(root), before);
}

#[test]
fn explicit_snippet_file_overrides_auto_selection() {
    // Both exist: the named one is updated, the other untouched.
    let temp = initialized_repo("explicit-both");
    let root = temp.path();
    write(root, "AGENTS.md", b"agents\n");
    write(root, "CLAUDE.md", b"claude\n");

    assert_eq!(
        init_ok(root, &["--write-snippet", "--snippet-file", "CLAUDE.md"]),
        reported(&[], &["CLAUDE.md"], "CLAUDE.md")
    );
    assert_eq!(
        read(root, "CLAUDE.md"),
        format!("claude\n\n{}", snippet()).as_bytes()
    );
    assert_eq!(read(root, "AGENTS.md"), b"agents\n");

    // Only AGENTS.md exists: an explicit CLAUDE.md is created, not the
    // auto-selected AGENTS.md.
    let temp = initialized_repo("explicit-create");
    let root = temp.path();
    write(root, "AGENTS.md", b"agents\n");

    assert_eq!(
        init_ok(root, &["--write-snippet", "--snippet-file", "CLAUDE.md"]),
        reported(&["CLAUDE.md"], &[], "CLAUDE.md")
    );
    assert_eq!(read(root, "CLAUDE.md"), snippet().as_bytes());
    assert_eq!(read(root, "AGENTS.md"), b"agents\n");

    // Neither exists: an explicit AGENTS.md is created too.
    let temp = initialized_repo("explicit-agents");
    let root = temp.path();
    assert_eq!(
        init_ok(root, &["--write-snippet", "--snippet-file", "AGENTS.md"]),
        reported(&["AGENTS.md"], &[], "AGENTS.md")
    );
    assert_eq!(read(root, "AGENTS.md"), snippet().as_bytes());
}

#[test]
fn other_snippet_file_names_are_rejected_before_filesystem_work() {
    let temp = bare_repo("bad-names");
    let root = temp.path();
    fs::create_dir_all(root.join("sub")).unwrap();
    let before = tree(root);
    let absolute = root.join("AGENTS.md").display().to_string();

    for name in [
        "agents.md",
        "Claude.md",
        "./AGENTS.md",
        "../AGENTS.md",
        "sub/AGENTS.md",
        absolute.as_str(),
        "README.md",
        "AGENTS.md ",
        "",
    ] {
        let error = init_err(root, &["--write-snippet", "--snippet-file", name], 2);
        assert_eq!(error["error"], "invalid_arguments", "{name:?}");
        assert!(
            error["hint"].as_str().unwrap().contains("AGENTS.md"),
            "{name:?}"
        );
        assert_eq!(tree(root), before, "{name:?}");
    }
}

#[test]
fn append_separates_existing_content_by_one_blank_line() {
    let body = snippet();
    for (label, contents, expected) in [
        ("empty", "", body.clone()),
        (
            "terminated",
            "# Rules\n\nBe careful.\n",
            format!("# Rules\n\nBe careful.\n\n{body}"),
        ),
        ("unterminated", "text", format!("text\n\n{body}")),
        ("blank-last", "text\n\n", format!("text\n\n{body}")),
        ("several-blank", "text\n\n\n", format!("text\n\n\n{body}")),
        ("spaces-last", "text\n  \n", format!("text\n  \n{body}")),
        ("only-newline", "\n", format!("\n{body}")),
    ] {
        let temp = initialized_repo(&format!("append-{label}"));
        let root = temp.path();
        write(root, "AGENTS.md", contents.as_bytes());

        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &["AGENTS.md"], "AGENTS.md"),
            "{label}"
        );
        assert_eq!(
            String::from_utf8(read(root, "AGENTS.md")).unwrap(),
            expected,
            "{label}"
        );
        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &[], "AGENTS.md"),
            "{label} repeat"
        );
    }
}

#[test]
fn outdated_block_is_replaced_preserving_surrounding_text() {
    let body = snippet_body();
    for (label, contents, expected) in [
        (
            "middle",
            "# Top\n\n<!-- rivet:start -->\nold advice\n<!-- rivet:end -->\n\n## After\nkeep me\n"
                .to_string(),
            format!("# Top\n\n{body}\n\n## After\nkeep me\n"),
        ),
        (
            "at-eof-without-newline",
            "intro\n<!-- rivet:start -->\nold\n<!-- rivet:end -->".to_string(),
            format!("intro\n{body}\n"),
        ),
        (
            "empty-block",
            "a\n<!-- rivet:start --><!-- rivet:end -->\nb\n".to_string(),
            format!("a\n{body}\nb\n"),
        ),
        (
            "indented-and-trailing",
            "  <!-- rivet:start -->\nx\n<!-- rivet:end --> tail\n".to_string(),
            format!("  {body} tail\n"),
        ),
    ] {
        let temp = initialized_repo(&format!("replace-{label}"));
        let root = temp.path();
        write(root, "AGENTS.md", contents.as_bytes());

        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &["AGENTS.md"], "AGENTS.md"),
            "{label}"
        );
        assert_eq!(
            String::from_utf8(read(root, "AGENTS.md")).unwrap(),
            expected,
            "{label}"
        );
        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &[], "AGENTS.md"),
            "{label} repeat"
        );
        assert_eq!(names(root), [".git", ".gitignore", ".rivet", "AGENTS.md"]);
    }
}

#[test]
fn repeated_write_snippet_changes_no_byte() {
    let temp = bare_repo("repeat");
    let root = temp.path();
    write(root, "CLAUDE.md", b"# Claude\r\nnotes");

    init_ok(root, &["--write-snippet"]);
    let after_first = tree(root);
    let metadata = fs::metadata(root.join("CLAUDE.md")).unwrap();

    for _ in 0..3 {
        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &[], "CLAUDE.md")
        );
        assert_eq!(tree(root), after_first);
        let again = fs::metadata(root.join("CLAUDE.md")).unwrap();
        assert_eq!(again.modified().unwrap(), metadata.modified().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(again.ino(), metadata.ino(), "the file was not rewritten");
        }
    }
    // A plain init after that reports null and leaves the block alone.
    assert_eq!(
        init_ok(root, &[]),
        "{\"schema_version\":1,\"created\":[],\"modified\":[],\"snippet_file\":null}\n"
    );
    assert_eq!(tree(root), after_first);
}

#[test]
fn malformed_or_duplicate_blocks_are_rejected_without_writing() {
    let start = "<!-- rivet:start -->";
    let end = "<!-- rivet:end -->";
    let current = snippet();
    for (label, contents, detail) in [
        (
            "start-without-end",
            format!("a\n{start}\nold\n"),
            "no end marker",
        ),
        (
            "end-without-start",
            format!("a\nold\n{end}\n"),
            "no start marker",
        ),
        (
            "end-before-start",
            format!("{end}\nold\n{start}\n"),
            "before the start marker",
        ),
        (
            "duplicate-blocks",
            format!("{current}\n{current}"),
            "repeated or nested",
        ),
        (
            "nested",
            format!("{start}\n{start}\ninner\n{end}\n{end}\n"),
            "repeated or nested",
        ),
        (
            "repeated-start",
            format!("{start}\n{start}\n{end}\n"),
            "repeated or nested",
        ),
        (
            "repeated-end",
            format!("{start}\n{end}\n{end}\n"),
            "repeated or nested",
        ),
        (
            "mention-in-prose",
            format!("Do not edit below `{start}` by hand.\n"),
            "no end marker",
        ),
    ] {
        // A fresh repository, so the rejection must also prevent `.rivet/`
        // and `.gitignore` from being created.
        let temp = bare_repo(&format!("malformed-{label}"));
        let root = temp.path();
        write(root, "AGENTS.md", contents.as_bytes());
        let before = tree(root);

        let error = init_err(root, &["--write-snippet"], 3);
        assert_eq!(error["error"], "repository_unavailable", "{label}");
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("AGENTS.md"), "{label}: {message}");
        assert!(message.contains(detail), "{label}: {message}");
        assert_eq!(tree(root), before, "{label}");
    }
}

/// CRLF files: the block keeps its exact shipped LF bytes (the recorded
/// snippet hash), while the line endings `init` adds outside the block follow
/// the file's style. A repeat run changes nothing.
#[test]
fn crlf_file_keeps_the_exact_lf_block() {
    let body = snippet();
    let temp = initialized_repo("crlf-append");
    let root = temp.path();
    write(root, "AGENTS.md", b"# Rules\r\nuse tabs");

    init_ok(root, &["--write-snippet"]);
    let expected = format!("# Rules\r\nuse tabs\r\n\r\n{body}");
    assert_eq!(
        String::from_utf8(read(root, "AGENTS.md")).unwrap(),
        expected
    );
    assert_eq!(
        init_ok(root, &["--write-snippet"]),
        reported(&[], &[], "AGENTS.md")
    );
    assert_eq!(
        String::from_utf8(read(root, "AGENTS.md")).unwrap(),
        expected
    );

    // A block that an editor converted to CRLF is not the shipped text, so it
    // is replaced with the LF bytes; the CRLF around it is kept.
    let temp = initialized_repo("crlf-replace");
    let root = temp.path();
    let crlf_block = snippet_body().replace('\n', "\r\n");
    write(
        root,
        "AGENTS.md",
        format!("top\r\n\r\n{crlf_block}\r\nbottom\r\n").as_bytes(),
    );
    assert_eq!(
        init_ok(root, &["--write-snippet"]),
        reported(&[], &["AGENTS.md"], "AGENTS.md")
    );
    let expected = format!("top\r\n\r\n{}\r\nbottom\r\n", snippet_body());
    assert_eq!(
        String::from_utf8(read(root, "AGENTS.md")).unwrap(),
        expected
    );
    assert_eq!(
        init_ok(root, &["--write-snippet"]),
        reported(&[], &[], "AGENTS.md")
    );
    assert_eq!(
        String::from_utf8(read(root, "AGENTS.md")).unwrap(),
        expected
    );
}

#[cfg(unix)]
#[test]
fn symlinked_instruction_file_is_rejected_and_tree_unchanged() {
    for (name, explicit, dangling) in [
        ("AGENTS.md", false, false),
        ("AGENTS.md", false, true),
        ("CLAUDE.md", true, false),
        ("CLAUDE.md", false, true),
    ] {
        let temp = bare_repo("symlink");
        let root = temp.path();
        let outside = TempDir::new("symlink-target");
        let target = outside.path().join("shared.md");
        if !dangling {
            fs::write(&target, b"shared\n").unwrap();
        }
        std::os::unix::fs::symlink(&target, root.join(name)).unwrap();
        let before = tree(root);
        let target_before = tree(outside.path());

        let mut args = vec!["--write-snippet"];
        if explicit {
            args.extend_from_slice(&["--snippet-file", name]);
        }
        let error = init_err(root, &args, 3);
        assert_eq!(error["error"], "repository_unavailable");
        let message = error["message"].as_str().unwrap();
        assert!(
            message.contains("symlink") && message.contains(name),
            "{message}"
        );
        assert_eq!(tree(root), before, "{name} dangling={dangling}");
        assert_eq!(tree(outside.path()), target_before);
    }
}

#[test]
fn non_file_instruction_destination_is_rejected() {
    let temp = bare_repo("dir-dest");
    let root = temp.path();
    fs::create_dir_all(root.join("AGENTS.md")).unwrap();
    let before = tree(root);

    let error = init_err(root, &["--write-snippet"], 3);
    assert_eq!(error["error"], "repository_unavailable");
    assert_eq!(tree(root), before);
}

#[cfg(unix)]
#[test]
fn instruction_file_permissions_are_preserved() {
    use std::os::unix::fs::PermissionsExt;

    for (mode, contents) in [
        (0o600, "notes\n".to_string()),
        (
            0o640,
            "<!-- rivet:start -->\nold\n<!-- rivet:end -->\n".to_string(),
        ),
        (0o664, "x".to_string()),
        (0o444, "read-only\n".to_string()),
    ] {
        let temp = initialized_repo("mode");
        let root = temp.path();
        write(root, "AGENTS.md", contents.as_bytes());
        fs::set_permissions(root.join("AGENTS.md"), fs::Permissions::from_mode(mode)).unwrap();

        assert_eq!(
            init_ok(root, &["--write-snippet"]),
            reported(&[], &["AGENTS.md"], "AGENTS.md"),
            "{mode:o}"
        );
        let after = fs::metadata(root.join("AGENTS.md")).unwrap();
        assert_eq!(after.permissions().mode() & 0o7777, mode, "{mode:o}");
        assert!(
            String::from_utf8(read(root, "AGENTS.md"))
                .unwrap()
                .ends_with(&snippet())
        );
        // No temporary sibling is left behind.
        assert_eq!(names(root), [".git", ".gitignore", ".rivet", "AGENTS.md"]);
    }
}

/// Selection uses exact entry names, identically on case-sensitive and
/// case-insensitive filesystems. A differently cased file is never written
/// through, nor shadowed by a new exact-name file.
#[test]
fn differently_cased_instruction_file_is_refused_not_written_through() {
    // Auto-selection would create AGENTS.md, but `agents.md` exists.
    let temp = bare_repo("case-auto");
    let root = temp.path();
    write(root, "agents.md", b"lowercase\n");
    let before = tree(root);

    let error = init_err(root, &["--write-snippet"], 3);
    assert_eq!(error["error"], "repository_unavailable");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("agents.md") && message.contains("AGENTS.md"));
    assert!(error["hint"].as_str().unwrap().contains("Rename agents.md"));
    assert_eq!(tree(root), before);

    // The same holds for an explicit destination.
    let error = init_err(root, &["--write-snippet", "--snippet-file", "AGENTS.md"], 3);
    assert_eq!(error["error"], "repository_unavailable");
    assert_eq!(tree(root), before);

    // A differently cased file does not count as existing for auto-selection:
    // with an exact CLAUDE.md, CLAUDE.md is the sole existing file.
    let temp = initialized_repo("case-other");
    let root = temp.path();
    write(root, "agents.md", b"lowercase\n");
    write(root, "CLAUDE.md", b"claude\n");
    assert_eq!(
        init_ok(root, &["--write-snippet"]),
        reported(&[], &["CLAUDE.md"], "CLAUDE.md")
    );
    assert_eq!(read(root, "agents.md"), b"lowercase\n");
}

#[test]
fn human_output_names_the_snippet_destination() {
    let temp = bare_repo("human");
    let root = temp.path();

    let output = run(root, &["init", "--write-snippet"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "rivet root: {}\ncreated .gitignore\ncreated .rivet/\ncreated .rivet/config.toml\ncreated AGENTS.md\nsnippet file: AGENTS.md\n",
            root.display()
        )
    );

    let output = run(root, &["init", "--write-snippet"]);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!(
            "rivet root: {}\nAlready initialized; nothing changed.\nsnippet file: AGENTS.md\n",
            root.display()
        )
    );
}
