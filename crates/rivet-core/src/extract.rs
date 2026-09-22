//! Owned, database-free extraction records.
//!
//! A language adapter returns an [`ExtractedFile`] with owned symbols,
//! identifier uses, import bindings, lexical scopes, and diagnostics: byte
//! [`Span`]s and plain strings only, never borrowed Tree-sitter `Node` handles
//! or database identifiers. T11 added named definitions and parse diagnostics;
//! T17 adds lexical uses, imports, and receiver hints; T18 adds owned lexical
//! scope facts. Resolution and persistence are later tasks.
//!
//! The T17 records derive `serde` so the store can persist a use's
//! [`UseHint`] as JSON and a scope's [`ScopeFacts`] as JSON without the
//! language adapter knowing about SQLite or a file path.

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
        /// The start byte of the nearest enclosing control-flow block at the
        /// use site, or `None` at the function body's top level. The resolver
        /// requires this to match the assignment's block.
        #[serde(default)]
        use_block: Option<u32>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// One import binding visible in a lexical scope (T18).
///
/// This is the persisted shape of [`ExtractedImport`]'s data: the store's
/// `scopes.facts_json` records `imports: [{alias, target_qualified, kind,
/// span}]` so a later resolver can re-resolve a use without reparsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeImport {
    /// The local binding name: the explicit alias, or the last segment of the
    /// imported qualified name.
    pub alias: String,
    /// The imported qualified name exactly as written (group prefix applied).
    pub target_qualified: String,
    /// Whether the binding names a class, function, or constant.
    pub kind: ImportKind,
    /// The binding identifier's byte range.
    pub span: Span,
}

/// One typed variable binding introduced in a lexical scope (T18).
///
/// Parameters, promoted properties, and any other typed local the adapter
/// recognizes. The span is the variable name's byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedBinding {
    /// The variable exactly as written, including a leading `$` for PHP.
    pub variable: String,
    /// The explicit type name exactly as written at the declaration.
    pub type_spelling: String,
    /// The variable name's byte range.
    pub span: Span,
}

/// One variable binding or rebinding introduced in a lexical scope
/// (T18/T21/T21a).
///
/// T18 recorded only direct `new` assignments. T21 recorded every simple
/// `$x = ...` assignment so the `new`-receiver rule can reject a variable that
/// was reassigned or whose assignment is not a direct `new`. T21a extends the
/// record to every recognized rebinding form (`foreach`, destructuring,
/// by-reference assignment, `catch`, by-reference closure capture, compound
/// assignment, `unset`, `global`, `static`, and increment/decrement), so a
/// variable the extractor cannot account for never looks singly assigned.
///
/// `direct_new` is true only when the right-hand side is a direct
/// `new <class>(...)`; then `class_spelling` holds the class name as written.
/// For any other binding or rebinding `class_spelling` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewBinding {
    /// The assigned variable exactly as written, including a leading `$`.
    pub variable: String,
    /// The class name exactly as written at the `new` site, or empty when the
    /// assignment is not a direct `new`.
    pub class_spelling: String,
    /// The assigned variable name's byte range.
    pub span: Span,
    /// Whether the right-hand side is a direct `new <class>(...)`.
    ///
    /// There is deliberately no `true` default. A refresh reparses stale facts
    /// because the extractor fingerprint now covers the fact schema (T22), but
    /// explicit `--no-refresh` can still read a snapshot committed by an older
    /// binary, so an absent `direct_new` must mean "not known to be a direct
    /// `new`" (unresolved), never the unsafe assumption that every old fact was
    /// an unconditional `new`.
    #[serde(default)]
    pub direct_new: bool,
    /// The start byte of the nearest enclosing control-flow block within the
    /// function body, or `None` at the body's top level. Two assignments share
    /// a block only when this value is equal, so a conditional assignment is
    /// not confused with an unconditional one.
    #[serde(default)]
    pub block: Option<u32>,
}

/// How a call argument's callee must be looked up (T21b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallArgKind {
    /// A plain function call `f(...)`.
    Function,
    /// A member call `$receiver->f(...)`.
    Method,
    /// A static call `Class::f(...)`.
    StaticMethod,
}

/// The receiver information a resolver needs to find a method declaration for
/// one call argument (T21b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CallReceiver {
    /// A class spelling to resolve through the alias/namespace scope chain.
    Class {
        /// The class name exactly as written (`SurveySvc`, `\App\Svc`).
        spelling: String,
    },
    /// `$this`, `self`, or `static`: the method is declared on the class that
    /// encloses the call.
    SelfClass,
    /// No usable receiver (an unknown variable, `parent`, or a dynamic name).
    Unknown,
}

