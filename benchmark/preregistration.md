# Preregistration: held-out study heldout-01

> **Status: frozen.** This file is committed before any run of any held-out task. It must not
> change after that. The study manifest `benchmark/studies/heldout-01/study.toml` pins this
> file's SHA-256, and the runner refuses to start if the file differs or is uncommitted.

## Question and scope

Does adding rivet and its managed snippet (arm C) reduce the input tokens a coding agent uses,
compared with the same agent that has only an efficient text-search prompt (arm B), without an
unacceptable loss of task success?

- **Tasks:** read-only code-navigation tasks in PHP, on one private Laravel application.
- **Harness:** one host and one model: Claude Code headless with `claude-opus-5-5`.
- **What a result covers:** only this project, this harness, this model and this task mix. It
  says nothing about other languages, projects, harnesses or models.

## Frozen artifacts

| Item | Value |
|---|---|
| rivet commit | `9838fc51bcb1f13d36e0fcd076258f82913d90af` (crates tree `e8743698e7d24860849d1be85eb108a553e79e27`) |
| rivet binary SHA-256 | `89e3f10f81df936da1cc089e19b82ea93aca2ce649d1416f05bc9118bb4e062a` (release build of that commit) |
| Managed snippet SHA-256 | `1819530610c1418d4b0d9bce84363504d99f5029d628558185ede35a82d3b5f7` |
| Harness | Claude Code `2.1.280`, headless, the user's own login (not an API key, so `--bare` is unavailable) |
| Model and settings | `claude-opus-5-5`, `{"effortLevel":"high"}` |
| Flags, tools, arm prompts, sandbox | exactly as in the study manifest, hashed into its `study_manifest_sha256` |
| Runner and analysis | analysis version `t48a-1`; runner at rivet commit `e1282ae47e0b`. Full SHA-256s of the seven runner scripts are listed under "Runner script hashes" below. Short forms: `checks.py` `befcae0182ecfb66…`, `extract.py` `d41207769fdfa1e7…`, `report.py` `1236fe21e35b7f74…`, `run_study.py` `4310e6f3e6bf5061…`, `sandbox.py` `6acd2d8f238c3615…`, `study.py` `e2659d51c1d27432…`, `transcript.py` `f81cad99c7784682…` |
| Tasks | `tasks_sha256` `f96062c7949580eb7770180a14ab57e68ab5701c61eac10bf416a17d664eb819` over 20 task directories and 80 files (definition in TASK-FORMAT.md); task texts and gold sealed, outside the repository |
| Corpus | `fluent` at `fc96ad75ea658e60df52dbbb8543077b0b931d14`, proprietary, not published |

### Runner script hashes

- `benchmark/runner/checks.py`: `befcae0182ecfb669cc6bbe25bbe957ea51291776c584981f65574ff50e9bab8`
- `benchmark/runner/extract.py`: `d41207769fdfa1e7805792c95aed3736391fecfc86be49334e0f94a2e96e57d1`
- `benchmark/runner/report.py`: `1236fe21e35b7f747c206fcbe0e243f5c2b1dc57d1ad1b7499026d2a05c4160b`
- `benchmark/runner/run_study.py`: `4310e6f3e6bf5061ce4fcca26ceb7d2285b526e976cab66a81780e0be9c41273`
- `benchmark/runner/sandbox.py`: `6acd2d8f238c3615198d0c96dd5bf48489ca607301d9ce4325e24e0597f808eb`
- `benchmark/runner/study.py`: `e2659d51c1d27432129f73dcac473ab70887edc4f39903eae93ad8dbd28de315`
- `benchmark/runner/transcript.py`: `f81cad99c7784682a8c26914579f6d6bcb75c3ce7e371fdd811da1dd2f04a66a`

## Design

- **Arms:**
  - B is the primary control: Claude Code's read and search tools plus the efficient-search
    prompt verbatim from BENCHMARK.md.
  - C is the treatment: B plus rivet and exactly the managed snippet.
- **Tasks:** 20 held-out tasks, h01–h20, 4 in each of five categories:

  | Category | Check | Limits (USD / turns / s) |
  |---|---|---|
  | locate | `exact_symbol` | 1.00 / 15 / 600 |
  | callers | `set_f1`, F1 ≥ 0.8 | 1.50 / 25 / 900 |
  | tests | `set_f1`, F1 ≥ 0.8 | 2.00 / 30 / 1200 |
  | trace | `accepted_path` | 1.50 / 25 / 900 |
  | dependencies | `set_f1`, F1 ≥ 0.8 | 1.25 / 20 / 900 |

