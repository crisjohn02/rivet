# RX1: attribute rivet exits inside chained shell commands

Read `AGENTS.md` first and follow it. Then read only:

- `orchestration/review-notes/token-economy-audit-2026-09-24.md` (TE1): the Errors section and
  the last bullet of Other observations.
- `benchmark/runner/transcript.py` (`rivet_invocations`, `rivet_calls`, `rivet_metrics`,
  `_segments`, `fallbacks`) and `benchmark/runner/extract.py`.
- `benchmark/runner/TASK-FORMAT.md`, "Heuristic extractors".
- `benchmark/runner/test_runner.py`, the transcript tests.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model.

## Why

TE1 found 13 rivet `ambiguous_symbol` exits in the held-out transcripts, but the extractor
attributed only 5: the ones that stood alone in their tool call. The other 8 ran inside chained
commands such as `rivet symbol X; rg ...` or `rivet refs X | head`. The tool result mixes several
programs' output, and only the shell's last exit status is visible.

This affects secondary metrics only (`rivet_errors_by_exit`, fallbacks), never the primary
metric. Future studies need it right.

## Build

- **Recognise errors from their text.** Attribute a rivet error from its human error output when
  it appears anywhere in a tool result, not only from the tool call's exit status.
  - rivet's human error text has a documented, stable form: see OUTPUT-CONTRACT's error section
    and `crates/rivet-cli/src/human.rs`.
  - Recognise each error code by that form. Map the code to its exit code using the contract's
    table.
- **One count per error.** Never count an error twice: once from the exit status and once from
  the text.
- **Unattributable errors.** When a chained command's rivet segment cannot be matched to an error
  text, keep today's `unattributed` bucket.
- **Tests:** use synthetic transcripts:
  - a standalone error;
  - an error followed by `;` then `rg`;
  - an error piped into `head`;
  - two rivet calls in one command, one failing;
  - an `rg` match that merely contains the word `error`, which must not count.
- **Docs:** update TASK-FORMAT.md "Heuristic extractors" to describe the rule.
- **Pilot outputs:** for the committed fixtures, pilot `report.md`, `summary.json` and
  `per-task.csv` must stay byte-identical, or change only in fields that come from error
  attribution. List any that change.

## Checks

```
python3 benchmark/runner/test_runner.py
cargo fmt --all -- --check
python3 tests/gold/check_gold.py
```

Then add and tick an RX1 row in `docs/TASKS.md`.

## Report

- the matching rule;
- the tests;
- the check results;
- any fixture output that changed.
