# Pilot 03: retest after LR1 and LR2

The numerical tables are in [report.md](report.md), generated from the private
per-run data. This is still a pilot. It supports no savings claim, and every
confirmatory gate is not evaluated.

## What changed from pilot 02

- **rivet build:** the build now includes T36d (LR1) and LR2.
  - **LR1 (T36d):** `refs` on a base class lists its subclasses.
  - **LR2:** reference mode excludes same-name uses that evidence rules out. It
    reports how many it excluded, in the text line
    `N name matches excluded by evidence (see --mode candidates)`.
- **Seed:** the seed is new.
- **Everything else is the same as pilot 02:** the five tasks, three trials per
  arm, harness version, model, effort, tool policy, prompt, snippet and sandbox.

## What it shows

| Measure | Pilot 02 | Pilot 03 |
|---|---|---|
| Runs passed, B / C | 15 / 15 | 15 / 15 |
| Mean input tokens per run, B | 67,406 | 89,651 |
| Mean input tokens per run, C | 56,552 | 59,339 |
| Ratio of task-weighted means, C against B | 0.84 | 0.66 |
| Mean of per-task ratios | 0.86 | 0.84 |
| Estimated cost, C against B | 0.93 | 0.85 |
| Paired trials where C used fewer tokens | 10 of 15 | 11 of 15 |

Per task, as the ratio of three-trial means, C against B:

| Task | Category | Pilot 02 | Pilot 03 | Pilot 03 per-trial |
|---|---|---|---|---|
| p1 | locate | 0.91 | 0.95 | 0.99, 0.93, 0.92 |
| p2 | callers | 0.92 | 1.20 | 1.18, 1.43, 1.04 |
| p3 | tests | 0.85 | 0.58 | 0.34, 0.71, 0.76 |
| p4 | trace | 0.73 | 0.51 | 0.35, 0.57, 0.63 |
| p5 | dependencies | 0.88 | 0.97 | 1.21, 0.82, 0.93 |

## Reading

- **The larger pooled saving comes from arm B, not from rivet.**
  - Arm B has no rivet, and its configuration was identical in both studies. It
    used a third more tokens in pilot 03, almost all on p3 and p4, with the
    same number of tool calls and turns.
  - Arm C's mean barely moved, from 56,552 to 59,339.
  - The mean of per-task ratios, which weights each task equally, is
    unchanged at 0.84 against 0.86.
  - So pilot 03 does not show that LR1 or LR2 lowered the agent's token use.
- **LR2 made the callers task worse, in all three trials.**
  - On p2, C now used more tokens than B in every trial: ratio 1.20, up from
    0.92.
  - In each run:
    - `refs` returned the two true callers plus the exclusion line.
    - The agent followed the pointer and re-ran with `--mode candidates`.
    - It read the source lines of the excluded uses to confirm them. These are
      the two text fallbacks counted per run.
  - The excluded uses were correctly excluded: they were static calls of the
    same name on unrelated classes.
  - Every answer was correct.
  - The cost comes from how the exclusion is announced, not from the exclusion
    itself.
- **Adoption is unchanged.** C called rivet in 12 of 15 runs and never with
  `--json`. It again never called rivet on the locate task, p1.
- **Spend:** $4.10 for this study. $9.35 in total across the pilot studies,
  against the $25 approval.

## Next change

- **Fix the exclusion line.**
  - The line should stay, because it keeps the result honest: the list omits
    same-name uses.
  - It should give the reason per category, for example
    `4 same-name uses ruled out (unrelated receiver class: 4)`.
  - It should stop pointing the agent at `--mode candidates` as a next step.
  - Retest the callers task with the fix before any further pilot.
- **Treat pilot-to-pilot differences with caution.** Arm B's variation across
  days is as large as any effect measured here. Compare only arms run within
  the same study.
