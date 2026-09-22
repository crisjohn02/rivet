//! Owned, database-free extraction records.
//!
//! A language adapter returns an [`ExtractedFile`] with owned symbols and
//! diagnostics: byte [`Span`]s and plain strings only, never borrowed
//! Tree-sitter `Node` handles or database identifiers. Later tasks add uses,
//! imports, and scopes; T11 needs only named definitions and parse
//! diagnostics.

use crate::kinds::SymbolKind;
use crate::span::Span;

/// One extracted named definition.
///
/// `parent_index` is an index into the same `Vec<ExtractedSymbol>` as the
/// symbol, or `None` for a top-level definition. For PHP, members point at
/// their enclosing class/interface/trait/enum; namespaces are recorded as
/// [`SymbolKind::Module`] but are not parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSymbol {
    /// The fully qualified name in the language's native form.
    pub qualified_name: String,
    /// The short declared name, with language-specific spelling (PHP
    /// properties keep their leading `$`).
    pub name: String,
    /// The declaration kind.
    pub kind: SymbolKind,
    /// The whole declaration node range, including modifiers.
    pub span: Span,
    /// Index of the enclosing container symbol in the owning
    /// `Vec<ExtractedSymbol>`, if any.
    pub parent_index: Option<usize>,
    /// A collapsed signature, filled by a later task (T14). Always `None` in
    /// T11.
    pub signature: Option<String>,
    /// An attached doc comment, filled by a later task (T14). Always `None`
    /// in T11.
    pub doc_comment: Option<String>,
}

/// The owned result of extracting one file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractedFile {
    /// Extracted named definitions, sorted deterministically.
    pub symbols: Vec<ExtractedSymbol>,
    /// File-level diagnostics. A parse or resource failure yields no symbols
    /// and one diagnostic.
    pub diagnostics: Vec<Diagnostic>,
}

/// A bounded file-level extraction diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// A stable snake_case code, such as `parse_error` or `resource_limit`.
    pub code: String,
    /// A short human-readable detail.
    pub detail: String,
    /// The offending byte offset, when the diagnostic has one.
    pub start_byte: Option<u32>,
}
