//! T36 acceptance matrix, "Identity": literal `#`/`%` in paths, duplicate
//! declarations, nested definitions on one line, and paths that differ only
//! in case (spec §10; OUTPUT-CONTRACT "Coordinates and symbol objects").
//!
//! Required result: no ID collisions and no arbitrary ambiguity resolution.

mod support;

use std::fs;

use serde_json::Value;
use support::{failure, git_repo, run, success, write};

/// The `(id, file, start_byte)` of every ambiguity candidate, in order.
fn candidates(error: &Value) -> Vec<(String, String, u64)> {
    error["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| {
            (
                candidate["id"].as_str().unwrap().to_string(),
                candidate["file"].as_str().unwrap().to_string(),
                candidate["start_byte"].as_u64().unwrap(),
            )
        })
        .collect()
}

const F_PHP: &[u8] = b"<?php\nnamespace N;\nfunction f(): void {}\n";

/// Literal `#` and `%` in a path are escaped as `%23` and `%25` in the ID
/// component only; the `file` field keeps the literal path. Two paths whose
/// escaped and unescaped spellings could collide (`a#b` and `a%23b`) get
/// distinct IDs, each ID round-trips to its own symbol, and an unescaped
/// spelling of the ID is not guessed at.
#[test]
fn literal_hash_and_percent_in_paths_are_escaped_without_collision() {
    let temp = git_repo("escape");
    let root = temp.path();
    write(root, "a#b/x.php", F_PHP);
    write(root, "a%23b/x.php", F_PHP);
    write(
        root,
        "100%.php",
        b"<?php\nnamespace N;\nfunction pct(): void {}\n",
    );
    write(root, "call.php", b"<?php\nnamespace N;\nf();\npct();\n");

    let error = failure(&run(root, &["symbol", "N\\f", "--json"]), 5);
    // Candidates sort by file bytes: `#` (0x23) before `%` (0x25).
    assert_eq!(
        candidates(&error),
        vec![
            ("a%23b/x.php#N\\f".to_string(), "a#b/x.php".to_string(), 19),
            (
                "a%2523b/x.php#N\\f".to_string(),
                "a%23b/x.php".to_string(),
                19
            ),
        ]
    );

    for (id, file) in [
        ("a%23b/x.php#N\\f", "a#b/x.php"),
        ("a%2523b/x.php#N\\f", "a%23b/x.php"),
    ] {
        let found = success(&run(root, &["symbol", id, "--json"]));
        assert_eq!(found["symbol"]["id"], id);
        assert_eq!(found["symbol"]["file"], file);
        // `file:line` takes the literal repository path.
        let by_line = success(&run(root, &["symbol", &format!("{file}:3"), "--json"]));
        assert_eq!(by_line["symbol"]["id"], id);
        // The call in call.php is not bound to either, since two declarations
        // share the name; both see it only as an unresolved name match.
        let refs = success(&run(root, &["refs", id, "--json"]));
        assert_eq!(refs["symbol"]["id"], id);
        assert_eq!(refs["total"], 1);
        assert_eq!(refs["references"][0]["resolution"], "name_match");
        assert!(refs["references"][0]["resolved_target"].is_null());
    }

    let pct = success(&run(root, &["refs", "N\\pct", "--json"]));
    assert_eq!(pct["symbol"]["id"], "100%25.php#N\\pct");
    assert_eq!(pct["symbol"]["file"], "100%.php");
    assert_eq!(pct["references"][0]["resolved_target"], "100%25.php#N\\pct");
    assert_eq!(pct["references"][0]["resolution"], "exact");

    // Unescaped spellings are not canonical IDs and are never guessed at.
    for query in ["a#b/x.php#N\\f", "100%.php#N\\pct"] {
        let error = failure(&run(root, &["symbol", query, "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{query}");
    }
}

/// Duplicate declarations in one file get one-based file-order ordinals on
/// every member (`#1` included). A short name is ambiguous in candidate
/// order; each ordinal ID round-trips; the unsuffixed ID and out-of-range
/// ordinals find nothing; a call to the duplicated name binds to neither.
#[test]
fn duplicate_declarations_get_distinct_ordinals_and_are_never_picked_arbitrarily() {
    let temp = git_repo("duplicates");
    let root = temp.path();
    let source = "<?php\nnamespace N;\nfunction g(): void {}\nfunction g(): void {}\nclass C { function m() {} function m() {} }\ng();\n";
    write(root, "d.php", source.as_bytes());

    let error = failure(&run(root, &["symbol", "N\\g", "--json"]), 5);
    assert_eq!(
        candidates(&error),
        vec![
            ("d.php#N\\g#1".to_string(), "d.php".to_string(), 19),
            ("d.php#N\\g#2".to_string(), "d.php".to_string(), 41),
        ]
    );
    let error = failure(&run(root, &["symbol", "N\\C::m", "--json"]), 5);
    assert_eq!(
        candidates(&error),
        vec![
            ("d.php#N\\C::m#1".to_string(), "d.php".to_string(), 73),
            ("d.php#N\\C::m#2".to_string(), "d.php".to_string(), 89),
        ]
    );

    for (id, start) in [
        ("d.php#N\\g#1", 19),
        ("d.php#N\\g#2", 41),
        ("d.php#N\\C::m#1", 73),
        ("d.php#N\\C::m#2", 89),
    ] {
        let found = success(&run(root, &["symbol", id, "--json"]));
        assert_eq!(found["symbol"]["id"], id);
        assert_eq!(found["symbol"]["start_byte"], start, "{id}");
    }
    for id in [
        "d.php#N\\g",
        "d.php#N\\g#0",
        "d.php#N\\g#3",
        "d.php#N\\C::m",
    ] {
        let error = failure(&run(root, &["symbol", id, "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{id}");
    }

    // The call `g()` is not bound to either duplicate.
    for id in ["d.php#N\\g#1", "d.php#N\\g#2"] {
        let refs = success(&run(root, &["refs", id, "--json"]));
        assert_eq!(refs["total"], 1, "{id}");
        assert_eq!(refs["by_resolution"]["exact"], 0, "{id}");
        assert!(refs["references"][0]["resolved_target"].is_null(), "{id}");
    }
    // Context on the ambiguous name is the same ambiguity, at offset zero.
    let error = failure(
        &run(root, &["context", "N\\g", "--tokens", "4000", "--json"]),
        5,
    );
    assert_eq!(error["total"], 2);
}

/// Several definitions on one line keep distinct IDs, and `file:line` returns
/// every innermost one instead of picking the shortest. Before T36 the rule
/// compared span lengths only, so `h` (21 bytes) was returned alone although
/// `inner` (25 bytes) is equally innermost on the same line.
#[test]
fn nested_definitions_on_one_line_are_distinct_and_file_line_is_ambiguous() {
    let temp = git_repo("one-line");
    let root = temp.path();
    let source = "<?php\nnamespace N;\nfunction h(): void {} function k(): void { function inner(): void {} }\nclass L { function one() {} }\n";
    write(root, "d.php", source.as_bytes());

    for (name, id) in [
        ("N\\h", "d.php#N\\h"),
        ("N\\k", "d.php#N\\k"),
        ("N\\inner", "d.php#N\\inner"),
        ("N\\L", "d.php#N\\L"),
        ("N\\L::one", "d.php#N\\L::one"),
    ] {
        let found = success(&run(root, &["symbol", name, "--json"]));
        assert_eq!(found["symbol"]["id"], id);
        assert_eq!(found["symbol"]["start_line"], found["symbol"]["end_line"]);
    }

    // Line 3: `h` and `inner` are both innermost; `k` contains `inner`.
    let error = failure(&run(root, &["symbol", "d.php:3", "--json"]), 5);
    assert_eq!(
        candidates(&error)
            .into_iter()
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>(),
        vec!["d.php#N\\h".to_string(), "d.php#N\\inner".to_string()]
    );
    assert_eq!(error["total"], 2);
    // Line 4: the method is the one innermost symbol inside its class.
    let method = success(&run(root, &["symbol", "d.php:4", "--json"]));
    assert_eq!(method["symbol"]["id"], "d.php#N\\L::one");
}

/// Paths are compared byte for byte and never case-folded: a query spelled
/// with different case does not resolve to the indexed file. This machine's
/// filesystem may be case-insensitive (APFS default), where `a.php` and
/// `A.php` cannot coexist; the two-file case is then covered by the
/// `rivet-index` unit test `paths_differing_only_in_case_are_distinct`, and
/// runs here end to end only on a case-sensitive filesystem.
#[test]
fn paths_differing_only_in_case_are_never_folded() {
    let temp = git_repo("case");
    let root = temp.path();
    write(root, "d.php", F_PHP);

    for query in ["D.php:3", "D.PHP#N\\f", "d.PHP#N\\f", "d.php#n\\f"] {
        let error = failure(&run(root, &["symbol", query, "--json"]), 4);
        assert_eq!(error["error"], "symbol_not_found", "{query}");
    }
    let found = success(&run(root, &["symbol", "d.php#N\\f", "--json"]));
    assert_eq!(found["symbol"]["file"], "d.php");

    write(
        root,
        "D.php",
        b"<?php\nnamespace N;\nfunction upper(): void {}\n",
    );
    let listing: Vec<String> = fs::read_dir(root)
        .expect("read root")
        .map(|entry| entry.expect("entry").file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".php"))
        .collect();
    if listing.len() == 1 {
        eprintln!("case-insensitive filesystem: D.php overwrote d.php; two-file case not run");
        return;
    }
    let lower = success(&run(root, &["symbol", "d.php#N\\f", "--json"]));
    let upper = success(&run(root, &["symbol", "D.php#N\\upper", "--json"]));
    assert_eq!(lower["symbol"]["file"], "d.php");
    assert_eq!(upper["symbol"]["file"], "D.php");
    let index = success(&run(root, &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["files_seen"], 2);
}
