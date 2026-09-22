# Benchmark Runbook

> **Status:** protocol draft. No results exist. Commit the protocol and checks before confirmatory runs. The primary question is whether Rivet saves input tokens without an unacceptable loss of task success compared with a well-prompted text-tool workflow.

## Pilot before the full benchmark

After the PHP slice works, run 4–6 development tasks under B and C to catch unusable output, excessive refresh cost, and tasks without valid checks. Use the pilot to estimate variance, cost, and sample size. Pilot tasks and tuning runs are not confirmatory evidence; reserve held-out tasks before changing the tool/snippet based on results.

## Configurations

| Config | Tools and instructions | Role |
|---|---|---|
| A | Standard shell and harness-native read/edit tools; harness default instructions | Secondary control |
| B | A plus the efficient search prompt below | Primary control |
| C | B plus Rivet and exactly the managed snippet | Primary treatment; isolates adding the tool/snippet to an already efficient baseline |
| D | C plus an explicit request to try Rivet first | Optional adoption experiment, not part of the success gate |

Config B prompt, verbatim:

```text
When searching code, prefer `rg -n <pattern> <dir>` scoped to a likely directory over repository-wide searches. Read specific line ranges rather than whole files. Before reading a file, search for the symbol's definition and read only the enclosing function or class.
```

Keep model identifier, harness version and settings, repository commit, task text, resource limits, environment, and network policy constant. Record tool/snippet/help hashes. Use a shared image but deny execution of the Rivet binary for A/B at the runner boundary; merely removing it from PATH is not isolation. If enforcement is unavailable, disclose the limitation and flag contaminated runs under a preregistered rule. Do not selectively discard inconvenient outcomes.

Each run gets a fresh checkout, home directory, agent session, and empty `.rivet/` cache. Setup dependencies ahead of time; do not expose gold checks, task metadata, hidden tests, or other runs' artifacts inside the agent workspace. Keep the external evaluator isolated. Allow network only for the model endpoint, with an identical allowlist. Repository-provided agent instructions must be normalized identically across arms, with the policy and original hashes recorded.

Randomize configuration order within task/trial blocks and interleave arms over time. Record seeds if supported, but do not assume deterministic agent behavior. Pin sampling settings rather than relying on moving harness defaults. Cache policy must be the same in every arm; capture provider-reported cached and uncached token counts separately.

## Repositories and tasks

At least two real, permissively licensed projects pinned to commits, one PHP and one TypeScript/TSX, with reproducible test suites. Prefer at least one project unlikely to be memorized; low popularity alone does not establish that. Use at least 20 held-out tasks spanning navigation and small edits, with objective checks and balanced language/category coverage.

```text
benchmark/tasks/<id>/
  task.toml      repository, commit, category, limits, check type
  prompt.md      identical task wording across configurations
  setup.sh       creates the starting task state
  check.sh       trusted evaluator entrypoint, outside agent workspace
  gold/          hidden expected answers/bindings/tests
```

| Task | Pass condition |
|---|---|
| Fix a failing test | Hidden tests and existing suite pass |
| Locate a bug | Structured final answer identifies a gold file and symbol |
| Rename a symbol | Tests pass; a binding-aware gold check finds no stale references to the target and preserves unrelated same-name symbols |
| Find tests for a symbol | F1 against a hand-verified gold set ≥ 0.8; specify whether direct references or behavioral tests are requested |
| Trace a call path | Structured answer matches an accepted gold path; allow multiple valid paths explicitly |
| Add validation / small feature | Hidden behavioral tests and existing suite pass |

Do not use plain `rg` as a semantic rename oracle or claim it excludes comments/strings. Validate each starting state and gold solution before including a task. “Explain” tasks require a predefined objective rubric or are excluded.

## Trials, limits, and failures

The minimum design is 20 tasks × 5 trials × 3 required configurations = 300 runs. D adds 100 runs if included. These are lower bounds, not a claim of statistical power: a 5 percentage-point non-inferiority margin may require more tasks/trials. Use pilot variance to select a funded sample size before confirmatory runs. If the budget cannot distinguish the outcomes, report inconclusive.

With limited usage, progress in stages: first local CLI timings with no model calls (T37), then runner/extractor dry runs on synthetic transcripts, then a capped 4–6 task B/C pilot. One initial trial per arm is 8–12 pilot runs and checks usability only; repetitions and the full held-out study require their own explicit run/spend budget. Do not silently treat the small pilot as the 300-run study or use it for a general savings claim.

Preregister equal per-run wall-clock, model-token, and tool-call limits. Timeouts, agent crashes, invalid answers, and exhausted limits count as failures with their consumed tokens included. Infrastructure/provider outages have a separate objective classification and symmetric retry rule (for example, rerun the whole affected task/trial block once); preserve both transcripts. Warm-index measurements are a separate experiment, not mixed into the main cold-start result. Initial indexing cost is part of the treatment's cost.

## Collected artifacts

Store complete transcripts, evaluator outcomes, tool stdout/stderr, source hashes associated with Rivet positions, and a derived row per run:

```text
run_id, task_id, repository_commit, config, trial, execution_order
model_id, harness_version, settings_hash, tool_version, snippet_hash
pass, termination_reason, infrastructure_failure
input_tokens_total, input_tokens_cached, input_tokens_uncached, output_tokens
tool_calls_total, tool_calls_by_name, edits, failed_edits
wall_clock_seconds, indexing_seconds
rivet_invocations_by_command, rivet_errors_by_exit, refresh_mode
rivet_to_text_fallbacks, snapshot_mismatches
```

