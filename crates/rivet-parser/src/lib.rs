//! Tree-sitter driver, grammar dispatch, source validation.
//!
//! T15 moves the parse-and-extract step out of `rivet-cli` and into this
//! crate (ARCHITECTURE "Workspace boundaries"). [`parse_file`] creates the
//! Tree-sitter parser for a language's pinned grammar, parses the bytes, and
//! dispatches to the language adapter's `extract`. The language adapters own
//! the parse/error policy and enforce the deterministic resource bounds of
//! [`ResourceLimits`] (spec §27: visited syntax nodes and extracted uses per
//! file), so a file that exceeds a bound or contains an error node yields
//! diagnostics and no facts.
//!
//! T41 dispatches `.ts` (including `.d.ts`) to the TypeScript grammar and
//! `.tsx` to the TSX grammar, and applies the v0.1 parse policy at this level
//! too: [`parse_tree`] parses with a language's grammar and
//! [`first_parse_error`] finds the first ERROR or MISSING node, which makes the
//! file a parse failure. T42 adds the TypeScript definition adapter
//! (`rivet_languages::typescript`), but TypeScript stays unindexed until T43
//! adds uses: [`LanguageId::has_extractor`] is the one switch and stays off,
//! and this crate does not dispatch to the adapter yet, so [`parse_file`]
//! yields no facts for TypeScript, only the `parse_error` diagnostic of a
//! failing tree. Refresh never calls it for such a language: it counts the
//! file as `unsupported` without reading it.

use rivet_core::ExtractedFile;
use rivet_languages::LanguageId;
pub use rivet_languages::ResourceLimits;

/// Parses `bytes` with the grammar for `language` and extracts owned facts
/// under the spec's default [`ResourceLimits`].
///
/// The parser is created per call; the language grammars are compile-time
/// constants from [`rivet_languages::grammar`]. `bytes` must be the exact
/// source read from disk (no CRLF normalization or BOM stripping).
pub fn parse_file(language: LanguageId, bytes: &[u8]) -> ExtractedFile {
    parse_file_with_limits(language, bytes, ResourceLimits::DEFAULT)
}

/// [`parse_file`] with explicit resource bounds, so tests can lower them.
pub fn parse_file_with_limits(
    language: LanguageId,
    bytes: &[u8],
    limits: ResourceLimits,
) -> ExtractedFile {
    let tree = parse_tree(language, bytes);
    dispatch(language, bytes, &tree, limits)
}

/// Parses `bytes` with the pinned grammar for `language`, without extracting.
///
/// `.ts` and `.d.ts` files use [`LanguageId::Typescript`] and `.tsx` files
/// [`LanguageId::Tsx`] ([`rivet_languages::language_for_path`]). The tree may
/// contain error nodes; [`first_parse_error`] applies the parse policy.
pub fn parse_tree(language: LanguageId, bytes: &[u8]) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&rivet_languages::grammar(language))
        .expect("pinned grammar must load");
    parser
        .parse(bytes, None)
        .expect("parser must return a tree")
}

/// The first node that makes a tree a parse failure under the v0.1 policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    /// The node's kind: `ERROR`, or the kind of a MISSING node (such as `)`).
    pub kind: String,
    /// The node's start byte.
    pub start_byte: u32,
}

/// Applies the v0.1 parse policy (ARCHITECTURE "Parse and coverage policy")
/// to `tree`: a tree containing any ERROR or MISSING node is a parse failure,
/// reported at the first such node in pre-order. `None` means the file parses.
///
/// This is the rule the PHP adapter applies before extracting; a language's
/// adapter also checks the node bound first, so a file that is both too large
/// and malformed is `resource_limit` rather than `parse_error`.
pub fn first_parse_error(tree: &tree_sitter::Tree) -> Option<ParseFailure> {
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            return Some(ParseFailure {
                kind: node.kind().to_string(),
                start_byte: node.start_byte() as u32,
            });
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                // `has_error` was set, so some node is ERROR or MISSING.
                return Some(ParseFailure {
                    kind: "ERROR".to_string(),
                    start_byte: root.start_byte() as u32,
                });
            }
        }
    }
}

