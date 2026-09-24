//! The authored TypeScript/TSX fixture and its gold.
//!
//! T41 wrote these checks before any TypeScript extractor existed; they pin
//! what T42-T45 verify against:
//!
//! - every fixture file dispatches to the intended grammar (`.ts` and `.d.ts`
//!   to TypeScript, `.tsx` to TSX, `.js`/`.mts` and the rest to none), and
//!   only the intentionally broken file fails the v0.1 parse policy;
//! - `tests/gold/typescript-authored.toml` is well formed: each table's task
//!   tag, the canonical IDs (spec §10.1 escaping and ordinals), bindings that
//!   name a recorded use, and containers that are the innermost named
//!   container of their use;
//! - every gold span is the fixture's bytes and a node of the pinned grammar's
//!   tree (a declaration is exactly one named node, a use exactly one
//!   identifier node, a non-use lies in a comment, string, or JSX text, or is
//!   a lowercase intrinsic element name);
//! - every occurrence of a focus name outside comments is accounted for, so a
//!   later candidate-mode check can rely on the gold being complete.
//!
//! T42 is the first done task, and [`t42_entries_match_the_extractor`] is its
//! harness: it runs the TypeScript adapter on every supported fixture file and
//! compares every T42 entry, `[[declaration]]` and `[[not_a_declaration]]`
//! alike, with the extracted symbols. T43's harness,
//! [`t43_entries_match_the_extractor`], compares every `[[use]]` and
//! `[[not_a_use]]` with the extracted uses, and requires the extracted uses of
//! the focus names to be exactly the gold's. `tests/gold/check_gold.py`
//! checks only the recorded spans and names these tests (and, for T43, the
//! CLI's `typescript_index.rs`) as the owners of the comparison.

#![cfg(feature = "lang-typescript")]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rivet_core::{ExtractedFile, SymbolId, assign_ordinals};
use rivet_languages::{LanguageId, grammar, language_for_path, typescript};
use serde::Deserialize;
use tree_sitter::{Node, Parser, Tree};

/// The tasks a TypeScript gold entry can be tagged with.
const TASKS: [&str; 4] = ["T42", "T43", "T44", "T45"];

/// The file the gold marks as intentionally broken.
const BROKEN: &str = "src/broken.ts";

