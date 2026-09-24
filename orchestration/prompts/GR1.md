# GR1: vendor a minimally patched tree-sitter-typescript 0.23.2

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint.
- `docs/BUILDING.md` (toolchain, C compiler, grammar pins) and the workspace `Cargo.toml`
  (the `=0.23.2` pin and its comment about `tree-sitter-language`).
- `crates/rivet-parser/` (grammar dispatch) and `crates/rivet-languages/src/lib.rs`
  (`EXTRACTOR_FINGERPRINT` and its history).
- `crates/rivet-languages/src/typescript/README.md`, "Known limitations on real code (T46)", and
  `tests/real/RESULTS.md`, the parse-failure table.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model. Network is allowed
to fetch the upstream grammar at tag `v0.23.2`
(`https://github.com/tree-sitter/tree-sitter-typescript`), to install a pinned `tree-sitter` CLI,
and to run `tests/real/fetch.sh`.

## Why

The pinned grammar, 0.23.2, is still the latest release, and upstream has had no grammar commits
since. On Hono it rejects 8 of 357 files (T46), including `src/context.ts` and `src/types.ts`, for
two constructs:

1. consecutive generic call signatures in an interface or object type separated only by a newline:

   ```ts
   interface G {
     <K>(k: K): number
     <K>(k: K): string
   }
   ```

   (the same with `;` parses, and so do non-generic call signatures separated by newlines);
2. `export type * from './x'` and `export type * as ns from './x'` (TypeScript 5.0).

The user approved vendoring a minimal patched fork, pinned in the repository.

## Decisions (orchestrator, final)

1. **Layout.** Vendor the grammar as a path crate at `vendor/tree-sitter-typescript/`, derived from
   the upstream `v0.23.2` tag. Keep upstream's LICENSE (MIT) and add a `RIVET-PATCHES.md` that
   lists every change from upstream, the upstream commit, the exact `tree-sitter` CLI version used to
   regenerate, and the command. Include `grammar.js`/`common/define-grammar.js`, `scanner.c`,
   the generated `src/` files for both `typescript` and `tsx`, and the Rust binding; leave out
   bindings for other languages, the test corpus beyond what you need, and CI files. Point the
   workspace dependency at the path crate. Keep the crate name and public API
   (`LANGUAGE_TYPESCRIPT`, `LANGUAGE_TSX`) so no Rust call site changes. Set its version to
   `0.23.2-rivet.1` (or the nearest valid form) and use that in the fingerprint.
2. **Minimal patch.** Change only what the two constructs need, in `grammar.js`/`define-grammar.js`
   or the external scanner. Find the real cause (for construct 1 it is likely the automatic-semicolon
   logic before `<`); do not paper over it with a broad rule. Every other valid input must parse to
   the **same tree** as before: prove it.
3. **Regeneration.** Use the same `tree-sitter` CLI version upstream used for 0.23.2 if it runs on
   this machine (the generated `parser.c` header names it); otherwise the nearest version whose ABI
   the pinned runtime (`tree-sitter =0.27.0`) accepts, and say why. Record the version and install
   method (for example `npm install -g tree-sitter-cli@<v>` or `cargo install --locked
   tree-sitter-cli --version <v>`). Nothing in the rivet build may require the CLI: the generated C
   is committed.
4. **Fingerprint.** Bump `EXTRACTOR_FINGERPRINT`'s TypeScript component to name the patched grammar
   (`ts=0.23.2-rivet.1`), with a history note, so every store re-extracts TypeScript. PHP is
   untouched.
5. **Docs.** BUILDING (the vendored grammar, how to regenerate it), the TypeScript README
   known-limitations section (the parse-failure table as resolved by GR1, with the new Hono
   numbers), and a dated GR1 addendum in `tests/real/RESULTS.md` and a `[project.rivet_index_gr1]`
   block in `tests/real/manifest.toml`; do not overwrite T46's numbers.

## Tests (adversarial required)

- **Tree identity.** Parse every `.ts`/`.tsx`/`.d.ts` file in the authored fixture and in Hono with
  both the upstream crate (from the cargo registry, in a throwaway test or script not committed to
  the build) and the patched grammar, and compare S-expressions. Every file upstream parsed without
  error must give an identical tree. Report the count; any difference is a FAIL to explain.
- **Upstream corpus.** Run the upstream grammar test corpus (`tree-sitter test`) against the patched
  grammar and report the result; add corpus cases for both constructs, including negatives
  (a generic call signature after a type that could take type arguments, `a\n<b>(c)` in an
  expression context must keep its current parse, `export type *` without `from` is an error).
- **Rust tests.** Minimal forms of both constructs parse without error and extract the expected
  symbols/imports (the interface's name; `export type * from` recorded as a re-export
  `ModuleImport` like `export *`, never followed).
- **Real code.** `tests/real/check_real.py` passes; Hono's parse failures drop from 8 (report any
  that remain and why).
- PHP output byte-identical apart from the digest; TypeScript fixture output identical apart from
  the digest unless a fixture file used a construct that now parses (say which).

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p rivet-languages --no-default-features --all-targets -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
python3 benchmark/runner/test_runner.py
python3 tests/real/test_check_real.py
cargo build --release && tests/real/fetch.sh && python3 tests/real/check_real.py
```

Also confirm a clean build from an empty `CARGO_TARGET_DIR` works with no `tree-sitter` CLI on
PATH.

Then add a ticked GR1 row in `docs/TASKS.md` and update the checkpoint.

## Report

- the root cause of each construct and the exact patch (diff of `grammar.js`/scanner);
- CLI version and why; vendored size;
- tree-identity count, corpus results, Hono before/after (parse failures, bindings by tier);
- goldens changed; check results; contradictions.
