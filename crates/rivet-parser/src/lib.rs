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
//! TypeScript extraction is still a stub: its adapter returns an empty
//! `ExtractedFile` until T42. The driver parses with the right grammar either
//! way, so enabling the adapter later needs no CLI change. [`has_extractor`]
//! reports which languages have an adapter, so refresh can refuse to count a
//! file as indexed when nothing would extract it (AF5).

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
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&rivet_languages::grammar(language))
        .expect("pinned grammar must load");
    let tree = parser
        .parse(bytes, None)
        .expect("parser must return a tree");
    dispatch(language, bytes, &tree, limits)
}

/// Whether `language` has an extraction adapter in this build.
///
/// A file in a language without one yields no facts even when it parses, so
/// counting it as indexed would claim coverage rivet does not have (AF5,
/// audit finding 14). This must stay in step with [`dispatch`]: it is true
/// exactly for the languages `dispatch` hands to an adapter.
pub fn has_extractor(language: LanguageId) -> bool {
    #[cfg(feature = "lang-php")]
    if language == LanguageId::Php {
        return true;
    }
    let _ = language;
    false
}

/// Dispatches a parsed tree to the language adapter's `extract`.
///
/// Only PHP has an adapter in this milestone. Other enabled grammars parse but
/// extract nothing, which matches the previous CLI behavior; adding a
/// TypeScript adapter later changes only this function.
fn dispatch(
    language: LanguageId,
    bytes: &[u8],
    tree: &tree_sitter::Tree,
    limits: ResourceLimits,
) -> ExtractedFile {
    #[cfg(feature = "lang-php")]
    if language == LanguageId::Php {
        return rivet_languages::php::extract_with_limits(bytes, tree, limits);
    }
    let _ = (language, bytes, tree, limits);
    ExtractedFile::default()
}

#[cfg(all(test, feature = "lang-php"))]
mod tests {
    use super::{ResourceLimits, has_extractor, parse_file, parse_file_with_limits};
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

    /// PHP has an adapter; TypeScript does not until T42.
    #[test]
    fn only_php_has_an_extractor() {
        assert!(has_extractor(LanguageId::Php));
        #[cfg(feature = "lang-typescript")]
        {
            assert!(!has_extractor(LanguageId::Typescript));
            assert!(!has_extractor(LanguageId::Tsx));
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