- **Target selection:** seeded (`random.Random(20260927)` over the sorted universe of first-party
  methods and functions), with fixed eligibility rules per category and every examined candidate
  logged privately. Rejecting an eligible candidate was allowed only for unverifiable gold, an
  unresolvable second answer, or a duplicate code path, never for expected tool performance.
  - The universe held 4,784 declarations.
  - 51 candidates were examined: 20 assigned, 28 ineligible for every open category, and 3
    rejected from `tests` as not verifiable by reading.
  - No candidate was rejected for a second answer or a duplicate path, and none was replaced in
    batch 2.
- **Expected favour, stated per task before any run:** text search 7, rivet 3, neutral 10.
  Of the neutral ones, 6 lean slightly towards rivet and 1 towards text search.
- **Trials:** 5 per arm per task, so 100 task/trial blocks and 200 scheduled runs.
- **Order:** blocks run trial-major, in task order h01–h20: trial 1 of every task, then trial 2,
  and so on. The arm order within each block is randomized from the study seed `20260928` (a
  function of seed, task and trial). Arms are interleaved over time. An early stop therefore costs
  whole trials across all tasks, never whole tasks.
- **Retry rule:** `rerun_block_once`. An infrastructure or provider failure reruns the whole block
  once, and both transcripts are kept.
- **Workspaces:** every run gets a fresh copy of the pinned corpus with a one-commit git history
  and no `.rivet/` cache, so arm C's cold index is part of its cost. Repository agent instructions
  are removed identically in both arms.
- **Isolation:** every run executes under `sandbox-exec`, which denies reads of the gold, the
  partition notes, the rest of the corpus, the user's originals, rivet's repository and
  worktrees, other runs, and the orchestrator's session files. Arm B cannot execute rivet.
- **Budget:** a $60 cap across the whole study, including retries, enforced before each block.
  - Projected spend is $26–52. That is the pilots' mean of about $0.13 per run, scaled up to 2×
    for a corpus about 5× larger.
  - The guard reserves the costliest block ($4.50) before starting it, so the study can run until
    about $55.50 has been charged.

## Endpoints

1. **Primary efficiency metric:** the ratio of equally task-weighted means of total input
   tokens, C against B, over all included blocks.
   - Total input tokens are provider-reported input plus cache reads plus cache creation, summed
     over every model request in a run.
   - Failed runs count with their consumed tokens.
2. **Quality gate:** the task success difference C − B, in percentage points, with equal task
   weighting. It passes if the lower one-sided 95% bound is above −5.
3. **Efficiency gate:** passes if the upper one-sided 95% bound on the primary ratio is below
   1.00.
   - The point estimate is reported as measured, with no threshold on it.
   - This replaces the draft protocol's gate (point estimate ≤ 0.70 and upper bound < 1.00). See
     Deviations.
4. **Outcome:**
   - `invalid` if a validity rule fails;
   - else `fail_quality` if the quality gate fails;
   - else `inconclusive` if the efficiency gate fails;
   - else `pass`.
5. **Secondary, point estimates only:**
   - the successful-run-only token ratio;
   - estimated cost, tool calls and wall-clock ratios;
   - per-task and per-category results;
   - adoption and fallback rates.

## Analysis

- **Bootstrap:** a paired cluster bootstrap that resamples whole tasks with replacement within
  language strata (one stratum here, PHP), keeping each task's blocks for both arms together.
  - `bootstrap_replicates = 10000`, `analysis_seed = 20260927`.
  - The upper bound is the replicate ratio at sorted index `ceil(0.95·R) − 1`.
  - The lower bound is the replicate success difference at sorted index `ceil(0.05·R) − 1`.
  - A replicate whose B denominator is zero has a ratio of +∞.
  - The full procedure is in TASK-FORMAT.md at the frozen analysis version.
- **Inclusion:**
  - Timeouts, crashes, invalid or missing answers and exhausted limits are failures and are
    included with their tokens.
  - A block is excluded from both arms if, after its one allowed rerun, either arm lacks an
    includable run: an infrastructure failure, missing provider usage, or contamination (an arm-B
    run that executed rivet).
