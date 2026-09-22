//! T11 PHP named-definition extraction tests.
//!
//! The three authored fixture files are extracted and compared against
//! `tests/gold/php-authored.toml`. Gold records exact declaration spans; the
//! test requires exactly one extracted symbol per gold declaration (same
//! qualified name, kind, and byte span) and rejects unexplained extras except
//! namespace `Module` records, which gold deliberately omits.

#![cfg(feature = "lang-php")]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rivet_core::{ExtractedFile, ExtractedSymbol, SymbolKind};
use rivet_languages::{LanguageId, grammar, php};
use tree_sitter::{Parser, Tree};

#[derive(serde::Deserialize)]
struct GoldFile {
    #[serde(default)]
    declaration: Vec<GoldDeclaration>,
}

#[derive(serde::Deserialize)]
struct GoldDeclaration {
    file: String,
    qualified_name: String,
    kind: String,
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

fn extract_file(name: &str) -> (Vec<u8>, ExtractedFile) {
    let source = std::fs::read(fixture_path(name)).expect("read fixture");
    let tree = parse_php(&source);
    let extracted = php::extract(&source, &tree);
    (source, extracted)
}

fn matches_gold(symbol: &ExtractedSymbol, gold: &GoldDeclaration) -> bool {
    symbol.qualified_name == gold.qualified_name
        && symbol.kind.as_str() == gold.kind
        && symbol.span.start_byte() == gold.start_byte
        && symbol.span.end_byte() == gold.end_byte
}

/// Every gold declaration has exactly one extracted match; every non-module
/// extracted symbol is claimed by gold.
#[test]
fn php_authored_declarations_match_gold() {
    let gold_path = repo_root().join("tests/gold/php-authored.toml");
    let gold_text = std::fs::read_to_string(&gold_path).expect("read gold TOML");
    let gold: GoldFile = toml::from_str(&gold_text).expect("parse gold TOML");

    let files: BTreeSet<String> = gold.declaration.iter().map(|d| d.file.clone()).collect();
    let mut problems: Vec<String> = Vec::new();

    for file in &files {
        let (_source, extracted) = extract_file(file);
        if !extracted.diagnostics.is_empty() {
            problems.push(format!(
                "{file}: unexpected diagnostics {:?}",
                extracted.diagnostics
            ));
        }

        let gold_for_file: Vec<&GoldDeclaration> = gold
            .declaration
            .iter()
            .filter(|d| &d.file == file)
            .collect();

        for declaration in &gold_for_file {
            let found = extracted
                .symbols
                .iter()
                .filter(|symbol| matches_gold(symbol, declaration))
                .count();
            if found != 1 {
                problems.push(format!(
                    "{file}: gold {} ({}) [{}, {}) matched {found} extracted symbols",
                    declaration.qualified_name,
                    declaration.kind,
                    declaration.start_byte,
                    declaration.end_byte
                ));
            }
        }

        for symbol in &extracted.symbols {
            // Namespaces are recorded but intentionally absent from gold.
            if symbol.kind == SymbolKind::Module {
                continue;
            }
            let claimed = gold_for_file.iter().any(|d| matches_gold(symbol, d));
            if !claimed {
                problems.push(format!(
                    "{file}: extra extracted symbol {} ({}) [{}, {})",
                    symbol.qualified_name,
                    symbol.kind,
                    symbol.span.start_byte(),
                    symbol.span.end_byte()
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

/// A malformed file publishes no facts and reports the first missing node.
#[test]
fn malformed_php_yields_one_parse_error() {
    let source = b"<?php\nclass Broken {\n  public function oops( {\n}";
    let tree = parse_php(source);
    let extracted = php::extract(source, &tree);

    assert!(
        extracted.symbols.is_empty(),
        "malformed input must not produce symbols: {:?}",
        extracted.symbols
    );
    assert_eq!(extracted.diagnostics.len(), 1, "exactly one diagnostic");
    assert_eq!(extracted.diagnostics[0].code, "parse_error");
    // The first missing node in the grammar smoke snippet is the `)` after
    // `oops(`, at byte 44.
    assert_eq!(extracted.diagnostics[0].start_byte, Some(44));
    assert!(
        extracted.diagnostics[0].detail.contains(" at byte 44"),
        "detail should name the byte: {:?}",
        extracted.diagnostics[0].detail
    );
}

/// A file with no namespace keeps bare qualified names.
#[test]
fn file_without_namespace_uses_bare_names() {
    let source = b"<?php\nfunction solo(): void {}\n";
    let tree = parse_php(source);
    let extracted = php::extract(source, &tree);

    assert!(extracted.diagnostics.is_empty());
    let solo = extracted
        .symbols
        .iter()
        .find(|symbol| symbol.name == "solo")
        .expect("function solo must be extracted");
    assert_eq!(solo.qualified_name, "solo");
    assert_eq!(solo.kind, SymbolKind::Function);
    assert_eq!(solo.parent_index, None);
}

/// The qualified names extracted from `source`, in symbol order.
fn qualified_names(source: &[u8]) -> Vec<String> {
    let tree = parse_php(source);
    let extracted = php::extract(source, &tree);
    assert!(extracted.diagnostics.is_empty(), "snippet must parse");
    extracted
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.clone())
        .collect()
}

/// AF1: a global `namespace { }` block declares bare global names rather than
/// inheriting the previous block's namespace.
#[test]
fn global_namespace_block_declares_bare_names() {
    let source =
        b"<?php\nnamespace A { class X {} }\nnamespace { class Y {} function helper(){} }\n";
    assert_eq!(qualified_names(source), ["A", "A\\X", "Y", "helper"]);

    // A named block after a global block takes its own name again.
    let source =
        b"<?php\nnamespace { class Y {} }\nnamespace B { class Z { public function m() {} } }\n";
    assert_eq!(qualified_names(source), ["Y", "B", "B\\Z", "B\\Z::m"]);
}

/// AF1: each unbraced block qualifies its own declarations.
#[test]
fn unbraced_namespace_blocks_qualify_their_own_declarations() {
    let source =
        b"<?php\nnamespace First;\nclass Widget {}\nnamespace Second;\nfunction caller() {}\n";
    assert_eq!(
        qualified_names(source),
        ["First", "First\\Widget", "Second", "Second\\caller"]
    );
}

/// A method's `parent_index` resolves to its owning class record.
#[test]
fn method_parent_index_points_at_class() {
    let (_source, extracted) = extract_file("SurveyService.php");
    let launch = extracted
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "App\\Services\\SurveyService::launch")
        .expect("launch method must be extracted");
    let parent = launch.parent_index.expect("launch has a parent");
    assert_eq!(
        extracted.symbols[parent].qualified_name,
        "App\\Services\\SurveyService"
    );
    assert_eq!(extracted.symbols[parent].kind, SymbolKind::Class);
}

/// Anonymous class members and closures are not addressable symbols in v0.1
/// (spec §10.1), so they must not be emitted at all.
#[test]
fn anonymous_class_members_and_closures_are_not_symbols() {
    let source = b"<?php\n$o = new class {\n    public function z() {}\n    private $p = 1;\n    public const C = 1;\n};\n$f = function () { return 1; };\n";
    let tree = parse_php(source);
    let extracted = php::extract(source, &tree);

    assert!(
        extracted.diagnostics.is_empty(),
        "well-formed input has no diagnostics: {:?}",
        extracted.diagnostics
    );
    assert!(
        extracted.symbols.is_empty(),
        "anonymous definitions must not be extracted: {:?}",
        extracted.symbols
    );
}

/// A named class after an anonymous class in the same file is still extracted
/// with its methods; only the anonymous members are skipped.
#[test]
fn named_class_after_anonymous_class_is_extracted() {
    let source = b"<?php\n$o = new class { public function z() {} };\nclass Named {\n    public function keep() {}\n}\n";
    let tree = parse_php(source);
    let extracted = php::extract(source, &tree);

    assert!(extracted.diagnostics.is_empty());
    let qualified: Vec<&str> = extracted
        .symbols
        .iter()
        .map(|symbol| symbol.qualified_name.as_str())
        .collect();
    assert!(qualified.contains(&"Named"), "{qualified:?}");
    assert!(qualified.contains(&"Named::keep"), "{qualified:?}");
    assert!(
        !qualified.iter().any(|name| name.ends_with("z")),
        "anonymous member must be absent: {qualified:?}"
    );
}

/// Enum cases (pure and backed) are recorded as `const` symbols owned by their
/// enum, with case-sensitive lookup names (T25).
#[test]
fn enum_cases_are_const_symbols_with_enum_parent() {
    let (_source, extracted) = extract_file("Enums.php");

    let hearts = extracted
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "App\\Enums\\Suit::Hearts")
        .expect("pure enum case Hearts");
    assert_eq!(hearts.kind, SymbolKind::Const);
    assert_eq!(hearts.name, "Hearts");
    let parent = hearts.parent_index.expect("enum case has an enum parent");
    assert_eq!(extracted.symbols[parent].qualified_name, "App\\Enums\\Suit");
    assert_eq!(extracted.symbols[parent].kind, SymbolKind::Enum);

    let active = extracted
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "App\\Enums\\Status::Active")
        .expect("backed enum case Active");
    assert_eq!(active.kind, SymbolKind::Const);

    // Enum cases are case-sensitive in PHP; `Const` lookup keeps exact spelling.
    assert_eq!(php::lookup_name("Hearts", SymbolKind::Const), "Hearts");
    assert_eq!(php::lookup_name("Hearts", SymbolKind::Method), "hearts");
}

/// Promoted constructor properties, static/readonly members, and multi-name
/// property/constant declarations are each extracted once per declared name.
#[test]
fn promoted_and_multi_name_members_are_extracted() {
    let (_source, extracted) = extract_file("Members.php");

    let find = |name: &str| {
        extracted
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };

    // Promoted constructor properties are `property` symbols owned by the class
    // and keep their `$`.
    for name in [
        "App\\Members\\AbstractThing::$seed",
        "App\\Members\\AbstractThing::$tag",
    ] {
        let property = find(name);
        assert_eq!(property.kind, SymbolKind::Property, "{name}");
        assert!(property.name.starts_with('$'), "{name} keeps $");
        let parent = property.parent_index.expect("promoted property parent");
        assert_eq!(extracted.symbols[parent].name, "AbstractThing");
    }

    // Multi-name property and constant declarations produce one symbol per
    // name, all sharing the whole declaration span.
    for (first, second) in [
        (
            "App\\Members\\AbstractThing::FIRST",
            "App\\Members\\AbstractThing::SECOND",
        ),
        (
            "App\\Members\\AbstractThing::$left",
            "App\\Members\\AbstractThing::$right",
        ),
    ] {
        let a = find(first);
        let b = find(second);
        assert_eq!(
            a.span, b.span,
            "multi-name members share the declaration span"
        );
    }

    // Each member's stored signature names only itself, not the shared header.
    assert_eq!(
        find("App\\Members\\AbstractThing::FIRST")
            .signature
            .as_deref(),
        Some("public const FIRST = 1")
    );
    assert_eq!(
        find("App\\Members\\AbstractThing::SECOND")
            .signature
            .as_deref(),
        Some("public const SECOND = 2")
    );
    assert_eq!(
        find("App\\Members\\AbstractThing::$left")
            .signature
            .as_deref(),
        Some("public int $left")
    );
    assert_eq!(
        find("App\\Members\\AbstractThing::$right")
            .signature
            .as_deref(),
        Some("public int $right = 2")
    );
}

/// An interface's bodyless method keeps a sensible signature: `header()` drops
/// the trailing `;` when there is no body.
#[test]
fn interface_bodyless_method_signature_drops_the_semicolon() {
    let (_source, extracted) = extract_file("Interface.php");

    let name = extracted
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "App\\Contracts\\Named::name")
        .expect("interface method name");
    assert_eq!(name.kind, SymbolKind::Method);
    assert_eq!(
        name.signature.as_deref(),
        Some("public function name(): string")
    );
}

/// Members of the anonymous class inside a new fixture are not extracted as
/// addressable symbols (spec §10.1).
#[test]
fn anonymous_class_members_in_authored_fixture_are_not_extracted() {
    let (_source, extracted) = extract_file("Members.php");

    assert!(
        !extracted
            .symbols
            .iter()
            .any(|symbol| symbol.name == "hidden"),
        "the anonymous class method must not leak: {:?}",
        extracted
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect::<Vec<_>>()
    );
}
