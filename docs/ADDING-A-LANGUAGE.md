# Adding a Language

> **Status:** design contract, not implemented Rust API. PHP is first; TypeScript/TSX is second. Additional languages wait for the benchmark gate.

Language-specific extraction, identifier comparison, import interpretation, and receiver hints belong in `crates/rivet-languages/src/<lang>/`, behind `lang-<lang>`. SQLite and CLI behavior remain generic.

## MVP support boundary

| Construct | PHP | TypeScript / TSX |
|---|---|---|
| Files | `.php` using the PHP grammar with mixed HTML support | `.ts` and `.tsx` using their respective grammars; `.d.ts` declarations allowed |
| Named definitions | Namespaces, classes, interfaces, traits represented as class-kind, enums, named functions/methods, properties, constants | Named functions/classes/interfaces, enums, methods, fields, namespaces/modules, named const bindings including arrow functions, type aliases (`type_alias`) |
| Qualified names | Namespace `\`, member `::`; properties retain `$` | Lexical nesting with `.`; file path distinguishes modules |
| Lexical imports | `use` class/function/const bindings and aliases, including grouped forms | Direct relative named/default imports of explicitly exported declarations; namespace imports only for direct named members |
| Local receiver hints | `$this`, `self`, preceding `new`, native explicit parameter/property types including promotion | `this`, preceding `new`, explicit parameter/field/variable annotations |
| Candidate extraction | Identifier uses in code, including top-level and interpolated expressions | Identifier uses in code including JSX expressions and capitalized JSX component names; literal JSX text excluded |
| Reference-mode exclusion (LR2, spec §11.5) | An unresolved same-name use is excluded when its form cannot name the target kind, or when its receiver class (from the receiver hints above, indexed or not) can share no instance with the target's class: no indexed or anonymous subtype of the target's class has the receiver class in its declared `extends`/`implements` closure, and, for an unindexed receiver, no such subtype reaches an unindexed class. Never for a trait target, a possible-trait receiver, a refused hint, a name without a qualified name, or a snapshot with a supertype of unknown name. Unindexed classes are assumed never to extend or implement an indexed class | Not applied (T43): for a target or use in a TypeScript file neither rule applies. The PHP form table would drop real references (`Outer.f()`, a namespace import's `util.format()`, `Color.Red`, `new Foo()`, `typeof x`), and structural typing lets a receiver typed `Foo` hold any shape-compatible object, so a receiver class proves nothing |
| Not resolved in v0.1 | Container bindings, magic methods, trait composition, inheritance traversal for binding (the hierarchy is read only for exclusion), `static::`, variable function/member names, docblock inferred types | Path aliases, package resolution, re-exports, CommonJS, inheritance, structural typing, unions/generics, decorators' runtime effects, computed dynamic names |

Unsupported resolution stays unresolved; it does not silently become `exact`. Runtime-generated names and literal strings are not semantic references. Extraction coverage is about documented syntax, not arbitrary dynamic behavior. PHP kind-dependent case rules apply to lookup (for example, methods versus properties); TypeScript identifiers are case-sensitive. Record exact normalization rules and tests in the adapter README.

A TypeScript binding to an arrow/function expression is a named `function` symbol rather than a second duplicate `const` symbol. Anonymous expressions have no independently addressable symbol in v0.1; their uses attach to the nearest named container or null. TSX JSX lowercase intrinsic element names are not symbol references.

TypeScript definitions (T42) follow these rules; the adapter README has the full list:

- A type alias is a `type_alias` symbol, one per alias even when generic. Its lookup is case-sensitive, like every TypeScript identifier.
- Generic type parameters are not symbols; the generic class, interface, function, or alias is one ordinary symbol.
- Function-local declarations (consts, functions, classes inside a function body) are not symbols, as PHP locals are not. Their uses attach to the enclosing named symbol (T43).
- A getter and a setter are each a `method`. A pair sharing a name gets spec §10.1 ordinals in source order, and each signature keeps `get`/`set`.
- Overload signatures fold into one `function` or `method` symbol. When an implementation exists in the same scope, the symbol is the implementation's declaration, with its span and signature. When none exists (`.d.ts` files, ambient declarations, interface method overloads), the symbol is the first signature. The other signatures are not symbols, and no output records how many folded, so IDs stay stable.
- A string-named ambient module (`declare module "x" {}`) is not a symbol, and neither are its members. Identifier-named ambient declarations are named as the same declaration in a `.ts` file.
- A span includes `export`, `export default`, and `declare`. Class and interface member spans follow the grammar and exclude a trailing `;`.
- As in PHP, enum members are `const`; interface members are `method` or `property`; fields and constructor parameter properties are `property`; the constructor is `Class.constructor`; an ES `#private` name keeps its `#`, escaped per spec §10.1; declaration merging (an interface or namespace declared twice) gets ordinals.
- `const X = class {}` is one `class` symbol named `X`, by the arrow-function rule. An anonymous default-exported class or function is no symbol, and neither are its members (the PHP anonymous-class rule).

