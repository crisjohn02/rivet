//! T36 acceptance matrix, "Determinism": the same supported snapshot and
//! options give the same query bytes, whether the cache was built cold, by a
//! long incremental history, or by `--force`, and across repeated and
//! concurrent invocations (spec §5.1; OUTPUT-CONTRACT "Transport and common
//! rules": navigation commands are byte-identical for the same snapshot).
//!
//! `init` and `index` describe work performed and are excluded, as the
//! contract says. Extraction is sequential in this build (no parallel
//! extraction exists yet), so "sequential versus parallel extraction" has no
//! second path to compare; concurrent *processes* are compared instead.

mod support;

use std::fs;
use std::path::Path;
use std::process::Output;

use support::{TempDir, copy_fixture, git_repo, run, write};

const EXTRA_PHP: &[u8] = b"<?php\nnamespace App\\Extra;\n\nuse App\\Services\\SurveyService;\n\nfunction extra(SurveyService $svc): void\n{\n    $svc->launch();\n    \\App\\Boot\\launch();\n}\n";

/// The navigation queries compared byte for byte: successes and failures of
/// every query command, with option variants.
fn navigation_queries() -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "symbol",
            "App\\Services\\SurveyService::launch",
            "--source",
            "--json",
        ],
        vec!["symbol", "App\\Services\\SurveyService", "--json"],
        vec!["symbol", "launch", "--json"],
        vec![
            "symbol", "launch", "--limit", "1", "--offset", "1", "--json",
        ],
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
        vec!["refs", "App\\Boot\\launch", "--limit", "1", "--json"],
        vec![
            "refs",
            "App\\Services\\SurveyService",
            "--kind",
            "type,import",
            "--json",
        ],
        vec!["context", "App\\Services\\SurveyService::launch", "--json"],
        vec![
            "context",
            "App\\Reporting\\ReportService::runTyped",
            "--tokens",
            "200",
            "--json",
        ],
        vec![
            "context",
            "App\\Services\\SurveyService",
            "--collapse",
            "always",
            "--depth",
            "1",
            "--json",
        ],
        vec!["context", "App\\Extra\\extra", "--json"],
        vec!["symbol", "App\\Services\\SurveyService::launch", "--json"],
    ]
}

/// The exit code and both streams of each navigation query, in order.
fn answers(root: &Path) -> Vec<(Option<i32>, Vec<u8>, Vec<u8>)> {
    navigation_queries()
        .iter()
        .map(|args| {
            let Output {
                status,
                stdout,
                stderr,
            } = run(root, args);
            (status.code(), stdout, stderr)
        })
        .collect()
}

/// Asserts two answer lists are byte-identical, naming the first difference.
fn assert_same_answers(
    left: &[(Option<i32>, Vec<u8>, Vec<u8>)],
    right: &[(Option<i32>, Vec<u8>, Vec<u8>)],
    what: &str,
) {
    let queries = navigation_queries();
    assert_eq!(left.len(), right.len());
    for (index, (a, b)) in left.iter().zip(right).enumerate() {
        assert!(
            a == b,
            "{what}: {:?} differs\nleft:  {:?} {} {}\nright: {:?} {} {}",
            queries[index],
            a.0,
            String::from_utf8_lossy(&a.1),
            String::from_utf8_lossy(&a.2),
            b.0,
            String::from_utf8_lossy(&b.1),
            String::from_utf8_lossy(&b.2),
        );
    }
}

/// Edits `SurveyService.php` into the final state of the history.
fn final_survey_service(root: &Path) {
    let path = root.join("SurveyService.php");
    let original = fs::read_to_string(&path).expect("read fixture");
    let edited = original.replacen(
        "    public function relaunch(): void",
        "    public function audit(): void\n    {\n        $this->launch();\n    }\n\n    public function relaunch(): void",
        1,
    );
    assert_ne!(edited, original, "fixture shape changed");
    fs::write(path, edited).expect("write edit");
}

