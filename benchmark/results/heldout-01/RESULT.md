# Held-out study heldout-01: result

The numerical tables are in [report.md](report.md), generated from the private per-run data by
the preregistered analysis. The preregistration is
[benchmark/preregistration.md](../../preregistration.md). This file gives the written reading.
It does not change any computed number.

## Outcome: inconclusive

| Endpoint | Preregistered rule | Result | Gate |
|---|---|---|---|
| Primary: mean input tokens, C/B, equal task weighting | report as measured | **0.960** (B 65,897, C 63,266; 4.0% fewer) | — |
| Efficiency gate | upper one-sided 95% bound on C/B < 1.00 | upper bound **1.011** | **not met** |
| Quality gate | lower one-sided 95% bound on success C − B > −5 pp | difference +0.0 pp, lower bound +0.0 pp | met |

- **Scale:** 200 of 200 scheduled runs completed, and every run in both arms passed.
- **Exclusions:** none. No block was excluded, no run was contaminated, no isolation or sandbox
  check failed, and there was no infrastructure failure or rerun.
- **Consistency:** every run used Claude Code `2.1.280` and `claude-opus-5-5`.
- **Spend:** $25.67 of the $60 cap.

On this project, harness, model and task mix, adding rivet did not measurably reduce the agent's
input tokens. The point estimate is a 4% reduction, and the data are consistent with no
reduction at all. There is no supported public savings claim.

## Secondary results (point estimates, not gates)

| Measure, C against B | Ratio |
|---|---|
| Estimated cost | 0.995 |
| Tool calls | 0.789 (21% fewer) |
| Wall-clock time | 1.075 (7.5% longer) |
| Successful-run-only input tokens | 0.960 (every run succeeded) |
| Tasks where C's mean was lower | 10 of 20 |
| Paired blocks where C used fewer tokens | 45 of 100 |

- **Per category** (mean of task ratios):
  - trace 0.94
  - callers 0.95
  - tests 0.96
  - locate 1.00
  - dependencies 1.07
- **Per task:** ratios range from 0.75 to 1.16.
- **Adoption:**
  - Arm C called rivet in 79 of 100 runs: `symbol` 68 times, `refs` 64 and `context` 19.
  - It never used `--json`.
  - rivet exited with `ambiguous_symbol` 5 times, and one other rivet error could not be
    attributed to a command.
  - The extractor counted 38 text-search fallbacks after a rivet call.

## Reading

- **The pilots did not carry over.** On the pilot project, pilots 02 and 03 gave a mean of
  per-task ratios of 0.86 and 0.84. Here it is 0.98, and the ratio of task-weighted means is
  0.96.
  - The snippet, resolver and output changes made during the pilots (SN1, T36d, LR2, EX1) were
    all motivated by the pilot project's runs.
  - This study cannot tell whether the smaller effect comes from that tuning, from the held-out
    project's code shape, or from its task mix. Its expected-favour tally leaned towards text
    search: 7 tasks against 3.
- **Fewer tool calls, not fewer tokens.** Arm C made 21% fewer tool calls but used about the same
  number of tokens and slightly more wall time.
  - So rivet replaced several searches with fewer, larger results rather than shrinking what
    the agent read.
  - This is an observation, not an identified cause.
- **A strong baseline.** Arm B's efficient-search prompt alone does well on these tasks. By the
  protocol's own interpretation guidance, that weakens the case for the tool on read-only
  navigation of this kind.
- **Honesty held.** No arm-C answer was wrong, and success was identical.

## Disclosures

- **Interim look.** At about 100 of 200 runs, the user asked how token usage looked. The
  orchestrator computed an interim ratio over 57 complete blocks: 0.987, with cost 1.009. It was
  recorded privately at that time. Nothing was changed: design, endpoints, analysis, tasks and
  rivet all stayed the same, and the study ran to completion as preregistered.
- **Deviations.** Every deviation from BENCHMARK.md is listed in the preregistration, including
  PHP only, one project, arm A dropped, navigation-only tasks and the efficiency gate set after
  the pilots. No new deviation arose during the run.
- **Cost figures.** Cost is Claude Code's per-run estimate, not billing data.

## What this means for next steps

- **These tasks are spent.** The 20 held-out tasks have now been run. Any rivet change motivated
  by these results must be confirmed on a new, untouched held-out set, not on these tasks again.
- **Exploratory work.** Breaking down where arm C's tokens go (rivet output sizes against the
  reads they replaced, and the `ambiguous_symbol` exits) is legitimate. It must be labeled
  exploratory, and it is not evidence for a claim.