TypeScript uses, scopes, and imports (T43) follow these rules; the adapter README has the full list:

- A callee, a capitalized or dotted JSX component name, and a decorator are `call` uses; a name in a type position (annotations, generic arguments, heritage clauses, `as`/`satisfies`, `typeof x`), a `new` target, and an `instanceof` class operand are `type`; a member read or written through a receiver is `read`/`write`, and a bare assigned name `write`; the local binding an import creates (the alias when there is one) and the name in a re-export specifier are `import`; any other identifier in code, such as a name before `.`, is `unknown`.
- JSX text, attribute names, string attribute values, lowercase intrinsic element names, and closing tag names are not uses; `{expr}` containers and template interpolations are code. Declaration names are never uses.
- A use's container is the innermost function, method, class, interface, or enum symbol containing it; anonymous functions and classes are transparent.
- Each scope records the names it binds (`let`/`const`/class/enum/interface/type alias in their block; `var` and function declarations in the nearest function, namespace, or module scope; parameters and type parameters in their function) with their value or type space, so a same-name local is seen to hide an import. Imports (named, default, aliased, namespace, `import type`, `require` imports) and re-exports (`export { a } from`, `export *`) are recorded in the scope they are written in, specifier as written; `require(...)` in an expression is a call.
- Receiver hints are recorded for T45 (`this` in a named class, a `const` bound by `new`, one explicit named parameter, field, or variable type); no TypeScript use is bound until T44/T45 add TypeScript rules, and the resolver's rule table binds each language's uses with its own rules over its own declarations only.
- Lookup is case-sensitive for every TypeScript kind, and a use matches a target by name only within one language: in a mixed repository `rivet symbol foo` finds a PHP class `Foo` (PHP folds class names) but never a TypeScript class `Foo`, and a PHP class `Foo` and a TypeScript class `Foo` are two symbols.

PHP namespace lookup uses indexed qualified names without executing Composer/autoload code. TypeScript relative module lookup checks an explicit ordered candidate set: exact supported path, `.ts`, `.tsx`, `.d.ts`, `/index.ts`, `/index.tsx`, `/index.d.ts`; require a unique valid module candidate across the applicable set, never guess among duplicates. Resolve named/default exports only when they identify an explicit local declaration; anonymous default exports remain unresolved. Extensions outside this matrix, including `.js`, `.jsx`, `.mts`, and `.cts`, are not enabled by inference in v0.1.

## Adapter contract

A conceptual interface (exact Rust types follow the dependency spike):

```rust
pub trait Language: Send + Sync {
    fn id(&self) -> LanguageId;
    fn grammar(&self, variant: GrammarVariant) -> tree_sitter::Language;
    fn extract(&self, source: &str, tree: &Tree) -> ExtractedFile;
    fn normalize_identifier(&self, name: &str, kind: IdentifierKind) -> String;
    fn signature(&self, symbol: &ExtractedSymbol, source: &str) -> String;
}
```

