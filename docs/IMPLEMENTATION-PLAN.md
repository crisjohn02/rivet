# Implementation Plan

> **Status:** pre-coding review, 2026-09-22. No implementation or performance result is claimed. The working directory currently contains documentation and licenses only, with no Git metadata or Cargo workspace.

For limited-usage coding sessions, follow [TASKS.md](TASKS.md): one small task per session, explicit acceptance checks, and a persistent checkpoint. This document provides rationale and overall milestones; TASKS owns the execution order and progress.

[INTEGRATION.md](INTEGRATION.md) defines how normal users set up Codex and Claude Code, with snippet installation and smoke checks under T33a–T33c. A thin optional skill follows the pilot as T40a; a separate agent is not part of the default design.

## Assessment

The useful product is a small local CLI that combines symbol lookup, honest reference evidence, and compact source context. The model-free design and comparison against an efficient `rg` prompt are strengths. The business case remains a hypothesis: structural output must save enough agent work to cover index maintenance, output overhead, and incomplete language semantics.

Draft v0.2 had implementation-blocking contradictions. Draft v0.3 resolves them as follows:

| Risk found | Decision |
|---|---|
| “Never stale” with mtime/size-only checks | Hash content by default; label metadata/cached modes; serve source and positions from one stored snapshot |
| Raw `rg` superset versus excluding unrelated references | Separate reference and candidate modes; test annotated use spans and aliases |
| Top-level uses lost by mandatory symbol foreign keys | File-owned uses with nullable containing symbols |
| Incoming edges invalidated only when their target changes | Preserve raw lexical facts and re-resolve all uses after content changes initially |
| Per-batch writes expose mixed graph state | One refresh transaction, bounded lock wait, and rollback on failure |
| Target truncation contradicts full/signature-only context | Full or signature only; explicit budget error if neither fits |
| Unspecified PageRank weights and floating-point ordering | Fixed integer priorities for the MVP; graph ranking requires an ablation |
| JSON exceptions, missing pagination, conflicting fields | One normative output contract, including init/snippet and partial coverage |
| Conditional success-only benchmark and “within noise” | All-run token metric, explicit non-inferiority gate, paired analysis, inconclusive outcome |
| Install/build examples imply a released package | Mark package names, target testing, and toolchain versions as unverified until the build exists |

## Start with a vertical slice

Use PHP first, then TypeScript/TSX. Implement `index → symbol → refs → context` over a small authored fixture, with the real CLI/output path. Avoid building all language adapters, release infrastructure, or graph algorithms before one useful query works. Pin real benchmark repositories before the pilot; the authored fixture is enough for initial extraction and resolver work.

| Step | Deliverable | Acceptance before moving on |
|---|---|---|
| 0 | Workspace, dependency spike, fixture and contract skeleton | Pin compatible Rust/Tree-sitter/grammar versions; parse tiny PHP and TSX files; validate JSON examples and define integration-test ownership |
| 1 | Root/config/scanner/store and `index` | Correct ignores/worktrees; creates/deletes/renames tracked; preserved-metadata edits detected in content mode; interrupted refresh leaves the previous snapshot intact |
| 2 | PHP symbols and query resolution | Canonical IDs, duplicate ordinals, file:line ambiguity, Unicode/CRLF spans, exact stored source; stable bytes across rebuild and incremental histories |
| 3 | PHP uses, imports, scopes, bindings and `refs` | Top-level/aliased/shadowed references covered; same-name targets separated; tiers match gold evidence; parse failures remove old facts |
| 4 | PHP context | Correct priority/collapse/overlap behavior, strict estimated source budget, bounded traversal, deterministic tiny-budget errors |
| 5 | Snippet/help and pilot | Init is idempotent; snippet is honest about uncertainty and limits; B/C pilot records adoption, correctness, and total query cost |
| 6 | TypeScript/TSX and real fixtures | Same adapter contract works without language branches in core; exercise the previously pinned PHP/TS benchmark projects; classify unsupported constructs |
| 7 | Held-out benchmark and release readiness | Preregister/run the benchmark, pass quality and efficiency gates or report inconclusive/re-scope; only then expand commands |

