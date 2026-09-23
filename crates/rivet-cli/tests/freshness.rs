//! T36 acceptance matrix, "Freshness" and "Cache invalidation": the cases the
//! earlier suites did not drive end to end, namely a real Git branch switch
//! and Git ignore-rule changes (spec §§12.4, 26; ARCHITECTURE "Refresh and
//! invalidation").
//!
//! The other cases of both rows live in `refresh.rs`, `reresolve.rs`,
//! `freshness_modes.rs`, and `coverage_honesty.rs`; see `ACCEPTANCE.md`.

mod support;

use std::fs;
use std::path::Path;

use serde_json::Value;
use support::{TempDir, failure, git, git_repo, run, success, write};

/// Recursively copies the working tree at `from` into `to`, skipping `.git`
/// and `.rivet`.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).expect("read tree") {
        let entry = entry.expect("entry");
        let name = entry.file_name();
        if name == ".git" || name == ".rivet" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().expect("file type").is_dir() {
            fs::create_dir_all(&target).expect("create dir");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The navigation answers compared across a branch switch.
fn branch_queries() -> Vec<Vec<&'static str>> {
    vec![
        vec!["symbol", "App\\alpha", "--source", "--json"],
        vec!["refs", "App\\beta", "--mode", "candidates", "--json"],
        vec!["refs", "App\\Widget::run", "--json"],
        vec!["context", "App\\alpha", "--json"],
        vec!["symbol", "App\\gamma", "--json"],
        vec!["symbol", "App\\old", "--json"],
    ]
}

fn branch_answers(root: &Path) -> Vec<(Option<i32>, Vec<u8>, Vec<u8>)> {
    branch_queries()
        .iter()
        .map(|args| {
            let output = run(root, args);
            (output.status.code(), output.stdout, output.stderr)
        })
        .collect()
}

/// A real `git checkout` between branches that edit (same size), add, delete,
/// and move files is picked up by the next query with no explicit index, and
/// the answers equal a cold build of the checked-out tree; switching back
/// restores the first branch's answers byte for byte.
#[test]
fn a_git_branch_switch_is_reflected_by_the_next_query() {
    let home = TempDir::new("branch-home");
    let temp = TempDir::new("branch");
    let root = temp.path();
    // main: alpha calls beta; Widget::run is called through a typed receiver.
    write(
        root,
        "a.php",
        b"<?php\nnamespace App;\nfunction alpha(Widget $w): void { beta(); $w->run(); }\n",
    );
    write(
        root,
        "b.php",
        b"<?php\nnamespace App;\nfunction beta(): void {}\n",
    );
    write(
        root,
        "w.php",
        b"<?php\nnamespace App;\nclass Widget { public function run(): void {} }\n",
    );
    write(
        root,
        "old.php",
        b"<?php\nnamespace App;\nfunction old(): void {}\n",
    );
    git(root, home.path(), &["init", "-q"]);
    git(root, home.path(), &["add", "-A"]);
    git(root, home.path(), &["commit", "-q", "-m", "main"]);

    git(root, home.path(), &["checkout", "-q", "-b", "feature"]);
    // Same-size edit: `beta` becomes `gamm` would change the size, so the
    // call target is swapped for another four-letter name declared elsewhere.
    write(
        root,
        "a.php",
        b"<?php\nnamespace App;\nfunction alpha(Widget $w): void { gama(); $w->run(); }\n",
    );
    fs::remove_file(root.join("old.php")).expect("delete old.php");
    write(
        root,
        "g.php",
        b"<?php\nnamespace App;\nfunction gama(): void {}\nfunction gamma(): void {}\n",
    );
    fs::create_dir_all(root.join("lib")).expect("lib");
    fs::rename(root.join("w.php"), root.join("lib/w.php")).expect("move w.php");
    git(root, home.path(), &["add", "-A"]);
    git(root, home.path(), &["commit", "-q", "-m", "feature"]);

    git(root, home.path(), &["checkout", "-q", "main"]);
    let main_answers = branch_answers(root);
    // On main, alpha calls beta, gamma does not exist, old does.
    let beta = success(&run(root, &["refs", "App\\beta", "--json"]));
    assert_eq!(beta["total"], 1);
    assert_eq!(main_answers[4].0, Some(4), "gamma is absent on main");
    assert_eq!(main_answers[5].0, Some(0), "old exists on main");

    git(root, home.path(), &["checkout", "-q", "feature"]);
    let feature_answers = branch_answers(root);
    let beta = success(&run(root, &["refs", "App\\beta", "--json"]));
    assert_eq!(beta["total"], 0, "the feature branch no longer calls beta");
    let gama = success(&run(root, &["refs", "App\\gama", "--json"]));
    assert_eq!(gama["total"], 1);
    assert_eq!(gama["references"][0]["resolution"], "exact");
    let run_refs = success(&run(root, &["refs", "App\\Widget::run", "--json"]));
    assert_eq!(
        run_refs["symbol"]["file"], "lib/w.php",
        "the moved file's new ID"
    );
    assert_eq!(run_refs["references"][0]["resolution"], "scoped");
    assert_eq!(feature_answers[4].0, Some(0), "gamma exists on feature");
    assert_eq!(feature_answers[5].0, Some(4), "old is deleted on feature");

    // Each branch's answers equal a cold build of that tree.
    let cold = TempDir::new("branch-cold");
    fs::create_dir_all(cold.path().join(".git")).expect("cold .git");
    copy_tree(root, cold.path());
    assert_eq!(branch_answers(cold.path()), feature_answers);

    git(root, home.path(), &["checkout", "-q", "main"]);
    assert_eq!(
        branch_answers(root),
        main_answers,
        "switching back restores main"
    );
}

