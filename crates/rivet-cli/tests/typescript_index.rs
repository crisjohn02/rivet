//! T43: TypeScript indexed end to end; T44 and T45: its bindings end to end.
//!
//! These tests drive the built binary over the authored TypeScript fixture
//! and small temporary repositories:
//!
//! - every T43 gold `[[use]]` is persisted with its kind, container, and
//!   receiver, no `[[not_a_use]]` span is, and candidate mode lists exactly
//!   the gold's uses of each focus name (this is the end-to-end half of the
//!   T43 gold harness; `rivet-languages/tests/typescript_gold.rs` is the
//!   extractor half);
//! - every T44 and T45 gold `[[binding]]` holds in the published index and in
//!   `refs` (the end-to-end half of the T44 and T45 gold harness;
//!   `rivet-index/tests/typescript_gold.rs` is the extractor-and-resolver
//!   half), every receiver binding is `scoped` and every other one `exact`,
//!   and no TypeScript use has a receiver class;
//! - removing an export unbinds its dependents on the next refresh, with no
//!   other file reparsed;
//! - reference mode excludes nothing by evidence for a TypeScript target;
//! - PHP and TypeScript declarations of one name are different symbols,
//!   TypeScript lookup is case-sensitive, and a use matches only a target of
//!   its own language;
//! - a cache written before T43 (TypeScript stored as `unsupported`) is
//!   refreshed to the new facts, and `--no-refresh` refuses it;
//! - editing one `.ts` file re-extracts that file only; and
//! - query output and bindings are byte-identical across `--force` and file
//!   creation order.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;
use support::{TempDir, failure, fixture_dir, git_repo, run, run_env, success, write};

/// The authored TypeScript fixture.
fn typescript_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/typescript/authored")
}

/// Every regular file under `root`, as sorted `/`-separated relative paths.
fn files_under(root: &Path) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under root");
                let parts: Vec<String> = rel
                    .components()
                    .map(|part| part.as_os_str().to_str().expect("UTF-8").to_string())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, root, &mut out);
    out.sort();
    out
}

/// Copies every file under `source` to `dest/prefix`, in sorted order or,
/// with `reverse`, in reverse, so file creation order differs between copies.
fn copy_tree(source: &Path, dest: &Path, prefix: &str, reverse: bool) {
    let mut files = files_under(source);
    if reverse {
        files.reverse();
    }
    for rel in files {
        let bytes = fs::read(source.join(&rel)).expect("read fixture file");
        let target = if prefix.is_empty() {
            rel
        } else {
            format!("{prefix}/{rel}")
        };
        write(dest, &target, &bytes);
    }
}

/// A Git root holding a copy of the authored TypeScript fixture.
fn typescript_repo(label: &str) -> TempDir {
    let temp = git_repo(label);
    copy_tree(&typescript_fixture(), temp.path(), "", false);
    temp
}

/// The TypeScript gold.
fn gold() -> toml::Table {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/gold/typescript-authored.toml");
    let text = fs::read_to_string(path).expect("read TypeScript gold");
    toml::from_str(&text).expect("parse TypeScript gold")
}

/// The entries of one gold table tagged `task`.
fn entries<'a>(gold: &'a toml::Table, table: &str, task: &str) -> Vec<&'a toml::Table> {
    gold.get(table)
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(toml::Value::as_table)
                .filter(|entry| entry["task"].as_str() == Some(task))
                .collect()
        })
        .unwrap_or_default()
}

fn text<'a>(entry: &'a toml::Table, key: &str) -> &'a str {
    entry[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} in {entry:?}"))
}

fn number(entry: &toml::Table, key: &str) -> i64 {
    entry[key]
        .as_integer()
        .unwrap_or_else(|| panic!("{key} in {entry:?}"))
}

fn focus_names(gold: &toml::Table) -> Vec<String> {
    gold["focus_names"]
        .as_array()
        .expect("focus_names")
        .iter()
        .map(|name| name.as_str().expect("a name").to_string())
        .collect()
}

fn open_db(root: &Path) -> Connection {
    Connection::open(root.join(".rivet").join("index.db")).expect("open index.db")
}

/// One persisted use: `(ref_kind, containing_symbol or "", receiver or "",
/// spelling)` by `(file, start_byte, end_byte)`.
type UseKey = (String, i64, i64);
type UseFields = (String, String, String, String);

fn persisted_uses(root: &Path) -> BTreeMap<UseKey, Vec<UseFields>> {
    let conn = open_db(root);
    let mut stmt = conn
        .prepare(
            "SELECT file, start_byte, end_byte, ref_kind, containing_symbol, receiver, spelling
             FROM uses",
        )
        .expect("prepare uses");
    let mut out: BTreeMap<UseKey, Vec<UseFields>> = BTreeMap::new();
    let rows = stmt
        .query_map([], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get(1)?, row.get(2)?),
                (
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    row.get::<_, String>(6)?,
                ),
            ))
        })
        .expect("query uses");
    for row in rows {
        let (key, fields) = row.expect("use row");
        out.entry(key).or_default().push(fields);
    }
    out
}

/// How many stored rows of `table` belong to a use in a TypeScript file.
fn typescript_rows(root: &Path, table: &str) -> i64 {
    open_db(root)
        .query_row(
            &format!(
                "SELECT count(*) FROM {table} JOIN uses USING (use_id)
                 JOIN files ON files.path = uses.file WHERE files.language = 'typescript'"
            ),
            [],
            |row| row.get(0),
        )
        .expect("count rows")
}

