//! Owned, database-free extraction records.
//!
//! A language adapter returns an [`ExtractedFile`] with owned symbols,
//! identifier uses, import bindings, lexical scopes, and diagnostics: byte
//! [`Span`]s and plain strings only, never borrowed Tree-sitter `Node` handles
//! or database identifiers. T11 added named definitions and parse diagnostics;
//! T17 adds lexical uses, imports, and receiver hints; T18 adds owned lexical
//! scope facts. T43 adds the TypeScript scope facts: [`LocalBinding`]s in
//! their [`BindingSpace`], and [`ModuleImport`]s. Resolution and persistence
//! are later tasks.
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
/// [`SymbolKind::Module`] but are not parents. For TypeScript (T42), members
/// point at their nearest enclosing class, interface, enum, or namespace,
/// whose qualified name prefixes theirs.
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
    /// The range of the declared name itself, when the adapter records one.
    ///
    /// The TypeScript adapter (T42) records it for every symbol. The PHP
    /// adapter records none, and a record rebuilt from stored rows has none
    /// either: the store persists no name span.
    pub name_span: Option<Span>,
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
        /// Whether the type was declared on a parameter or on a property
        /// (AF3). The two differ in what PHP enforces, so the resolver trusts
        /// them under different conditions. An absent value means
        /// [`TypedOrigin::Parameter`], the stricter of the two.
        #[serde(default)]
        origin: TypedOrigin,
    },
    /// The receiver is a class named explicitly in a static call, a
    /// class-constant access, or a static property access (`Foo::make()`,
    /// `Foo::BAR`, `Foo::$prop`) (AF4). `self`, `static`, and `parent` are
    /// never recorded this way.
    NamedClass {
        /// The class name exactly as written before `::`.
        class_spelling: String,
    },
    /// The use names an imported binding.
    Imported {
        /// The local binding name introduced by the import.
        binding: String,
    },
    /// No supported lexical evidence was found.
    Unresolved,
}

/// Where the declared type of a [`UseHint::Typed`] receiver was written (AF3).
///
/// PHP checks a parameter's declared type only when the function is called, so
/// the body may rebind the variable to anything; a parameter type is evidence
/// only while the variable is never rebound. PHP checks a typed property on
/// every assignment, so a property type holds however often it is reassigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedOrigin {
    /// A parameter, including a promoted constructor parameter used as the
    /// local variable inside the constructor. The default, because it is the
    /// origin the resolver trusts least.
    #[default]
    Parameter,
    /// A typed property (declared or promoted), read through `$this->name`.
    /// For TypeScript (T43), a class field or constructor parameter property
    /// read through `this.name`.
    Property,
    /// A variable declared with an explicit type annotation (T43, TypeScript:
    /// `const x: Foo = ...`). The PHP adapter never records it.
    Variable,
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
    /// For a direct `new` assignment, the end byte of the `new` expression on
    /// the right-hand side (AF3). A call inside that expression (the
    /// constructor itself or one of its arguments) runs before the assignment
    /// completes, so it cannot rebind the variable afterwards. `None` for any
    /// other binding, and for facts written before AF3; the resolver then
    /// measures from the variable's own span, which only over-suppresses.
    #[serde(default)]
    pub value_end: Option<u32>,
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
    /// An object creation `new Class(...)` (AF3). The callee is always
    /// `__construct`, looked up on the receiver class like a method.
    Constructor,
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
    /// A local variable whose class comes from a receiver hint (AF3). The
    /// hint is only as trustworthy as the variable, so the resolver re-checks
    /// the variable under the same conditions as the matching receiver rule
    /// before it reads the method's parameter list.
    Variable {
        /// The receiver variable exactly as written, including its `$`.
        variable: String,
        /// The hint the class came from.
        evidence: ReceiverEvidence,
    },
    /// No usable receiver (an unknown variable, `parent`, or a dynamic name).
    Unknown,
}

/// The local hint behind a [`CallReceiver::Variable`] (AF3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReceiverEvidence {
    /// The variable's most recent assignment was `new <class_spelling>(...)`.
    New {
        /// The class name exactly as written at the `new` site.
        class_spelling: String,
        /// The start byte of the nearest enclosing control-flow block at the
        /// call, as in [`UseHint::NewExpr`].
        #[serde(default)]
        use_block: Option<u32>,
    },
    /// The variable is a parameter declared with this single class type.
    TypedParameter {
        /// The type name exactly as written at the declaration.
        type_spelling: String,
    },
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

/// Which parameters of one indexed function or method are declared by
/// reference, read from the parse tree at extraction time (AF3).
///
/// `by_ref[i]` is true when the parameter at 0-based position `i` is declared
/// by reference (`&$x`, `&...$xs`, or a promoted `&$x`). The resolver treats a
/// position past the end of the list as unknown, never as by-value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParameterList {
    /// Index of the declaring symbol in the owning [`ExtractedFile::symbols`];
    /// persistence rewrites it to the canonical symbol ID, as for
    /// [`ScopeFacts::declares`].
    pub symbol: usize,
    /// Per-position by-reference flags, in declaration order.
    pub by_ref: Vec<bool>,
}

