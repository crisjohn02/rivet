# rivet's patched tree-sitter-typescript

This directory is a vendored copy of
[tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript)
at tag `v0.23.2`, commit `f975a621f4e7f532fe322e13c4f79495e0a7b2e7`
(2024-11-10), with two grammar patches. It was added by GR1 (user-approved
2026-09-24) because 0.23.2 is still the latest release, upstream has had no
grammar commits since, and the pinned grammar rejected 8 of Hono's 357
eligible files (`tests/real/RESULTS.md`, T46). Upstream's MIT license is kept
in `LICENSE`.

The crate is `tree-sitter-typescript` version `0.23.2-rivet.1`, with the same
public API as upstream (`LANGUAGE_TYPESCRIPT`, `LANGUAGE_TSX`, the node-types
and query constants), so no Rust call site changed. rivet's extractor
fingerprint names it (`ts=0.23.2-rivet.1`), so every store re-extracts
TypeScript once.

## What is here

| Path | From upstream | Changed |
|---|---|---|
| `common/define-grammar.js`, `common/scanner.h` | yes | patches 1 and 2 below |
| `typescript/grammar.js`, `tsx/grammar.js`, `*/src/scanner.c` | yes | no |
| `typescript/src/`, `tsx/src/` (`parser.c`, `grammar.json`, `node-types.json`, `tree_sitter/*.h`) | generated | regenerated; only `parser.c` and `grammar.json` differ from upstream |
| `bindings/rust/lib.rs`, `bindings/rust/build.rs` | yes | no |
| `queries/*.scm` | yes | no (the Rust binding embeds them) |
| `test/corpus/{declarations,expressions,functions,types}.txt` | yes | no |
| `test/corpus/rivet.txt` | no | rivet's corpus cases for both patches |
| `tree-sitter.json` | yes | no (the CLI reads it to find the grammars and the corpus) |
| `Cargo.toml` | rewritten | version `0.23.2-rivet.1`, `publish = false`, no `include`/`readme`, dev-dependency `tree-sitter` from the workspace (0.27.0) instead of 0.24 |
| `regenerate.sh`, `RIVET-PATCHES.md` | no | added |

Left out: bindings for other languages (C, Go, Node, Python, Swift), the
Node and build files (`package.json`, `binding.gyp`, Makefiles, CMake,
`common/common.mak`), CI and fuzz configuration, examples, and the upstream
README. The upstream `src/` files of the tag are byte-identical to the
crates.io `tree-sitter-typescript` 0.23.2 package rivet pinned before.

## Patch 1: generic call signatures separated only by a newline

```ts
interface G {
  <K>(k: K): number
  <K>(k: K): string
}
```

**Root cause.** Members of an object type or interface body are separated by
`,`, `;`, or an automatic semicolon, which the external scanner inserts at a
line break unless the next line continues the current one. Its rule treats
`<` on the next line as a continuation, always (`a\n< b` is a comparison), so
no separator is produced and the second signature cannot start. `(` and `[`
already have a type-context exception (a signature `(k: K): number` on the
next line works), but `<` did not. TypeScript itself never continues a type
with type arguments across a line break (`parseTypeArgumentsOfTypeReference`
requires no preceding line break), so between members a `<` on a new line
always starts a new call signature. The only place in a member list where a
`<` on a new line does continue is directly after a member's name, where it
opens a method signature's type parameters (`a\n<K>(k: K): K` is the method
`a`, for both TypeScript and upstream).

**Fix.** Two external tokens are appended to the grammar's externals (and to
the scanner's `TokenType` enum in the same order):

- `_call_signature_automatic_semicolon`, added as a third member separator of
  `object_type` (which interface bodies alias), beside `,` and `_semicolon`.
  It is valid only between object-type members.
- `__type_member_name`, an optional token in `method_signature` right after
  the name and optional `?`. The scanner never produces it; its validity marks
  the parse states right after a member's name.

In `scan_automatic_semicolon`, after a line break, `<` produces
`_call_signature_automatic_semicolon` when that token is valid and
`__type_member_name` is not; otherwise it returns false exactly as before.
Nothing else in the scanner changes, and neither token is visible in a tree
(both are hidden and zero-width). During error recovery every external is
valid, so the marker is valid and `<` behaves as upstream.

Consequences, all pinned by `test/corpus/rivet.txt`:

