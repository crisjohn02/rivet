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

Signatures are the declaration header as written, from the modifiers through
the body `{` or the final `;`, with whitespace collapsed and a trailing `;`
dropped. A `/** ... */` docblock immediately above (no blank line) attaches.
`signature_summary` renders a container as its signature line, one body-free
line per direct member, and `}`; method bodies collapse to `{ … }`.

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