/// A long incremental history (edits, a new file, a deletion and restore, a
/// rename and rename back, a parse failure and its repair, a duplicate
/// declaration that appears and goes away), with a query refreshing after each
/// step, ends in exactly the query bytes of a cold build of the final tree, and
/// of `index --force` on the incremental cache.
#[test]
fn a_clean_rebuild_and_an_incremental_history_give_identical_query_bytes() {
    let history = git_repo("determinism-history");
    let root = history.path();
    copy_fixture(root);
    let original_report = fs::read(root.join("ReportService.php")).expect("read report");
    let probe = ["refs", "App\\Services\\SurveyService::launch", "--json"];
    let step = |label: &str| {
        let output = run(root, &probe);
        assert_eq!(output.status.code(), Some(0), "{label}: {output:?}");
    };

    step("cold");
    final_survey_service(root);
    step("edit");
    write(root, "Extra.php", EXTRA_PHP);
    step("add");
    fs::remove_file(root.join("ReportService.php")).expect("delete");
    step("delete");
    write(root, "ReportService.php", &original_report);
    step("restore");
    fs::rename(root.join("boot.php"), root.join("boot2.php")).expect("rename");
    step("rename");
    fs::rename(root.join("boot2.php"), root.join("boot.php")).expect("rename back");
    step("rename back");
    write(
        root,
        "Extra.php",
        b"<?php\nnamespace App\\Extra;\nfunction extra( {\n",
    );
    step("parse failure");
    write(root, "Extra.php", EXTRA_PHP);
    step("repair");
    write(
        root,
        "Dup.php",
        b"<?php\nnamespace App\\Boot;\nfunction launch(): void {}\n",
    );
    step("duplicate");
    fs::remove_file(root.join("Dup.php")).expect("remove duplicate");
    step("duplicate removed");
    let incremental = answers(root);

    // A cold build of the final tree in a different directory.
    let cold = git_repo("determinism-cold");
    copy_fixture(cold.path());
    final_survey_service(cold.path());
    write(cold.path(), "Extra.php", EXTRA_PHP);
    let clean = answers(cold.path());
    assert_same_answers(&incremental, &clean, "incremental versus clean rebuild");

    // Every answer is a real result, not a uniform error.
    assert!(
        clean.iter().filter(|answer| answer.0 == Some(0)).count() >= 12,
        "most navigation queries must succeed"
    );

    // `index --force` on the incremental cache changes no query byte.
    let forced = run(root, &["index", "--force", "--json"]);
    assert_eq!(forced.status.code(), Some(0));
    assert_same_answers(&incremental, &answers(root), "after --force");

    // Neither does deleting the cache entirely.
    fs::remove_dir_all(root.join(".rivet")).expect("remove cache");
    assert_same_answers(&incremental, &answers(root), "after deleting .rivet");
}

/// The same queries repeated, with and without `--no-refresh`, are
/// byte-identical, errors included.
#[test]
fn repeated_queries_are_byte_identical_for_every_query_command() {
    let temp = git_repo("determinism-repeat");
    copy_fixture(temp.path());
    write(temp.path(), "Extra.php", EXTRA_PHP);
    let first = answers(temp.path());
    for round in 0..2 {
        assert_same_answers(&first, &answers(temp.path()), &format!("round {round}"));
    }
    for (args, answer) in navigation_queries().iter().zip(&first) {
        let mut cached_args = args.clone();
        cached_args.insert(cached_args.len() - 1, "--no-refresh");
        let cached = run(temp.path(), &cached_args);
        let cached_again = run(temp.path(), &cached_args);
        assert_eq!(cached.stdout, cached_again.stdout, "{cached_args:?}");
        assert_eq!(cached.stderr, cached_again.stderr, "{cached_args:?}");
        assert_eq!(cached.status.code(), answer.0, "{cached_args:?}");
        // The cached answer differs from the refreshed one only in the
        // freshness label.
        let relabeled = String::from_utf8_lossy(&cached.stdout)
            .replace("\"freshness\":\"cached\"", "\"freshness\":\"content\"")
            + &String::from_utf8_lossy(&cached.stderr)
                .replace("\"freshness\":\"cached\"", "\"freshness\":\"content\"");
        let expected =
            String::from_utf8_lossy(&answer.1).into_owned() + &String::from_utf8_lossy(&answer.2);
        assert_eq!(relabeled, expected, "{cached_args:?}");
    }
}

/// Several processes querying an edited, never-indexed tree at once all
/// succeed, and processes running the same query print the same bytes as a
/// later sequential run: two concurrent queries never see a mixed snapshot.
#[test]
fn concurrent_queries_on_an_unindexed_tree_agree_byte_for_byte() {
    for round in 0..3 {
        let temp: TempDir = git_repo("determinism-concurrent");
        copy_fixture(temp.path());
        write(temp.path(), "Extra.php", EXTRA_PHP);
        let root = temp.path().to_path_buf();
        let queries = [
            vec!["refs", "App\\Services\\SurveyService::launch", "--json"],
            vec![
                "symbol",
                "App\\Services\\SurveyService::launch",
                "--source",
                "--json",
            ],
            vec!["context", "App\\Extra\\extra", "--json"],
        ];
        let handles: Vec<_> = (0..9)
            .map(|index| {
                let root = root.clone();
                let args = queries[index % queries.len()].clone();
                std::thread::spawn(move || (index % 3, run(&root, &args)))
            })
            .collect();
        let outputs: Vec<(usize, Output)> = handles
            .into_iter()
            .map(|handle| handle.join().expect("query thread"))
            .collect();
        for (kind, output) in &outputs {
            assert_eq!(
                output.status.code(),
                Some(0),
                "round {round} query {kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stderr.is_empty());
            let sequential = run(&root, &queries[*kind]);
            assert_eq!(
                output.stdout, sequential.stdout,
                "round {round} query {kind}"
            );
        }
    }
}