Use provider usage for the primary token metric: sum all model requests in a run, including repeated context and cached input. If cache counts are unavailable, mark unavailable; do not infer them. Cost in currency and cache-adjusted cost are secondary and need a recorded price schedule. Files/lines read are optional descriptive metrics with a documented extractor; do not present shell-heuristic counts as exact.

Define fallback as text-tool use within the next two tool calls on the same identifier/file, with an audited extractor. A snapshot mismatch means a returned span/source disagrees with the stored hash it claims, or a stable-tree default refresh misses an edit. A file changed after indexing is a concurrent edit, not automatically a stale-result bug. Preserve evidence to distinguish them.

## Pre-registration and decision rule

Commit `benchmark/preregistration.md` before held-out runs, containing corpus/task hashes, arm prompts, exact settings, budgets, randomization seed, retry rules, sample size, analysis script version, and these endpoints:

1. Primary efficiency metric: mean total input tokens across **all assigned runs**, C/B ratio, with equal task weighting. Successful-run-only comparisons are secondary because conditioning on success can select easier runs.
2. Quality gate: task success difference `C - B`; the lower one-sided 95% confidence bound must exceed -0.05 (five percentage points).
3. Proposed efficiency gate: C/B point estimate ≤ 0.70 and upper one-sided 95% confidence bound < 1.00. This supports an observed 30% reduction and evidence of some reduction; it does not prove the true reduction is at least 30%.
4. Both gates must pass. Quality failure → fix/re-scope; uncertainty → inconclusive and no automatic command expansion. A comparisons and tool-call/wall-clock metrics are secondary. D is exploratory.

Use a paired cluster bootstrap: resample whole tasks within language strata, retaining each sampled task's arm/trial block, then recompute task means and contrasts. Use 10,000 replicates and a fixed recorded seed. Report intervals, individual task outcomes, and category/language breakdowns. With only two repositories, conclusions apply to the selected corpus, not all codebases. Document zero-denominator handling and missing-run rules before analysis. Do not change endpoints after seeing results; any exploratory reanalysis must be labeled.

## Interpretation

Report adoption and fallback rates alongside effect estimates. Correlations between command usage and gains cannot identify which command caused a gain; successful agents may choose different commands. To test whether `context` adds value, randomize a separate navigation-only versus navigation-plus-context ablation with a matching snippet.

An inconclusive or negative result still informs scope: poor reference precision calls for resolver work; high refresh cost calls for indexing work; low adoption calls for help/snippet work; good performance from the efficient prompt alone weakens the case for a new binary. Confirmed snapshot-consistency bugs block performance conclusions until fixed and rerun under a new version.

Publish scripts, corpus pins, per-run results, confidence intervals, total spend, and known limitations. Reuse the small pilot for development checks; rerun the confirmatory benchmark for material changes after preserving held-out validity.

## Report files and reproducible generation

Use [benchmark/REPORT-TEMPLATE.md](../benchmark/REPORT-TEMPLATE.md) for both pilot and confirmatory reports. It exists now; the runner, extractors, generator, and results do not. Keep every study in its own directory so reruns cannot silently replace earlier evidence:

```text
benchmark/
  REPORT-TEMPLATE.md
  preregistration.md                 frozen protocol before confirmatory runs
  raw/<study-id>/<run-id>/            transcripts, provider usage, evaluator results
  results/<study-id>/
    report.md                        human-readable report
    preregistration.md               copy of the study's frozen protocol, if applicable
    manifest.json                    versions, hashes, environment, limits, run schedule
    runs.csv                         one record per attempt, including failures/retries
    per-task.csv                     all task/arm aggregates
    summary.json                     estimates, intervals, gates, analysis metadata
    local-performance.csv            optional raw local timing samples
    SHA256SUMS                       checksums of report inputs and outputs
```

The planned pipeline is `runner artifacts → extract.py → runs.csv → report.py → summary.json + per-task.csv + report.md`. These script names are implementation targets, not available commands. Per-attempt rows include an attempt ID, scheduled block ID, and analysis inclusion/retry reason so the analysis applies the frozen retry rule without double-counting reruns. Preserve originals outside agent workspaces; review transcripts for secrets before publishing and document redactions.

The generator owns numerical tables and gate outcomes and rewrites template links relative to the report destination. Human commentary explains findings and limitations with run IDs; it must not manually change computed results. Identical inputs, analysis version, and seed must reproduce the same numerical artifacts. Reference the completed report from the README/release notes only after its evidence is reviewed; the empty template must never appear as a performance result.

Before consuming model usage, validate extraction/analysis with tiny synthetic cases: known token ratios, unequal task trial counts, a cheaper failed run, a retry, missing usage, zero denominators, and incomplete arm blocks. Check paired resampling and deterministic seeds. Missing measurements are never zero-filled; an integrity failure or an uninformative analysis produces an explicit invalid/inconclusive outcome. These checks validate the reporting machinery, not Rivet's claimed benefit.

## What a public claim must include

The selling point to test is **lower measured agent input-token usage at comparable task success**. Publish observed reduction with its uncertainty, success rates/difference, end-to-end time, comparator, model/harness versions, task/corpus scope, and a link to the report. Expose regressions and indexing cost alongside gains. “Faster” requires measured time savings; “cheaper” requires billing evidence; a hard model-token cap cannot be inferred from Rivet's source estimator.

The proposed success gate is a 30% observed token reduction with evidence of some reduction and a bounded success regression. It does not establish that every task saves 30%, that quality is identical, or that all models/harnesses benefit. Cross-harness claims require separately reported host results; merely running a setup smoke test in both hosts is insufficient.
