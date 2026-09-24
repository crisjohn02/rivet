//! T44: the authored TypeScript fixture's bindings, from the extractor
//! through the resolver.
//!
//! This is the extractor-and-resolver half of the T44 gold harness
//! (`crates/rivet-cli/tests/typescript_index.rs` is the end-to-end half). It
//! extracts every fixture file with the real TypeScript adapter
//! (`rivet_parser::parse_file`), turns the facts into the rows refresh
//! persists (canonical IDs, use rows, and `scopes.facts_json` with
//! `declares` as IDs), runs [`Resolver`] over them, and requires:
//!
//! - every T44 `[[binding]]` to hold: an `exact` entry binds its use to
//!   `expected_target` at that tier, and a `name_match` entry binds nothing;
//! - no use to bind a `not_target`;
//! - every T45 entry to stay unbound until T45 is done (receivers are T45's,
//!   and T44 must not bind them at any tier);
//! - no TypeScript use to get a receiver class; and
//! - the links to be identical whatever order the rows arrive in.
//!
//! Row building mirrors `rivet-cli`'s refresh (`build_facts`); the CLI half
//! checks the real persistence path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rivet_core::{ExtractedFile, LineIndex, ParseStatus, Resolution, SymbolId, assign_ordinals};
use rivet_index::{ResolvedLinks, Resolver};
use rivet_languages::{language_for_path, typescript};
use rivet_store::{FileRow, ScopeRow, SymbolRow, UseRow};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/typescript/authored")
}

fn gold() -> toml::Table {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/gold/typescript-authored.toml");
    toml::from_str(&std::fs::read_to_string(path).expect("read gold")).expect("parse gold")
}

/// Every regular file under the fixture root, as sorted `/` paths.
fn fixture_files() -> Vec<String> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under root");
                let parts: Vec<&str> = rel
                    .components()
                    .map(|part| part.as_os_str().to_str().expect("UTF-8"))
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    let root = fixture_root();
    let mut out = Vec::new();
    visit(&root, &root, &mut out);
    out.sort();
    out
}

/// The rows refresh would publish for the fixture.
struct Rows {
    files: Vec<FileRow>,
    symbols: Vec<SymbolRow>,
    uses: Vec<UseRow>,
    scopes: Vec<ScopeRow>,
}

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
                .expect("an ID")
                .as_canonical()
        })
        .collect()
}

fn fixture_rows() -> Rows {
    let mut rows = Rows {
        files: Vec::new(),
        symbols: Vec::new(),
        uses: Vec::new(),
        scopes: Vec::new(),
    };
    let mut next_use_id = 1_i64;
    for path in fixture_files() {
        let Some(language) = language_for_path(&path) else {
            rows.files.push(FileRow {
                path,
                language: None,
                mtime_ns: 0,
                size: 0,
                content_hash: None,
                source: None,
                parse_status: ParseStatus::Unsupported,
            });
            continue;
        };
        let source = std::fs::read(fixture_root().join(&path)).expect("read fixture file");
        let extracted = rivet_parser::parse_file(language, &source);
        let parse_status = if extracted.diagnostics.is_empty() {
            ParseStatus::Ok
        } else {
            ParseStatus::ParseError
        };
        rows.files.push(FileRow {
            path: path.clone(),
            language: Some(language.name().to_string()),
            mtime_ns: 0,
            size: source.len() as u64,
            content_hash: None,
            source: None,
            parse_status,
        });
        let ids = canonical_ids(&path, &extracted);
        let lines = LineIndex::new(&source);
        for (index, symbol) in extracted.symbols.iter().enumerate() {
            rows.symbols.push(SymbolRow {
                id: ids[index].clone(),
                file: path.clone(),
                name: symbol.name.clone(),
                lookup_name: typescript::lookup_name(&symbol.name, symbol.kind),
                qualified_name: symbol.qualified_name.clone(),
                kind: symbol.kind,
                parent_id: symbol.parent_index.map(|parent| ids[parent].clone()),
                start_byte: symbol.span.start_byte(),
                end_byte: symbol.span.end_byte(),
                start_line: lines.start_line(symbol.span),
                end_line: lines.end_line(symbol.span),
                signature: symbol.signature.clone(),
                doc_comment: symbol.doc_comment.clone(),
            });
        }
        for use_ in &extracted.uses {
            let position = lines.line_col(use_.span.start_byte()).expect("a position");
            rows.uses.push(UseRow {
                use_id: Some(next_use_id),
                file: path.clone(),
                containing_symbol: use_.containing_symbol_index.map(|index| ids[index].clone()),
                scope_key: use_.scope_key.clone(),
                spelling: use_.spelling.clone(),
                lookup_name: typescript::use_lookup_name(&use_.spelling),
                ref_kind: use_.ref_kind,
                start_byte: use_.span.start_byte(),
                end_byte: use_.span.end_byte(),
                line: position.line,
                col: position.column,
                receiver: use_.receiver.clone(),
                hint_json: serde_json::to_string(&use_.hint).expect("hint JSON"),
            });
            next_use_id += 1;
        }
        for scope in &extracted.scopes {
            // The persisted shape: every fact, `declares` as canonical IDs.
            let mut facts = serde_json::to_value(&scope.facts).expect("facts JSON");
            facts["declares"] = scope
                .facts
                .declares
                .iter()
                .map(|index| serde_json::Value::from(ids[*index].clone()))
                .collect();
            rows.scopes.push(ScopeRow {
                file: path.clone(),
                scope_key: scope.scope_key.clone(),
                parent_scope_key: scope.parent_scope_key.clone(),
                facts_json: facts.to_string(),
            });
        }
    }
    rows
}