/// The `(file, start_byte, end_byte)` of every reference in a refs response
/// for `target`, checking each one's tier and resolved target against the
/// gold: a use the gold binds to `target` has the gold's tier (`exact` for
/// T44, `scoped` for T45) with that target, and every other use is
/// `name_match` and unbound (the gold binds no focus-name use to another
/// declaration).
fn checked_spans(
    value: &Value,
    target: &str,
    links: &BTreeMap<UseKey, Option<(String, String)>>,
) -> BTreeSet<(String, i64, i64)> {
    value["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| {
            let key = (
                item["file"].as_str().expect("file").to_string(),
                item["start_byte"].as_i64().expect("start"),
                item["end_byte"].as_i64().expect("end"),
            );
            let want = links
                .get(&key)
                .unwrap_or_else(|| panic!("focus use {key:?} has no gold binding entry"));
            match want {
                Some((bound, tier)) if bound == target => {
                    assert_eq!(item["resolution"], tier.as_str(), "{item}");
                    assert_eq!(item["resolved_target"], target, "{item}");
                }
                Some(other) => panic!("{key:?} is bound to {other:?}, not {target}"),
                None => {
                    assert_eq!(item["resolution"], "name_match", "{item}");
                    assert!(item["resolved_target"].is_null(), "{item}");
                }
            }
            key
        })
        .collect()
}

fn no_exclusion(value: &Value) {
    assert_eq!(
        value["by_exclusion"],
        serde_json::json!({"incompatible_form": 0, "unrelated_receiver": 0}),
        "{value}"
    );
}

// ---------------------------------------------------------------------------
// The gold, end to end.
// ---------------------------------------------------------------------------

