# Authored TypeScript/TSX fixture

A small hand-written TypeScript project used as the declaration, use, and
binding gold source for the TypeScript tasks (T42-T45). T41 wrote it before
any TypeScript extractor existed. All files are UTF-8 with LF line endings;
`src/report.ts` has one non-ASCII character (`café`) before a use on the same
line, so byte and character columns differ there. The fixture root stands in
for a repository root: canonical IDs in the gold start at `src/`.

Expected spans live in `../../../gold/typescript-authored.toml`, whose header
states the span, naming, and task-tag conventions. Run `python3
tests/gold/check_gold.py` from the repository root to check them, and
`cargo test -p rivet-languages --test typescript_gold` to check them against
the pinned grammar.

## Files

| File | Covers |
|---|---|
| `src/models.ts` | interfaces with property and method signatures, interface declaration merging, a regular and a `const` enum, an exported namespace with a nested namespace, a `module` block; the undecided type alias and generic type parameter next to a generic interface, class, and function |
| `src/services/survey.ts` | an abstract class; a subclass with static, annotated, and ES private (`#`) fields, a constructor with parameter properties, a static method with a `new` expression, same-name `launch`, `this.launch()`, an inherited `this.log()` call, and a getter/setter pair |
| `src/util.ts` | overload signatures and their implementation, consts bound to an arrow and to a function expression, plain and multi-name consts, a default export naming a local function |
| `src/report.ts` | named, aliased, renamed-default, and namespace imports; the second `launch`; receivers typed by a field, a parameter (also with `?.`), a variable, and a preceding `new`; an unannotated receiver; top-level calls; template-literal text and interpolation; a local that shadows an import |
| `src/anonymous.ts` | an anonymous default-exported class, a function-expression IIFE, an arrow IIFE, and callbacks inside and outside a named function |
| `src/barrel.ts` | a re-export with an alias and an `export *` |
| `src/unresolved.ts` | imports that must stay unresolved: through the barrel (re-export and `export *`), a path alias, a package, `require`, `import type`, and an ambiguous relative path |
| `src/pick.ts`, `src/pick/index.ts` | both candidates for `./pick`, so the ordered candidate rule finds two modules |
| `src/components/Button.tsx` | a props interface, a function component, a const arrow component, lowercase intrinsic elements |
| `src/components/App.tsx` | component names as uses, intrinsic elements, literal JSX text with identifier-like words, a fragment, attribute names and string values, JSX expression containers with a callback, a closing tag, and an anonymous default-exported function component |
| `src/types.d.ts` | ambient declarations: a function, a const, a class, a namespace with members, a top-level interface, and a string-named ambient module |
| `src/broken.ts` | a missing `)`: the only file with an error node |
| `src/legacy.js` | JavaScript, which is not inferred from the TypeScript adapter |
| `src/esm.mts` | `.mts`, which the TypeScript grammar parses cleanly but v0.1 does not claim |
| `tsconfig.json` | the `@/*` path alias that `src/unresolved.ts` imports through; v0.1 never reads it |

## Cases

Each case is marked in the source with a `// gold: (x)` comment and tagged on
its gold entries.

- **a** — same-name `launch`: `Survey.launch` (an interface method signature),
  `SurveyService.launch`, and `ReportService.launch`.
- **b** — direct relative imports: named, aliased (`launchAll as runAll`), and a
  default import under another name (`makeLabel` for `helper`), whose
  spellings differ from their targets.
- **c** — top-level calls with no containing symbol.
- **d** — `this.launch()` inside `SurveyService`, scoped through `this`.
- **e** — `created.launch()` after `const created = new SurveyService(...)`.
- **f** — receivers typed by an explicit annotation: a field
  (`this.service.launch()`), a parameter (`svc.launch()`, `svc?.launch()`),
  and a variable (`annotated.launch()`).
- **g** — `x.launch()` with `x` unannotated: stays `name_match`.
- **h** — literal text is not code: a string literal, a comment, and template
  literal text; template interpolations are.
- **i** — `this.log()` names a method inherited from `BaseService`: inheritance
  is not traversed, so it stays `name_match`.
- **j** — interfaces, declaration merging (`Settings#1`, `Settings#2`), enums,
  namespaces, and module blocks.
- **k** — class members: abstract, protected, static, annotated and ES private
  fields, the constructor, and parameter properties.
- **l** — functions: overloads and consts bound to arrow and function
  expressions (one `function` symbol each).
- **m** — plain named consts, including two names in one statement.
- **n** — a default export that names a local declaration.
- **o** — a namespace import used as `util.format(...)`.
- **p** — a local `double` that shadows the imported `double`.
- **q** — anonymous containers: uses inside attach to the nearest named
  container or to none.
- **r** — TSX declarations: a props interface and two components.
- **s** — TSX uses: capitalized components are uses; intrinsic elements, JSX
  text, and string attribute values are not.
- **t** — ambient declarations in a `.d.ts` file.
- **u** — constructs the docs do not settle (recorded as `[[undecided]]`).
- **v** — the parse failure.
- **w** — unsupported extensions.
- **x** — import forms that stay unresolved.
- **y** — an ambiguous relative module path.
- **z** — same-file lexical bindings.

`export default class { ... }` is deliberately not followed by a statement
that starts with `(`: the pinned grammar reads that pair as one call
expression, with no error node.