/// The coverage of a response.
fn files_seen(value: &Value) -> u64 {
    value["index"]["coverage"]["files_seen"]
        .as_u64()
        .expect("files_seen")
}

/// Adding a root `.gitignore` rule, a nested `.gitignore` rule, or a
/// `.git/info/exclude` rule removes the matched file's facts on the next
/// query; removing the rule restores them; `respect_gitignore = false` in the
/// configuration re-admits them while the rule is present.
#[test]
fn git_ignore_rule_changes_remove_and_restore_facts() {
    let temp = git_repo("ignore-invalidation");
    let root = temp.path();
    write(
        root,
        "keep.php",
        b"<?php\nnamespace App;\nfunction keep(): void { gone(); }\n",
    );
    write(
        root,
        "sub/gone.php",
        b"<?php\nnamespace App;\nfunction gone(): void {}\n",
    );

    let before = success(&run(root, &["refs", "App\\gone", "--json"]));
    assert_eq!(files_seen(&before), 2);
    assert_eq!(before["references"][0]["resolution"], "exact");

    for (rule_file, rule) in [
        (".gitignore", "sub/gone.php\n"),
        ("sub/.gitignore", "gone.php\n"),
        (".git/info/exclude", "sub/gone.php\n"),
    ] {
        write(root, rule_file, rule.as_bytes());
        let error = failure(&run(root, &["refs", "App\\gone", "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{rule_file}");
        // The use in keep.php survives with nothing left to bind to.
        let keep = success(&run(root, &["symbol", "App\\keep", "--json"]));
        assert!(
            keep["calls"]["items"][0]["resolved_target"].is_null(),
            "{rule_file}: {keep}"
        );
        // The ignored file is not seen; an ignore file in the tree is itself
        // an eligible (unsupported) file, `.git/info/exclude` is not.
        let expected_seen = if rule_file.starts_with(".git/") { 1 } else { 2 };
        assert_eq!(files_seen(&keep), expected_seen, "{rule_file}");

        // Git rules off: the file is back while the rule is still present.
        write(
            root,
            ".rivet/config.toml",
            b"[index]\nrespect_gitignore = false\n",
        );
        let admitted = success(&run(root, &["refs", "App\\gone", "--json"]));
        assert_eq!(
            admitted["references"][0]["resolution"], "exact",
            "{rule_file}"
        );
        fs::remove_file(root.join(".rivet/config.toml")).expect("remove config");

        fs::remove_file(root.join(rule_file)).expect("remove rule");
        let restored = success(&run(root, &["refs", "App\\gone", "--json"]));
        assert_eq!(restored["index"], before["index"], "{rule_file}");
        assert_eq!(restored["references"], before["references"], "{rule_file}");
    }
}
