# Pilot 01: result and decision

The numerical tables are in [report.md](report.md), generated from the private
per-run data. This file is the written result the generated report leaves open:
what the pilot shows, what it does not, and the next change. It is a pilot. It
supports no savings claim, and every confirmatory gate is not evaluated.

## What was run

Five read-only navigation tasks on the private `timesheet` Laravel application,
the corpus's pilot partition, one per category: locate, callers, tests, trace,
and dependencies. Each ran once in each arm on Claude Code 2.1.280 with
`claude-opus-5-5` at high effort:

- **Arm B:** Claude Code's read and search tools plus the benchmark's
  efficient-search prompt.
- **Arm C:** the same, plus rivet and exactly the managed snippet.

Every run executed in a macOS sandbox that denied reads of the gold answers,
the rest of the corpus, the user's originals, rivet's own repository and other
runs, and arm B could not execute rivet at all. The user's plugins, MCP servers,
skills and hooks were not loaded. A separate smoke run validated the harness
first.

## What it shows

| Measure | B | C | C against B |
|---|---|---|---|
| Tasks passed | 5 of 5 | 5 of 5 | same |
| Mean input tokens per run | 69,540 | 55,777 | ratio 0.80, 20% fewer |
| Mean cost per run, Claude Code's estimate | $0.139 | $0.121 | 13% less |
| Mean tool calls per run | 5.6 | 4.4 | 21% fewer |
| Mean wall time per run | 24.0 s | 25.3 s | 5% longer |

Per task, C used fewer input tokens on four tasks and more on one, and the
split matches rivet use exactly:

- **On the four tasks where C called rivet, it used 14% to 30% fewer input
  tokens.** The largest saving, 30%, came on the tests task, the costliest.
- **On the one task where C did not call rivet, it used 27% more.** That was the
  locate task, answerable with a single text search, which T38 predicted would
  favour arm B. C searched a little more and carried the snippet's instructions
  in its context without using them.

Adoption was 4 of 5 runs, with 9 rivet invocations: `refs` 5 times, `symbol`
twice and `context` twice, and no rivet errors. A direct probe confirmed the
snippet reached arm C's agent, so the one non-adoption was the agent's choice,
not a delivery failure.

**Every rivet call used the plain text form. None used `--json`**, although the
snippet says to add `--json` for structured results. The agent read about 40 KB
of rivet text output across the pilot. T37 measured the text form at 18% to 32%
of the JSON's size for `symbol` and `refs` and 51% to 69% for `context`, so the
same answers as JSON would have cost substantially more.

## What it does not show

- **Nothing statistical.** One trial per arm per task gives no estimate of
  variance, so any single difference may be noise. The benchmark doc calls this
  pilot a usability check only.
- **The ratio is a ratio of task-weighted means,** which the costliest task
  dominates. The mean of the five per-task ratios is 0.90, a 10% reduction.
  Both summarise the same five pairs; neither is robust at this size.
- **The tasks are not independent or held out.** Three share one area of the
  code, and they were written after rivet was built, by the same project.
- **One model, one harness, one small codebase.** The pilot says nothing about
  other agents, other codebases, or TypeScript, which rivet does not yet index.
- **Costs are Claude Code's estimates** under a subscription login, not billed
  amounts.
- **Isolation was flag-based, not a fresh home directory.** No leak was
  observed, but a fresh environment per run was not achievable without an API
  key.

## Decision

T40 chooses between fix and retest, proceed to TypeScript, or re-scope. The
recommendation is **fix and retest**, not yet proceed:

1. **Align the snippet with observed behaviour before any confirmatory run.**
   Agents read the text form and never used `--json`, yet the snippet
   recommends `--json`. Either make the text form the documented default for
   agents, or show that `--json` helps, before freezing the benchmark
   treatment. The snippet carries a recorded hash, so this is a deliberate
   treatment change for the user to approve.
2. **Measure variance.** Rerun these five tasks with at least three trials per
   arm, on a new explicit budget, to learn whether a 10% to 20% difference
   survives repetition and to size the confirmatory study.
3. **Improve Laravel resolution before measuring it further.** T37 found that
   for a common Laravel method name, 920 of 922 references are name matches.
   rivet still helped on the pilot's tasks, but its references for idiomatic
   Laravel code are mostly unresolved. That is the most likely limit on the
   savings.

Proceeding to TypeScript, T41 onward behind Gate G, is not yet justified: the
PHP effect is suggestive but unmeasured for variance.

Spend: the pilot, the smoke run and one delivery probe cost $1.45 in total,
Claude Code's estimate, against the $25 approved.
