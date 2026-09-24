# TypeScript adapter

Extraction for `.ts`, `.d.ts`, and `.tsx` files using the pinned
`tree-sitter-typescript` grammars: `.ts` and `.d.ts` with the TypeScript
grammar, `.tsx` with the TSX grammar (`crate::language_for_path`). One adapter
serves both; every node kind it reads has the same name and fields in each.
The adapter produces owned facts only; cross-file resolution belongs to the
generic resolver (`docs/ADDING-A-LANGUAGE.md` "Adapter contract").

T42 extracts named definitions; T43 adds uses, lexical scopes, and imports,
turns `LanguageId::has_extractor` on for TypeScript and TSX, and dispatches
both to this adapter in `rivet_parser`. T44 adds each module's exports, the
`type` uses that name values, and module-local `declares`; with them the
resolver's TypeScript rules bind direct relative imports, namespace-import
members, and same-file lexical bindings (`exact`). T45 records what the
receiver rules need (the static side of `this` and of each member, and where
an annotation's type name or a `new` target is written) and binds `this`,
typed, and `new` receivers (`scoped`). Every other TypeScript reference is
`name_match`.

## Named definitions (T42)

| TypeScript construct | `kind` | Notes |
|---|---|---|
| `function`, `function*`, `declare function`, `export default function f` | `function` | Overload signatures fold (below). |
| `class`, `abstract class`, `declare class` | `class` | Decorators on the class are inside its span. |
| `const X = class { ... }` | `class` | Named by the binding, by the arrow-function rule; its members are `X.member`. |
| `interface` | `interface` | Declaring one twice gets ordinals (declaration merging). |
| `enum`, `const enum` | `enum` | |
| enum member | `const` | As a PHP enum case. Its span is the member (`Draft`, `Active = "active"`). |
| `namespace`, `module`, `declare namespace`, `declare module Name` | `module` | Nested namespaces nest their names. A dotted `namespace A.B.C` is one symbol named `C` with qualified name `A.B.C`. |
| `type X = ...` | `type_alias` | One symbol, generic or not. |
| `const` binding, `declare const` | `const` | Each name of a multi-name statement is one symbol with the statement's span. |
| `const` bound to an arrow function, function expression, or generator function | `function` | One symbol, not a second `const`. |
| method, constructor, getter, setter, abstract method, class method signature | `method` | The constructor is `Class.constructor`. |
| field (`static`, `readonly`, `declare`, `abstract`, `accessor`, `#private`) | `property` | An ES private name keeps its `#`, escaped `%23` in the ID. |
| constructor parameter property | `property` | A parameter with an accessibility modifier, `readonly`, or `override`, spanning the parameter. |
| interface property signature | `property` | Including a function-typed one (`onPress: () => void`). |
| interface method signature, including `get`/`set` signatures | `method` | |

Qualified names use lexical nesting joined with `.`, and the file path
distinguishes modules. A member's parent is its nearest enclosing named
container: class, interface, enum, or namespace. So the parent's qualified
name plus `.` plus the member's name is always the member's qualified name.
Unlike PHP, a namespace is a parent: a TypeScript namespace is a declaration
block whose members are its exports, where a PHP namespace is only a name
prefix. Canonical IDs follow spec §10.1: `%` and `#` are escaped, and
duplicate qualified names in one file get ordinals in `(start_byte, end_byte,
kind)` order.

### Not symbols

- A generic type parameter.
- A declaration inside a function or method body, an arrow function, a class
  static block, or a top-level block (`if`, `{ }`): locals are never symbols,
  as in PHP.
- An anonymous class or function, including an anonymous `export default`
  class or function, and their members: the PHP anonymous-class rule.
  `export default class { ... }` directly followed by a statement that starts
  with `(` parses as one call expression in the pinned grammar; the class is
  still anonymous and nothing in it is a symbol.
- A class expression not bound to a `const`, and its members.
- An object literal's methods and properties (`const api = { get() {} }` is one
  `const`).
- `let` and `var` bindings, and a `const` bound to a destructuring pattern.
- A string-named ambient module (`declare module "x" { ... }`, `declare module
  "x";`) and everything in it. Its name is a module specifier, not an
  identifier.