/// Every fixture file and the grammar it must dispatch to. Adding a fixture
/// file without listing it here fails [`fixture_files_use_the_intended_grammar`].
const FIXTURE_FILES: [(&str, Option<LanguageId>); 17] = [
    ("README.md", None),
    ("src/anonymous.ts", Some(LanguageId::Typescript)),
    ("src/barrel.ts", Some(LanguageId::Typescript)),
    ("src/broken.ts", Some(LanguageId::Typescript)),
    ("src/components/App.tsx", Some(LanguageId::Tsx)),
    ("src/components/Button.tsx", Some(LanguageId::Tsx)),
    ("src/esm.mts", None),
    ("src/legacy.js", None),
    ("src/models.ts", Some(LanguageId::Typescript)),
    ("src/pick.ts", Some(LanguageId::Typescript)),
    ("src/pick/index.ts", Some(LanguageId::Typescript)),
    ("src/report.ts", Some(LanguageId::Typescript)),
    ("src/services/survey.ts", Some(LanguageId::Typescript)),
    ("src/types.d.ts", Some(LanguageId::Typescript)),
    ("src/unresolved.ts", Some(LanguageId::Typescript)),
    ("src/util.ts", Some(LanguageId::Typescript)),
    ("tsconfig.json", None),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gold {
    done_tasks: Vec<String>,
    focus_names: Vec<String>,
    #[serde(default)]
    declaration: Vec<Declaration>,
    #[serde(default)]
    not_a_declaration: Vec<NotDeclaration>,
    #[serde(default, rename = "use")]
    uses: Vec<Use>,
    #[serde(default)]
    not_a_use: Vec<NotUse>,
    #[serde(default)]
    binding: Vec<Binding>,
    #[serde(default)]
    undecided: Vec<Undecided>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    task: String,
    case: String,
    file: String,
    id: String,
    qualified_name: String,
    kind: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    text: String,
    end_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotDeclaration {
    task: String,
    case: String,
    file: String,
    why: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    text: String,
    end_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Use {
    task: String,
    case: String,
    file: String,
    start_byte: usize,
    end_byte: usize,
    line: usize,
    column: usize,
    text: String,
    ref_kind: String,
    container: String,
    receiver: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotUse {
    task: String,
    case: String,
    file: String,
    why: String,
    start_byte: usize,
    end_byte: usize,
    line: usize,
    column: usize,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    task: String,
    case: String,
    file: String,
    start_byte: usize,
    end_byte: usize,
    line: usize,
    column: usize,
    text: String,
    expected_target: String,
    expected_resolution: String,
    #[serde(default)]
    not_target: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Undecided {
    task: String,
    case: String,
    file: String,
    form: String,
    question: String,
    proposal: String,
    start_byte: usize,
    end_byte: usize,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    column: Option<usize>,
    text: String,
    #[serde(default)]
    end_text: Option<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_root() -> PathBuf {
    repo_root().join("tests/fixtures/typescript/authored")
}

fn load_gold() -> Gold {
    let path = repo_root().join("tests/gold/typescript-authored.toml");
    let text = std::fs::read_to_string(&path).expect("read TypeScript gold");
    toml::from_str(&text).expect("TypeScript gold must parse with no unknown tables or fields")
}

/// Every regular file under `root`, as sorted `/`-separated relative paths.
fn walk(root: &Path) -> Vec<String> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read fixture directory") {
            let path = entry.expect("fixture entry").path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under root");
                let parts: Vec<String> = rel
                    .components()
                    .map(|part| part.as_os_str().to_str().expect("UTF-8 path").to_string())
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

fn parse(id: LanguageId, source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(id))
        .expect("pinned grammar must load");
    parser
        .parse(source, None)
        .expect("parser must return a tree")
}

/// One parsed, cleanly parsing fixture file.
struct Parsed {
    source: Vec<u8>,
    tree: Tree,
}

/// Every supported fixture file except the broken one, parsed with the
/// grammar its extension dispatches to.
fn parsed_files() -> BTreeMap<String, Parsed> {
    let mut out = BTreeMap::new();
    for rel in walk(&fixture_root()) {
        let Some(id) = language_for_path(&rel) else {
            continue;
        };
        if rel == BROKEN {
            continue;
        }
        let source = std::fs::read(fixture_root().join(&rel)).expect("read fixture file");
        let tree = parse(id, &source);
        out.insert(rel, Parsed { source, tree });
    }
    out
}

/// Every node of `tree` in pre-order.
fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut cursor = tree.walk();
    loop {
        out.push(cursor.node());
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return out;
            }
        }
    }
}

/// One-based line and UTF-8 byte column of `offset`.
fn line_col(source: &[u8], offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    let line = before.iter().filter(|&&byte| byte == b'\n').count() + 1;
    let line_start = before
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    (line, offset - line_start + 1)
}

/// Spec §10.1: escape `%` and `#` in one ID component.
fn escape(component: &str) -> String {
    component.replace('%', "%25").replace('#', "%23")
}

const IDENTIFIER_KINDS: [&str; 4] = [
    "identifier",
    "type_identifier",
    "property_identifier",
    "private_property_identifier",
];

const SYMBOL_KINDS: [&str; 10] = [
    "class",
    "function",
    "method",
    "interface",
    "struct",
    "enum",
    "module",
    "property",
    "const",
    "type_alias",
];

const CONTAINER_KINDS: [&str; 5] = ["function", "method", "class", "interface", "enum"];

const REF_KINDS: [&str; 7] = [
    "call",
    "type",
    "import",
    "assignment",
    "read",
    "write",
    "unknown",
];

const TIERS: [&str; 3] = ["exact", "scoped", "name_match"];

#[test]
fn fixture_files_use_the_intended_grammar() {
    let listed: Vec<&str> = FIXTURE_FILES.iter().map(|(path, _)| *path).collect();
    let on_disk = walk(&fixture_root());
    assert_eq!(
        on_disk, listed,
        "fixture files changed; update FIXTURE_FILES"
    );

    for (rel, expected) in &FIXTURE_FILES {
        assert_eq!(language_for_path(rel), *expected, "{rel} dispatch");
        let Some(id) = expected else {
            continue;
        };
        let source = std::fs::read(fixture_root().join(rel)).expect("read fixture file");
        let tree = parse(*id, &source);
        assert_eq!(
            tree.root_node().has_error(),
            *rel == BROKEN,
            "{rel}: only {BROKEN} may fail the parse policy"
        );
    }

    // `.mts` is not claimed although the TypeScript grammar parses it cleanly.
    let esm = std::fs::read(fixture_root().join("src/esm.mts")).expect("read esm.mts");
    assert!(!parse(LanguageId::Typescript, &esm).root_node().has_error());
    // The TSX files need the TSX grammar: under plain TypeScript JSX fails.
    let app = std::fs::read(fixture_root().join("src/components/App.tsx")).expect("read App");
    assert!(parse(LanguageId::Typescript, &app).root_node().has_error());
}

/// Checks one entry's task tag against its table's allowed tasks, and that it
/// names a supported, cleanly parsing fixture file.
fn tagged(
    problems: &mut Vec<String>,
    files: &BTreeMap<String, Parsed>,
    table: &str,
    (task, allowed): (&str, &[&str]),
    file: &str,
    case: &str,
) {
    if !allowed.contains(&task) {
        problems.push(format!(
            "{table} {file} ({case}): task {task} not in {allowed:?}"
        ));
    }
    if !files.contains_key(file) {
        problems.push(format!(
            "{table} {file}: not a supported, cleanly parsing fixture file"
        ));
    }
    if case.is_empty() {
        problems.push(format!("{table} {file}: empty case"));
    }
}

#[test]
fn gold_is_well_formed() {
    let gold = load_gold();
    let files = parsed_files();
    let mut problems: Vec<String> = Vec::new();

    let done: BTreeSet<&str> = gold.done_tasks.iter().map(String::as_str).collect();
    if done.len() != gold.done_tasks.len() {
        problems.push(format!("done_tasks repeats a task: {:?}", gold.done_tasks));
    }
    for task in &done {
        if !TASKS.contains(task) {
            problems.push(format!("done_tasks names unknown task {task}"));
        }
    }
    if gold.focus_names.is_empty() {
        problems.push("focus_names is empty".to_string());
    }

    for d in &gold.declaration {
        tagged(
            &mut problems,
            &files,
            "declaration",
            (&d.task, &["T42"]),
            &d.file,
            &d.case,
        );
    }
    for d in &gold.not_a_declaration {
        tagged(
            &mut problems,
            &files,
            "not_a_declaration",
            (&d.task, &["T42"]),
            &d.file,
            &d.case,
        );
        assert!(!d.why.is_empty());
    }
    for u in &gold.uses {
        tagged(
            &mut problems,
            &files,
            "use",
            (&u.task, &["T43"]),
            &u.file,
            &u.case,
        );
    }
    for u in &gold.not_a_use {
        tagged(
            &mut problems,
            &files,
            "not_a_use",
            (&u.task, &["T43"]),
            &u.file,
            &u.case,
        );
        assert!(!u.why.is_empty());
    }
    for b in &gold.binding {
        tagged(
            &mut problems,
            &files,
            "binding",
            (&b.task, &["T44", "T45"]),
            &b.file,
            &b.case,
        );
    }
    for u in &gold.undecided {
        tagged(
            &mut problems,
            &files,
            "undecided",
            (&u.task, &["T42", "T43"]),
            &u.file,
            &u.case,
        );
        let expected_form = if u.task == "T42" {
            "declaration"
        } else {
            "use"
        };
        if u.form != expected_form {
            problems.push(format!(
                "undecided {} [{}, {}): a {} question has form {}",
                u.file, u.start_byte, u.end_byte, u.task, u.form
            ));
        }
        if u.question.is_empty() || u.proposal.is_empty() {
            problems.push(format!("undecided {}: empty question or proposal", u.file));
        }
        // A done task has settled every construct it was asked to.
        if done.contains(u.task.as_str()) {
            problems.push(format!(
                "undecided {} [{}, {}): {} is done but this construct is still undecided",
                u.file, u.start_byte, u.end_byte, u.task
            ));
        }
    }

    // Canonical IDs: spec §10.1 escaping, with an ordinal on every duplicate
    // qualified name in a file, in (start_byte, end_byte, kind) order.
    let mut by_name: BTreeMap<(&str, &str), Vec<&Declaration>> = BTreeMap::new();
    for d in &gold.declaration {
        if !SYMBOL_KINDS.contains(&d.kind.as_str()) {
            problems.push(format!("{}: kind {} is not a contract kind", d.id, d.kind));
        }
        by_name
            .entry((d.file.as_str(), d.qualified_name.as_str()))
            .or_default()
            .push(d);
    }
    for ((file, qualified_name), mut group) in by_name {
        let base = format!("{}#{}", escape(file), escape(qualified_name));
        group.sort_by(|a, b| {
            (a.start_byte, a.end_byte, &a.kind).cmp(&(b.start_byte, b.end_byte, &b.kind))
        });
        for (index, d) in group.iter().enumerate() {
            let expected = if group.len() == 1 {
                base.clone()
            } else {
                format!("{base}#{}", index + 1)
            };
            if d.id != expected {
                problems.push(format!("id {} should be {expected}", d.id));
            }
        }
    }
    let ids: BTreeMap<&str, &Declaration> = gold
        .declaration
        .iter()
        .map(|d| (d.id.as_str(), d))
        .collect();
    if ids.len() != gold.declaration.len() {
        problems.push("declaration ids are not unique".to_string());
    }

    // A use's container is the innermost container-kind declaration whose span
    // contains it, or "" when there is none.
    let mut use_spans: BTreeSet<(&str, usize, usize)> = BTreeSet::new();
    for u in &gold.uses {
        if !use_spans.insert((u.file.as_str(), u.start_byte, u.end_byte)) {
            problems.push(format!(
                "use {} [{}, {}) repeated",
                u.file, u.start_byte, u.end_byte
            ));
        }
        if !REF_KINDS.contains(&u.ref_kind.as_str()) {
            problems.push(format!("use {}: ref_kind {}", u.file, u.ref_kind));
        }
        let innermost = gold
            .declaration
            .iter()
            .filter(|d| {
                d.file == u.file
                    && CONTAINER_KINDS.contains(&d.kind.as_str())
                    && d.start_byte <= u.start_byte
                    && u.end_byte <= d.end_byte
            })
            .min_by_key(|d| d.end_byte - d.start_byte)
            .map_or("", |d| d.id.as_str());
        if u.container != innermost {
            problems.push(format!(
                "use {} [{}, {}) {:?}: container {:?}, innermost is {innermost:?}",
                u.file, u.start_byte, u.end_byte, u.text, u.container
            ));
        }
    }

    // Each binding names exactly one recorded use and a justified tier.
    let mut binding_spans: BTreeSet<(&str, usize, usize)> = BTreeSet::new();
    for b in &gold.binding {
        let key = (b.file.as_str(), b.start_byte, b.end_byte);
        if !use_spans.contains(&key) {
            problems.push(format!(
                "binding {} [{}, {}) names no use",
                b.file, b.start_byte, b.end_byte
            ));
        }
        if !binding_spans.insert(key) {
            problems.push(format!(
                "binding {} [{}, {}) repeated",
                b.file, b.start_byte, b.end_byte
            ));
        }
        if !TIERS.contains(&b.expected_resolution.as_str()) {
            problems.push(format!(
                "binding {}: tier {}",
                b.file, b.expected_resolution
            ));
        }
        // A lexical binding (T44) is `exact` when it resolves; a receiver
        // hint (T45) is at most `scoped` (spec 11.3, 11.4).
        let tiers: &[&str] = if b.task == "T44" {
            &["exact", "name_match"]
        } else {
            &["scoped", "name_match"]
        };
        if !tiers.contains(&b.expected_resolution.as_str()) {
            problems.push(format!(
                "binding {} [{}, {}): a {} binding cannot be {}",
                b.file, b.start_byte, b.end_byte, b.task, b.expected_resolution
            ));
        }
        let resolved = b.expected_resolution != "name_match";
        if resolved == b.expected_target.is_empty() {
            problems.push(format!(
                "binding {} [{}, {}): {} with target {:?}",
                b.file, b.start_byte, b.end_byte, b.expected_resolution, b.expected_target
            ));
        }
        for target in [Some(&b.expected_target), b.not_target.as_ref()]
            .into_iter()
            .flatten()
            .filter(|target| !target.is_empty())
        {
            if !ids.contains_key(target.as_str()) {
                problems.push(format!("binding target {target} is not a declaration id"));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "gold problems:\n{}",
        problems.join("\n")
    );
}

/// The nodes of `tree` whose span is exactly `[start, end)`, outermost first.
fn exact_nodes(tree: &Tree, start: usize, end: usize) -> Vec<Node<'_>> {
    nodes(tree)
        .into_iter()
        .filter(|node| node.start_byte() == start && node.end_byte() == end)
        .collect()
}

/// A declaration-like span as the gold records it.
struct BlockSpan<'a> {
    table: &'a str,
    file: &'a str,
    start: usize,
    end: usize,
    lines: (usize, usize),
    text: &'a str,
    end_text: &'a str,
}

/// A declaration-like span: `text` begins it, `end_text` ends it, its lines
/// match, and exactly its bytes are one named grammar node.
fn check_block(problems: &mut Vec<String>, files: &BTreeMap<String, Parsed>, span: BlockSpan<'_>) {
    let BlockSpan {
        table,
        file,
        start,
        end,
        lines,
        text,
        end_text,
    } = span;
    let Some(parsed) = files.get(file) else {
        return;
    };
    let source = &parsed.source;
    if !(start < end && end <= source.len()) {
        problems.push(format!("{table} {file} [{start}, {end}): out of range"));
        return;
    }
    let bytes = &source[start..end];
    if text.is_empty() || !bytes.starts_with(text.as_bytes()) {
        problems.push(format!("{table} {file} [{start}, {end}): text {text:?}"));
    }
    if end_text.is_empty() || !bytes.ends_with(end_text.as_bytes()) {
        problems.push(format!(
            "{table} {file} [{start}, {end}): end_text {end_text:?}"
        ));
    }
    let actual = (line_col(source, start).0, line_col(source, end - 1).0);
    if actual != lines {
        problems.push(format!(
            "{table} {file} [{start}, {end}): lines {actual:?}, recorded {lines:?}"
        ));
    }
    if !exact_nodes(&parsed.tree, start, end)
        .iter()
        .any(Node::is_named)
    {
        problems.push(format!(
            "{table} {file} [{start}, {end}) {text:?}: no named grammar node has this span"
        ));
    }
}

/// A use-like span as the gold records it.
struct PointSpan<'a> {
    table: &'a str,
    file: &'a str,
    start: usize,
    end: usize,
    line: usize,
    column: usize,
    text: &'a str,
}

/// A use-like span: exactly the recorded text at the recorded line and
/// column. Returns the kinds of the nodes with exactly this span, each with
/// its parent's kind, or `None` when the bytes do not match.
fn check_point(
    problems: &mut Vec<String>,
    files: &BTreeMap<String, Parsed>,
    span: PointSpan<'_>,
) -> Option<Vec<(String, Option<String>)>> {
    let PointSpan {
        table,
        file,
        start,
        end,
        line,
        column,
        text,
    } = span;
    let parsed = files.get(file)?;
    let source = &parsed.source;
    if !(start < end && end <= source.len()) || &source[start..end] != text.as_bytes() {
        problems.push(format!("{table} {file} [{start}, {end}): text {text:?}"));
        return None;
    }
    let actual = line_col(source, start);
    if actual != (line, column) {
        problems.push(format!(
            "{table} {file} [{start}, {end}): line/column {actual:?}, recorded {:?}",
            (line, column)
        ));
    }
    Some(
        exact_nodes(&parsed.tree, start, end)
            .iter()
            .map(|node| {
                (
                    node.kind().to_string(),
                    node.parent().map(|parent| parent.kind().to_string()),
                )
            })
            .collect(),
    )
}

fn is_identifier(kinds: &[(String, Option<String>)]) -> bool {
    kinds
        .iter()
        .any(|(kind, _)| IDENTIFIER_KINDS.contains(&kind.as_str()))
}

#[test]
fn gold_spans_match_the_fixture_and_the_grammar() {
    let gold = load_gold();
    let files = parsed_files();
    let mut problems: Vec<String> = Vec::new();
    let p = &mut problems;

    for d in &gold.declaration {
        check_block(
            p,
            &files,
            BlockSpan {
                table: "declaration",
                file: &d.file,
                start: d.start_byte,
                end: d.end_byte,
                lines: (d.start_line, d.end_line),
                text: &d.text,
                end_text: &d.end_text,
            },
        );
    }
    for d in &gold.not_a_declaration {
        check_block(
            p,
            &files,
            BlockSpan {
                table: "not_a_declaration",
                file: &d.file,
                start: d.start_byte,
                end: d.end_byte,
                lines: (d.start_line, d.end_line),
                text: &d.text,
                end_text: &d.end_text,
            },
        );
    }
    for u in &gold.undecided {
        let span = |line, column| PointSpan {
            table: "undecided",
            file: &u.file,
            start: u.start_byte,
            end: u.end_byte,
            line,
            column,
            text: &u.text,
        };
        match (
            u.form.as_str(),
            u.start_line,
            u.end_line,
            u.end_text.as_deref(),
            u.line,
            u.column,
        ) {
            ("declaration", Some(start_line), Some(end_line), Some(end_text), None, None) => {
                check_block(
                    p,
                    &files,
                    BlockSpan {
                        table: "undecided",
                        file: &u.file,
                        start: u.start_byte,
                        end: u.end_byte,
                        lines: (start_line, end_line),
                        text: &u.text,
                        end_text,
                    },
                );
            }
            ("use", None, None, None, Some(line), Some(column)) => {
                if let Some(kinds) = check_point(p, &files, span(line, column))
                    && !is_identifier(&kinds)
                {
                    p.push(format!(
                        "undecided {} [{}, {}) is {kinds:?}",
                        u.file, u.start_byte, u.end_byte
                    ));
                }
            }
            _ => p.push(format!(
                "undecided {} [{}, {}): fields do not match form {}",
                u.file, u.start_byte, u.end_byte, u.form
            )),
        }
    }
    for u in &gold.uses {
        let span = PointSpan {
            table: "use",
            file: &u.file,
            start: u.start_byte,
            end: u.end_byte,
            line: u.line,
            column: u.column,
            text: &u.text,
        };
        if let Some(kinds) = check_point(p, &files, span)
            && !is_identifier(&kinds)
        {
            p.push(format!(
                "use {} [{}, {}) is {kinds:?}",
                u.file, u.start_byte, u.end_byte
            ));
        }
        // The receiver is the object of the member expression whose property
        // is this use, or "" when the use is not such a property.
        if let Some(parsed) = files.get(&u.file) {
            let receiver = exact_nodes(&parsed.tree, u.start_byte, u.end_byte)
                .into_iter()
                .filter_map(|node| {
                    let parent = node.parent()?;
                    (parent.kind() == "member_expression"
                        && parent.child_by_field_name("property") == Some(node))
                    .then(|| parent.child_by_field_name("object"))
                    .flatten()
                })
                .map(|object| {
                    String::from_utf8_lossy(&parsed.source[object.start_byte()..object.end_byte()])
                        .into_owned()
                })
                .next()
                .unwrap_or_default();
            if u.receiver != receiver {
                p.push(format!(
                    "use {} [{}, {}): receiver {:?}, grammar says {receiver:?}",
                    u.file, u.start_byte, u.end_byte, u.receiver
                ));
            }
        }
    }
    for b in &gold.binding {
        check_point(
            p,
            &files,
            PointSpan {
                table: "binding",
                file: &b.file,
                start: b.start_byte,
                end: b.end_byte,
                line: b.line,
                column: b.column,
                text: &b.text,
            },
        );
    }
    for u in &gold.not_a_use {
        let span = PointSpan {
            table: "not_a_use",
            file: &u.file,
            start: u.start_byte,
            end: u.end_byte,
            line: u.line,
            column: u.column,
            text: &u.text,
        };
        let Some(kinds) = check_point(p, &files, span) else {
            continue;
        };
        let tree = &files[&u.file].tree;
        // Literal text: a comment, string fragment, or JSX text node holds it.
        let literal = nodes(tree).iter().any(|node| {
            matches!(node.kind(), "comment" | "string_fragment" | "jsx_text")
                && node.start_byte() <= u.start_byte
                && u.end_byte <= node.end_byte()
        });
        // Settled by T43: a JSX attribute name, and a closing tag's name.
        let jsx_name = kinds.iter().any(|(kind, parent)| {
            (kind == "property_identifier" && parent.as_deref() == Some("jsx_attribute"))
                || (kind == "identifier" && parent.as_deref() == Some("jsx_closing_element"))
        });
        // A lowercase intrinsic JSX element name.
        let intrinsic = u.text.starts_with(|c: char| c.is_ascii_lowercase())
            && kinds.iter().any(|(kind, parent)| {
                kind == "identifier"
                    && matches!(
                        parent.as_deref(),
                        Some(
                            "jsx_opening_element"
                                | "jsx_closing_element"
                                | "jsx_self_closing_element"
                        )
                    )
            });
        if !(literal || intrinsic || jsx_name) {
            p.push(format!(
                "not_a_use {} [{}, {}) {:?} is neither literal text, an intrinsic element, \
                 a JSX attribute name, nor a closing tag name",
                u.file, u.start_byte, u.end_byte, u.text
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "span problems:\n{}",
        problems.join("\n")
    );
}

/// The name identifiers a declaration-like node introduces.
fn declared_names(node: Node<'_>) -> Vec<(usize, usize)> {
    match node.kind() {
        "export_statement" => node
            .child_by_field_name("declaration")
            .or_else(|| node.child_by_field_name("value"))
            .map(declared_names)
            .unwrap_or_default(),
        "ambient_declaration" | "expression_statement" => {
            node.named_child(0).map(declared_names).unwrap_or_default()
        }
        "lexical_declaration" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .filter(|child| child.kind() == "variable_declarator")
                .filter_map(|child| child.child_by_field_name("name"))
                .map(|name| (name.start_byte(), name.end_byte()))
                .collect()
        }
        "required_parameter" => node
            .child_by_field_name("pattern")
            .map(|name| vec![(name.start_byte(), name.end_byte())])
            .unwrap_or_default(),
        "property_identifier" | "type_identifier" | "identifier" => {
            vec![(node.start_byte(), node.end_byte())]
        }
        _ => node
            .child_by_field_name("name")
            .map(|name| vec![(name.start_byte(), name.end_byte())])
            .unwrap_or_default(),
    }
}

/// Whether `byte` can continue an identifier (ASCII view; `$` and `#` count
/// so `#double` or `$double` is not a whole-word match).
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'#')
}

#[test]
fn focus_names_are_fully_annotated() {
    let gold = load_gold();
    let files = parsed_files();
    let mut problems: Vec<String> = Vec::new();

    // Spans that account for an occurrence directly.
    let mut listed: BTreeMap<(&str, usize, usize), usize> = BTreeMap::new();
    for (file, start, end) in gold
        .uses
        .iter()
        .map(|u| (u.file.as_str(), u.start_byte, u.end_byte))
        .chain(
            gold.not_a_use
                .iter()
                .map(|u| (u.file.as_str(), u.start_byte, u.end_byte)),
        )
        .chain(
            gold.undecided
                .iter()
                .filter(|u| u.form == "use")
                .map(|u| (u.file.as_str(), u.start_byte, u.end_byte)),
        )
    {
        *listed.entry((file, start, end)).or_default() += 1;
    }
    // Declaration names, found from each declaration-like span's node.
    let mut names: BTreeSet<(&str, usize, usize)> = BTreeSet::new();
    let block_spans = gold
        .declaration
        .iter()
        .map(|d| (d.file.as_str(), d.start_byte, d.end_byte))
        .chain(
            gold.not_a_declaration
                .iter()
                .map(|d| (d.file.as_str(), d.start_byte, d.end_byte)),
        )
        .chain(
            gold.undecided
                .iter()
                .filter(|u| u.form == "declaration")
                .map(|u| (u.file.as_str(), u.start_byte, u.end_byte)),
        );
    for (file, start, end) in block_spans {
        let Some(parsed) = files.get(file) else {
            continue;
        };
        if let Some(node) = exact_nodes(&parsed.tree, start, end)
            .into_iter()
            .find(Node::is_named)
        {
            for (name_start, name_end) in declared_names(node) {
                names.insert((file, name_start, name_end));
            }
        }
    }

    let mut seen = 0_usize;
    for (file, parsed) in &files {
        let all = nodes(&parsed.tree);
        for name in &gold.focus_names {
            let needle = name.as_bytes();
            let source = &parsed.source;
            let mut from = 0;
            while let Some(found) = source[from..]
                .windows(needle.len())
                .position(|window| window == needle)
            {
                let start = from + found;
                let end = start + needle.len();
                from = end;
                let before_ok = start == 0 || !is_word_byte(source[start - 1]);
                let after_ok = end == source.len() || !is_word_byte(source[end]);
                if !(before_ok && after_ok) {
                    continue;
                }
                let in_comment = all.iter().any(|node| {
                    node.kind() == "comment" && node.start_byte() <= start && end <= node.end_byte()
                });
                if in_comment {
                    continue;
                }
                seen += 1;
                let key = (file.as_str(), start, end);
                let count =
                    listed.get(&key).copied().unwrap_or(0) + usize::from(names.contains(&key));
                if count != 1 {
                    let (line, column) = line_col(source, start);
                    problems.push(format!(
                        "{file}:{line}:{column} {name}: accounted for {count} times (want exactly one)"
                    ));
                }
            }
        }
    }
    assert!(seen > 0, "no focus-name occurrences found");
    assert!(
        problems.is_empty(),
        "focus-name gaps:\n{}",
        problems.join("\n")
    );
}

/// Every supported fixture file, the broken one included, extracted by the
/// TypeScript adapter with the grammar its extension dispatches to, visiting
/// files in `order`.
fn extract_fixture(order: &[String]) -> BTreeMap<String, (Vec<u8>, ExtractedFile)> {
    let mut out = BTreeMap::new();
    for rel in order {
        let Some(id) = language_for_path(rel) else {
            continue;
        };
        let source = std::fs::read(fixture_root().join(rel)).expect("read fixture file");
        let tree = parse(id, &source);
        let extracted = typescript::extract(&source, &tree);
        out.insert(rel.clone(), (source, extracted));
    }
    out
}

/// One file's canonical symbol IDs, computed exactly as refresh computes PHP
/// IDs: `rivet_core` duplicate ordinals and spec §10.1 escaping.
fn canonical_ids(path: &str, extracted: &ExtractedFile) -> Vec<String> {
    let items: Vec<_> = extracted
        .symbols
        .iter()
        .map(|symbol| (symbol.qualified_name.as_str(), symbol.span, symbol.kind))
        .collect();
    extracted
        .symbols
        .iter()
        .zip(assign_ordinals(&items))
        .map(|(symbol, ordinal)| {
            SymbolId::new(path, &symbol.qualified_name, ordinal)
                .expect("a non-empty path and qualified name")
                .as_canonical()
        })
        .collect()
}

/// T42's harness: the extractor's symbols are exactly the gold declarations.
///
/// For every supported fixture file, the extracted `(id, qualified_name,
/// kind, start_byte, end_byte)` set equals the file's `[[declaration]]` set,
/// in both directions. Each symbol's name span is the name's own bytes and one
/// of the names its gold node declares, and a parent's qualified name
/// prefixes its member's. No `[[not_a_declaration]]` span is a symbol span,
/// and no symbol's name lies inside one. `src/broken.ts` yields no facts and
/// one `parse_error`. Extraction is byte-identical across runs and file
/// orders.
#[test]
fn t42_entries_match_the_extractor() {
    let gold = load_gold();
    assert!(
        gold.done_tasks.iter().any(|task| task == "T42"),
        "this harness verifies T42; list it in done_tasks"
    );
    let order = walk(&fixture_root());
    let extracted = extract_fixture(&order);
    let files = parsed_files();
    let mut problems: Vec<String> = Vec::new();

    let (_, broken) = &extracted[BROKEN];
    if !(broken.symbols.is_empty()
        && broken.diagnostics.len() == 1
        && broken.diagnostics[0].code == "parse_error")
    {
        problems.push(format!(
            "{BROKEN}: want no facts and one parse_error, got {broken:?}"
        ));
    }

    type Key = (String, String, String, String, u32, u32);
    let mut actual: BTreeSet<Key> = BTreeSet::new();
    for (file, (source, file_facts)) in &extracted {
        if file == BROKEN {
            continue;
        }
        if !file_facts.diagnostics.is_empty() {
            problems.push(format!("{file}: diagnostics {:?}", file_facts.diagnostics));
        }
        let ids = canonical_ids(file, file_facts);
        for (symbol, id) in file_facts.symbols.iter().zip(ids) {
            let key = (
                file.clone(),
                id.clone(),
                symbol.qualified_name.clone(),
                symbol.kind.as_str().to_string(),
                symbol.span.start_byte(),
                symbol.span.end_byte(),
            );
            if !actual.insert(key) {
                problems.push(format!("{id}: extracted twice"));
            }
            let Some(name_span) = symbol.name_span else {
                problems.push(format!("{id}: no name span"));
                continue;
            };
            let (start, end) = (
                name_span.start_byte() as usize,
                name_span.end_byte() as usize,
            );
            if source.get(start..end) != Some(symbol.name.as_bytes()) {
                problems.push(format!(
                    "{id}: name span [{start}, {end}) is not {:?}",
                    symbol.name
                ));
            }
            // The name is one the gold node at this span declares.
            let declared = files
                .get(file)
                .and_then(|parsed| {
                    exact_nodes(
                        &parsed.tree,
                        symbol.span.start_byte() as usize,
                        symbol.span.end_byte() as usize,
                    )
                    .into_iter()
                    .find(Node::is_named)
                })
                .map(declared_names)
                .unwrap_or_default();
            if !declared.contains(&(start, end)) {
                problems.push(format!(
                    "{id}: name span [{start}, {end}) is not declared by its node ({declared:?})"
                ));
            }
            let expected_qualified = match symbol.parent_index {
                Some(parent) => format!(
                    "{}.{}",
                    file_facts.symbols[parent].qualified_name, symbol.name
                ),
                None => symbol.name.clone(),
            };
            if symbol.qualified_name != expected_qualified {
                problems.push(format!(
                    "{id}: qualified name is not its parent's plus its name ({expected_qualified})"
                ));
            }
        }
        // Not-declarations: never a symbol span, and no name inside one.
        for d in gold.not_a_declaration.iter().filter(|d| &d.file == file) {
            for symbol in &file_facts.symbols {
                let span = (
                    symbol.span.start_byte() as usize,
                    symbol.span.end_byte() as usize,
                );
                let name_inside = symbol.name_span.is_some_and(|name| {
                    d.start_byte <= name.start_byte() as usize
                        && name.end_byte() as usize <= d.end_byte
                });
                if span == (d.start_byte, d.end_byte) || name_inside {
                    problems.push(format!(
                        "not_a_declaration {file} [{}, {}) ({}) is extracted as {}",
                        d.start_byte, d.end_byte, d.why, symbol.qualified_name
                    ));
                }
            }
        }
    }

    let expected: BTreeSet<Key> = gold
        .declaration
        .iter()
        .filter(|d| d.task == "T42")
        .map(|d| {
            (
                d.file.clone(),
                d.id.clone(),
                d.qualified_name.clone(),
                d.kind.clone(),
                d.start_byte as u32,
                d.end_byte as u32,
            )
        })
        .collect();
    for missing in expected.difference(&actual) {
        problems.push(format!("gold declaration not extracted: {missing:?}"));
    }
    for extra in actual.difference(&expected) {
        problems.push(format!("extracted symbol not in gold: {extra:?}"));
    }

    // Determinism: same bytes, same records, in any file order.
    let mut reversed = order.clone();
    reversed.reverse();
    let again = extract_fixture(&reversed);
    if format!("{extracted:?}") != format!("{again:?}") {
        problems.push("extraction differs between runs or file orders".to_string());
    }

    assert!(
        problems.is_empty(),
        "T42 extractor problems:\n{}",
        problems.join("\n")
    );
    let verified = gold.declaration.iter().filter(|d| d.task == "T42").count()
        + gold
            .not_a_declaration
            .iter()
            .filter(|d| d.task == "T42")
            .count();
    println!("typescript T42: {verified} entries verified against the extractor");
}

/// T43's harness: the extractor's uses match every T43 gold entry.
///
/// For every supported fixture file:
///
/// - every `[[use]]` is extracted exactly once at its span, with its
///   `ref_kind`, its container (the canonical ID of the extracted
///   `containing_symbol_index`, or none), and its receiver;
/// - no extracted use lies inside a `[[not_a_use]]` span;
/// - every extracted use of a focus name is a `[[use]]`, so candidate mode
///   finds exactly the gold's same-name uses (the focus names are fully
///   annotated, [`focus_names_are_fully_annotated`]);
/// - every use's scope is recorded and its parent chain reaches the module
///   scope, and every `import` use is the span of an import binding or
///   re-export recorded in that scope;
/// - `src/broken.ts` yields no uses or scopes.
///
/// Determinism across runs and file orders is checked over the whole
/// extraction by [`t42_entries_match_the_extractor`].
#[test]
fn t43_entries_match_the_extractor() {
    let gold = load_gold();
    assert!(
        gold.done_tasks.iter().any(|task| task == "T43"),
        "this harness verifies T43; list it in done_tasks"
    );
    let order = walk(&fixture_root());
    let extracted = extract_fixture(&order);
    let mut problems: Vec<String> = Vec::new();

    let (_, broken) = &extracted[BROKEN];
    if !(broken.uses.is_empty() && broken.scopes.is_empty()) {
        problems.push(format!("{BROKEN}: uses or scopes {broken:?}"));
    }

    type Key = (String, usize, usize);
    // (ref_kind, container id or "", receiver or "") by (file, span).
    let mut actual: BTreeMap<Key, Vec<(String, String, String)>> = BTreeMap::new();
    for (file, (source, file_facts)) in &extracted {
        if file == BROKEN {
            continue;
        }
        let ids = canonical_ids(file, file_facts);
        let scopes: BTreeMap<&str, &rivet_core::ExtractedScope> = file_facts
            .scopes
            .iter()
            .map(|scope| (scope.scope_key.as_str(), scope))
            .collect();
        for scope in &file_facts.scopes {
            if let Some(parent) = scope.parent_scope_key.as_deref()
                && !scopes.contains_key(parent)
            {
                problems.push(format!(
                    "{file}: scope {} has no parent {parent}",
                    scope.scope_key
                ));
            }
        }
        if !scopes.contains_key(typescript::MODULE_SCOPE_KEY) {
            problems.push(format!("{file}: no module scope"));
        }
        for use_ in &file_facts.uses {
            let (start, end) = (
                use_.span.start_byte() as usize,
                use_.span.end_byte() as usize,
            );
            if source.get(start..end) != Some(use_.spelling.as_bytes()) {
                problems.push(format!(
                    "{file} [{start}, {end}): spelling {:?}",
                    use_.spelling
                ));
            }
            let container = use_
                .containing_symbol_index
                .map(|index| ids[index].clone())
                .unwrap_or_default();
            actual.entry((file.clone(), start, end)).or_default().push((
                use_.ref_kind.as_str().to_string(),
                container,
                use_.receiver.clone().unwrap_or_default(),
            ));
            // The scope chain reaches the module scope.
            let mut key = Some(use_.scope_key.as_str());
            let mut chain = Vec::new();
            while let Some(current) = key {
                let Some(scope) = scopes.get(current) else {
                    problems.push(format!("{file} [{start}, {end}): no scope {current}"));
                    break;
                };
                if chain.contains(&current) {
                    problems.push(format!("{file}: scope cycle at {current}"));
                    break;
                }
                chain.push(current);
                key = scope.parent_scope_key.as_deref();
            }
            if chain.last() != Some(&typescript::MODULE_SCOPE_KEY) {
                problems.push(format!(
                    "{file} [{start}, {end}): scope chain {chain:?} misses the module scope"
                ));
            }
            // An import use is the span of an import or re-export of its scope.
            if use_.ref_kind == rivet_core::RefKind::Import {
                let recorded = scopes.get(use_.scope_key.as_str()).is_some_and(|scope| {
                    scope
                        .facts
                        .module_imports
                        .iter()
                        .any(|import| import.span == use_.span)
                });
                if !recorded {
                    problems.push(format!(
                        "{file} [{start}, {end}) {:?}: an import use with no import binding",
                        use_.spelling
                    ));
                }
            }
        }
    }

    // Every gold use, exactly.
    let mut expected: BTreeMap<Key, (String, String, String)> = BTreeMap::new();
    for u in gold.uses.iter().filter(|u| u.task == "T43") {
        let key = (u.file.clone(), u.start_byte, u.end_byte);
        let want = (u.ref_kind.clone(), u.container.clone(), u.receiver.clone());
        match actual.get(&key).map(Vec::as_slice) {
            Some([got]) if *got == want => {}
            Some(got) => problems.push(format!(
                "use {} [{}, {}) {:?}: want {want:?}, extracted {got:?}",
                u.file, u.start_byte, u.end_byte, u.text
            )),
            None => problems.push(format!(
                "use {} [{}, {}) {:?}: not extracted (want {want:?})",
                u.file, u.start_byte, u.end_byte, u.text
            )),
        }
        expected.insert(key, want);
    }
    // No use inside a not-a-use span.
    for u in gold.not_a_use.iter().filter(|u| u.task == "T43") {
        for (file, start, end) in actual.keys() {
            if *file == u.file && u.start_byte <= *start && *end <= u.end_byte {
                problems.push(format!(
                    "not_a_use {} [{}, {}) ({}) is extracted as a use",
                    u.file, u.start_byte, u.end_byte, u.why
                ));
            }
        }
    }
    // Every extracted use of a focus name is in the gold.
    for (file, (source, _)) in &extracted {
        for (key, _) in actual.range((file.clone(), 0, 0)..(file.clone(), usize::MAX, usize::MAX)) {
            let spelling = &source[key.1..key.2];
            if gold
                .focus_names
                .iter()
                .any(|name| name.as_bytes() == spelling)
                && !expected.contains_key(key)
            {
                problems.push(format!(
                    "extracted use {} [{}, {}) {:?} of a focus name is not in the gold",
                    key.0,
                    key.1,
                    key.2,
                    String::from_utf8_lossy(spelling)
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "T43 extractor problems:\n{}",
        problems.join("\n")
    );
    let verified = gold.uses.iter().filter(|u| u.task == "T43").count()
        + gold.not_a_use.iter().filter(|u| u.task == "T43").count();
    println!("typescript T43: {verified} entries verified against the extractor");
}
