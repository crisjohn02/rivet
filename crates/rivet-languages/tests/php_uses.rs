//! T17 PHP use/import extraction tests.
//!
//! The three authored fixture files are extracted and compared against the
//! `[[use]]` and `[[not_a_use]]` entries in `tests/gold/php-authored.toml`.
//! Every gold use span must have exactly one extracted match with the same
//! ref kind; no extracted use may overlap a `not_a_use` span; and every
//! extracted `Call` must be listed in gold (the gold lists every `launch`
//! call). Non-`launch` uses are allowed.

#![cfg(feature = "lang-php")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rivet_core::extract::{ExtractedUse, ImportKind, TypedOrigin, UseHint};
use rivet_core::{ExtractedFile, RefKind};
use rivet_languages::{LanguageId, grammar, php};
use tree_sitter::{Parser, Tree};

#[derive(serde::Deserialize)]
struct GoldFile {
    #[serde(default, rename = "use")]
    uses: Vec<GoldUse>,
    #[serde(default)]
    not_a_use: Vec<GoldNotUse>,
}

#[derive(serde::Deserialize)]
struct GoldUse {
    file: String,
    start_byte: u32,
    end_byte: u32,
    ref_kind: String,
}

#[derive(serde::Deserialize)]
struct GoldNotUse {
    file: String,
    start_byte: u32,
    end_byte: u32,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_path(name: &str) -> PathBuf {
    repo_root().join("tests/fixtures/php/authored").join(name)
}

fn parse_php(source: &[u8]) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&grammar(LanguageId::Php))
        .expect("pinned PHP grammar must load");
    parser
        .parse(source, None)
        .expect("parser must return a tree")
}

fn extract_file(name: &str) -> ExtractedFile {
    let source = std::fs::read(fixture_path(name)).expect("read fixture");
    let tree = parse_php(&source);
    php::extract(&source, &tree)
}

fn load_gold() -> GoldFile {
    let gold_path = repo_root().join("tests/gold/php-authored.toml");
    let gold_text = std::fs::read_to_string(&gold_path).expect("read gold TOML");
    toml::from_str(&gold_text).expect("parse gold TOML")
}

fn find_use(extracted: &ExtractedFile, start: u32, end: u32) -> &ExtractedUse {
    extracted
        .uses
        .iter()
        .find(|use_| use_.span.start_byte() == start && use_.span.end_byte() == end)
        .unwrap_or_else(|| panic!("no extracted use at [{start}, {end})"))
}

fn matches_gold(use_: &ExtractedUse, gold: &GoldUse) -> bool {
    use_.span.start_byte() == gold.start_byte
        && use_.span.end_byte() == gold.end_byte
        && use_.ref_kind.as_str() == gold.ref_kind
}