- generic call signatures on new lines parse after members of every kind
  (with or without a return type, after a comment, after `]`, `>`, or a
  construct signature), in interfaces and in type literals;
- a generic call signature after a type that could take type arguments
  (`a: Foo` / `<K>(k: K): K`) is a new member, as TypeScript reads it;
- right after a member name (`a`, `b?`, `[Symbol.iterator]`, `'c'`) the next
  line's `<` still opens the method signature's type parameters;
- outside object types nothing changes: `a\n<b>(c)` is still one call,
  `return\n<T>(x: T) => x` and TSX `return\n<p />` still return the value, a
  class method's type parameters may still start on the next line, and a
  variable annotation `let x: Map\n<string, number> = y` still takes the type
  arguments (upstream's reading, although TypeScript would not).

The one class of input whose error-free tree changes: inside an object type
or interface body, a type whose type arguments start on the next line
(`headers: Record\n<string, string>`). Upstream read `Record<string, string>`;
the patch ends the member at the line break, as TypeScript does, so such
input is now a parse error, or, when a signature follows
(`): Promise\n<T>(t: T): T`), parses as TypeScript reads it. TypeScript
rejects the former too.

## Patch 2: `export type * from`

```ts
export type * from './x'
export type * as ns from './x'
```

**Root cause.** TypeScript 5.0 syntax the 0.23.2 grammar predates: its
TypeScript `export_statement` adds `export type { ... } [from]` to the
JavaScript forms but has no type-only form of `export *`.

**Fix.** One alternative in `export_statement`:
`seq('export', 'type', choice('*', $.namespace_export), $._from_clause, $._semicolon)`.
The tree is the `export * from` tree with an anonymous `type` token: the `*`
token or a `namespace_export` node, and the `source` field. `from` is required,
so `export type *` alone, `export type * as ns;`, and a clause mixed in are
still errors. No new node type (`node-types.json` is unchanged).

## Regeneration

- **CLI:** `tree-sitter-cli` **0.24.4**, installed with
  `npm install --prefix <scratch>/cli tree-sitter-cli@0.24.4` (the npm package
  downloads the prebuilt binary; it reports
  `tree-sitter 0.24.4 (fc8c1863e2e5724a0c40bb6e6cfc8631bfe5908b)`).
  Upstream's `package.json` asks for `^0.24.4`, and 0.24.4 was published the
  same day as the tag's "chore: regenerate" commit. Regenerating the
  unmodified tag with it and `tree-sitter-javascript` **0.23.1** (the base
  grammar `define-grammar.js` requires; `^0.23.1` upstream) reproduces
  upstream's committed `typescript/src` and `tsx/src` byte for byte, so the
  patched output differs from upstream only by the patches. It emits ABI 14
  (`LANGUAGE_VERSION 14`), which the pinned `tree-sitter` 0.27.0 runtime
  accepts.
- **Command:** `vendor/tree-sitter-typescript/regenerate.sh` copies the
  grammar sources into `target/grammar-regen/`, installs the two pinned npm
  packages there, runs `tree-sitter generate` in `typescript/` and `tsx/`,
  runs `tree-sitter test`, and copies the generated `src/` back.
  `regenerate.sh --check` does the same but only compares, and fails if the
  committed C differs from what the grammar sources generate.
- **Result:** `typescript/src/parser.c` (5,870 to 6,333 parse states) and
  `tsx/src/parser.c` (5,986 to 6,449), and both `grammar.json` files, change.
  `node-types.json` and `tree_sitter/*.h` are identical to upstream.

The rivet build never runs the CLI: the generated C is committed, and
`bindings/rust/build.rs` only compiles it with the C compiler.

## Verification (GR1)

- `tree-sitter test`: all 112 upstream corpus cases pass, and so do the 12
  cases of `test/corpus/rivet.txt` (three of them `:error` cases). Against
  unpatched upstream, the four rivet cases that pin unchanged behaviour and
  the three `:error` cases pass, and the five that need a patch fail.
- Tree identity against the crates.io `tree-sitter-typescript` 0.23.2, with a
  throwaway harness that dumps every node (kind, field, named, missing, extra,
  byte range) plus the S-expression: every file upstream parsed without error
  gives an identical tree. The counts are in `tests/real/RESULTS.md`, "GR1
  addendum".