- **Validity:** the outcome is `invalid` and no gate is evaluated if any of these holds:
  - a task has fewer than 3 included blocks;
  - more than 10% of scheduled blocks are excluded;
  - a dry-run row is present;
  - the analysis version differs.
- **Early stop:** if the budget cap stops the study early, the analysis runs on the completed
  blocks under the same validity rules.
- **After the results:** endpoints do not change once results are seen, and any further
  analysis is labeled exploratory.

## Sample size rationale

- **Design size:** 20 tasks × 5 trials × 2 arms is BENCHMARK.md's minimum design for the primary
  comparison. It is not a power calculation.
- **Pilot variance:** on the pilot partition, pilots 02 and 03 (5 tasks × 3 trials) gave C/B
  ratios of 0.84 and 0.66, with a mean of per-task ratios of 0.86 and 0.84.
- **Effect size:** the pilots suggest a reduction of roughly 15%.
- **Spread:** within-arm trial-to-trial spread was 4–26% of a task's mean, and arm B's own mean
  moved by a third between studies.
- **What the design can show:** it is not established that 20 tasks are enough to exclude "no
  reduction" at that effect size. If they are not, the preregistered outcome is `inconclusive`.
  The design will not bound the reduction tightly either way.

## Deviations from BENCHMARK.md

1. **PHP only.** The protocol asks for a PHP and a TypeScript/TSX project. rivet has no
   TypeScript adapter yet (T41 onward).
2. **One project.** The protocol asks for at least two. The second corpus project is the pilot
   partition, and its TypeScript is held out for later.
3. **Arm A dropped.** A is a secondary control. Only B and C are run, and A comparisons are not
   reported.
4. **Navigation only.** The harness runs read-only tasks. Edit categories (fix a failing test,
   rename, small feature) are not included.
5. **Efficiency gate changed after the pilots.** The user chose this before any held-out task
   was written or run, after seeing pilot estimates near 0.84 on the pilot partition. The
   original 30% gate would most likely have failed at that effect size.
6. **Task author.** The held-out tasks were written by an agent working inside the project that
   develops rivet. Seeded selection and the fixed rejection reasons limit, but do not remove, the
   chance that its knowledge of rivet shaped the tasks.
7. **Harness auth.** Runs use the user's Claude Code login, not an API key, so `--bare` isn't
   available. The runner isolates settings sources, MCP, slash commands and session persistence
   with flags, and removes repository instructions. Any leakage of the user's global instructions
   is a limitation.
8. **Cost.** Currency cost is Claude Code's per-run estimate, not billing data.
   - **Harness version:** every run records its harness version. If any run reports a version
     other than `2.1.280`, it is kept, and the report lists it as a deviation.

9. **Task-writing notes, from the private authoring records:**
   - The dependency code (`vendor/`) is absent from the corpus copy, so reach through framework
     internals can't be verified by reading. That caused the 3 `tests` rejections, and it tilts
     the `tests` targets toward direct calls and HTTP requests.
   - Five prompts carry one task-specific sentence that rules out a second reasonable answer.
   - Batch 2's trace prompts say to judge calls as written; batch 1's do not.
   - Two task pairs share part of a call path, so they are not fully independent.
   - One private `notes.md` holds a wrong boundary example. It is corrected in the private README
     and affects no gold, prompt or check.
10. **Checker fix before freezing.** CK1 made `set_f1` decide pass or fail in exact arithmetic.
    Before it, an exact F1 of 0.8 could fail through floating-point rounding. No pilot run's
    outcome changes: none of the 48 pilot `set_f1` runs is near the threshold.

## Prior contact with the held-out project

- **T35:** pinned the project and wrote a 28-entry hand-verified resolver gold sample, which
  checks rivet's correctness and is not an agent task. It still verifies unchanged on the frozen
  build.
- **Resolver changes:** none was motivated by that sample.
- **T37:** measured local CLI timings on the project. There were no model calls.
- **PF1:** the refresh speed-up for queries on an unchanged tree was motivated by a no-change
  refresh time measured on this project in T35. It is a performance change and alters no output.
- **Pilots:** pilots 01–04 and every change they motivated (SN1, T36d, LR2, EX1) used only the
  pilot partition.

## Publication

The results will be published under `benchmark/results/heldout-01/`:

- the generated report, summary, per-task aggregates and RESULT.md;
- the outcome, whatever it is;
- total spend and these deviations.

Task texts, gold, transcripts and per-run records stay private because the corpus is
proprietary.