- `declare global { ... }` itself; its declarations are named as top-level
  ones.
- A member named by a string or number literal (`"content-type": string`) or a
  computed name (`[Symbol.iterator]()`); an index, call, or construct
  signature.
- Re-exports, `export =`, `export as namespace`, `import X = ...`, and other
  statements that declare nothing.

### Overloads

Functions or methods in the same body with the same qualified name, accessor,
and static-ness are one overload set.

- With an implementation in that body, the implementation is the one symbol,
  with its own span, signature, and doc comment.
- Without one (a `.d.ts` file, an ambient or abstract declaration, interface
  method overloads), the first signature is the one symbol.
- The other signatures are not symbols, and nothing records how many folded,
  so adding or removing a signature does not change an ID.
- A getter and a setter are never one set, so a pair sharing a name is two
  `method` symbols with ordinals. Two merged interfaces or namespaces are two
  bodies: a member declared in both is two symbols with ordinals.

### Spans

A declaration's span includes `export`, `export default`, and `declare`: for
an exported or ambient declaration it is the wrapping statement. A class or
interface member's span follows the grammar: it excludes a trailing `;`. The
pinned grammar puts a field's decorators inside the field node but a method's
decorators before the method node, so a decorated field's span includes its
decorators and a decorated method's does not. The grammar parses a bare
`namespace X {}` statement as an expression statement holding the namespace;
the span is that statement, as for any statement.

## Uses (T43)

`uses.rs` walks the whole file once. Every identifier written in code is one
use, classified by its position:

| Position | `ref_kind` | Receiver |
|---|---|---|
| A callee: `f()`, `a.f()`, `a?.f()`, a tagged template `` tag`...` `` | `call` | the object's text for a member (`a`, `this.service`, `toLabel(value)`), without `?.` |
| A capitalized JSX element name `<Button />`, or a dotted one `<ui.Button />` | `call` | `ui` for a dotted name |
| A decorator `@Injectable()` or `@observable` | `call` | as for a callee |
| A type position: annotations, return types, generic arguments, heritage (`extends`, `implements`), `as`/`satisfies`/`<T>x`, `keyof`, `typeof x` | `type` | `ns` for `ns.Type`, `w` for `typeof w.u` |
| A `new` target, and the class operand of `instanceof` (as in PHP) | `type` | `ns` for `new ns.Foo()` |
| The local binding an import creates (the alias when there is one: `runAll` in `{ launchAll as runAll }`) | `import` | none |
| The name in a re-export specifier (`double` in `export { double as twice } from`) | `import` | none |
| A member read through a receiver `a.b` | `read` | `a` |
| A member written: `a.b = 1`, `a.b += 1`, `a.b++`, `this.#count += 1` | `write` | `a` |
| A bare name assigned or updated: `x = 1`, `x++`, `[a, b] = pair`, `({ c } = o)` | `write` | none |
| Any other identifier in code: a name before `.`, an argument, a shorthand property `{ s }`, a local export `export { a }`, `export default x` | `unknown` | none |

Not uses: comments, string literals, template-literal text (its
`${...}` interpolations are code), JSX text, JSX attribute names, string
attribute values, lowercase intrinsic element names (`<div>`, `<my-element>`)
and namespaced ones (`<svg:rect>`), a closing tag's name (`</Card>`: the
element is one reference), and every declaration name: functions, classes,
members, parameters, variables and destructured names, type parameters, enum
members, object keys, labels, an index signature's key, a type predicate's
parameter, `export as namespace X`, and `default` in a re-export.

A use's container is the innermost function, method, class, interface, or
enum symbol whose span contains it, as for PHP. A namespace, a `const`, a
property, and a type alias are not containers. Anonymous functions and
classes are not symbols, so a use in a callback belongs to the enclosing named
function, and one in a top-level IIFE or an anonymous default export has none.

## Scopes (T43)

The module is `top:file`; every other scope is `{container}:{ordinal}`, the
index of the innermost container symbol where it opens (or `top`) and its
one-based pre-order ordinal, with its parent recorded. A scope opens for each
namespace or ambient module body, function, method, arrow function, and class
static block, and for each block, `for`/`for...in/of`, `catch`, `switch` body,
class, interface, type alias, signature, mapped type, or conditional type that
binds a name. Each records the names it binds (`locals`) with their space:

