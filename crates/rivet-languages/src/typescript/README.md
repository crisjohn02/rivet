# TypeScript adapter

Extraction for `.ts`, `.d.ts`, and `.tsx` files using the pinned
`tree-sitter-typescript` grammars: `.ts` and `.d.ts` with the TypeScript
grammar, `.tsx` with the TSX grammar (`crate::language_for_path`). One adapter
serves both; every node kind it reads has the same name and fields in each.
The adapter produces owned facts only; cross-file resolution belongs to the
generic resolver (`docs/ADDING-A-LANGUAGE.md` "Adapter contract").

T42 extracts named definitions. Uses, imports, and scopes are T43. Until then
`LanguageId::has_extractor` stays `false` for TypeScript and `rivet_parser`
does not dispatch to this adapter, so indexing is unchanged: definitions
without uses would make `refs` report misleadingly empty results.

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
returns the declared name exactly as written, `#` included; nothing is folded.
The generic matcher compares a `type_alias` case-sensitively
(`rivet_index::lookup_name_matches`). The other kinds, which PHP shares, still
use PHP's rules there until TypeScript symbols are indexed (T43, T45).

## Parse policy and limits

A tree with any ERROR or MISSING node publishes no facts and yields one
`parse_error` diagnostic (`<kind> at byte <n>`). A tree with more than
`ResourceLimits::max_visited_nodes` nodes publishes no facts and yields one
`resource_limit` diagnostic; the node bound is checked first, as for PHP. The
use bound cannot bite until T43 extracts uses.

## Not resolved in v0.1

Per `docs/ADDING-A-LANGUAGE.md` "MVP support boundary": path aliases, package
resolution, re-exports, CommonJS, inheritance, structural typing,
unions/generics, decorators' runtime effects, and computed dynamic names.