/// How a class-like declaration names one supertype (T36d).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupertypeRelation {
    /// A class's `extends` parent, or one name of an interface's `extends`
    /// list.
    Extends,
    /// One name of a class's or enum's `implements` list.
    Implements,
}

impl SupertypeRelation {
    /// The snake_case form used in persisted facts.
    pub fn as_str(&self) -> &'static str {
        match self {
            SupertypeRelation::Extends => "extends",
            SupertypeRelation::Implements => "implements",
        }
    }
}

/// One supertype a named class-like declaration names in its header (T36d).
///
/// The name is kept exactly as written. Its lexical context (namespace and
/// imports) is not duplicated: the same name is also recorded as a
/// [`RefKind::Type`] use with span [`span`](Self::span), whose scope chain
/// carries it, and whose binding (if any) is the resolved supertype.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredSupertype {
    /// Index of the declaring class-like symbol in the owning
    /// [`ExtractedFile::symbols`]; persistence rewrites it to the canonical
    /// symbol ID, as for [`ScopeFacts::declares`].
    pub symbol: usize,
    /// Whether the name appears in an `extends` or an `implements` clause.
    pub relation: SupertypeRelation,
    /// The name as written (`Base`, `Lib\Base`, `\Vendor\Base`).
    pub spelling: String,
    /// The span of the name, identical to its type use's span.
    pub span: Span,
}

/// One supertype an anonymous class names in its header (LR2).
///
/// An anonymous class is not a symbol, but an object of it is an instance of
/// every supertype it names, so reference-mode exclusion must count it as a
/// possible common subtype. Like [`DeclaredSupertype`], the name is also a
/// [`RefKind::Type`] use with span [`span`](Self::span) in the same scope, so
/// it resolves through that use's binding and scope chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymousSupertype {
    /// The start byte of the `anonymous_class` node, which identifies the
    /// anonymous class within its file.
    pub class_start: u32,
    /// Whether the name appears in an `extends` or an `implements` clause.
    pub relation: SupertypeRelation,
    /// The name as written.
    pub spelling: String,
    /// The span of the name, identical to its type use's span.
    pub span: Span,
}

/// Which declaration space a lexically bound name occupies (T43).
///
/// TypeScript keeps values and types apart: a local `const Foo` hides an
/// imported `Foo` from a value use but not from a `type` use, and a type
/// parameter `T` hides only types. The PHP adapter records no
/// [`LocalBinding`], so it never uses this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSpace {
    /// A value only: a variable, parameter, `catch` parameter, function, or
    /// enum member.
    Value,
    /// A type only: an interface, a type alias, a type parameter, or an
    /// `infer` or mapped-type name.
    Type,
    /// Both a value and a type (or namespace): a class, an enum, a namespace,
    /// or an `import X = ...` alias.
    Both,
}

/// One name a lexical scope binds directly (T43, TypeScript).
///
/// Every declared name is recorded, whether or not it is a symbol: a local
/// `const`, a parameter, a type parameter, a top-level function. A name
/// binds for the whole scope, wherever in the scope it is declared, so the
/// span says where the declaration is, not where the binding starts.
/// Import bindings are not locals; they are [`ModuleImport`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBinding {
    /// The declared name exactly as written.
    pub name: String,
    /// The declaration space the name occupies.
    pub space: BindingSpace,
    /// The declared name's byte range.
    pub span: Span,
}

/// The form of one TypeScript import or re-export (T43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleImportKind {
    /// `import { a } from "m"`, `import { a as b } from "m"`, and
    /// `import { default as b } from "m"`: binds one named export.
    Named,
    /// `import b from "m"`: binds the default export.
    Default,
    /// `import * as b from "m"`: binds the module namespace object.
    Namespace,
    /// `import b = require("m")`: CommonJS interop, which v0.1 never resolves.
    Require,
    /// `export { a } from "m"` and `export { a as b } from "m"`: re-exports one
    /// export and binds no local name.
    ReExport,
    /// `export * from "m"` and `export * as b from "m"`: re-exports every
    /// export and binds no local name.
    ReExportAll,
}