/// Dispatches a parsed tree to the language adapter's `extract`.
///
/// A language whose [`LanguageId::has_extractor`] is false is not dispatched
/// to an adapter: its file yields no facts, and a tree that fails the parse
/// policy yields one `parse_error` diagnostic, as an adapter would report it.
/// TypeScript's definition adapter exists (T42), but T43 replaces that arm
/// with it and flips the switch in the same change, once uses exist; a test
/// below fails if the two disagree.
fn dispatch(
    language: LanguageId,
    bytes: &[u8],
    tree: &tree_sitter::Tree,
    limits: ResourceLimits,
) -> ExtractedFile {
    let _ = (bytes, tree, limits);
    match language {
        #[cfg(feature = "lang-php")]
        LanguageId::Php => rivet_languages::php::extract_with_limits(bytes, tree, limits),
        #[cfg(feature = "lang-typescript")]
        LanguageId::Typescript | LanguageId::Tsx => without_extractor(tree),
    }
}

/// The result for a language with no adapter: no facts, and the parse-policy
/// diagnostic when the tree fails it.
#[cfg(feature = "lang-typescript")]
fn without_extractor(tree: &tree_sitter::Tree) -> ExtractedFile {
    let diagnostics = first_parse_error(tree)
        .map(|failure| rivet_core::Diagnostic {
            code: "parse_error".to_string(),
            detail: format!("{} at byte {}", failure.kind, failure.start_byte),
            start_byte: Some(failure.start_byte),
        })
        .into_iter()
        .collect();
    ExtractedFile {
        diagnostics,
        ..ExtractedFile::default()
    }
}

#[cfg(all(test, feature = "lang-php"))]
mod tests {
    use super::{ResourceLimits, parse_file, parse_file_with_limits};
    use rivet_languages::LanguageId;

    /// Two calls, three `unknown`/`call` uses: `f`, `g`, and `h`.
    const SMALL: &[u8] = b"<?php\nf();\ng();\nh();\n";

