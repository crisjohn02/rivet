# Pilot 02: variance retest

The numerical tables are in [report.md](report.md), generated from the private
per-run data. This is still a pilot. It supports no savings claim, and every
confirmatory gate is not evaluated.

## What changed from pilot 01

- **Trials:** three per arm per task instead of one, so there are 30 runs.
  The seed is new, so blocks ran in a different order.
- **Snippet (SN1):** the snippet now says the text output is the default for
  the agent and `--json` is for scripts.
- **Everything else is the same as pilot 01:** the five tasks, harness, model,
  effort, tools, prompt and sandbox.

## What it shows

| Measure | B | C | C against B |
|---|---|---|---|
| Runs passed | 15 of 15 | 15 of 15 | same |
| Mean input tokens per run | 67,406 | 56,552 | ratio 0.84, 16% fewer |
| Mean of per-task ratios | | | 0.86 |
| Mean estimated cost per run | $0.131 | $0.122 | 7% less |
| Paired trials where C used fewer tokens | | | 10 of 15 |

Per task, as the ratio of three-trial means, C against B:

| Task | Category | Ratio | Per-trial ratios |
|---|---|---|---|
| p1 | locate | 0.91 | 0.65, 1.09, 1.10 |
| p2 | callers | 0.92 | 0.98, 1.40, 0.55 |
| p3 | tests | 0.85 | 0.99, 0.78, 0.79 |
| p4 | trace | 0.73 | 0.62, 1.01, 0.61 |
| p5 | dependencies | 0.88 | 0.62, 1.19, 0.84 |

- **Direction:** arm C's mean was lower on all five tasks. Pilot 01 had C
  higher on one task.
- **Noise:** within one arm, the spread across three trials of a task is 4% to
  26% of its mean. That is about the size of the effect, so no single task's
  ratio can be read on its own.
- **Adoption:** C called rivet in 12 of 15 runs (`refs` 13 times, `context` 6,
  `symbol` 5). It never used `--json`, as the new snippet intends.
- **Where C skipped rivet:** all three p1 runs. p1 is a simple locate task,
  which the agent finished in about three calls with plain search.
- **Spend:** $3.80 in total, against a $9 cap for this study.

## Reading

- **Reproduced:** pilot 01's direction held with three trials: about 15% fewer
  input tokens and 7% to 13% less cost, at equal success.
- **Not established:** the per-trial noise is large, and 5 tasks × 3 trials
  cannot put a tight interval on the size of the effect. A savings claim still
  needs the confirmatory design on the held-out project.
- **Next:** the Laravel resolution work (LR1, LR2), then a third pilot on the
  same design. That pilot measures whether better `refs` precision changes
  C's token use.