#[test]
fn every_t43_gold_use_is_persisted() {
    let temp = typescript_repo("t43-gold");
    let index = success(&run(temp.path(), &["index", "--json"]));
    // The 82 gold declarations; T44's 55 exact bindings (four of them
    // through T44a's directory-only specifiers) and T45's 16 scoped ones.
    assert_eq!(
        (&index["symbols"], &index["uses"], &index["bindings"]),
        (&Value::from(82), &Value::from(144), &Value::from(71)),
        "{index}"
    );
    let gold = gold();
    let uses = persisted_uses(temp.path());
    let mut problems = Vec::new();
    let mut expected: BTreeSet<UseKey> = BTreeSet::new();
    for entry in entries(&gold, "use", "T43") {
        let key = (
            text(entry, "file").to_string(),
            number(entry, "start_byte"),
            number(entry, "end_byte"),
        );
        let want = (
            text(entry, "ref_kind").to_string(),
            text(entry, "container").to_string(),
            text(entry, "receiver").to_string(),
            text(entry, "text").to_string(),
        );
        match uses.get(&key).map(Vec::as_slice) {
            Some([got]) if *got == want => {}
            got => problems.push(format!("use {key:?}: want {want:?}, stored {got:?}")),
        }
        expected.insert(key);
    }
    for entry in entries(&gold, "not_a_use", "T43") {
        let (file, start, end) = (
            text(entry, "file"),
            number(entry, "start_byte"),
            number(entry, "end_byte"),
        );
        for key in uses.keys() {
            if key.0 == file && start <= key.1 && key.2 <= end {
                problems.push(format!(
                    "not_a_use {file} [{start}, {end}) ({}) is stored as a use",
                    text(entry, "why")
                ));
            }
        }
    }
    let focus = focus_names(&gold);
    for (key, rows) in &uses {
        if rows.iter().any(|row| focus.contains(&row.3)) && !expected.contains(key) {
            problems.push(format!(
                "stored focus-name use {key:?} {rows:?} is not in the gold"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    // No TypeScript use has a receiver class: TypeScript has no
    // evidence-based exclusion (spec §11.5).
    assert_eq!(typescript_rows(temp.path(), "receiver_classes"), 0);
}

/// Every stored binding: `(target, resolution)` by `(file, start_byte,
/// end_byte)` of its use.
fn persisted_bindings(root: &Path) -> BTreeMap<UseKey, (String, String)> {
    let conn = open_db(root);
    let mut stmt = conn
        .prepare(
            "SELECT uses.file, uses.start_byte, uses.end_byte, target_id, resolution
             FROM bindings JOIN uses USING (use_id)",
        )
        .expect("prepare bindings");
    stmt.query_map([], |row| {
        Ok((
            (row.get(0)?, row.get(1)?, row.get(2)?),
            (row.get(3)?, row.get(4)?),
        ))
    })
    .expect("query bindings")
    .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    .expect("binding rows")
}

/// The gold's expected link for each `[[binding]]` span of a done task:
/// `Some((target, tier))` or `None` for a use that stays unresolved. Entries
/// of a task not yet done are expected unbound.
fn expected_links(gold: &toml::Table) -> BTreeMap<UseKey, Option<(String, String)>> {
    let done: Vec<&str> = gold["done_tasks"]
        .as_array()
        .expect("done_tasks")
        .iter()
        .map(|task| task.as_str().expect("a task"))
        .collect();
    let mut out = BTreeMap::new();
    for task in ["T44", "T45"] {
        for entry in entries(gold, "binding", task) {
            let key = (
                text(entry, "file").to_string(),
                number(entry, "start_byte"),
                number(entry, "end_byte"),
            );
            let tier = text(entry, "expected_resolution");
            let want = (done.contains(&task) && tier != "name_match")
                .then(|| (text(entry, "expected_target").to_string(), tier.to_string()));
            out.insert(key, want);
        }
    }
    out
}

/// Each stored use's `hint_json` by `(file, start_byte, end_byte)`.
fn persisted_hints(root: &Path) -> BTreeMap<UseKey, String> {
    let conn = open_db(root);
    let mut stmt = conn
        .prepare("SELECT file, start_byte, end_byte, hint_json FROM uses")
        .expect("prepare hints");
    stmt.query_map([], |row| {
        Ok(((row.get(0)?, row.get(1)?, row.get(2)?), row.get(3)?))
    })
    .expect("query hints")
    .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    .expect("hint rows")
}

#[test]
fn every_t44_and_t45_gold_binding_holds_end_to_end() {
    let temp = typescript_repo("t44-gold");
    success(&run(temp.path(), &["index", "--json"]));
    let gold = gold();
    let bound = persisted_bindings(temp.path());
    let mut problems = Vec::new();
    for (key, want) in expected_links(&gold) {
        let got = bound.get(&key).cloned();
        if got != want {
            problems.push(format!("binding {key:?}: want {want:?}, stored {got:?}"));
        }
    }
    // A `not_target` is never the stored target.
    for entry in entries(&gold, "binding", "T44") {
        if let Some(not_target) = entry.get("not_target").and_then(toml::Value::as_str) {
            let key = (
                text(entry, "file").to_string(),
                number(entry, "start_byte"),
                number(entry, "end_byte"),
            );
            if bound
                .get(&key)
                .is_some_and(|(target, _)| target == not_target)
            {
                problems.push(format!("binding {key:?} is bound to its not_target"));
            }
        }
    }
    // A binding through a receiver hint is `scoped` (T45); every other
    // TypeScript binding is `exact` (T44).
    let hints = persisted_hints(temp.path());
    for (key, (target, tier)) in &bound {
        let want = if hints[key] == r#"{"kind":"unresolved"}"# {
            "exact"
        } else {
            "scoped"
        };
        if tier != want {
            problems.push(format!("{key:?} -> {target} is {tier}, want {want}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    // `refs` reports each bound use of a target with its tier and target,
    // aliases included: `runAll` and `makeLabel` are references to
    // `launchAll` and `helper` through their bindings (spec §11.5).
    for (target, alias) in [
        ("src/util.ts#launchAll", "runAll"),
        ("src/util.ts#helper", "makeLabel"),
    ] {
        let value = success(&run(temp.path(), &["refs", target, "--json"]));
        let items = value["references"].as_array().expect("references");
        let aliased: Vec<&Value> = items
            .iter()
            .filter(|item| item["file"] == "src/report.ts")
            .collect();
        assert_eq!(aliased.len(), 2, "{target}: {value}");
        for item in aliased {
            assert_eq!(item["resolution"], "exact", "{item}");
            assert_eq!(item["resolved_target"], target, "{item}");
        }
        let spellings: BTreeSet<String> = bound
            .iter()
            .filter(|(_, (bound_target, _))| bound_target == target)
            .map(|(key, _)| {
                let source = fs::read(temp.path().join(&key.0)).expect("read");
                String::from_utf8_lossy(&source[key.1 as usize..key.2 as usize]).into_owned()
            })
            .collect();
        assert!(spellings.contains(alias), "{target}: {spellings:?}");
    }
    assert_eq!(typescript_rows(temp.path(), "receiver_classes"), 0);
}

#[test]
fn candidate_and_reference_mode_list_exactly_the_gold_focus_uses() {
    let temp = typescript_repo("t43-candidates");
    let gold = gold();
    let links = expected_links(&gold);
    let targets = [
        ("launch", "src/services/survey.ts#SurveyService.launch"),
        ("double", "src/util.ts#double"),
        ("SurveyService", "src/services/survey.ts#SurveyService"),
        ("Button", "src/components/Button.tsx#Button"),
    ];
    assert_eq!(
        focus_names(&gold),
        targets
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>()
    );
    for (name, target) in targets {
        let want: BTreeSet<(String, i64, i64)> = entries(&gold, "use", "T43")
            .into_iter()
            .filter(|entry| text(entry, "text") == name)
            .map(|entry| {
                (
                    text(entry, "file").to_string(),
                    number(entry, "start_byte"),
                    number(entry, "end_byte"),
                )
            })
            .collect();
        assert!(!want.is_empty(), "{name}");
        for mode in ["candidates", "references"] {
            let value = success(&run(
                temp.path(),
                &["refs", target, "--mode", mode, "--limit", "500", "--json"],
            ));
            assert_eq!(value["symbol"]["language"], "typescript");
            assert_eq!(checked_spans(&value, target, &links), want, "{name} {mode}");
            assert_eq!(value["total"], want.len(), "{name} {mode}");
            let scoped = links
                .values()
                .flatten()
                .filter(|(bound, tier)| bound == target && tier == "scoped")
                .count();
            assert_eq!(value["by_resolution"]["scoped"], scoped, "{name} {mode}");
            no_exclusion(&value);
        }
    }
}

// ---------------------------------------------------------------------------
// Receiver bindings through the query commands (T45).
// ---------------------------------------------------------------------------

/// `(file, line, resolution, resolved_target or "")` of each listed use.
fn listed(items: &Value) -> Vec<(String, i64, String, String)> {
    items
        .as_array()
        .expect("items")
        .iter()
        .map(|item| {
            (
                item["file"].as_str().expect("file").to_string(),
                item["line"].as_i64().expect("line"),
                item["resolution"].as_str().expect("resolution").to_string(),
                item["resolved_target"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn receiver_bindings_flow_through_refs_symbol_and_context() {
    let temp = typescript_repo("t45-queries");
    let launch = "src/services/survey.ts#SurveyService.launch";
    let scoped = |file: &str, line: i64| {
        (
            file.to_string(),
            line,
            "scoped".to_string(),
            launch.to_string(),
        )
    };
    let receivers = vec![
        scoped("src/report.ts", 17),
        scoped("src/report.ts", 23),
        scoped("src/report.ts", 28),
        scoped("src/report.ts", 29),
        scoped("src/report.ts", 35),
        scoped("src/services/survey.ts", 44),
    ];
    let untyped = (
        "src/report.ts".to_string(),
        41,
        "name_match".to_string(),
        String::new(),
    );

    // `refs`: the five typed/`new` receivers in report.ts and `this.launch()`
    // in `relaunch` are `scoped`; the untyped `x.launch()` is `name_match`.
    let value = success(&run(temp.path(), &["refs", launch, "--json"]));
    let mut want = receivers.clone();
    want.insert(5, untyped.clone());
    assert_eq!(listed(&value["references"]), want, "{value}");
    assert_eq!(
        value["by_resolution"],
        serde_json::json!({"exact": 0, "scoped": 6, "name_match": 1})
    );
    no_exclusion(&value);
    let value = success(&run(
        temp.path(),
        &["refs", launch, "--min-resolution", "scoped", "--json"],
    ));
    assert_eq!(listed(&value["references"]), receivers, "{value}");
    assert_eq!(value["total"], 6);

    // Human output marks only the name-only use with `?`.
    let output = run(temp.path(), &["refs", launch]);
    assert_eq!(output.status.code(), Some(0));
    let human = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        human.contains("7 references  (6 scoped, 1 name_match)"),
        "{human}"
    );
    let rows: Vec<&str> = human
        .lines()
        .filter(|line| line.starts_with("src/"))
        .collect();
    assert_eq!(rows.len(), 7, "{human}");
    for row in rows {
        if row.starts_with("src/report.ts:41:") {
            assert!(row.ends_with("name_match ?"), "{row}");
        } else {
            assert!(row.ends_with("call  scoped"), "{row}");
        }
    }

    // `symbol`: the callers (scoped by default) are the receiver uses, and
    // the untyped one is counted, not listed.
    let value = success(&run(temp.path(), &["symbol", launch, "--json"]));
    assert_eq!(listed(&value["called_by"]["items"]), receivers, "{value}");
    assert_eq!(value["called_by"]["hidden_name_match"], 1);
    let callers: BTreeSet<&str> = value["called_by"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["containing_symbol"]["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        callers,
        BTreeSet::from([
            "src/report.ts#ReportService.launch",
            "src/report.ts#ReportService.runNew",
            "src/report.ts#ReportService.runTyped",
            "src/report.ts#ReportService.runVariable",
            "src/services/survey.ts#SurveyService.relaunch",
        ])
    );
    let output = run(temp.path(), &["symbol", launch]);
    let human = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        human.contains("called by: (+1 name-only not listed)"),
        "{human}"
    );
    assert!(
        human.contains("src/services/survey.ts:44:10  SurveyService.relaunch     scoped"),
        "{human}"
    );

    // `runTyped`'s own calls are its scoped callee.
    let run_typed = "src/report.ts#ReportService.runTyped";
    let value = success(&run(temp.path(), &["symbol", run_typed, "--json"]));
    let calls: Vec<(String, String)> = value["calls"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| {
            (
                item["resolved_target"]
                    .as_str()
                    .expect("target")
                    .to_string(),
                item["resolution"].as_str().expect("tier").to_string(),
            )
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            (launch.to_string(), "scoped".to_string()),
            (launch.to_string(), "scoped".to_string())
        ],
        "{value}"
    );

    // `context`: with a budget that leaves out the receiver's class, the
    // scoped callee is its own segment.
    let args = [
        "context", run_typed, "--depth", "1", "--tokens", "120", "--json",
    ];
    let value = success(&run(temp.path(), &args));
    let segments: Vec<(String, String, String)> = value["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .map(|segment| {
            (
                segment["symbol"]["id"].as_str().expect("id").to_string(),
                segment["reason"].as_str().expect("reason").to_string(),
                segment["resolution"].as_str().expect("tier").to_string(),
            )
        })
        .collect();
    assert_eq!(
        segments,
        vec![
            (
                run_typed.to_string(),
                "target".to_string(),
                "exact".to_string()
            ),
            (
                launch.to_string(),
                "callee".to_string(),
                "scoped".to_string()
            ),
        ],
        "{value}"
    );
    let output = run(
        temp.path(),
        &["context", run_typed, "--depth", "1", "--tokens", "120"],
    );
    let human = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(
        human.contains("SurveyService.launch  [callee, full]"),
        "{human}"
    );
    // At full depth the callers of that callee come in through it, scoped.
    let value = success(&run(temp.path(), &["context", run_typed, "--json"]));
    let second: Vec<(&str, &str)> = value["segments"]
        .as_array()
        .expect("segments")
        .iter()
        .filter(|segment| segment["reason"] == "second_degree")
        .map(|segment| {
            (
                segment["symbol"]["id"].as_str().expect("id"),
                segment["resolution"].as_str().expect("tier"),
            )
        })
        .collect();
    assert!(
        second.contains(&("src/report.ts#ReportService.runNew", "scoped")),
        "{value}"
    );
}

/// T45: renaming a receiver class's method unbinds the receiver uses in an
/// unchanged file on the next refresh, and restoring it rebinds them.
#[test]
fn renaming_a_receiver_method_unbinds_its_callers_on_the_next_refresh() {
    let temp = typescript_repo("t45-freshness");
    fs::remove_file(temp.path().join("src/broken.ts")).expect("remove broken.ts");
    success(&run(temp.path(), &["index", "--json"]));
    let launch = "src/services/survey.ts#SurveyService.launch";
    let before = persisted_bindings(temp.path());
    let report_links = |bindings: &BTreeMap<UseKey, (String, String)>| -> Vec<UseKey> {
        bindings
            .iter()
            .filter(|(key, (target, tier))| {
                key.0 == "src/report.ts" && target == launch && tier == "scoped"
            })
            .map(|(key, _)| key.clone())
            .collect()
    };
    assert_eq!(report_links(&before).len(), 5, "{before:?}");

    let survey = temp.path().join("src/services/survey.ts");
    let original = fs::read(&survey).expect("read survey.ts");
    let text = String::from_utf8(original.clone()).expect("UTF-8");
    assert_eq!(text.matches("  launch(): void {").count(), 1);
    fs::write(
        &survey,
        text.replace("  launch(): void {", "  start(): void {"),
    )
    .expect("edit");
    let log_dir = TempDir::new("t45-reparsed");
    let log = log_dir.path().join("reparsed.log");
    let log_value = log.to_str().expect("UTF-8 path").to_string();
    success(&run_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_value.as_str())],
    ));
    let reparsed: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        reparsed,
        ["src/services/survey.ts"],
        "report.ts is untouched"
    );
    let after = persisted_bindings(temp.path());
    // report.ts's `launch` uses bind nothing now: `ReportService.launch` is
    // not their receiver's class, and `start` is not their spelling.
    for key in report_links(&before) {
        assert_eq!(after.get(&key), None, "{key:?}");
    }
    // `this.service` still binds its field, and nothing names `launch` any
    // more in `refs`.
    assert!(
        after.values().any(
            |(target, tier)| target == "src/report.ts#ReportService.service" && tier == "scoped"
        ),
        "{after:?}"
    );
    let value = failure(&run(temp.path(), &["refs", launch, "--json"]), 4);
    assert_eq!(value["error"], "symbol_not_found", "{value}");

    fs::write(&survey, &original).expect("restore survey.ts");
    success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(persisted_bindings(temp.path()), before);
}

// ---------------------------------------------------------------------------
// No evidence-based exclusion for TypeScript.
// ---------------------------------------------------------------------------

const LIB_TS: &[u8] = b"\
export namespace Outer {
  export function f(): void {}
}
export function format(value: number): string {
  return String(value);
}
export const LIMIT = 3;
export class Foo {
  run(): void {}
}
export class Other {
  run(): void {}
}
export enum Color {
  Red,
  Green,
}
";

/// The uses must stay unresolved to test exclusion, so `lib` is imported
/// through a path alias, which v0.1 never resolves: a relative import would
/// bind most of them (T44).
const APP_TS: &[u8] = b"\
import * as util from \"@/lib\";
import { Outer, LIMIT, Foo, Color, Other } from \"@/lib\";

Outer.f();
util.format(1);
console.log(LIMIT);
new Foo();
export const red = Color.Red;
export function go(other: Other): void {
  other.run();
}
";

/// The PHP control: the same forms PHP's form table does rule out.
const PHP_CONTROL: &[u8] = b"<?php\nnamespace App;\nfunction f(): void {}\n$x->f();\nf();\n";

#[test]
fn reference_mode_excludes_nothing_for_typescript() {
    let temp = git_repo("t43-no-exclusion");
    write(temp.path(), "src/lib.ts", LIB_TS);
    write(temp.path(), "src/app.ts", APP_TS);
    write(temp.path(), "control.php", PHP_CONTROL);
    success(&run(temp.path(), &["index", "--json"]));

    // (target, the use it must keep as `file:start` of the named identifier)
    let app = String::from_utf8(APP_TS.to_vec()).expect("UTF-8");
    let offset = |needle: &str| app.find(needle).expect("in app.ts") as i64;
    let cases = [
        // A function called through a qualifier.
        ("src/lib.ts#Outer.f", offset("f();"), "call"),
        // A namespace import's member.
        ("src/lib.ts#format", offset("format(1)"), "call"),
        // A bare read of a top-level const.
        ("src/lib.ts#LIMIT", offset("LIMIT);"), "unknown"),
        // A class named through `new`.
        ("src/lib.ts#Foo", offset("Foo();"), "type"),
        // An enum member through its enum.
        ("src/lib.ts#Color.Red", offset("Red;"), "read"),
        // A method on a receiver typed with an unrelated class.
        ("src/lib.ts#Foo.run", offset("run();"), "call"),
    ];
    for (target, start, kind) in cases {
        let value = success(&run(temp.path(), &["refs", target, "--json"]));
        no_exclusion(&value);
        let kept = value["references"]
            .as_array()
            .expect("references")
            .iter()
            .any(|item| {
                item["file"] == "src/app.ts"
                    && item["start_byte"] == start
                    && item["ref_kind"] == kind
                    && item["resolution"] == "name_match"
            });
        assert!(kept, "{target}: {value}");
    }

    // PHP still excludes: `$x->f()` cannot name the function `App\f`.
    let php = success(&run(temp.path(), &["refs", "App\\f", "--json"]));
    assert_eq!(php["by_exclusion"]["incompatible_form"], 1, "{php}");
}

// ---------------------------------------------------------------------------
// Mixed PHP and TypeScript repositories.
// ---------------------------------------------------------------------------

#[test]
fn php_and_typescript_declarations_are_different_symbols() {
    let temp = git_repo("t43-mixed");
    write(temp.path(), "Foo.php", b"<?php\nclass Foo {}\n");
    write(temp.path(), "use.php", b"<?php\nnew Foo();\nnew foo();\n");
    write(temp.path(), "foo.ts", b"export class Foo {}\n");
    write(
        temp.path(),
        "use.ts",
        b"import { Foo } from \"./foo\";\nnew Foo();\nnew foo();\n",
    );
    let index = success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(index["index"]["coverage"]["complete"], true, "{index}");

    // One name, two languages: two symbols, so the short name is ambiguous.
    let ambiguous = failure(&run(temp.path(), &["symbol", "Foo", "--json"]), 5);
    let ids: Vec<&str> = ambiguous["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|candidate| candidate["id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["Foo.php#Foo", "foo.ts#Foo"], "{ambiguous}");

    // PHP class names fold ASCII case; TypeScript names never do.
    for query in ["foo", "FOO"] {
        let found = success(&run(temp.path(), &["symbol", query, "--json"]));
        assert_eq!(found["symbol"]["id"], "Foo.php#Foo", "{query}");
        assert_eq!(found["symbol"]["language"], "php", "{query}");
    }

    // A use matches only a target of its own language: PHP's `foo` names the
    // PHP class, TypeScript's `foo` names nothing, and neither language's uses
    // appear in the other's refs.
    let spans = |target: &str| -> BTreeSet<String> {
        let value = success(&run(
            temp.path(),
            &["refs", target, "--mode", "candidates", "--json"],
        ));
        value["references"]
            .as_array()
            .expect("references")
            .iter()
            .map(|item| {
                format!(
                    "{}:{}:{}",
                    item["file"].as_str().expect("file"),
                    item["line"],
                    item["ref_kind"].as_str().expect("kind")
                )
            })
            .collect()
    };
    assert_eq!(
        spans("Foo.php#Foo"),
        BTreeSet::from(["use.php:2:type".to_string(), "use.php:3:type".to_string()])
    );
    assert_eq!(
        spans("foo.ts#Foo"),
        BTreeSet::from(["use.ts:1:import".to_string(), "use.ts:2:type".to_string()])
    );

    // The PHP uses bind the PHP class; the TypeScript import and `new Foo()`
    // bind the TypeScript class (T44), and `new foo()` binds nothing.
    let php = success(&run(temp.path(), &["refs", "Foo.php#Foo", "--json"]));
    assert_eq!(php["by_resolution"]["exact"], 2, "{php}");
    let typescript = success(&run(temp.path(), &["refs", "foo.ts#Foo", "--json"]));
    assert_eq!(typescript["by_resolution"]["exact"], 2, "{typescript}");
    let targets: BTreeSet<String> = persisted_bindings(temp.path())
        .into_iter()
        .filter(|(key, _)| key.0 == "use.ts")
        .map(|(key, (target, _))| format!("{}:{}:{target}", key.0, key.1))
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from([
            "use.ts:9:foo.ts#Foo".to_string(),
            "use.ts:33:foo.ts#Foo".to_string()
        ])
    );
}

// ---------------------------------------------------------------------------
// Cache upgrade, freshness, and determinism.
// ---------------------------------------------------------------------------

/// The PHP fixture at the root and the TypeScript fixture under `ts/`.
fn mixed_fixture_repo(label: &str, reverse: bool) -> TempDir {
    let temp = git_repo(label);
    copy_tree(&fixture_dir(), temp.path(), "", reverse);
    copy_tree(&typescript_fixture(), temp.path(), "ts", reverse);
    temp
}

/// Navigation queries over both languages, compared byte for byte.
fn navigation_queries() -> Vec<Vec<&'static str>> {
    vec![
        vec!["symbol", "double", "--json"],
        vec!["symbol", "ts/src/report.ts:17", "--json"],
        vec![
            "symbol",
            "ts/src/services/survey.ts#SurveyService",
            "--json",
        ],
        vec!["refs", "double", "--json"],
        vec![
            "refs",
            "ts/src/services/survey.ts#SurveyService.launch",
            "--mode",
            "candidates",
            "--json",
        ],
        // T45: the receiver bindings, in every query that lists them.
        vec![
            "refs",
            "ts/src/services/survey.ts#SurveyService.launch",
            "--json",
        ],
        vec![
            "refs",
            "ts/src/services/survey.ts#SurveyService.launch",
            "--min-resolution",
            "scoped",
        ],
        vec![
            "symbol",
            "ts/src/services/survey.ts#SurveyService.launch",
            "--json",
        ],
        vec![
            "context",
            "ts/src/report.ts#ReportService.runTyped",
            "--json",
        ],
        vec!["refs", "ts/src/components/Button.tsx#Button", "--json"],
        vec![
            "context",
            "ts/src/services/survey.ts#SurveyService.launch",
            "--json",
        ],
        vec!["context", "ts/src/components/App.tsx#App", "--json"],
        vec!["refs", "App\\Services\\SurveyService::launch", "--json"],
        vec!["context", "App\\Services\\SurveyService::launch", "--json"],
        vec!["symbol", "launch", "--json"],
    ]
}

/// The exit code and both streams of every navigation query.
fn answers(root: &Path) -> Vec<(Option<i32>, Vec<u8>, Vec<u8>)> {
    navigation_queries()
        .iter()
        .map(|args| {
            let output = run(root, args);
            (output.status.code(), output.stdout, output.stderr)
        })
        .collect()
}

/// Rewrites a current cache into what a pre-T43 binary wrote: the old
/// fingerprints, and every TypeScript file stored as `unsupported` with no
/// content hash, source, or facts.
fn make_pre_t43_cache(root: &Path) {
    let conn = open_db(root);
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("foreign keys");
    conn.execute_batch(
        "UPDATE meta SET value = 'php=0.24.2;ts=0.23.2;fact-schema=11'
           WHERE key = 'extractor_fingerprint';
         UPDATE meta SET value = 'php-rules-v3' WHERE key = 'resolver_fingerprint';
         DELETE FROM symbols WHERE file IN (SELECT path FROM files WHERE language = 'typescript');
         DELETE FROM uses WHERE file IN (SELECT path FROM files WHERE language = 'typescript');
         DELETE FROM scopes WHERE file IN (SELECT path FROM files WHERE language = 'typescript');
         DELETE FROM diagnostics WHERE file IN (SELECT path FROM files WHERE language = 'typescript');
         UPDATE files SET parse_status = 'unsupported', content_hash = NULL, source = NULL
           WHERE language = 'typescript';",
    )
    .expect("rewrite the cache as a pre-T43 one");
    let symbols: i64 = conn
        .query_row(
            "SELECT count(*) FROM symbols JOIN files ON files.path = symbols.file
             WHERE files.language = 'typescript'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(symbols, 0);
}

#[test]
fn a_pre_t43_cache_refreshes_to_the_new_facts() {
    let fresh = mixed_fixture_repo("t43-upgrade-fresh", false);
    let fresh_index = success(&run(fresh.path(), &["index", "--json"]));
    let expected = answers(fresh.path());

    for freshness in ["metadata", "content"] {
        let temp = mixed_fixture_repo(&format!("t43-upgrade-{freshness}"), false);
        success(&run(temp.path(), &["index", "--json"]));
        make_pre_t43_cache(temp.path());

        // `--no-refresh` refuses facts written by other rules.
        let refused = failure(
            &run(temp.path(), &["symbol", "double", "--no-refresh", "--json"]),
            3,
        );
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|message| message.contains("extractor_fingerprint")),
            "{refused}"
        );

        // A refresh in either mode reparses every file, TypeScript included,
        // even though every file's size and mtime are unchanged.
        let upgraded = success(&run(
            temp.path(),
            &["index", "--freshness", freshness, "--json"],
        ));
        for key in ["symbols", "uses", "bindings"] {
            assert_eq!(upgraded[key], fresh_index[key], "{freshness} {key}");
        }
        assert_eq!(
            upgraded["index"]["coverage"], fresh_index["index"]["coverage"],
            "{freshness}"
        );
        // Every enabled-language file (9 PHP, 14 TypeScript) is regenerated;
        // the 5 ordinary unsupported files are not.
        assert_eq!(
            (&upgraded["updated"], &upgraded["unchanged"]),
            (&Value::from(23), &Value::from(5)),
            "{freshness}"
        );
        assert_eq!(answers(temp.path()), expected, "{freshness}");
    }
}

#[test]
fn editing_one_typescript_file_re_extracts_only_that_file() {
    // A file that failed to parse is attempted again on every refresh, as for
    // PHP (it has no facts to reuse), so the broken fixture file is left out
    // here to keep the reparse log to the edited file.
    let temp = typescript_repo("t43-freshness");
    fs::remove_file(temp.path().join("src/broken.ts")).expect("remove broken.ts");
    success(&run(temp.path(), &["index", "--json"]));
    let before = persisted_ids(temp.path());

    let util = temp.path().join("src/util.ts");
    let mut bytes = fs::read(&util).expect("read util.ts");
    bytes.extend_from_slice(
        b"\nexport function triple(n: number): number {\n  return double(n) + n;\n}\n",
    );
    fs::write(&util, &bytes).expect("edit util.ts");

    let log_dir = TempDir::new("t43-reparsed");
    let log = log_dir.path().join("reparsed.log");
    let log_value = log.to_str().expect("UTF-8 path").to_string();
    success(&run_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_value.as_str())],
    ));
    let reparsed: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(reparsed, ["src/util.ts"]);

    // Every other file's use rows are the stored ones, IDs included.
    let after = persisted_ids(temp.path());
    for (key, id) in &before {
        if key.0 != "src/util.ts" {
            assert_eq!(after.get(key), Some(id), "{key:?}");
        }
    }
    assert!(
        after.len() > before.len(),
        "the new function's uses are extracted"
    );
    let value = success(&run(temp.path(), &["refs", "src/util.ts#double", "--json"]));
    assert!(
        value["references"]
            .as_array()
            .expect("references")
            .iter()
            .any(|item| item["containing_symbol"]["id"] == "src/util.ts#triple"),
        "{value}"
    );

    // The incremental snapshot answers exactly as a fresh index of the same
    // bytes does.
    let fresh = typescript_repo("t43-freshness-fresh");
    fs::remove_file(fresh.path().join("src/broken.ts")).expect("remove broken.ts");
    fs::write(fresh.path().join("src/util.ts"), &bytes).expect("write util.ts");
    for args in [
        &["refs", "src/util.ts#double", "--json"][..],
        &["symbol", "triple", "--json"][..],
        &["context", "src/util.ts#triple", "--json"][..],
    ] {
        let incremental = run(temp.path(), args);
        let cold = run(fresh.path(), args);
        assert_eq!(incremental.stdout, cold.stdout, "{args:?}");
        assert_eq!(incremental.stderr, cold.stderr, "{args:?}");
    }
}