/// Gold use spans have exactly one extracted match; no extracted use overlaps a
/// negative span; every extracted `Call` is listed in gold.
#[test]
fn php_authored_uses_match_gold() {
    let gold = load_gold();

    let mut files: BTreeSet<&str> = BTreeSet::new();
    for use_ in &gold.uses {
        files.insert(use_.file.as_str());
    }
    for not in &gold.not_a_use {
        files.insert(not.file.as_str());
    }

    let mut problems: Vec<String> = Vec::new();
    for file in files {
        let extracted = extract_file(file);
        if !extracted.diagnostics.is_empty() {
            problems.push(format!(
                "{file}: unexpected diagnostics {:?}",
                extracted.diagnostics
            ));
            continue;
        }

        let gold_for_file: Vec<&GoldUse> =
            gold.uses.iter().filter(|use_| use_.file == file).collect();

        for gold_use in &gold_for_file {
            let found = extracted
                .uses
                .iter()
                .filter(|use_| matches_gold(use_, gold_use))
                .count();
            if found != 1 {
                problems.push(format!(
                    "{file}: gold {} [{}..{}] matched {found} extracted uses",
                    gold_use.ref_kind, gold_use.start_byte, gold_use.end_byte
                ));
            }
        }

        for not in gold.not_a_use.iter().filter(|not| not.file == file) {
            for use_ in &extracted.uses {
                let overlaps =
                    use_.span.start_byte() < not.end_byte && not.start_byte < use_.span.end_byte();
                if overlaps {
                    problems.push(format!(
                        "{file}: extracted {} {:?} [{}..{}] overlaps not_a_use [{}..{}]",
                        use_.ref_kind,
                        use_.spelling,
                        use_.span.start_byte(),
                        use_.span.end_byte(),
                        not.start_byte,
                        not.end_byte
                    ));
                }
            }
        }

        for use_ in &extracted.uses {
            if use_.ref_kind == RefKind::Call
                && !gold_for_file.iter().any(|g| matches_gold(use_, g))
            {
                problems.push(format!(
                    "{file}: extracted call {:?} [{}..{}] is not in gold",
                    use_.spelling,
                    use_.span.start_byte(),
                    use_.span.end_byte()
                ));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "gold mismatches:\n{}",
        problems.join("\n")
    );
}

/// Receiver hints, containing symbols, and import bindings for the fixture.
#[test]
fn php_authored_receiver_hints_and_imports() {
    let boot = extract_file("boot.php");
    let survey = extract_file("SurveyService.php");
    let report = extract_file("ReportService.php");

    // Case c: top-level call with no containing symbol.
    let top_level = find_use(&boot, 179, 185);
    assert_eq!(top_level.ref_kind, RefKind::Call);
    assert_eq!(
        top_level.containing_symbol_index, None,
        "top-level launch() must not attach to a symbol"
    );

    // Case d: `$this->launch()` scoped through `$this`.
    let this_call = find_use(&survey, 640, 646);
    assert_eq!(this_call.hint, UseHint::This { is_static: None });
    assert_eq!(this_call.receiver.as_deref(), Some("$this"));

    // Case e: `$svc->launch()` after `$svc = new \App\...\SurveyService()`.
    let new_call = find_use(&boot, 267, 273);
    match &new_call.hint {
        UseHint::NewExpr {
            class_spelling,
            use_block,
            ..
        } => {
            assert_eq!(class_spelling, "\\App\\Services\\SurveyService");
            assert_eq!(*use_block, None, "case e is at file scope");
        }
        other => panic!("expected NewExpr hint, got {other:?}"),
    }

    // Case f: typed parameter receiver.
    let typed_call = find_use(&report, 736, 742);
    match &typed_call.hint {
        UseHint::Typed {
            type_spelling,
            origin,
            ..
        } => {
            assert_eq!(type_spelling, "SurveyService");
            assert_eq!(*origin, TypedOrigin::Parameter, "a parameter type (AF3)");
        }
        other => panic!("expected Typed hint, got {other:?}"),
    }

    // Case g: unknown receiver stays unresolved.
    let unknown_call = find_use(&report, 862, 868);
    assert_eq!(unknown_call.hint, UseHint::Unresolved);

    // Case h: the interpolated call is present as a use.
    let interpolated = find_use(&boot, 430, 436);
    assert_eq!(interpolated.ref_kind, RefKind::Call);

    // Case b: the aliased import is recorded with its local binding name.
    let alias_import = report
        .imports
        .iter()
        .find(|import| import.spelling_alias == "SurveySvc")
        .expect("alias import SurveySvc must be recorded");
    assert_eq!(
        alias_import.target_qualified,
        "App\\Services\\SurveyService"
    );
    assert_eq!(alias_import.kind, ImportKind::Class);

    // The unaliased import records the trailing name as its binding.
    let direct_import = report
        .imports
        .iter()
        .find(|import| import.spelling_alias == "SurveyService")
        .expect("direct import SurveyService must be recorded");
    assert_eq!(
        direct_import.target_qualified,
        "App\\Services\\SurveyService"
    );
}
