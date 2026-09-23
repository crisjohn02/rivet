# Pilot 04: the callers task after EX1

The numerical tables are in [report.md](report.md), generated from the private
per-run data. This is a targeted retest of one task. It supports no savings
claim, and every confirmatory gate is not evaluated.

## What was run

- **Task:** p2 (callers) only, three trials per arm, both arms, so arm B is a
  control within the same study.
- **rivet build:** includes EX1. The exclusion line now states its reason, as in
  `4 same-name uses ruled out (unrelated receiver class: 4)`, and no longer
  points at `--mode candidates`.
- **Everything else is the same as pilot 03.**

## What it shows

| Measure, p2 | Pilot 02 | Pilot 03 | Pilot 04 |
|---|---|---|---|
| Runs passed | 6 of 6 | 6 of 6 | 6 of 6 |
| Arm C mean input tokens | 35,618 | 39,392 | 28,415 |
| Arm C per-trial tokens | 35,172, 45,257, 26,426 | 37,608, 40,674, 39,893 | 25,310, 33,982, 25,954 |
| C against B, ratio of means | 0.92 | 1.20 | 0.62 |
| C against B, per trial | 0.98, 1.40, 0.55 | 1.18, 1.43, 1.04 | 0.80, 1.34, 0.33 |
| Text fallbacks per C run | 1, 1, 1 | 2, 2, 2 | 1, 1, 1 |

## Reading

- **The audit is gone.**
  - In pilot 03, every C run followed the old line's pointer to candidates mode
    and then read the excluded sites.
  - In pilot 04, no run did that in response to the line.
  - One run (trial 2) chose `--mode candidates` as its very first call, before it
    could see any exclusion line. It was also that trial's most expensive C run.
  - The remaining fallback in each run is the `rg` the agent chains onto its
    first `refs` call, as in pilot 02.
- **C got cheaper in absolute terms:** 28,415 mean tokens against 39,392 in
  pilot 03 and 35,618 in pilot 02.
- **Read the 0.62 ratio with care.** B's third trial used 79,824 tokens, more
  than twice its other two, and that single run pulls the ratio of means down.
  The per-trial ratios, 0.80, 1.34 and 0.33, span the same wide range seen in
  earlier pilots.
- **Spend:** $0.54 for this study, $9.89 in total across the pilot studies,
  against the $25 approval.