#[test]
fn removing_an_export_unbinds_its_dependents_on_the_next_refresh() {
    // As above, the broken file is left out so the reparse log names only the
    // edited file.
    let temp = typescript_repo("t44-freshness");
    fs::remove_file(temp.path().join("src/broken.ts")).expect("remove broken.ts");
    success(&run(temp.path(), &["index", "--json"]));
    let before = persisted_bindings(temp.path());
    let outside = |bindings: &BTreeMap<UseKey, (String, String)>| -> BTreeSet<UseKey> {
        bindings
            .iter()
            .filter(|(key, (target, _))| key.0 != "src/util.ts" && target == "src/util.ts#double")
            .map(|(key, _)| key.clone())
            .collect()
    };
    // Three dependents import `double`.
    let previously = outside(&before);
    let dependents: BTreeSet<&str> = previously.iter().map(|key| key.0.as_str()).collect();
    assert_eq!(
        dependents,
        BTreeSet::from([
            "src/anonymous.ts",
            "src/components/App.tsx",
            "src/report.ts"
        ])
    );

    // `export const double` becomes a module-local `const double`.
    let util = temp.path().join("src/util.ts");
    let original = fs::read(&util).expect("read util.ts");
    let text = String::from_utf8(original.clone()).expect("UTF-8");
    assert_eq!(text.matches("export const double").count(), 1);
    fs::write(&util, text.replace("export const double", "const double")).expect("edit");

    let log_dir = TempDir::new("t44-reparsed");
    let log = log_dir.path().join("reparsed.log");
    let log_value = log.to_str().expect("UTF-8 path").to_string();
    success(&run_env(
        temp.path(),
        &["index", "--json"],
        &[("RIVET_DEBUG_REPARSED", log_value.as_str())],
    ));
    let reparsed: Vec<String> = fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(reparsed, ["src/util.ts"], "no dependent is reparsed");

    let after = persisted_bindings(temp.path());
    // No dependent binds `double` any more, although none was reparsed...
    assert!(outside(&after).is_empty(), "{after:?}");
    // ...util.ts's own `double(count)` still binds by the same-file rule...
    assert!(
        after
            .iter()
            .any(|(key, (target, _))| key.0 == "src/util.ts" && target == "src/util.ts#double"),
        "{after:?}"
    );
    // ...and every other dependent binding is unchanged.
    for (key, link) in &before {
        if key.0 != "src/util.ts" && link.0 != "src/util.ts#double" {
            assert_eq!(after.get(key), Some(link), "{key:?}");
        }
    }
    // Those uses bind nothing else either.
    let rebound: Vec<&UseKey> = previously
        .iter()
        .filter(|key| after.contains_key(*key))
        .collect();
    assert!(rebound.is_empty(), "{rebound:?}");

    // The incremental snapshot binds exactly as a fresh index of the bytes.
    let fresh = typescript_repo("t44-freshness-fresh");
    fs::remove_file(fresh.path().join("src/broken.ts")).expect("remove broken.ts");
    fs::write(
        fresh.path().join("src/util.ts"),
        fs::read(&util).expect("read"),
    )
    .expect("write util.ts");
    success(&run(fresh.path(), &["index", "--json"]));
    assert_eq!(persisted_bindings(fresh.path()), after);

    // Restoring the export rebinds every dependent.
    fs::write(&util, &original).expect("restore util.ts");
    success(&run(temp.path(), &["index", "--json"]));
    assert_eq!(persisted_bindings(temp.path()), before);
}

