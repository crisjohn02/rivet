//! Tree-sitter driver, grammar dispatch, source validation.
//!
//! T15 moves the parse-and-extract step out of `rivet-cli` and into this
//! crate (ARCHITECTURE "Workspace boundaries"). [`parse_file`] creates the
//! Tree-sitter parser for a language's pinned grammar, parses the bytes, and
//! dispatches to the language adapter's `extract`. The language adapters own
//! the deterministic node cap and parse/error policy, so a file that exceeds
//! the cap or contains an error node yields diagnostics and no facts.
//!
//! TypeScript extraction is still a stub: its adapter returns an empty
//! `ExtractedFile` until T42. The driver parses with the right grammar either
//! way, so enabling the adapter later needs no CLI change.

use rivet_core::ExtractedFile;
use rivet_languages::LanguageId;

/// Parses `bytes` with the grammar for `language` and extracts owned facts.
///
/// The parser is created per call; the language grammars are compile-time
/// constants from [`rivet_languages::grammar`]. `bytes` must be the exact
/// source read from disk (no CRLF normalization or BOM stripping).
pub fn parse_file(language: LanguageId, bytes: &[u8]) -> ExtractedFile {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&rivet_languages::grammar(language))
        .expect("pinned grammar must load");
    let tree = parser
        .parse(bytes, None)
        .expect("parser must return a tree");
    dispatch(language, bytes, &tree)
}

/// Dispatches a parsed tree to the language adapter's `extract`.
///
/// Only PHP has an adapter in this milestone. Other enabled grammars parse but
/// extract nothing, which matches the previous CLI behavior; adding a
/// TypeScript adapter later changes only this function.
fn dispatch(language: LanguageId, bytes: &[u8], tree: &tree_sitter::Tree) -> ExtractedFile {
    #[cfg(feature = "lang-php")]
    if language == LanguageId::Php {
        return rivet_languages::php::extract(bytes, tree);
    }
    let _ = (language, bytes, tree);
    ExtractedFile::default()
}

#[cfg(all(test, feature = "lang-php"))]
mod tests {
    use super::parse_file;
    use rivet_languages::LanguageId;

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
