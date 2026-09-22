//! Owned, database-free extraction records.
//!
//! A language adapter returns an [`ExtractedFile`] with owned symbols,
//! identifier uses, import bindings, and diagnostics: byte [`Span`]s and plain
//! strings only, never borrowed Tree-sitter `Node` handles or database
//! identifiers. T11 added named definitions and parse diagnostics; T17 adds
//! lexical uses, imports, and receiver hints. Resolution and persistence are
//! later tasks.

use crate::kinds::{RefKind, SymbolKind};
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

/// The kind of a lexical import binding created by a `use` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportKind {
    /// A class, interface, trait, or enum binding.
    Class,
    /// A function binding.
    Function,
    /// A constant binding.
    Const,
}

impl ImportKind {
    /// The snake_case form used by the output contract.
    pub fn as_str(&self) -> &'static str {
        match self {
            ImportKind::Class => "class",
            ImportKind::Function => "function",
            ImportKind::Const => "const",
        }
    }
}

/// Lexical evidence recorded for one use by the language adapter.
///
/// These hints are not final cross-file resolution tiers (see
/// docs/ADDING-A-LANGUAGE.md "Adapter contract"); the generic resolver turns
/// them into at most one `exact`/`scoped` binding or leaves the use unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseHint {
    /// The receiver is `$this` inside a class body.
    This,
    /// The receiver is `self`, `static`, or `parent`.
    SelfOrStatic,
    /// The receiver variable's most recent assignment in the same function
    /// body was `new <class_spelling>(...)`.
    NewExpr {
        /// The class name exactly as written at the `new` site.
        class_spelling: String,
    },
    /// The receiver is a parameter, promoted parameter, or typed property with
    /// the recorded explicit type.
    Typed {
        /// The type name exactly as written at the declaration.
        type_spelling: String,
    },
    /// The use names an imported binding.
    Imported {
        /// The local binding name introduced by the import.
        binding: String,
    },
    /// No supported lexical evidence was found.
    Unresolved,
}

/// One extracted identifier use.
///
/// A use records where the identifier was written ([`span`](Self::span)), its
/// written spelling, its syntactic kind, the nearest named container, the
/// receiver expression text when one exists, a deterministic lexical scope key,
/// and the adapter's receiver hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedUse {
    /// The identifier exactly as written in source.
    pub spelling: String,
    /// The syntactic form of the use.
    pub ref_kind: RefKind,
    /// The identifier's byte range, excluding any receiver.
    pub span: Span,
    /// Index into the owning `ExtractedFile::symbols` of the innermost named
    /// container (function/method/class/interface/enum) whose span contains
    /// this use, or `None` at file scope.
    pub containing_symbol_index: Option<usize>,
    /// Source text of the receiver expression (`$x`, `Foo`, `$this->svc`), or
    /// `None` for uses without a receiver.
    pub receiver: Option<String>,
    /// A deterministic key identifying the lexical scope within the file.
    pub scope_key: String,
    /// The adapter's lexical receiver hint.
    pub hint: UseHint,
}

/// One lexical import binding created by a `use` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedImport {
    /// The local binding name: the explicit alias, or the last segment of the
    /// imported qualified name.
    pub spelling_alias: String,
    /// The imported qualified name exactly as written (group prefix applied).
    pub target_qualified: String,
    /// Whether the binding names a class, function, or constant.
    pub kind: ImportKind,
    /// The binding identifier's byte range.
    pub span: Span,
}

/// The owned result of extracting one file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtractedFile {
    /// Extracted named definitions, sorted deterministically.
    pub symbols: Vec<ExtractedSymbol>,
    /// Extracted identifier uses, sorted deterministically.
    pub uses: Vec<ExtractedUse>,
    /// Import bindings created by `use` declarations, in source order.
    pub imports: Vec<ExtractedImport>,
    /// File-level diagnostics. A parse or resource failure yields no facts and
    /// one diagnostic.
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