/// Every stored use's ID by `(file, start_byte, end_byte, ref_kind)`.
fn persisted_ids(root: &Path) -> BTreeMap<(String, i64, i64, String), i64> {
    let conn = open_db(root);
    let mut stmt = conn
        .prepare("SELECT file, start_byte, end_byte, ref_kind, use_id FROM uses")
        .expect("prepare");
    stmt.query_map([], |row| {
        Ok((
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?),
            row.get(4)?,
        ))
    })
    .expect("query")
    .collect::<rusqlite::Result<BTreeMap<_, _>>>()
    .expect("rows")
}

#[test]
fn query_bytes_are_identical_across_force_and_file_order() {
    let forward = mixed_fixture_repo("t43-order-forward", false);
    let reverse = mixed_fixture_repo("t43-order-reverse", true);
    success(&run(forward.path(), &["index", "--json"]));
    success(&run(reverse.path(), &["index", "--json"]));
    let first = answers(forward.path());
    // Every query answers; `symbol launch` is ambiguous (exit 5) by design.
    let codes: Vec<Option<i32>> = first.iter().map(|(code, _, _)| *code).collect();
    let mut want = vec![Some(0); codes.len()];
    *want.last_mut().expect("queries") = Some(5);
    assert_eq!(
        codes,
        want,
        "{:?}",
        first
            .iter()
            .map(|(_, _, stderr)| String::from_utf8_lossy(stderr).into_owned())
            .collect::<Vec<_>>()
    );
    assert_eq!(answers(reverse.path()), first, "file creation order");
    // The bindings themselves agree, TypeScript's included (T44).
    let bindings = persisted_bindings(forward.path());
    assert!(
        bindings.keys().any(|key| key.0.starts_with("ts/")),
        "{bindings:?}"
    );
    assert_eq!(
        persisted_bindings(reverse.path()),
        bindings,
        "file creation order"
    );

    success(&run(forward.path(), &["index", "--force", "--json"]));
    assert_eq!(answers(forward.path()), first, "--force");
    assert_eq!(persisted_bindings(forward.path()), bindings, "--force");
    // Repeated queries agree too.
    assert_eq!(answers(forward.path()), first, "repeated");
}
