# SY1: `symbol` call lists default to resolved tiers, and report what they hide

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint table.
- `orchestration/review-notes/token-economy-audit-2026-09-24.md` (TE1), sections 2, 5 and 7.
- `docs/OUTPUT-CONTRACT.md`, the `symbol` section and the reference-object section.
- The spec section that defines `symbol` output: search `rivet-agent-native-codebase-cli-spec.md`
  for `called_by`.
- `crates/rivet-cli/src/symbol.rs`, `crates/rivet-cli/src/references.rs` (`Selection::Contained`,
  pagination), and the `symbol` human renderer.
- The `symbol` goldens under `crates/rivet-cli/tests/golden/`.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model.

## Why

TE1 measured `symbol` output in the held-out runs.

- **Name-only rows carry most of the output.** Rows marked `?` were about 53% of `symbol`
  output tokens.
- **None of them was used.** Of 882 such rows, none became an item of any final answer.
- **Agents already filtered them.** The only list-shaping they did was piping `symbol` through a
  filter that removed `?` rows.

The estimated saving is about 1,200 tokens per run. This is an estimate, not evidence.

## Build

1. **New default.** The default `--min-resolution` for `symbol`'s `calls` and `called_by` becomes
   `scoped`, so the lists keep `exact` and `scoped` rows.
   - `--min-resolution name_match` restores today's output exactly.
   - The default applies to text and to `--json` alike.
   - An explicit `--min-resolution` always wins.
   - `refs` and `context` are **unchanged**.
2. **Honest counts.** Each list reports how many name-only rows the tier filter hid.
   - In JSON, add `hidden_name_match` to each list object, next to `total`, `truncated` and
     `next_offset`. It is always present: 0 when nothing is hidden, and 0 under an explicit
     `--min-resolution name_match`.
   - It counts rows removed only by the tier filter, before pagination, and does not depend on
     `--limit` or `--offset`.
   - `total` keeps meaning the rows after filtering.
   - Document the field in OUTPUT-CONTRACT, including its ordering and determinism.
   - In human output, when a list's count is non-zero, add it to that list's heading line as
     `(+N name-only not listed)`. Add no command or flag pointer; this is the lesson from EX1.
   - A list with 0 shown and N hidden must still print its heading with the count. It must never
     look like there are no calls.
3. **Format defect.** In human output, a receiver that spans several source lines is printed across
   those lines, and it widens every row's padding. Collapse every whitespace run inside a
   displayed receiver to a single space.
   - JSON keeps the receiver text unchanged.
   - Say which choice you made in the contract.
4. **Spec and help.**
   - Update the spec's `symbol` output text and `OUTPUT-CONTRACT.md` for the new default and the
     new field.
   - Update `rivet symbol --help` so it states the default tier and how to include name-only rows.
   - Do not touch the managed snippet. A later task rewrites it; its hash must stay
     `1819530610c1418d4b0d9bce84363504d99f5029d628558185ede35a82d3b5f7`.

## Tests

- **Default and restore:**
  - with the default, a fixture symbol with both resolved and name-only callers lists only the
    resolved ones, and `hidden_name_match` has the right count in both lists;
  - `--min-resolution name_match` gives output byte-identical to a HEAD build, apart from the new
    field, which is 0.
- **Hidden counts:**
  - a list whose only rows are name-only shows total 0, the hidden count, and the heading with
    `(+N name-only not listed)`;
  - the hidden count is page-independent with `--limit 1 --offset 1`.
- **Explicit tier:** `--min-resolution exact` hides `scoped` and `name_match` rows. Decide whether
  `hidden_name_match` counts only name-only rows, as its name says, and document it.
- **Receivers:** a multi-line receiver renders on one line in text and is unchanged in JSON.
- **Unchanged commands:** `refs`, `context` and `symbol --signature-only` produce byte-identical
  output to HEAD.
- **Determinism:** output is identical across a `--force` refresh.
- **Goldens:** update the goldens that change, and list each one with the reason.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
python3 benchmark/runner/test_runner.py
```

Then add and tick an SY1 row in `docs/TASKS.md` and update the checkpoint.

## Report

- the exact new heading formats;
- the JSON field;
- every golden changed;
- the check results;
- any contradiction between the documents and the code. Report it; do not silently resolve it.