/// `(target, resolution)` by `(file, start_byte, end_byte)` of the bound use.
type Bound = BTreeMap<(String, u32, u32), (String, Resolution)>;

fn bound(rows: &Rows, links: &ResolvedLinks) -> Bound {
    let by_id: BTreeMap<i64, &UseRow> = rows
        .uses
        .iter()
        .map(|row| (row.use_id.expect("an ID"), row))
        .collect();
    links
        .bindings
        .iter()
        .map(|binding| {
            let row = by_id[&binding.use_id];
            (
                (row.file.clone(), row.start_byte, row.end_byte),
                (binding.target_id.clone(), binding.resolution),
            )
        })
        .collect()
}

fn text<'a>(entry: &'a toml::Table, key: &str) -> &'a str {
    entry[key].as_str().unwrap_or_else(|| panic!("{key}"))
}

fn span(entry: &toml::Table) -> (String, u32, u32) {
    (
        text(entry, "file").to_string(),
        entry["start_byte"].as_integer().expect("start") as u32,
        entry["end_byte"].as_integer().expect("end") as u32,
    )
}

#[test]
fn t44_entries_match_the_extractor_and_resolver() {
    let gold = gold();
    let done: Vec<&str> = gold["done_tasks"]
        .as_array()
        .expect("done_tasks")
        .iter()
        .map(|task| task.as_str().expect("a task"))
        .collect();
    assert!(
        done.contains(&"T44"),
        "this harness verifies T44; list it in done_tasks"
    );
    let rows = fixture_rows();
    let links = Resolver::new(&rows.files, &rows.symbols, &rows.uses, &rows.scopes).resolve_links();
    let bound = bound(&rows, &links);
    let use_spans: BTreeMap<(String, u32, u32), usize> =
        rows.uses.iter().fold(BTreeMap::new(), |mut spans, row| {
            *spans
                .entry((row.file.clone(), row.start_byte, row.end_byte))
                .or_default() += 1;
            spans
        });

    let mut problems = Vec::new();
    let mut verified = 0;
    for entry in gold["binding"]
        .as_array()
        .expect("bindings")
        .iter()
        .filter_map(toml::Value::as_table)
    {
        let key = span(entry);
        if use_spans.get(&key) != Some(&1) {
            problems.push(format!("binding {key:?}: not exactly one extracted use"));
            continue;
        }
        let got = bound.get(&key);
        match text(entry, "task") {
            "T44" => {
                verified += 1;
                let want = match text(entry, "expected_resolution") {
                    "name_match" => None,
                    "exact" => Some((
                        text(entry, "expected_target").to_string(),
                        Resolution::Exact,
                    )),
                    other => {
                        problems.push(format!("binding {key:?}: T44 tier {other}"));
                        continue;
                    }
                };
                if got != want.as_ref() {
                    problems.push(format!(
                        "binding {key:?} ({}): want {want:?}, resolved {got:?}",
                        text(entry, "case")
                    ));
                }
            }
            // Receivers are T45's: until it is done none of its uses binds.
            "T45" if !done.contains(&"T45") => {
                if let Some(got) = got {
                    problems.push(format!("T45 binding {key:?} is bound early: {got:?}"));
                }
            }
            _ => {}
        }
        if let Some(not_target) = entry.get("not_target").and_then(toml::Value::as_str)
            && got.is_some_and(|(target, _)| target == not_target)
        {
            problems.push(format!("binding {key:?} resolves to its not_target"));
        }
    }
    if !links.receiver_classes.is_empty() {
        problems.push(format!(
            "TypeScript receiver classes: {:?}",
            links.receiver_classes
        ));
    }

    // Determinism: the same links whatever order the rows arrive in.
    let mut reversed = Rows {
        files: rows.files.clone(),
        symbols: rows.symbols.clone(),
        uses: rows.uses.clone(),
        scopes: rows.scopes.clone(),
    };
    reversed.files.reverse();
    reversed.symbols.reverse();
    reversed.uses.reverse();
    reversed.scopes.reverse();
    let again = Resolver::new(
        &reversed.files,
        &reversed.symbols,
        &reversed.uses,
        &reversed.scopes,
    )
    .resolve_links();
    if again != links {
        problems.push("links differ when the rows arrive in reverse order".to_string());
    }

    assert!(
        problems.is_empty(),
        "T44 resolver problems:\n{}",
        problems.join("\n")
    );
    let exact = bound
        .values()
        .filter(|(_, tier)| *tier == Resolution::Exact)
        .count();
    println!(
        "typescript T44: {verified} entries verified against the extractor and resolver; \
         the fixture's {} uses give {exact} exact and {} scoped bindings",
        rows.uses.len(),
        bound.len() - exact
    );
}