| Declaration | Where it binds | Space |
|---|---|---|
| `let`, `const`, `class`, `enum`, `interface`, `type`, `import X = A.B` | the enclosing block (or `for`, `switch`) | value / both / type |
| `var`, `function` (even in a block, which can only over-report shadowing) | the nearest function, namespace, or module scope | value |
| parameters (and destructured ones), `catch` parameter | the function / `catch` scope | value |
| type parameters, `infer U`, a mapped type's key | the declaring function, class, interface, alias, or type | type |
| a named function or class expression's own name | its own scope | value / both |
| enum members | the enum's scope, where initializers can name them | value |
| `namespace A.B` | `A` in the enclosing scope | both |

A name binds for its whole scope, wherever in it it is declared. `declares`
lists the module-local symbols among a scope's locals; class and interface
members are in no scope's `declares`, because a bare name never reaches them
(a constructor parameter property is a parameter inside the constructor), and
neither are globals (T44, below). A use's scope
chain, nearest first, therefore shows whether a local of the same name hides
an import: the fixture's `shadowed` function binds `double` itself, so its
`double(7)` finds the local before the module scope's import.

## Imports (T43)

Import bindings and re-exports are `ModuleImport`s of the scope they are
written in (the module, or a string-named ambient module body), never
`locals`; `ExtractedFile::imports` holds PHP `use` bindings only.

| Form | `kind` | `local` | `imported` | `exported` |
|---|---|---|---|---|
| `import { a } from "m"`, `import { a as b }` | `named` | `a` / `b` | `a` | none |
| `import b from "m"`, `import { default as b }` | `default` | `b` | `default` | none |
| `import * as ns from "m"` | `namespace` | `ns` | none | none |
| `import q = require("m")` | `require` | `q` | none | none |
| `export { a } from "m"`, `export { a as b } from "m"` | `re_export` | none | `a` | `a` / `b` |
| `export * from "m"`, `export * as ns from "m"` | `re_export_all` | none | none | none / `ns` |

`type_only` records `import type`, `export type`, and a `type` modifier on one
specifier; `import type` is an ordinary import binding, which T44 resolves
like any other.
The specifier is kept exactly as written, relative (`./util`), path alias
(`@/util`), or package (`lodash`); nothing here decides which module it names.
`require(...)` in an expression is a `call` of `require`, not an import, and
`import "./side-effect"` binds nothing and is not recorded.

## Exports (T44)

Each local `export` statement written in a module's own scope (the file's
module scope, or a string-named ambient module body) records what the module
exports from its own declarations, as `module_exports` of that scope:

| Form | `exported` | `local` |
|---|---|---|
| `export function f`, `export class C`, `export interface I`, `export type T`, `export enum E`, `export declare ...`, `export import X = ...` | the name | the name |
| `export const a = 1, b = 2`, `export let`, `export var`, `export const { c } = o` | each bound name | the name |
| `export namespace A.B.C {}` | `A` | `A` |
| `export default function f`, `export default class C`, `export default interface I` | `default` | the name |
| `export default x` (an identifier) | `default` | `x` |
| `export default <expression>`, `export default class {}`, `export default function () {}` | `default` | none |
| `export { a }`, `export { a as b }`, `export { a as default }` | `a` / `b` / `default` | `a` |