`ExtractedFile` owns definitions, identifier uses, lexical scopes, import bindings, assignment/type facts, and diagnostics. It contains byte spans, not borrowed `Node` handles. A use records its nearest named container if any, lexical scope, written spelling, use kind, optional receiver, and an evidence hint. These hints are not final cross-file resolution tiers. The generic resolver turns persisted facts into at most one `exact`/`scoped` binding; otherwise the use remains unresolved.

Keep extraction independent from resolution so an unchanged file's raw uses can be re-resolved when other files change. Preserve aliases and scope/shadowing facts; receiver text alone is insufficient. Calls are uses with kind `call`, not a duplicate reference row. Import statements can both create bindings and contain identifier-use spans, but those are distinct concepts.

## Implementation checklist

- Add an optional, pinned compatible grammar dependency and forward the language feature from the CLI. TS and TSX need explicit grammar variants.
- Register supported extensions; do not infer general JavaScript support from the TypeScript adapter. Shebang detection is deferred until a language needs it.
- Prefer reviewed `.scm` queries where grammar patterns suffice; use a documented scope walk where binding order/shadowing needs it.
- Define symbol naming, duplicate handling, signature summaries, and lookup normalization. Canonical IDs follow spec §10's escaping and ordinal rules.
- Extract named definitions, imports, uses, and lexical facts. Traverse expressions inside interpolated strings/templates instead of excluding the entire string subtree.
- Emit no facts for a file containing `ERROR` or missing nodes under the v0.1 parse policy. Apply deterministic limits and cancellation.
- Add authored corner-case fixtures and a real, permissively licensed repository pinned to a commit, with attribution.
- Add gold use spans, target bindings, and per-tier expectations outside the fixture submodule.
- Run query snapshots, coverage/binding assertions, freshness invalidation, budget, and determinism checks.
- Document supported syntax and unresolved constructs in the adapter README.

## Query capture conventions

Use names such as `@symbol.<kind>`, `@symbol.name`, `@symbol.body`, `@call.name`, `@call.receiver`, `@import.source`, `@import.binding`, `@ref.type`, `@ref.read`, and `@ref.write` where applicable. Do not require body/receiver captures for constructs without them. Grammar node names must be validated against pinned grammar versions; illustrative queries are not compatibility guarantees.

## Fixtures and gold data

Authored fixtures should isolate same-name classes, aliases, local shadowing, reassignment, top-level uses, nested/anonymous functions, interpolation, malformed source, CRLF, and Unicode. Pair them with a small real repository, roughly 100–500 files, pinned under `tests/fixtures/<lang>/repo/` as a submodule. Keep gold in the parent repository, not inside the submodule: the authored fixture's gold is `tests/gold/<lang>-authored.toml` (`php-authored.toml`, `typescript-authored.toml`).

Gold data records exact `(file, start_byte, end_byte, ref_kind)` use spans, resolved declaration IDs where supported, and the justified tier. Counts alone can hide both a missing reference and an extra false positive. Include at least ten representative named symbols, plus negative same-name cases and alias uses whose spelling differs from the target.

Executable integration tests live in `crates/rivet-cli/tests/` (a virtual workspace will not discover root-level tests automatically). Required assertions:

- Candidate mode covers every annotated same-name identifier-use span in supported files.
- Reference mode includes supported gold bindings and unresolved matching uses while excluding known bindings to other definitions.
- Every alias-bound gold use is returned even if its text differs from the queried name.
- No claimed exact link depends solely on index-wide uniqueness, a receiver type guess, or runtime dispatch.
- Query bytes agree across repeated queries and full versus incremental builds.

`rg` is useful to help annotate fixtures, not an oracle for declarations, comments, strings, bindings, aliases, or runtime reference completeness.
