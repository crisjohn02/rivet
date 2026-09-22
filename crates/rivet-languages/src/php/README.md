# PHP adapter

Extraction for `.php` files using the pinned `tree-sitter-php` grammar. This
adapter produces owned facts only; cross-file resolution happens in the generic
resolver (`docs/ADDING-A-LANGUAGE.md` "Adapter contract").

## Supported syntax (v0.1)

Named definitions, with their contract `kind`:

| PHP construct | `kind` | Notes |
|---|---|---|
| `namespace` | `module` | Not a parent of classes; used to build qualified names. |
| `class`, `abstract class`, `final class` | `class` | |
| `interface` | `interface` | |
| `trait` | `class` | Traits are class-kind in v0.1. |
| `enum` (pure and backed) | `enum` | |
| `enum` case | `const` | The contract's `kind` enum has no enum-case value; a case is addressable and case-sensitive like a class constant. |
| `function` | `function` | |
| `method` | `method` | Includes abstract and interface methods with no body. |
| `property`, including `static`, `readonly`, and multi-name (`$a, $b`) declarations | `property` | The name keeps its leading `$`. |
| promoted constructor property (`__construct(private int $n)`) | `property` | A real property declared in a parameter list. |
| `const`, including multi-name and top-level `const` | `const` | |

Qualified names use the native PHP form: namespace separator `\`, member
separator `::` (`App\Services\SurveyService::launch`). A file without a
namespace uses the bare name. Members point at their enclosing
class/interface/trait/enum through `parent_index`; namespaces are not parents.

Lookup normalization (`lookup_name`): class, interface, trait, enum, namespace,
function, and method names are lowercased (PHP is case-insensitive there).
Property and constant names — including enum cases — keep their exact spelling
(PHP is case-sensitive there).

A use's lookup name (AF4) is its normalized short name (`use_short_name`): the
last segment of a qualified spelling, without a leading `$`. Call, type, and
class or function import uses fold ASCII case; property and constant accesses,
`use const` aliases, and `unknown` uses (a bare identifier whose kind is not
known) keep their case. Matching a use to a declaration folds by the
*declaration's* kind (`rivet_index::lookup_name_matches`), comparing properties
without the `$` on either side; `rivet symbol` short-name lookup applies the
same comparison.

Signatures are the declaration header as written, from the modifiers through
the body `{` or the final `;`, with whitespace collapsed and a trailing `;`
dropped. A `/** ... */` docblock immediately above (no blank line) attaches.
`signature_summary` renders a container as its signature line, one body-free
line per direct member, and `}`; method bodies collapse to `{ … }`.

## Receiver hints (AF3)

- A typed receiver records whether its type was declared on a **parameter**
  (including a promoted constructor parameter used as a local) or a
  **property** (`$this->name`). PHP checks a parameter type only on entry, so
  the resolver trusts it only when the variable is never rebound in its scope;
  a property type is checked on every assignment and survives reassignment.
- Only a single class type (`A`) or a nullable one (`?A`) is a typed receiver.
  A union, intersection, or DNF type (`A|B`, `A&B`, `(A&B)|null`, also
  `A|null`) and a by-reference parameter (`A &$x`) record none.
- `$y = &$x`, a by-reference `foreach` over `$x`, and `[&$x]` record a
  rebinding of `$x`.
- Constructor arguments are walked and recorded as call arguments of
  `Class::__construct` (`new static`, `new parent`, dynamic classes, and
  anonymous classes have an unknown receiver).
- By-reference parameter positions of each function and method are read from
  the parse tree, so attributes, default values, and comments cannot mislead
  them.
- A global scope records its explicit call sites and any `goto`; a function
  body records the names it can rebind through `global` or a literal
  `$GLOBALS` key, and whether it can rebind a global it does not name.

## Explicit class references (AF4)

- `Foo::make()`, `Foo::BAR`, `Foo::$prop`, and `Foo::class` record `Foo` as a
  `type` use, which binds `exact` through the normal class path. The member
  carries a `named_class` hint and binds `scoped` only when `Foo` declares it
  directly. `::class` records no member use.
- `self::` keeps its `self_or_static` hint and binds `scoped`; `static::` and
  `parent::` bind nothing.
- An `instanceof` class operand is a `type` use. A `catch` type already was.
- An anonymous class body is walked. Its uses attach to the nearest named
  container, but `$this`, `self`, `static`, and `new self` inside it record no
  receiver evidence, since they name the anonymous class. Its own typed
  properties still type `$this->prop` receivers.
- An enum case's own name is not a use.

## Not resolved in v0.1

Per `docs/ADDING-A-LANGUAGE.md` "MVP support boundary", these stay unresolved
rather than becoming `exact`: container bindings, magic methods, trait
composition, inheritance traversal, `static::`, variable function/member names,
and docblock-inferred types.

## Out of scope for extraction

Anonymous classes and closures are not addressable symbols (spec §10.1), so
their members are not emitted; their uses attach to the nearest named container
or the file scope. Runtime-generated names and literal strings are not
semantic references.

## Facts and invalidation

The extractor fingerprint (`EXTRACTOR_FINGERPRINT` in
`crates/rivet-languages/src/lib.rs`) includes the pinned grammar versions and a
`fact-schema` integer. Bump the integer whenever the shape or meaning of a
persisted fact changes, so the next refresh reparses stored files instead of
reusing stale facts.