/// One TypeScript import binding or re-export, as written (T43).
///
/// The adapter records the specifier exactly as written, relative or not
/// (`./util`, `@/util`, `lodash`), and never resolves it: which module a
/// specifier names, and which declaration an export names, is resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleImport {
    /// The import or re-export form.
    pub kind: ModuleImportKind,
    /// The local binding name (the alias when there is one), or `None` for a
    /// re-export, which binds no local name.
    pub local: Option<String>,
    /// The export name taken from the module as written (`double`; `default`
    /// for a default import), or `None` for a namespace import, a `require`
    /// import, and `export *`.
    pub imported: Option<String>,
    /// For a re-export, the name this module exports it under (`twice` in
    /// `export { double as twice } from "./util"`; `ns` in `export * as ns`);
    /// `None` for an import and for a bare `export *`.
    pub exported: Option<String>,
    /// The module specifier exactly as written, without its quotes.
    pub specifier: String,
    /// Whether the binding is type-only: `import type`, `export type`, or a
    /// `type` modifier on one specifier.
    pub type_only: bool,
    /// The span of the identifier the `import` use sits on: the local binding
    /// for an import, the re-exported name for a re-export specifier, the
    /// exported name of `export * as ns`, and the `*` of a bare `export *`.
    pub span: Span,
}

/// Owned lexical facts recorded for one scope (T18).
///
/// The persisted `scopes.facts_json` holds `imports`, `typed_bindings`,
/// `new_bindings`, `call_args`, `unanalysable`, `namespace_unattributed`,
/// `class_constant_accesses`, `global_scope`, `call_sites`, `goto_present`,
/// `global_names`, `dynamic_global_write`, `parameter_lists`, `supertypes`,
/// `anonymous_supertypes`, and `declares` for a PHP scope, and `locals`,
/// `module_imports`, and `declares` for a TypeScript scope (T43).
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
    /// True when this scope's variables are the program's global variables
    /// (AF3): for PHP, the top level of a file or of one namespace block. Such
    /// a variable can be rebound by any function that declares it `global` or
    /// writes it through `$GLOBALS`, so a call can rebind it.
    #[serde(default)]
    pub global_scope: bool,
    /// The span of every explicit call in this scope, in source order (AF3):
    /// function, method, nullsafe, and static calls, `new`, and `clone`.
    /// Recorded only in a [`global_scope`](Self::global_scope) scope, where a
    /// call can rebind a variable through `global`.
    #[serde(default)]
    pub call_sites: Vec<Span>,
    /// True when this scope contains a `goto` (AF3). A backward jump can run a
    /// call written after a use before that use, so source order alone no
    /// longer bounds which calls intervene. Recorded only in a global scope.
    #[serde(default)]
    pub goto_present: bool,
    /// Variables this scope can rebind in the global scope, sorted and
    /// deduplicated (AF3): each name in a `global` statement and each literal
    /// `$GLOBALS['name']` key written, referenced, or passed as an argument,
    /// with a leading `$`.
    #[serde(default)]
    pub global_names: Vec<String>,
    /// True when this scope may rebind a global variable it does not name
    /// (AF3): `global $$name`, a `$GLOBALS` write or reference with a
    /// non-literal key, `$GLOBALS` passed whole, or `eval` or an include
    /// inside a function body, whose code may declare anything `global`.
    #[serde(default)]
    pub dynamic_global_write: bool,
    /// By-reference parameter positions of each function and method declared
    /// directly in this scope (AF3), in symbol order.
    #[serde(default)]
    pub parameter_lists: Vec<ParameterList>,
    /// Declared supertypes of each named class, interface, and enum declared
    /// directly in this scope (T36d), in symbol order, then source order.
    #[serde(default)]
    pub supertypes: Vec<DeclaredSupertype>,
    /// Supertypes named by the headers of anonymous classes whose header
    /// uses belong to this scope (LR2), in source order.
    #[serde(default)]
    pub anonymous_supertypes: Vec<AnonymousSupertype>,
    /// Every name this scope binds directly, in source order (T43,
    /// TypeScript). A use's scope chain is its scope, then each
    /// `parent_scope_key`; the nearest scope that binds the use's name in the
    /// use's declaration space holds the declaration it names, so a local
    /// recorded here hides a same-name import of an enclosing scope.
    #[serde(default)]
    pub locals: Vec<LocalBinding>,
    /// TypeScript import bindings and re-exports written directly in this
    /// scope, in source order (T43): the module scope, or the body of a
    /// string-named ambient module.
    #[serde(default)]
    pub module_imports: Vec<ModuleImport>,
    /// Indices of declarations introduced directly in this scope.
    ///
    /// For PHP, every symbol, members under their class-like's scope. For
    /// TypeScript (T43), the symbols among this scope's [`locals`](Self::locals):
    /// a top-level or namespace-member declaration, or an enum member in its
    /// enum's scope. A class or interface member is in no TypeScript scope's
    /// `declares`, because a bare name never reaches it.
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
    /// Import bindings created by PHP `use` declarations, in source order.
    /// TypeScript import bindings are [`ScopeFacts::module_imports`] of the
    /// scope they are written in (T43); the TypeScript adapter leaves this
    /// empty.
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
