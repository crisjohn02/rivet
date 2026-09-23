# EX1: state the exclusion reason instead of pointing at candidates mode

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint table and the LR2 row.
- `docs/OUTPUT-CONTRACT.md`, the `by_exclusion` paragraph in the `refs` section.
- `rivet-agent-native-codebase-cli-spec.md` §11.5, only if it quotes the
  human-output line.
- `crates/rivet-cli/src/refs.rs`, the human renderer around the LR2 comment.
- `crates/rivet-cli/tests/evidence_exclusion.rs`, the test asserting the line.
- `benchmark/results/pilot-03/RESULT.md`, "Reading" and "Next change".

Work in this worktree only. Do not commit; the reviewer commits.

## Why

In pilot 03, on the callers task, every arm-C run did the same thing. It read
`4 name matches excluded by evidence (see --mode candidates)`, re-ran `refs`
with `--mode candidates`, then read the excluded sites to check them. The
exclusions were correct, and the audit made C cost more than the arm without
rivet. The line must still say that same-name uses were left out, because that
keeps the output honest. It should state why, and stop presenting candidate
mode as the next step.

## Build

Replace the human-output line in `refs` text output with exactly this form:

```text
N same-name use(s) ruled out (unrelated receiver class: a; form cannot reference a <kind>: b)
```

- `use` when N is 1, `uses` otherwise.
- **Categories:** list only the non-zero ones, in this fixed order: unrelated
  receiver class first, then form. Separate them with `; `.
- **`<kind>`:** the target's kind word as the human renderer already prints
  it in the header line, such as `method`, `function`, `property` or `class`.
  Reuse that word; do not add a new mapping. If an article other than `a` is
  needed (for example `an interface`), handle `a`/`an` by the first letter of
  the word.
- **Examples:**
  - `4 same-name uses ruled out (unrelated receiver class: 4)`
  - `1 same-name use ruled out (form cannot reference a method: 1)`
  - `3 same-name uses ruled out (unrelated receiver class: 2; form cannot reference a method: 1)`
- **Not in the line:** no mention of `--mode candidates`.
- **Unchanged:**
  - The line is still printed only when the sum is non-zero, in the same
    position.
  - JSON output (`by_exclusion`) does not change at all.
  - `--mode candidates` output does not change.
  - `context`, `symbol` and the managed snippet are not touched. The snippet
    hash must stay `1819530610c1418d4b0d9bce84363504d99f5029d628558185ede35a82d3b5f7`.

Update the OUTPUT-CONTRACT paragraph to give the new line's grammar. It should
also say that `--mode candidates` lists every same-name use, including those
ruled out, as a statement of fact rather than an instruction. Update spec §11.5
only if it quotes the old line.

## Tests

- Update the existing assertion to the new text.
- Add cases for:
  - receiver-only, form-only, and both categories;
  - singular `1 same-name use`;
  - a non-method target, so the kind word is not hard-coded (a function target
    with a `$x->f()` use gives `form cannot reference a function: 1`);
  - no line when both counts are zero;
  - `refs --json` byte-identical to a build of HEAD for one query with
    exclusions.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
```

Then add and tick an EX1 row in `docs/TASKS.md` and update the checkpoint table.

## Report

- the exact line for each tested case;
- files changed;
- check results;
- any contradiction between the documents and the code.
