//! TypeScript and TSX named-definition extraction (T42).
//!
//! One adapter serves both grammar variants: `.ts` and `.d.ts` files parse
//! with the TypeScript grammar and `.tsx` files with the TSX grammar
//! ([`crate::language_for_path`]), and every node kind this adapter reads has
//! the same name and fields in both. [`extract`] takes the tree either grammar
//! produced.
//!
//! T42 extracts named definitions only: owned [`ExtractedSymbol`] records with
//! kind, qualified name (lexical nesting joined with `.`), declaration span,
//! name span, parent, collapsed signature, and attached JSDoc comment. Which
//! nodes are definitions depends on where they sit (never in a function body,
//! an object literal, an anonymous class, or a string-named ambient module)
//! and on their siblings (overload signatures fold into one symbol), so the
//! definitions come from a documented scope walk ([`definitions`]) rather
//! than a `.scm` query; see the adapter README for the full rule set. Uses,
//! imports, and scopes are T43.
//!
//! The adapter is not yet reachable from indexing:
//! [`crate::LanguageId::has_extractor`] stays `false` for TypeScript, and
//! `rivet_parser` keeps its no-extractor arm, until T43 adds uses and flips
//! both together. Definitions without uses would make `refs` report
//! misleadingly empty results.
//!
//! Parse policy (docs/ARCHITECTURE.md "Parse and coverage policy"), exactly as
//! the PHP adapter applies it: a tree with any ERROR or MISSING node publishes
//! no facts and yields one `parse_error` diagnostic; a tree with more nodes
//! than [`ResourceLimits::max_visited_nodes`] publishes no facts and yields one
//! `resource_limit` diagnostic, checked first. The use bound
//! ([`ResourceLimits::max_extracted_uses`]) cannot bite yet: T42 extracts no
//! uses.

use rivet_core::{Diagnostic, ExtractedFile, SymbolKind};
use tree_sitter::{Node, Tree};

use crate::ResourceLimits;

mod definitions;
mod signature;

/// Extracts the named definitions of one parsed TypeScript or TSX file.
///
/// `source` must be exactly the bytes that produced `tree`, which either
/// TypeScript grammar variant may have produced. Symbols are in declaration
/// start order, so output is deterministic. On a parse or resource failure the
/// symbol list is empty and one diagnostic explains the skip.
///
/// Uses the spec's default [`ResourceLimits`]; see [`extract_with_limits`].
pub fn extract(source: &[u8], tree: &Tree) -> ExtractedFile {
    extract_with_limits(source, tree, ResourceLimits::DEFAULT)
}

/// [`extract`] with explicit resource bounds.
///
/// The node count is taken by one pre-order visit of the whole tree before any
/// extraction, so a file over the bound does no extraction work at all.
pub fn extract_with_limits(source: &[u8], tree: &Tree, limits: ResourceLimits) -> ExtractedFile {
    let root = tree.root_node();
    let first_error = match scan_tree(root, limits.max_visited_nodes) {
        Scan::ResourceLimit => {
            return no_facts(Diagnostic {
                code: "resource_limit".to_string(),
                detail: format!(
                    "visited more than {} Tree-sitter nodes",
                    limits.max_visited_nodes
                ),
                start_byte: None,
            });
        }
        Scan::Done(first_error) => first_error,
    };
    if root.has_error() {
        let (kind, byte) =
            first_error.unwrap_or_else(|| ("ERROR".to_string(), root.start_byte() as u32));
        return no_facts(Diagnostic {
            code: "parse_error".to_string(),
            detail: format!("{kind} at byte {byte}"),
            start_byte: Some(byte),
        });
    }
    ExtractedFile {
        symbols: definitions::extract_symbols(source, root),
        ..ExtractedFile::default()
    }
}

/// The result of a skipped file: no facts and one diagnostic.
fn no_facts(diagnostic: Diagnostic) -> ExtractedFile {
    ExtractedFile {
        diagnostics: vec![diagnostic],
        ..ExtractedFile::default()
    }
}

/// The outcome of the pre-extraction visit.
enum Scan {
    /// More nodes than the bound.
    ResourceLimit,
    /// Within the bound, with the first ERROR or MISSING node in pre-order.
    Done(Option<(String, u32)>),
}

/// Walks the tree once, counting every node (named or anonymous) and locating
/// the first ERROR or MISSING node in pre-order. More than `max_nodes` stops
/// the walk immediately. This mirrors the PHP adapter's visit so both
/// languages count the same way.
fn scan_tree(root: Node<'_>, max_nodes: u64) -> Scan {
    let has_error = root.has_error();
    let mut first_error: Option<(String, u32)> = None;
    let mut visited: u64 = 0;
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        visited += 1;
        if visited > max_nodes {
            return Scan::ResourceLimit;
        }
        if has_error && first_error.is_none() && (node.is_error() || node.is_missing()) {
            first_error = Some((node.kind().to_string(), node.start_byte() as u32));
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Scan::Done(first_error);
            }
        }
    }
}

/// The form used for short-name (`lookup_name`) matching.
///
/// TypeScript identifiers are case-sensitive for every declaration kind, so
/// the lookup name is the declared name exactly as written, including an ES
/// private name's leading `#`. Nothing is folded, not even ASCII case.
pub fn lookup_name(name: &str, _kind: SymbolKind) -> String {
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::lookup_name;
    use rivet_core::SymbolKind;

    #[test]
    fn lookup_name_is_the_name_as_written() {
        assert_eq!(
            lookup_name("SurveyService", SymbolKind::Class),
            "SurveyService"
        );
        assert_eq!(lookup_name("Launch", SymbolKind::Method), "Launch");
        assert_eq!(lookup_name("SurveyId", SymbolKind::TypeAlias), "SurveyId");
        assert_eq!(lookup_name("#attempts", SymbolKind::Property), "#attempts");
        assert_eq!(lookup_name("Ä", SymbolKind::Function), "Ä");
    }
}
