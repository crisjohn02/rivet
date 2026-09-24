# TypeScript adapter

Extraction for `.ts`, `.d.ts`, and `.tsx` files using the pinned
`tree-sitter-typescript` grammars: `.ts` and `.d.ts` with the TypeScript
grammar, `.tsx` with the TSX grammar (`crate::language_for_path`). One adapter
serves both; every node kind it reads has the same name and fields in each.
The adapter produces owned facts only; cross-file resolution belongs to the
generic resolver (`docs/ADDING-A-LANGUAGE.md` "Adapter contract").

T42 extracts named definitions; T43 adds uses, lexical scopes, and imports,
turns `LanguageId::has_extractor` on for TypeScript and TSX, and dispatches
both to this adapter in `rivet_parser`. No TypeScript use is bound until T44
(direct relative imports) and T45 (receiver hints) add TypeScript binding
rules: every TypeScript reference is `name_match`.

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
lists the symbols among a scope's locals; class and interface members are in
no scope's `declares`, because a bare name never reaches them (a constructor
parameter property is a parameter inside the constructor). A use's scope
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
specifier; `import type` is an ordinary import binding, which T44 resolves.
The specifier is kept exactly as written, relative (`./util`), path alias
(`@/util`), or package (`lodash`); nothing here decides which module it names.
`require(...)` in an expression is a `call` of `require`, not an import, and
`import "./side-effect"` binds nothing and is not recorded.

## Receiver hints (T43, for T45)

A member use records the hint T45 will need; none is a binding:

- `UseHint::This` for `this.m()` inside a named class's methods, accessors,
  field initializers, and static blocks, including arrow functions there. Not
  inside a nested `function` or object-literal method (their own `this`), and
  not in an anonymous or function-local class, which is not a symbol.
- `UseHint::Typed` with origin `parameter` for a receiver that is a parameter
  with one explicit named type (`svc: SurveyService`, `Foo<T>` as `Foo`,
  `ns.Foo`); `variable` for a variable with one; and `property` for
  `this.f` where `f` is a field or constructor parameter property with one
  (static fields for `this` in a static member).
- `UseHint::NewExpr` for a receiver that is a `const` initialized by
  `new C(...)`; an annotation wins over `new`.

A union, array, function, or other structural type records no hint; nor does
a `let` or `var` bound by `new` (it can be reassigned), a name bound twice in
its nearest scope, or an import binding. The nearest scope that binds the
name as a value decides.

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

Every TypeScript use stays unresolved until T44/T45: the resolver's rule table
(`rivet_index::resolve`) has no TypeScript entry, and PHP's rules see only
PHP uses and declarations. Reference-mode exclusion (spec §11.5) never
applies to a TypeScript target or use: PHP's form table would drop real
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