    /// The number of nodes in `bytes`' tree, counted like the adapter does.
    fn node_count(bytes: &[u8]) -> u64 {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&rivet_languages::grammar(LanguageId::Php))
            .expect("grammar");
        let tree = parser.parse(bytes, None).expect("tree");
        let mut cursor = tree.walk();
        let mut count = 0_u64;
        loop {
            count += 1;
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return count;
                }
            }
        }
    }

    /// The spec §27 defaults are the documented constants.
    #[test]
    fn default_limits_are_the_spec_values() {
        assert_eq!(ResourceLimits::DEFAULT.max_visited_nodes, 1_000_000);
        assert_eq!(ResourceLimits::DEFAULT.max_extracted_uses, 500_000);
        assert_eq!(ResourceLimits::default(), ResourceLimits::DEFAULT);
    }

    /// The node bound bites only when exceeded: exactly the tree's node count
    /// is accepted, one fewer is `resource_limit` with no facts.
    #[test]
    fn node_limit_is_a_count_that_bites_only_when_exceeded() {
        let nodes = node_count(SMALL);
        let at = ResourceLimits {
            max_visited_nodes: nodes,
            ..ResourceLimits::DEFAULT
        };
        let ok = parse_file_with_limits(LanguageId::Php, SMALL, at);
        assert!(ok.diagnostics.is_empty(), "{ok:?}");
        assert_eq!(ok.uses.len(), 3, "{ok:?}");

        let below = ResourceLimits {
            max_visited_nodes: nodes - 1,
            ..ResourceLimits::DEFAULT
        };
        let limited = parse_file_with_limits(LanguageId::Php, SMALL, below);
        assert_eq!(limited.diagnostics.len(), 1);
        assert_eq!(limited.diagnostics[0].code, "resource_limit");
        assert_eq!(
            limited.diagnostics[0].detail,
            format!("visited more than {} Tree-sitter nodes", nodes - 1)
        );
        assert!(limited.symbols.is_empty() && limited.uses.is_empty());
        assert!(limited.imports.is_empty() && limited.scopes.is_empty());
        // Same bytes, same limits, same answer.
        assert_eq!(
            format!("{limited:?}"),
            format!(
                "{:?}",
                parse_file_with_limits(LanguageId::Php, SMALL, below)
            )
        );
    }

    /// The use bound bites only when exceeded: three uses fit a bound of
    /// three, not a bound of two.
    #[test]
    fn use_limit_is_a_count_that_bites_only_when_exceeded() {
        let at = ResourceLimits {
            max_extracted_uses: 3,
            ..ResourceLimits::DEFAULT
        };
        let ok = parse_file_with_limits(LanguageId::Php, SMALL, at);
        assert!(ok.diagnostics.is_empty(), "{ok:?}");
        assert_eq!(ok.uses.len(), 3);

        let below = ResourceLimits {
            max_extracted_uses: 2,
            ..ResourceLimits::DEFAULT
        };
        let limited = parse_file_with_limits(LanguageId::Php, SMALL, below);
        assert_eq!(limited.diagnostics.len(), 1);
        assert_eq!(limited.diagnostics[0].code, "resource_limit");
        assert_eq!(limited.diagnostics[0].detail, "extracted more than 2 uses");
        assert!(limited.symbols.is_empty() && limited.uses.is_empty());
        assert!(limited.imports.is_empty() && limited.scopes.is_empty());
    }

    /// A malformed file over the node bound is `resource_limit`: the node
    /// count is taken before the error policy.
    #[test]
    fn node_limit_precedes_the_parse_error_policy() {
        let broken = b"<?php\nclass Broken {\n  public function oops( {\n}";
        let limited = parse_file_with_limits(
            LanguageId::Php,
            broken,
            ResourceLimits {
                max_visited_nodes: 2,
                ..ResourceLimits::DEFAULT
            },
        );
        assert_eq!(limited.diagnostics[0].code, "resource_limit");
        // A malformed file has no uses, so the use bound cannot mask it.
        let errored = parse_file_with_limits(
            LanguageId::Php,
            broken,
            ResourceLimits {
                max_extracted_uses: 0,
                ..ResourceLimits::DEFAULT
            },
        );
        assert_eq!(errored.diagnostics[0].code, "parse_error");
    }

    /// The extractor switch and the dispatch agree: a language whose switch
    /// is on extracts a declared function, and one whose switch is off
    /// extracts nothing from a valid file. T43 must flip the TypeScript switch
    /// and add its dispatch arm together.
    #[test]
    fn extractor_switch_matches_dispatch() {
        #[cfg_attr(not(feature = "lang-typescript"), allow(unused_mut))]
        let mut samples: Vec<(LanguageId, &[u8])> =
            vec![(LanguageId::Php, b"<?php\nfunction f(): void {}\n")];
        #[cfg(feature = "lang-typescript")]
        {
            samples.push((LanguageId::Typescript, b"function f(): void {}\n"));
            samples.push((LanguageId::Tsx, b"function f() { return <div />; }\n"));
        }
        for (language, source) in samples {
            let extracted = parse_file(language, source);
            assert!(
                extracted.diagnostics.is_empty(),
                "{language:?}: {extracted:?}"
            );
            assert_eq!(
                !extracted.symbols.is_empty(),
                language.has_extractor(),
                "{language:?}: the has_extractor switch disagrees with dispatch"
            );
        }
    }

    /// The driver extracts a named definition through the PHP adapter.
    #[test]
    fn php_parse_file_extracts_a_symbol() {
        let extracted = parse_file(
            LanguageId::Php,
            b"<?php\nnamespace App;\nfunction f(): void {}\n",
        );
        assert!(extracted.diagnostics.is_empty(), "{extracted:?}");
        assert!(
            extracted
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == "App\\f"),
            "{extracted:?}"
        );
    }

    /// A parse error yields one diagnostic and no facts.
    #[test]
    fn php_parse_file_reports_errors_without_facts() {
        let extracted = parse_file(
            LanguageId::Php,
            b"<?php\nclass Broken {\n  public function oops( {\n}",
        );
        assert_eq!(extracted.diagnostics.len(), 1);
        assert_eq!(extracted.diagnostics[0].code, "parse_error");
        assert!(extracted.symbols.is_empty());
    }
}