The spec's numbered milestones retain their product groupings; these steps are the implementation order. The TypeScript grammar smoke test in step 0 is a compatibility check, not a second full adapter.

## Correctness acceptance matrix

Turn these into integration fixtures while implementing the associated behavior; they are required scenarios, not a request to scaffold all tests before useful code.

| Area | Cases | Required result |
|---|---|---|
| Freshness | Same-size edit with restored mtime; deletion; rename; branch switch; new duplicate definition; changed import | Content-mode results reflect the new source and declaration bindings |
| Cache invalidation | Ignore/config/language/grammar/resolver changes | Excluded facts removed; affected parses/bindings rebuilt |
| Atomicity | Two concurrent queries; killed writer; lock timeout; editor changing a file during refresh | Complete committed snapshot or bounded explicit error, never mixed positions/source |
| Parser failures | Valid file becomes malformed; invalid UTF-8; oversize; deterministic resource limit | Old facts removed; coverage explains omissions; explicit target queries fail clearly |
| Resolution | Aliases; local shadowing; receiver reassignment; duplicate names; dynamic/static dispatch; top-level calls; interpolation | Supported bindings and tiers correct; uncertain uses retained without false exact claims |
| Identity | Literal `#`/`%` in path; duplicates; nested definitions on one line; case-sensitive paths | No ID collisions; no arbitrary ambiguity resolution |
| Budget | Zero/negative; tiny budget; large target; all collapse modes; parent/child overlap; high fanout | Invalid argument or fitting forms/error 8; no partial body; truthful omission/cap metadata |
| Output | Empty results; limit/offset boundary; ambiguity pages; JSON errors; init/snippet | Parseable bounded objects, correct totals, consistent stdout/stderr and exit codes |
| Filesystem | Worktree `.git` file; nested repo; symlink cache; outside symlink; FIFO; CRLF/Unicode | Root boundary and exclusion rules honored; no unsafe writes/reads |
| Determinism | Sequential/parallel extraction; clean rebuild versus incremental; repeated query | Same query bytes for the same supported snapshot and options |

## Dependency and packaging decisions

Before committing Cargo manifests:

- Verify compatible grammar ABI/API versions for PHP, TypeScript, and TSX; pin the dependency graph in `Cargo.lock` and pin CI toolchains. Determine MSRV from a tested dependency set instead of assuming 1.80.
- Keep language features forwarded by the CLI; prove no-default/PHP-only/default builds. Start sequentially; add Rayon when measured.
- Root is a virtual Cargo workspace, so put executable integration tests and Criterion benches in the owning package (`crates/rivet-cli/tests`, package `benches`). Shared fixture data may stay at repository root.
- Use authored fixtures for exact corner cases and pinned, licensed real repositories for realism. Keep `gold.toml` outside submodule directories so gold edits belong to Rivet. Preserve fixture attribution/licenses.
- Keep crate boundaries internal. Verify crates.io names, GitHub owner, artifact URLs, grammar licenses, and license notices before publishing. The executable can remain `rivet` even if the Cargo package needs another name.
- Build Linux and macOS on suitable runners and Windows on a native runner. Cross-building is not runtime testing. Record actual tested targets before advertising support.

## Measure before adding complexity

Record no-change content hashing, metadata-mode checking, full re-resolution, one-file edits, high-fanout queries, peak memory, and index size. Stored source trades disk space for consistent context and simpler concurrency. One transaction and full re-resolution trade throughput for fewer invalidation bugs. Accept or revise those tradeoffs using observed costs; do not silently weaken the freshness mode or coverage labels.

Potential future changes—PageRank, selective invalidation, daemon/watch mode, language servers, more languages, editing—each need a measured bottleneck or demonstrated task benefit. They are not prerequisites to the first release candidate.
