# Rivet Benchmark Report — STUDY_ID

> **Status: NOT RUN.** This is an empty template, not a performance result. Replace placeholders only with traceable measurements. A pilot cannot pass the confirmatory release gate. Follow the [benchmark runbook](../docs/BENCHMARK.md).

## Result at a glance

| Item | Result |
|---|---|
| Study type / run dates | NOT SET — pilot or confirmatory |
| Rivet version, commit, binary hash | NOT SET |
| Model identifier / harness version | NOT SET |
| Repositories, languages, tasks, trials | NOT SET |
| Scheduled / completed / failed / invalidated runs | NOT RUN |
| Primary comparison | C (efficient-search instructions + Rivet snippet) versus B (efficient-search instructions) |
| Mean input-token reduction | NOT MEASURED |
| Task success difference (C − B) | NOT MEASURED |
| End-to-end wall-time change | NOT MEASURED |
| Verdict | NOT RUN — pilot observation / gates passed / gates not met / inconclusive / invalid study |

**Supported public statement:** None. No benchmark has run.

After a valid confirmatory run, write a scoped statement naming the model/harness, corpus, comparator, observed token reduction, and success result. Link the evidence. Do not substitute the best task improvement for the aggregate result or source-estimate reductions for actual model-token savings.

## Reproduction and provenance

| Artifact / setting | Value or relative link |
|---|---|
| Frozen preregistration and revision | NOT SET |
| Study manifest, artifact checksums | NOT SET |
| Repository URLs, licenses, commit pins | NOT SET |
| Prompts, setup/check versions, task hashes | NOT SET |
| Instructions, snippet/help hashes | NOT SET |
| Model settings, context limits, cache policy | NOT SET |
| Rivet config, freshness, grammar/toolchain versions | NOT SET |
| Runner image/digest, OS, CPU, memory, storage | NOT SET |
| Run ordering / randomization seed | NOT SET |
| Per-run token / time / tool-call limits | NOT SET |
| Study run/spend cap and actual spend | NOT SET |
| Extractor/report code revision and exact reproduction commands | NOT SET |
| Raw transcripts, usage records, evaluator artifacts | NOT SET |

State whether caches began cold, initial indexing was included, hidden evaluators were isolated, and controls were prevented from executing Rivet. Record unavailable telemetry, restricted artifacts, and redactions explicitly. Do not publish credentials from transcripts.

## Task and run accounting

| Language / repository | Category | Tasks | Trials | Scheduled by arm | Accounted by arm |
|---|---|---|---|---|---|
| NOT SET | — | — | — | — | — |

| Outcome | A (optional) | B | C | D (optional) |
|---|---|---|---|---|
| Successful tasks | — | — | — | — |
| Evaluator failures | — | — | — | — |
| Time/token/tool-call limit reached | — | — | — | — |
| Agent crash / invalid final answer | — | — | — | — |
| Infrastructure failures | — | — | — | — |
| Rerun attempts | — | — | — | — |
| Missing / contaminated runs | — | — | — | — |

Account for every scheduled run and extra attempt. Explain retries/exclusions under the preregistered rule. Task failures remain in primary cost/success analysis. Missing usage is unknown, not zero. Explain whether incomplete accounting invalidates conclusions.

## Primary results and gates

Use equal task weighting and all eligible assigned runs, including task failures. Input tokens are provider-reported totals across all model requests, including repeated context and cached input. A/D cannot replace the preregistered B comparator after results are seen.

| Metric | A (optional) | B | C | C versus B | Two-sided 95% interval |
|---|---|---|---|---|---|
| Mean total input tokens / run | — | — | — | Ratio: —; reduction: —% | — |
| Task success rate | — | — | — | Difference: — percentage points | — |
| Mean end-to-end wall time / run | — | — | — | Change: —% | — |
| Mean tool calls / run | — | — | — | Change: —% | — |
| Mean output tokens / run | — | — | — | Change: —% | — |

| Confirmatory gate | Proposed preregistered requirement | Estimate / bound | Outcome |
|---|---|---|---|
| Efficiency point estimate | C/B ≤ 0.70 | NOT MEASURED | NOT EVALUATED |
| Evidence of reduction | One-sided 95% upper bound on C/B < 1.00 | NOT MEASURED | NOT EVALUATED |
| Success non-inferiority | One-sided 95% lower bound on C−B > −5 percentage points | NOT MEASURED | NOT EVALUATED |
| Study integrity | Valid accounting; no unresolved snapshot-consistency bug | NOT CHECKED | NOT EVALUATED |

Use the actual frozen thresholds if different and explain the preregistered choice. Record bootstrap method/seed/replicates and missing-block rules. Disclose unstable or degenerate intervals; uninformative intervals do not establish equivalence. Pilot gates are always “not evaluated.”

Token reduction is `100 × (1 − mean_C / mean_B)`. This is not automatically a reduction in billed cost or completion time. A 30% observed reduction plus the proposed confidence gate supports evidence of some reduction, not proof that the true reduction is at least 30%.

## Per-task and language breakdown

| Task | Language / category | B success | C success | B mean input | C mean input | Reduction | Wall-time change |
|---|---|---|---|---|---|---|---|
| NOT RUN | — | — | — | — | — | — | — |

Include every task here or link the complete table. Add language/category aggregates. Identify regressions and gains concentrated in a small subset. Do not omit negative tasks from published tables/charts.

## Adoption and failure analysis

| Diagnostic | C | Evidence |
|---|---|---|
| Runs invoking Rivet | — | Fraction of all eligible C runs |
| symbol / refs / context invocations | — | Command traces |
| Errors by exit code | — | Include ambiguity and budget errors |
| Text-tool fallbacks | — | Audited next-two-tool-calls rule from runbook |
| Snapshot-consistency incidents | — | Distinguish edits after indexing |
| Partial-coverage responses | — | Coverage metadata |

Describe representative wins and failures with run IDs, including common method names and edit/refresh behavior where present. Reference correctness requires gold checks. Command-use associations do not establish causality; a claim that context caused a gain needs a separate randomized ablation.

## Indexing overhead and optional cost analysis

| Local measurement | Corpus size / conditions | Result |
|---|---|---|
| Initial index wall time | — | — |
| Content-mode no-change query p50/p95 | — | — |
| One-file edit plus query p50/p95 | — | — |
| Common-name refs / context latency | — | — |
| Index size / peak memory (if measured) | — | — |
| Full response tokens versus source estimate | — | — |

Link raw local samples, separate cold/warm results, and keep local timings distinct from end-to-end agent timing. Currency claims require cached/uncached/output usage and a dated price schedule or billing evidence; otherwise mark cost unavailable.

## Limitations, deviations, and decision

- Corpus representativeness, supported syntax, possible model familiarity: NOT ASSESSED.
- Statistical power and uncertainty: NOT ASSESSED.
- Preregistration deviations, affected runs, sensitivity analyses: NOT ASSESSED.
- Missing telemetry/evidence: NOT ASSESSED.
- Integration variant (snippet or separately evaluated skill): NOT SET.
- Decision and next task: NOT DECIDED.

Pilot decisions are fix/retest, proceed, or re-scope. Confirmatory results must report passed gates, gates not met, or inconclusive; invalid studies require correction/rerun. Results on one model/harness/corpus do not establish universal savings across agents or codebases.
