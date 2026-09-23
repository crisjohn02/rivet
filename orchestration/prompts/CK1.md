# CK1: decide `set_f1` pass/fail in exact arithmetic

Read `AGENTS.md` first and follow it. Then read only:

- `benchmark/runner/TASK-FORMAT.md`, "Answers and check types".
- `benchmark/runner/checks.py`, `evaluate` and the task loader's `f1_threshold` handling.
- `benchmark/runner/test_runner.py`, the existing check tests.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model.

## Why

`set_f1` computes F1 in binary floating point and compares it with `>=`. A mathematically exact
F1 of 0.8 can then land on either side of the threshold:

- 6 correct plus 2 extra, against 7 gold items, evaluates to 0.7999999999999999 and **fails**;
- 4 correct plus 1 extra, against 5 gold items, evaluates to 0.8000000000000002 and passes.

The rule is "passes when F1 ≥ `f1_threshold`", so both must pass. The held-out study will freeze
this checker's hash, so fix it now.

## Build

- **Exact decision.** Decide pass/fail exactly, with `fractions.Fraction`:
  - F1 = 2·|A∩G| / (|A| + |G|), which is algebraically the same as the harmonic mean of
    precision and recall, and 0 when |A∩G| is 0.
  - Build the threshold with `Fraction(str(threshold))` from the manifest's decimal, so 0.8 means
    exactly 4/5. Pass when `f1_exact >= threshold_exact`.
- **Reported values.** Keep reporting `precision`, `recall` and `f1` as floats, computed from the
  exact fractions, so the output shape does not change.
- **Unchanged.** `exact_symbol`, `accepted_path`, answer parsing and every shape rule stay as
  they are.
- **Docs.** Update TASK-FORMAT.md to say the decision is exact and give the formula.

## Tests

Add each case against a threshold of 0.8:

- 7 gold items, 6 correct plus 2 extra: exactly 0.8, passes. It fails before this change; show
  that in your report.
- 5 gold items, 4 correct plus 1 extra: passes.
- 3 gold items, 2 correct plus 0 extra: exactly 0.8, passes.
- 7 gold items, 6 correct plus 3 extra: 0.75, fails.
- 5 gold items, 3 correct plus 0 extra: 0.75, fails.
- a threshold of 1.0 with a perfect answer: passes; one extra item: fails.
- F1 0 when nothing overlaps, and for an empty answer, unchanged.

Existing tests must pass unchanged, except any test that asserted the float artefact.

## Checks

```
python3 benchmark/runner/test_runner.py
cargo fmt --all -- --check
cargo test --workspace
python3 tests/gold/check_gold.py
```

Then add and tick a CK1 row in `docs/TASKS.md`.

## Report

- the diff summary;
- each new test;
- the check results;
- a list of any committed pilot result whose pass value would differ under the new rule. Compute
  this from the pilot fixtures in the repository only. The private run records are the reviewer's
  job.
