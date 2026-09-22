# Changelog

All notable changes to `rivet` are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versioning follows [Semantic Versioning](https://semver.org/) for the binary; the JSON `schema_version` is tracked separately and noted on every change to it.

Each entry that changes JSON output is marked **[additive]** or **[breaking]**.

## [Unreleased]

### Specification
- Draft v0.3 pre-coding review: defined content/metadata/cached freshness and atomic stored-source snapshots; separated candidate coverage from declaration references; specified IDs, coordinates, partial coverage, and invalidation; replaced MVP PageRank with fixed priorities; made estimated source budgets strict.
- Reconciled the proposed schema 1 before its first release: init/snippet JSON, pagination, nullable top-level containers, context errors, deterministic ordering, and explicit coverage. No released schema is changed.
- Revised benchmark to use a preregistered C/B all-run comparison, success non-inferiority gate, isolated runs, pilot, and inconclusive outcome.
- Draft v0.2 of the specification: added Related Work, Symbol Identity, Reference Resolution, and Harness Integration sections; made index freshness a requirement; specified context ranking and budget fitting; tightened the benchmark protocol; consolidated the MVP definition.
- Renamed the project from `cx` to `rivet`.

### Documentation
- Added an empty Markdown benchmark report template and specified artifact layout, reproducible report generation, public-claim requirements, and local reporting checks before model usage. Added T38a–T38c and T48a to keep reporting work resumable.
- Added INTEGRATION with normal Codex/Claude Code workflows, verified host documentation links, optional skill packaging, and environment/acceptance requirements. Split integration work into T33a–T33c and optional T40a. Clarified explicit snippet destination creation and direct context entry in the canonical snippet (benchmark treatment text changed).
- Added TASKS with 49 bounded task entries, a resumable checkpoint, focused validation, and gates before further language/benchmark spending. Aligned fixture-selection timing with the small-session execution order.
- Added IMPLEMENTATION-PLAN with review findings, build order, dependency decisions, and acceptance cases. Aligned all supporting docs with Draft v0.3 and removed unverified build/install claims.
- Added README, INSTALL, BUILDING, ARCHITECTURE, OUTPUT-CONTRACT, ADDING-A-LANGUAGE, BENCHMARK, AGENT-SNIPPET, RELEASING, CONTRIBUTING.

### Licensing
- Dual-licensed under MIT OR Apache-2.0. Added `LICENSE-MIT` and `LICENSE-APACHE`.

### Code
- None yet.

## Schema versions

| `schema_version` | Introduced in | Notes |
|---|---|---|
| 1 | unreleased | Initial contract, see docs/OUTPUT-CONTRACT.md |