`type_only` is set for `export type { a }` and a `type` modifier on one
specifier. The span is the local name where the statement writes it (a
declared name, or the `unknown` use of `export { a }` and `export default
a`), or the `default` keyword when there is no local name. A namespace
member's `export`, anything inside `declare global`, `export =`, and `export
as namespace` record nothing; re-exports stay `module_imports`. Which
declaration a local name identifies is resolution.

## Value-position `type` uses (T44)

A `new` target, the class operand of `instanceof`, the operand of a
type-position `typeof`, and a class's `extends` expression are `type` uses (as
in PHP), but each names a value. Their spans are recorded in the scope's
`value_type_uses`, so `typeof Foo` beside an `interface Foo` and a `const Foo`
is looked up among values.

## Global declarations (T44)

A script file (no top-level `import` or `export` statement; a side-effect
`import "./x"` counts) declares globals, and so does `declare global { ... }`.
A global can merge with declarations in other files, including unindexed
library files, so its symbol is in no scope's `declares`: no same-file binding
claims it. Its name is still a local, so it still hides an outer binding.

The body of a string-named ambient module (`declare module "x" { ... }`,
including a module augmentation) is marked `ambient_module`: it also sees the
exports of the module it declares or augments, so the resolver never looks
past it for a name the body does not bind itself.

## Receiver hints (T43, T45)

A member use records its receiver hint; none is a binding by itself:

- `UseHint::This` for `this.m()` inside a named class's methods, accessors,
  field initializers, and static blocks, including arrow functions there. Not
  inside a nested `function` or object-literal method (their own `this`), and
  not in an anonymous or function-local class, which is not a symbol. T45
  records `is_static`: `true` inside a static method or accessor, a static
  field initializer, or a static block, where `this` is the class
  constructor; `false` elsewhere.
- `UseHint::Typed` with origin `parameter` for a receiver that is a parameter
  with one explicit named type (`svc: SurveyService`, `Foo<T>` as `Foo`,
  `ns.Foo`); `variable` for a variable with one; and `property` for
  `this.f` where `f` is a field or constructor parameter property with one
  (static fields for `this` in a static member).
- `UseHint::NewExpr` for a receiver that is a `const` initialized by
  `new C(...)`; an annotation wins over `new`.

T45 adds `name_span` to `Typed` and `NewExpr`: the span of the `type` use the
annotation's type name records (`Foo`, the `Foo` of `Foo<T>` and of
`ns.Foo`), or of the `new` target (`C`, the `C` of `new ns.C()`). That use
sits in the scope where the annotation or `new` is written, which can differ
from the member use's scope, so the resolver resolves the class exactly as
T44 resolves that `type` use.

A union, array, function, or other structural type records no hint; nor does
a `let` or `var` bound by `new` (it can be reassigned), a name bound twice in
its nearest scope, or an import binding. The nearest scope that binds the
name as a value decides.

### Member sides (T45)

The module scope's `member_sides` records, for every class and interface
member symbol of the file, whether it is static: a class method, accessor, or
field declared `static` is; every other class member, every constructor
parameter property, and every interface member is not. A constructor is on
neither side and is not recorded (`x.constructor` names the class, not the
constructor method), and neither is a member of an anonymous or function-local
class, which is not a symbol.

### Receiver bindings (T45)

The resolver's receiver rule (`rivet_index::resolve::rules::ts_receivers`)
binds a call, read, or write through a hinted receiver, always `scoped`:

- the class is the named class enclosing a `this`, or the one class or
  interface a typed or `new` hint's `type` use binds to through the T44
  lexical rules (a type alias, enum, namespace, function, merged declaration,
  package or path-alias import, re-export, or unresolved name gives none; a
  generic type parameter of the same name hides the class);
- the class must itself declare exactly one method or property of the use's
  spelling (case-sensitive, `#` included) on the receiver's side: static for
  a static `this`, instance for every other receiver. A member of unknown
  side blocks the binding. Inheritance is never traversed, and an interface
  receiver binds the interface's own member.

TypeScript uses record no read/write distinction for accessors, so a getter
and a setter sharing a name are two candidates and `this.label` stays
unresolved. Calls on a class name (`SurveyService.create()`,
`Status.Active`) are not bound in v0.1.

## Signatures and doc comments

A signature is the declaration header as written, as for PHP: from the span
start through the start of the body (`{`), or the whole declaration when it has
no body, with whitespace collapsed and a trailing `;` dropped. A `const` or
field bound to an arrow function, function expression, generator function, or
class expression stops at that value's body (`export const double = (n:
number): number =>`). Other bindings, type aliases, properties, and enum
members keep their value (`export const RETRY_LIMIT = 3`). Accessors keep
`get`/`set` (`get label(): string`). Each name of a multi-name `const`
statement gets the shared prefix plus its own declarator
(`export const MAX_COUNT = 10`).

A `/** ... */` comment directly above the span attaches, by the PHP rule: not a
`//` or `/* */` comment, not `/**/`, and not across a blank line or another
statement. Decorators between a method's doc comment and the method are
skipped.

