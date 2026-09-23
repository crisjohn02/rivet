# CV1: a shorter coverage line that names its diagnostics instead of pointing at `--json`

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint table.
- `orchestration/review-notes/token-economy-audit-2026-09-24.md` (TE1): lever 5 and the
  "Other observations".
- `docs/OUTPUT-CONTRACT.md`: the coverage and diagnostics section (around "files_seen =") and
  every place that quotes the human coverage line.
- `crates/rivet-cli/src/human.rs`, the coverage-line renderer and its tests.
- The goldens under `crates/rivet-cli/tests/golden/` that contain `coverage:`.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model.

## Why

The human coverage line is printed on every call where coverage is not clean. It reads like
`coverage: incomplete; 1219 of 2815 files indexed; skipped 1595 unsupported, 1 parse_error; 1
diagnostics (listed with --json)` and costs about 60 tokens. In the held-out runs, its
`(listed with --json)` pointer prompted every one of the 5 `--json` calls, each about 2,800
tokens, only to read the diagnostics. This is the same failure EX1 fixed for the exclusion line.

## Build

- **Keep every fact** the line states today:
  - whether coverage is complete;
  - files indexed out of files seen;
  - each non-zero skip count by reason;
  - the diagnostics count.
- **Drop the pointer.** Remove the `--json` pointer entirely.
- **Name the diagnostics inline.** List the files of the first 2 diagnostics as `path (code)`, in
  the contract's diagnostic sort order, then `+N more` when more exist. Paths are
  repository-relative, as in the JSON.
  - Don't repeat the ordinary `unsupported` files. They are counted, not listed, as today.
- **Shorter wording.** Write the new line's grammar into OUTPUT-CONTRACT exactly, with an example.
  One acceptable shape:
  `coverage incomplete: 1219/2815 files indexed; skipped 1595 unsupported, 1 parse_error; diagnostics: app/X.php (parse_error)`.
  Aim for about half the current tokens without losing a fact.
- **When the line appears.** It still appears only when today's rule prints it.
- **JSON is unchanged.**
- **Do not touch the managed snippet.** Its hash must stay unchanged.

## Tests

- Update the existing coverage-line tests and goldens, and list every changed golden.
- Add these cases:
  - one diagnostic;
  - three diagnostics, which print `+1 more`;
  - diagnostics with no skip counts other than `unsupported`;
  - complete coverage with diagnostics;
  - a non-UTF-8 path diagnostic, whose escaped form stays escaped.
- `--json` output must be byte-identical to HEAD for the same queries.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
python3 benchmark/runner/test_runner.py
```

Then add and tick a CV1 row in `docs/TASKS.md` and update the checkpoint.

## Report

- the old and new line for each tested case;
- every changed golden;
- the check results;
- any contradiction between the documents and the code.