/// One positional call argument that passes a bare variable and can therefore
/// rebind it when the callee declares that parameter by reference (T21b).
///
/// A by-reference argument written with an explicit `&` at the call site is
/// already recorded as an ordinary rebinding (T21a) and is not duplicated here.
/// This fact exists because the call site alone does not say whether the
/// callee's parameter is by-reference; only the callee's indexed declaration or
/// a known builtin table can answer that, and that lookup needs the index. The
/// resolver consults these facts and either proves the parameter is by-value or
/// suppresses the variable's `new`-receiver binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallArg {
    /// The passed variable exactly as written, including a leading `$`.
    pub variable: String,
    /// The callee name exactly as written (`takesRef`, `preg_match`, `go`).
    pub callee: String,
    /// How to resolve `callee`.
    pub kind: CallArgKind,
    /// The positional argument index, 0-based. `None` for a named argument,
    /// whose parameter position cannot be mapped without the callee's
    /// declaration order; the resolver treats it as unknown and suppresses.
    #[serde(default)]
    pub position: Option<u32>,
    /// For a member or static call, how to find the method's class. `None` for
    /// a function call.
    #[serde(default)]
    pub receiver: Option<CallReceiver>,
    /// The passed variable's byte range.
    pub span: Span,
}

/// Owned lexical facts recorded for one scope (T18).
///
/// The persisted `scopes.facts_json` holds `imports`, `typed_bindings`,
/// `new_bindings`, `call_args`, `unanalysable`, `namespace_unattributed`,
/// `class_constant_accesses`, and `declares`.
/// [`declares`](Self::declares) holds indices into the owning
/// [`ExtractedFile::symbols`] because a language adapter has no file path; the
/// persistence layer rewrites each index to its canonical symbol ID.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScopeFacts {
    /// Imports visible in this scope, in source order.
    pub imports: Vec<ScopeImport>,
    /// Typed variable bindings introduced directly in this scope.
    pub typed_bindings: Vec<TypedBinding>,
    /// Variable bindings introduced directly in this scope, in source order.
    /// Each records whether it is a direct `new` assignment. A recognized
    /// rebinding (T21a) is recorded as a non-direct entry, so a variable with
    /// more than one entry is rejected as a receiver.
    pub new_bindings: Vec<NewBinding>,
    /// Call arguments that pass a bare variable and may rebind it (T21b).
    #[serde(default)]
    pub call_args: Vec<CallArg>,
    /// True when this scope contains a construct whose effect on local
    /// variables the adapter cannot bound: a dynamic variable write, a
    /// `$GLOBALS` write, `extract`, `eval`, or a call whose callee is dynamic.
    /// The `new`-receiver rule records no binding anywhere in such a scope.
    #[serde(default)]
    pub unanalysable: bool,
    /// True when the adapter cannot attribute this scope to exactly one
    /// namespace block (AF1): for PHP, code outside every namespace block of a
    /// namespaced file, or any code in a file that mixes namespace forms. The
    /// resolver records no binding for a use whose scope chain reaches such a
    /// scope, because every lexical lookup depends on the namespace.
    #[serde(default)]
    pub namespace_unattributed: bool,
    /// The name span of every class-constant access (`Foo::NAME`,
    /// `self::NAME`, `$x::NAME`) whose use is recorded in this scope, in
    /// source order (AF2).
    ///
    /// A class-constant read and an instance property read (`$x->name`) are
    /// both a `read` use with the same receiver text and hint, so the resolver
    /// needs this fact to match a member of the right kind. A static property
    /// read is distinguished by its `$`-prefixed spelling instead.
    #[serde(default)]
    pub class_constant_accesses: Vec<Span>,
    /// Indices of declarations introduced directly in this scope.
    pub declares: Vec<usize>,
}

/// One lexical scope's owned facts (T18).
///
/// `scope_key` is the same deterministic key recorded on every
/// [`ExtractedUse`]; `parent_scope_key` is the enclosing scope, or `None` for
/// the file scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedScope {
    /// Deterministic key identifying the scope within the file.
    pub scope_key: String,
    /// The enclosing scope's key, or `None` for the file scope.
    pub parent_scope_key: Option<String>,
    /// Lexical facts introduced directly in this scope.
    pub facts: ScopeFacts,
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
    /// Lexical scopes with owned facts, sorted by `scope_key`.
    pub scopes: Vec<ExtractedScope>,
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