The collapsed container form (spec §16.2, `crate::signature_summary`) is not
rendered for TypeScript yet; it reports `None`.

## Lookup normalization

TypeScript identifiers are case-sensitive for every kind, so `lookup_name`
returns the declared name exactly as written, `#` and `$` included; nothing is
folded, and a use's lookup name is its spelling (`use_lookup_name`). The
generic matcher (`rivet_index::lookup_name_matches`) folds case only for a PHP
declaration (T43), and a use matches a target only in the target's language.
So in a mixed repository `rivet symbol foo` finds a PHP class `Foo` but never
a TypeScript class `Foo`, a PHP class `Foo` and a TypeScript class `Foo` are
two symbols (`rivet symbol Foo` is ambiguous between them), and neither
language's uses appear in the other's `refs`.

## Resolution and exclusion

The resolver's TypeScript rule set (`rivet_index::resolve`, T44, T45) sees
only TypeScript uses and declarations, and PHP's rules only PHP's. Every
lexical binding it makes is `exact` (docs/ADDING-A-LANGUAGE.md "TypeScript
bindings"):

- a named, aliased, default, or `import type` import of a relative module
  binds its `import` use and every unshadowed use of its local name to the
  declaration the module exports under the imported name; a relative
  specifier starts with `./` or `../` or is a bare `.` or `..`, and one that
  names only a directory (a trailing `/`, or a last segment of `.` or `..`)
  names only that directory's `index.ts`, `index.tsx`, or `index.d.ts`
  (T44a);
- `ns.member` after `import * as ns` binds `member` to the module's export of
  that name, a lexical module-qualified name rather than receiver access;
- a use bound lexically, with shadowing and value/type spaces accounted for,
  to one module-local declaration of its own file binds to it.

Every receiver binding is `scoped` ("Receiver bindings (T45)" above).
Re-exports, `export *`, path aliases, packages, `require`, ambiguous or
unindexed modules, globals, merged declarations, and every receiver no hint
binds stay `name_match`, and no TypeScript use gets a receiver class.
Reference-mode exclusion (spec §11.5)
never applies to a TypeScript target or use: PHP's form table would drop real
TypeScript references such as `Outer.f()`, `util.format()`, and `Color.Red`,
and structural typing makes a receiver's class no evidence.

## Parse policy and limits

A tree with any ERROR or MISSING node publishes no facts and yields one
`parse_error` diagnostic (`<kind> at byte <n>`). A tree with more than
`ResourceLimits::max_visited_nodes` nodes publishes no facts and yields one
`resource_limit` diagnostic; the node bound is checked first, as for PHP. A
file that yields more than `ResourceLimits::max_extracted_uses` uses publishes
no facts and yields one `resource_limit` diagnostic (`extracted more than N
uses`).

## Not resolved in v0.1

Per `docs/ADDING-A-LANGUAGE.md` "MVP support boundary": path aliases, package
resolution, re-exports, CommonJS, inheritance, structural typing,
unions/generics, decorators' runtime effects, and computed dynamic names.

## Known limitations on real code (T46)

Measured on Hono v4.13.9 (`tests/real/manifest.toml`; details, method, and
the raw audit in `tests/real/RESULTS.md`). No resolver rule changed for this
measurement; these are the consequences of the rules above, with their
frequency on one real project.

**Grammar parse failures: 8 of 357 eligible files (2.2%).** The pinned
`tree-sitter-typescript` 0.23.2 grammar reports errors, so the parse policy
publishes no facts for these files:

| Cause | Files | Minimal form |
|---|---|---|
| consecutive generic call signatures in an interface or type literal separated only by a newline | 6 (`src/context.ts`, `src/types.ts`, `src/helper/factory/index.ts`, `src/helper/ssg/middleware.ts`, `src/jsx/hooks/index.ts`, `src/utils/body.ts`) | `interface G {` / `<K>(key: K): number` / `<K>(key: K): string` / `}` (a `;` after the first signature parses) |
| `export type * from '...'` (TypeScript 5.0) | 2 (`src/jsx/index.ts`, `src/jsx/dom/index.ts`) | `export type * from './x'` |

Two of the failed files are central: `src/context.ts` declares the `Context`
class, so the project's most common typed receiver (`c: Context`) never binds,
and 299 of the 1,634 named and default imports (18.3%) name a declaration in a
failed module and stay unresolved.

**Named and default imports (1,634):**

| Outcome | Count | Share |
|---|---:|---:|
| bound `exact` | 971 | 59.4% |
| relative, module not indexed (parse failure above) | 299 | 18.3% |
| bare `.` or `..` specifier (not `./` or `../`, so not a relative specifier under the module rule); resolved by T44a, which treats them, and any specifier ending in `/`, `.` or `..`, as a directory whose `index.ts`, `index.tsx`, or `index.d.ts` is the only candidate (not re-measured here) | 140 | 8.6% |
| package or built-in (`vitest`, `node:fs`) | 124 | 7.6% |
| relative, the module re-exports the name (`export { a } from`, `export *`) | 76 | 4.7% |
| relative, the module exports an import binding (`import { a } from './x'; export { a }`) | 20 | 1.2% |
| relative, other (not exported under that name, anonymous default) | 3 | 0.2% |
| relative, no candidate module | 1 | 0.1% |

No import used a path alias: Hono configures no tsconfig `paths`, and its
`@std/...` and `@hono/...` specifiers are packages.

**Member calls through a receiver (20,508):** 607 (3.0%) bind `scoped` through
a receiver hint, 117 (0.6%) bind `exact` as namespace-import members, and
19,784 (96.5%) stay `name_match`. By receiver shape over all 19,784
(mechanical, from the receiver text and recorded hint):

| Receiver shape | Count | Share |
|---|---:|---:|
| call or other expression (`expect(x).toBe()`, `(await f()).text()`) | 8,490 | 42.9% |
| identifier with no hint (a callback parameter `c`, an untyped `const`) | 5,060 | 25.6% |
| `new` hint whose class has no own member of that name, or no class resolved | 2,506 | 12.7% |
| property chain (`c.req.header()`, `this.a.b()`) | 1,909 | 9.6% |
| capitalized identifier with no hint (class name or global object: `Buffer.from()`, `Reflect.set()`) | 960 | 4.9% |
| annotation hint with no own member bound, or no class resolved | 853 | 4.3% |
| annotation with a qualified type name | 5 | 0.03% |
| `this` hint with no own member | 1 | 0.01% |

A seeded sample of 40, classified by reading the source (`tests/real/RESULTS.md`
lists each):

| Why it stays unresolved | Sample | Share |
|---|---:|---:|
| call or expression result as receiver (other) | 21 | 52.5% |
| untyped receiver | 8 | 20.0% |
| inherited member (`new Hono()` from `src/hono.ts`; `get`, `use`, `request` are declared on the base class in `src/hono-base.ts`) | 4 | 10.0% |
| re-export (the class is imported through `src/index.ts`, which exports its import binding of `Hono`) | 2 | 5.0% |
| class-name receiver (global `Buffer`, `Reflect`) | 2 | 5.0% |
| chained property | 2 | 5.0% |
| non-class annotation (`times: string[]`) (other) | 1 | 2.5% |
| qualified type annotation | 0 | 0% |
| package import | 0 | 0% |
| path alias | 0 | 0% |

The five qualified annotations are all `NodeJS.WritableStream`, a global
namespace from `@types/node` that is not in the repository. Neither
conservative gap the T45 review found (`ns.Inner.Foo`, deeper than one
namespace-import member, and `Local.Foo` through a same-file `namespace
Local`) occurs in Hono.

**Precision.** A seeded sample of 40 `exact` and 40 `scoped` bindings, each
checked against the source, found no wrong target (with 0 of 40, the one-sided
95% upper bound on the error rate is 7.2% per tier). `scoped` bindings on an
interface receiver (`let router: Router<string>; router.add()`) name the
interface's own method, as the receiver rule states, not the implementation
that runs.
