# Adding a Language

> **Status:** design contract, not implemented Rust API. PHP is first; TypeScript/TSX is second. Additional languages wait for the benchmark gate.

Language-specific extraction, identifier comparison, import interpretation, and receiver hints belong in `crates/rivet-languages/src/<lang>/`, behind `lang-<lang>`. SQLite and CLI behavior remain generic.

## MVP support boundary

| Construct | PHP | TypeScript / TSX |
|---|---|---|
| Files | `.php` using the PHP grammar with mixed HTML support | `.ts` and `.tsx` using their respective grammars; `.d.ts` declarations allowed |
| Named definitions | Namespaces, classes, interfaces, traits represented as class-kind, enums, named functions/methods, properties, constants | Named functions/classes/interfaces, enums, methods, fields, namespaces/modules, named const bindings including arrow functions, type aliases (`type_alias`) |
| Qualified names | Namespace `\`, member `::`; properties retain `$` | Lexical nesting with `.`; file path distinguishes modules |
| Lexical imports | `use` class/function/const bindings and aliases, including grouped forms | Direct relative named, aliased, default, and `import type` imports of explicitly exported declarations (T44); namespace imports only for direct named members (`ns.member`, a lexical module-qualified name, not receiver access); same-file lexical bindings with shadowing and value/type spaces accounted for |
| Local receiver hints | `$this`, `self`, preceding `new`, native explicit parameter/property types including promotion | `this` in a named class (static or instance side), a `const` bound by `new C(...)`, one explicit named parameter/field/parameter-property/variable annotation; the class is looked up where the annotation or `new` is written and must be one class or interface, whose own member on the matching side binds `scoped` (T45) |
| Candidate extraction | Identifier uses in code, including top-level and interpolated expressions | Identifier uses in code including JSX expressions and capitalized JSX component names; literal JSX text excluded |
| Reference-mode exclusion (LR2, spec §11.5) | An unresolved same-name use is excluded when its form cannot name the target kind, or when its receiver class (from the receiver hints above, indexed or not) can share no instance with the target's class: no indexed or anonymous subtype of the target's class has the receiver class in its declared `extends`/`implements` closure, and, for an unindexed receiver, no such subtype reaches an unindexed class. Never for a trait target, a possible-trait receiver, a refused hint, a name without a qualified name, or a snapshot with a supertype of unknown name. Unindexed classes are assumed never to extend or implement an indexed class | Not applied (T43): for a target or use in a TypeScript file neither rule applies. The PHP form table would drop real references (`Outer.f()`, a namespace import's `util.format()`, `Color.Red`, `new Foo()`, `typeof x`), and structural typing lets a receiver typed `Foo` hold any shape-compatible object, so a receiver class proves nothing |
| Not resolved in v0.1 | Container bindings, magic methods, trait composition, inheritance traversal for binding (the hierarchy is read only for exclusion), `static::`, variable function/member names, docblock inferred types | Path aliases, package resolution, re-exports, CommonJS, inheritance, structural typing, unions/generics, decorators' runtime effects, computed dynamic names, untyped or `let`/`var` receivers, class-name receivers (`Foo.create()`) |

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
- Receiver hints are recorded (`this` in a named class, a `const` bound by `new`, one explicit named parameter, field, or variable type); T45 binds them (below). The resolver's rule table binds each language's uses with its own rules over its own declarations only.
- Lookup is case-sensitive for every TypeScript kind, and a use matches a target by name only within one language: in a mixed repository `rivet symbol foo` finds a PHP class `Foo` (PHP folds class names) but never a TypeScript class `Foo`, and a PHP class `Foo` and a TypeScript class `Foo` are two symbols.

TypeScript bindings (T44) follow these rules; the adapter README and `rivet_index::resolve` have the full list. Every binding they make is `exact`:

- Exports are facts: each module records what it exports from its own declarations (`export` on a declaration, `export default` of a named declaration or of an identifier, local `export { a as b }` specifiers, and `default` with no local name for an anonymous or expression default). Re-exports stay recorded as import facts and are never followed.
- A named, aliased, or `import type` import binds its `import` use, and every unshadowed use of its local name, to the declaration the module exports under the imported name, following the module's own `export { x as a }` renames. A default import binds to the default export when it identifies one explicit local declaration.
- A namespace import's `ns.member` binds `member` to the declaration the module exports as `member`. It is a lexical module-qualified name, not member access on an object, so spec §11.4's receiver-evidence clause does not apply; `ns.member.deeper` binds only `member`.
- A use whose name resolves lexically, with shadowing accounted for, to exactly one module-local declaration of its own file binds to it. A type-position use looks among types (class, interface, enum, type alias, namespace) and any other use among values (function, class, enum, const, namespace); a `new` target, an `instanceof` or type-position `typeof` operand, and a class `extends` expression are `type` uses that look among values. A binding that is no symbol (a parameter, a `let`, a function-local declaration) hides every outer one of its name and space, so the use stays unresolved. A global declaration (in a script file, which has no top-level `import` or `export`, or inside `declare global`) can merge with other files' declarations, so it binds nothing.
- More than one candidate declaration is refused (spec §11.4): merged declarations such as `interface X` twice, a name bound both by an import and a local, and, inside a namespace or enum declared more than once in its file, any declaration outside the use's own body. Inside a string-named ambient module (`declare module "x" {}`, including an augmentation), a name the body does not bind itself is refused too, because the body also sees the exports of the module it declares or augments.
- Everything else stays unresolved: re-exports, `export *`, an exported import binding (`import { a } from "./x"; export { a }`), path aliases, packages, `require`, a name the module does not export, and an unresolved or ambiguous module. A member through any other receiver is T45's (below).

TypeScript receiver bindings (T45) follow these rules; the adapter README and `rivet_index::resolve::rules::ts_receivers` have the full list. Every binding they make is `scoped` (spec §11.4 rules 1-3), never `exact`, and never upgraded by uniqueness:

- The receiver's class comes from its hint: `this` names the named class enclosing the use (arrow functions are transparent); a parameter, field, constructor parameter property, or variable annotation names its type (`Foo`, `Foo<T>` as `Foo`, `ns.Foo`); a `const` initialized by `new C(...)` names `C`. An annotation or `new` target is looked up by the T44 lexical rules in the scope where it is written, not the member use's scope, so `const a: Foo` at module level names the module's `Foo` even when `a.m()` is inside a namespace declaring another `Foo`, and a generic type parameter `Foo` hides the class. An annotation needs a type-space declaration and a `new` target a value-space one.
- The declaration must be exactly one class or interface. A type alias, enum, namespace, or function, an unresolved or ambiguous name (including declaration merging such as `class X` plus `interface X`), a package or path-alias import, and a re-export give no receiver class.
- The receiver class must itself declare exactly one method or property (field, accessor, constructor parameter property, ES private name with its `#`, interface method or property signature) of the use's spelling, case-sensitive. `this` inside a static member, static block, or static field initializer binds only static members; every other receiver binds only instance members. A member whose side is not recorded (a constructor) blocks the binding. Inheritance is never traversed, an interface receiver binds the interface's own member and never an implementing class's, and a getter and setter sharing a name are two candidates, so a use of that name stays unresolved.
- Stays unresolved: untyped receivers, `let`/`var` bound by `new`, union, array, function, and structural annotations, a nested `function`'s or object-literal method's or anonymous class's `this`, calls on a class name (`SurveyService.create()`, `Status.Active`), computed members (`obj[name]()`), and chained receivers no hint covers (`this.a.b.m()`). No TypeScript use gets a receiver class, since TypeScript has no evidence-based exclusion (spec §11.5), so reference mode still lists every unresolved same-name use as `name_match`.

PHP namespace lookup uses indexed qualified names without executing Composer/autoload code. TypeScript relative module lookup applies only to `./` and `../` specifiers, joined to the importing file's directory, and checks an explicit ordered candidate set: exact supported path, `.ts`, `.tsx`, `.d.ts`, `/index.ts`, `/index.tsx`, `/index.d.ts`; require a unique valid module candidate across the applicable set, never guess among duplicates. Candidates are compared byte for byte with the indexed paths of the snapshot, never the filesystem, so `./Util` does not name `util.ts` on a case-insensitive disk. A candidate that is in the snapshot but not indexed (a parse failure, a size or encoding skip) is not valid, and still counts as a candidate: beside another candidate the import is ambiguous. A specifier that leaves the repository or has an empty segment (`.//x`, a trailing `/`) names no module. Resolve named/default exports only when they identify an explicit local declaration; anonymous default exports remain unresolved. Extensions outside this matrix, including `.js`, `.jsx`, `.mts`, and `.cts`, are not enabled by inference in v0.1.

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
