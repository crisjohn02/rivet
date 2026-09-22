# Small-Task Implementation Queue

> **Status:** implementation in progress; see the checkpoint below for the last completed task. Work one task per session by default, with no automatic continuation into the next task.

This queue breaks the [implementation plan](IMPLEMENTATION-PLAN.md) into resumable changes. It does not change the product scope or replace the [output contract](OUTPUT-CONTRACT.md).

## Current checkpoint

| Field | Value |
|---|---|
| Last completed task | T27 |
| Next task | T28 |
| Active task / partial progress | None |
| Blocker | None known |
| Last checks | AF4: `cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean; `cargo test --workspace` all pass (rivet-cli 9 unit + 16 bindings + 9 context_rank + 19 context_traversal + 7 freshness_modes + 7 index_json + 1 index_uses + 15 kind_aware_lookup + 11 namespace_scopes + 10 receiver_conservatism + 7 refresh + 10 refs_json + 4 reresolve + 7 silent_misses + 9 symbol_calls_json + 7 symbol_json + 13 symbol_query_forms + 5 symbol_source, rivet-core 60, rivet-index 90, rivet-languages 52 unit + 12 php_extract + 2 php_uses, rivet-parser 2 unit + 4 grammar_smoke, rivet-store 28; 0 failed); `python3 tests/gold/check_gold.py` verified 51 entries. |
| Decisions to carry forward | PHP first; sequential implementation; JSON before human formatting; no new commands |

Update this checkpoint at the end of each implementation session. Keep it short; the code and task checklist are the detailed record.

## Rules for keeping usage small

- **One task, one result.** Finish the selected task and its relevant checks, then stop. Do not begin the next task unless requested.
- Read this checkpoint, the selected row, and only the relevant document sections/source files. Do not reread the full spec each session.
- Use one agent. Avoid repeated planning, speculative abstractions, and unrelated cleanup.
- Add the smallest meaningful fixture/check for the behavior being implemented. Use targeted tests during development; run broader checks at the checkpoints below.
- If a task grows beyond one coherent change, split its unfinished scope into numbered subtasks such as T20a/T20b. Record what works and what remains; do not mark the parent done until both finish.
- Intermediate builds are development builds. A command not yet implemented must report that clearly; never emit placeholder success or empty results that look complete.
- Record actual commands and outcomes, plus any failing check. A task is complete only when its stated acceptance condition passes.
- No time/token estimates are promised. Dependency setup and resolver work can vary substantially; split based on observed complexity.
- Keep release automation, full benchmark runs, optimization, and the second language behind their gates. Do not spend on them just because they appear later in this file.

Suggested request for a coding session:

```text
Implement T01 from docs/TASKS.md only.
Read the current checkpoint and only the relevant docs/code.
Run the checks appropriate to that task, update its checkbox and checkpoint,
then stop. If it needs splitting, record a concrete next subtask.
```

Tasks run in numeric order unless a row says otherwise. A task may rely on earlier completed tasks; later hardening tasks do not excuse incorrect claims in earlier development builds.

## A. Establish a build and tiny fixtures

Read: BUILDING workspace/dependency guidance; ARCHITECTURE workspace boundaries; ADDING-A-LANGUAGE adapter boundary as needed.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | T01 | Create the local Rust workspace and runnable CLI shell. Check the installed toolchain, create the six planned crate boundaries with minimal contents, and keep packages unpublished. Add a minimal `.gitignore` for generated build/cache files. | Workspace checks successfully and `rivet --version` runs. Record the toolchain used. No scanner/parser/database implementation. |
| [x] | T02 | Pin compatible Tree-sitter, PHP, TypeScript/TSX grammar dependencies; wire language features and add tiny grammar smoke samples. | PHP and TSX snippets parse with their correct grammars; no-default, PHP-only, and default builds work. Commit the lockfile when version control is available. No TypeScript extraction. |
| [x] | T03 | Implement shared spans, symbol IDs, and resolution enums only. | Checks cover UTF-8/CRLF positions, `%`/`#` escaping, and duplicate ordinals. No database or language resolution. |
| [x] | T04 | Add a three-file authored PHP fixture and exact gold spans outside the fixture source. Include two same-name methods, an alias, and a top-level call. | Expected declarations/use locations are independently readable from the fixture. Record which cases remain unresolved by design. Do not build a general fixture framework. |

**Checkpoint A:** run workspace tests and feature builds once. Record compatibility decisions in the checkpoint or BUILDING, without expanding the architecture.

## B. Index files on the simple path

Read: spec §§22, 25–27; ARCHITECTURE schema/refresh; OUTPUT-CONTRACT transport, index metadata, and index response.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | T05 | Implement root discovery and config validation. Support `.git` files/directories, `.rivet/`, nearest-boundary precedence, and built-in defaults. | Nested/worktree fixtures resolve correctly; invalid config fails before indexing. No instruction-file changes. |
| [x] | T06 | Implement eligible-file traversal. Apply local ignore rules, configured exclusions, dependency defaults, and nested-repository boundaries. | Fixture inventory is deterministic and includes eligible untracked/hidden files; symlinks and special files are excluded. |
| [x] | T07 | Add bounded regular-file reads, source hashing, encoding/size/binary classification, and basic coverage records. | Stored candidate bytes and hashes agree; oversized, invalid-UTF-8, symlink, and FIFO cases cannot become normal source reads. |
| [x] | T08 | Create the SQLite schema and minimal store operations. Enable foreign keys and persist/retrieve file bytes and metadata. Validate cache destinations before writes. | An in-memory/temporary store round-trip preserves source bytes; symlinked cache destinations are rejected. No migrations framework beyond a format version. |
| [x] | T09 | Implement atomic publication of a file inventory and deterministic snapshot digest. | One transaction publishes all file rows; a deliberately failed transaction leaves the previous complete inventory intact. Digest ignores row IDs/mtime-only changes. |
| [x] | T10 | Wire the first real `rivet index --json` path, including coverage and work counters. Establish the shared success/error transport. | Indexing the tiny fixture produces valid output; repeat indexing has correct updated/unchanged counts. Unimplemented navigation commands clearly fail. |

**Checkpoint B:** index the fixture through the real binary. Run relevant store/CLI tests plus fmt/clippy. This is an inventory milestone, not a claim of completed language support.

## C. Deliver useful PHP symbol lookup

Read: ADDING-A-LANGUAGE PHP support and fixtures; spec §10; OUTPUT-CONTRACT symbol objects and symbol response.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | T11 | Extract PHP namespaces, classes, named functions, and methods into owned records. Detect parser error/missing nodes. | Fixture declaration names, parents, and byte spans match gold. Malformed input produces a file diagnostic and no extracted facts. |
| [x] | T12 | Persist symbol records and implement canonical-ID, short-name, and qualified-name lookup. | `symbol <query> --json` locates the fixture methods; duplicate short names return candidates and exit 5. Call lists are still explicitly unavailable in this development milestone. |
| [x] | T13 | Add `file:line`, query normalization, deterministic ambiguity pagination, and not-found suggestions. | Same-line nested ambiguity is not guessed; candidate limits/offsets and error envelopes match the contract. |
| [x] | T14 | Extract declaration signatures/docs and implement `symbol --source` from stored bytes. | Returned source hashes/spans match indexed bytes even if the live file subsequently changes; CRLF/Unicode source is preserved. |
| [x] | T15 | Add changed-content refresh and removals to the query path; avoid reparsing equal content. | An edit, deletion, rename, and valid-to-malformed transition update symbol results automatically. A same-size edit with restored mtime is detected in default content mode. |
| [x] | T16 | Add effective config/grammar invalidation plus explicit metadata/cached modes and force rebuild behavior. | Changed ignores/languages remove excluded facts; cached responses are labeled and require a compatible index; metadata mode never claims content verification. |

**Checkpoint C:** demonstrate `index → symbol → edit → symbol` from the CLI. Compare clean-rebuild and incremental symbol output. This is the first useful navigation result; measure no-change refresh cost now, without optimizing it yet.

## D. Deliver PHP references with honest evidence

Read: spec §11; ARCHITECTURE uses/scopes/bindings; OUTPUT-CONTRACT refs and call lists.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | T17 | Extract PHP call/use spans and containing symbols for the small fixture, including top-level uses and interpolated expressions. | Gold use spans match; literal text/declarations are excluded; call sites are not duplicated as unknown references. |
| [x] | T18 | Persist lexical scopes, imports/aliases, and unresolved use facts. | Facts survive a store round-trip with nullable containers and sufficient scope data to resolve without reparsing. No guessed target foreign keys. |
| [x] | T19 | Resolve direct PHP import/namespace bindings and supported lexical function references. | Alias uses link to the correct declarations; shadowing and conflicting candidates remain unresolved. Exact tiers require lexical evidence. |
| [x] | T20 | Resolve `$this`/`self` member uses and explicit native receiver types. | Correct declaration links are scoped; same-name classes remain distinct; dynamic/late-static behavior does not become exact. |
| [x] | T21 | Add preceding `new` receiver hints with conservative reassignment/control-flow handling. | A known safe assignment resolves as scoped; reassignment or uncertainty does not retain an unjustified binding. |
| [x] | T21a | Review fix for T21: make the `new`-receiver reassignment test cover every PHP rebinding form (`foreach`, destructuring, reference, `catch`, by-ref closure, compound assignment), not only simple `$x = ...`. | A variable rebound by any of those forms records no binding; the safe single-assignment case still binds `scoped`; unrecognised constructs default to no binding. |
| [x] | T21b | Review fix for T21a: suppress `new`-receiver bindings in a scope containing a rebinding the walker cannot analyse (callee-declared by-reference parameter, by-reference builtin, dynamic variable write, `$GLOBALS` write, `extract`, `eval`). | Each of those forms records no binding; an indexed callee's declared by-reference parameter is detected rather than suppressed; the safe case still binds `scoped`. |
| [x] | T22 | Re-resolve all persisted uses when indexed content/membership changes. | Adding a duplicate declaration, changing an import, or deleting a target updates bindings in unchanged files; unresolved uses survive. |
| [x] | T23 | Wire `refs` reference/candidate modes, filters, counts, and pagination. | Default mode excludes uses bound elsewhere; candidate mode includes them with query-relative name-match evidence; aliases are returned. Gold assertions check spans, not just counts. |
| [x] | T24 | Populate symbol call/caller lists with shared reference records and per-list pagination. | Top-level calls are retained, repeated call sites are counted correctly, and filters/order match the output contract. |

**Checkpoint D:** run the PHP gold binding/coverage suite and CLI tests. Add one common-name example such as `handle`. Inspect the first page manually: bounded output alone is not evidence of useful references.

## E. Deliver context directly from a query

Read: spec §§16 and 23; OUTPUT-CONTRACT context; ADDING-A-LANGUAGE signature behavior.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | T25 | Finish the remaining MVP PHP definition kinds and collapsed signatures: interfaces, traits-as-class, enums, properties, constants, and container member summaries. | Small focused examples produce correct identities and body-free summaries. Split by construct family if the pinned grammar makes this more than one coherent change. |
| [x] | T25a | Review fix for T25: correct the collapsed container form. Render each declared name's own signature instead of repeating the shared header, omit the body marker on bodyless declarations, and settle whether promoted properties are listed separately or left implied by the constructor. | A multi-name declaration renders one line per name with only that name; an abstract or interface method has no `{ … }`; a promoted property appears once, by a stated rule. Must land before T28 consumes the summary. |
| [x] | T26 | Collect direct context candidates and apply fixed integer priorities/deduplication. | Types, callees, callers, used imports, parent, and reference-based tests have deterministic reasons/order; name-only links do not expand context. |
| [x] | T27 | Add depth-two traversal, relationship flags, and fixed work caps. | Cycles/high fanout terminate predictably; excluded relationships stay excluded; cap metadata is truthful. |
| [ ] | T28 | Implement source estimation and greedy budget fitting for all collapse modes. | Exact-fit/tiny-budget cases obey the estimate; a target that cannot fit yields error 8; no partial body or over-budget success. |
| [ ] | T29 | Suppress parent/child source overlap and count budget/overlap/limit omissions. | Target/member/parent examples contain no duplicated full bodies and estimates use the final emitted summaries. |
| [ ] | T30 | Wire `context <query>` through discovery, refresh, query resolution, and JSON output. | A single invocation returns useful target/related source without requiring earlier symbol/refs commands. All context options and errors match the contract. |

**Checkpoint E:** run context/CLI tests and fmt/clippy. Compare the output to reading the three-file fixture manually. Record full rendered output size as well as the source estimate.

## E2. Audit fixes

A whole-codebase adversarial audit on 2026-09-22 found defects the suites did not catch; see `orchestration/review-notes/audit-2026-09-22.md`. Numbers in parentheses are the audit's finding numbers. AF1 through AF4 touch the resolver and PHP extractor and run in order; AF5 is independent.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [x] | AF1 | Fix namespace and scope structure: top-level closures see the file's namespace and imports (1); each namespace block of a multi-namespace file resolves against its own namespace and imports (2); a global `namespace { }` block declares global names (3). | Each audit reproduction yields the correct binding or none; no wrong `exact`; gold unchanged unless a gold span was itself wrong, which must be reported. |
| [x] | AF2 | Make declaration lookup kind-aware: a call binds only to a callable, a class use only to a class-like, a property access only to a property (4, 5); fold case only for ASCII, as PHP does (10); suppress the global function fallback when a PHP file that could declare the namespaced function failed to index (suspicion 3, confirmed during AF1). | Each audit reproduction yields the correct binding or none. |
| [x] | AF3 | Extend receiver conservatism to typed receivers and close the remaining rebinding forms: typed parameters honour unanalysable scopes, by-reference arguments and reassignment (6); union types stay unresolved (7); reference aliases, constructor by-reference arguments and `global` rebinding (8); attribute text cannot mislead by-reference parsing (9). | Each audit reproduction records no binding; safe typed and `new` cases still bind `scoped`. |
| [x] | AF4 | Close silent misses: walk anonymous class bodies (11); store a use's normalized short lookup name so unresolved qualified and constant uses name-match (12); record class references in static calls, class-constant access and `::class` (13); record an `instanceof` operand as a type use, restoring the binding AF2 correctly withdrew. | Each audit reproduction appears in refs; nothing previously correct changes tier. |
| [ ] | AF5 | Contract and coverage honesty: an invalid or unextracted TypeScript file is not reported as indexed (14); index-dependent errors carry `index` (15); `updated` counts regenerated files (16); the non-UTF-8 diagnostic reports a repository-relative path (17); `--no-refresh` rejects a cache from another extractor or resolver fingerprint (suspicion 2, confirmed before AF5). | Each item matches OUTPUT-CONTRACT or ARCHITECTURE, with a regression test. |

## F. Make the PHP slice safe and usable in a pilot

Read: ARCHITECTURE concurrency/parse policy; spec §§25–27; AGENT-SNIPPET; OUTPUT-CONTRACT errors/flags.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [ ] | T31 | Add writer lock timeout, race detection/retry, and cancellation rollback to refresh. | Competing processes and interrupted writers yield a complete snapshot or the documented bounded error; no silent cached fallback. |
| [ ] | T32 | Complete parser resource limits, skip/diagnostic handling, and direct-target errors. | A formerly valid file loses stale facts on parse/resource failure; unrelated queries report partial coverage; direct queries fail with the specified code. |
| [ ] | T33 | Implement idempotent `init` and `snippet`, including JSON, managed blocks, and guarded text writes. | Repeated init preserves config/text, both-instruction-file ambiguity is explicit, and symlinked write targets are rejected. |
| [ ] | T34 | Add compact human output/help for the implemented commands. | Resolution marks, pagination, coverage, and context forms remain visible; help fits the spec's limit and uses real supported examples. |
| [ ] | T35 | Select and pin licensed real PHP/TS benchmark repositories; add a small PHP gold sample outside the fixture checkout. Do not execute arbitrary setup scripts during selection. | Repository commits/license attribution are recorded, the PHP fixture exercises common method names, and future held-out tasks remain separate from pilot cases. |
| [ ] | T36 | Audit the CLI contract and the existing acceptance matrix; close omissions with focused regression cases. | Flags/errors/stream behavior, worktrees, symlink/FIFO handling, rebuild equivalence, Unicode, and partial coverage agree with the docs. Record any remaining gap as a subtask before proceeding. |

**Checkpoint F:** run the complete PHP workspace checks once. The pilot requires a real working slice, not all future platform/release infrastructure. An unresolved correctness gap blocks pilot interpretation.

T33 is split into the following smaller integration subtasks. Complete T33a/T33b before T34; perform T33c after T34 and before the pilot. Read [INTEGRATION](INTEGRATION.md) for the user workflow. Do not consume live host usage without an agreed run budget.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [ ] | T33a | Implement core init/config/.gitignore behavior and guarded writes. | Repeat init preserves existing content/config and rejects unsafe destinations. |
| [ ] | T33b | Implement snippet output and explicit/default instruction-file installation. | AGENTS.md and CLAUDE.md creation/update, both-file ambiguity, JSON, and repeat setup pass local fixtures. Shipped text matches AGENT-SNIPPET. |
| [ ] | T33c | Smoke-test normal setup in Codex and Claude Code using the same PHP fixture. | Record host versions, instruction loading, one ordinary task and one explicit request per available host, recovery/fallback behavior, and command traces. Missing access/budget is an explicit blocker; untested hosts stay planned. |

T33's original installer acceptance is satisfied by T33a/T33b; T33c is the separate live integration gate. Model-dependent adoption/recovery checks may share the agreed pilot runs instead of spending on duplicate sessions.

## G. Check value before spending on the rest

Read: BENCHMARK pilot, isolation, collected metrics, and decision rules; IMPLEMENTATION-PLAN measurement priorities.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [ ] | T37 | Measure the local PHP slice on the pinned repository without an agent benchmark. | Record cold index, no-change query, one-file edit plus query, common-name refs, context output bytes, and index size with environment details. Separate content hashing from resolution costs. |
| [ ] | T38 | Define 4–6 objective B/C pilot tasks and their checks using available harness access. Estimate model usage/cost from available evidence. | Starting states and gold solutions pass their checks; prompts, run caps, and artifacts are defined. No paid runs are launched as part of planning. |
| [ ] | T39 | Run the agreed small pilot, retaining successes, failures, token usage, wall time, and transcripts. | Both arms run under comparable limits, including indexing cost. If credentials, access, or an explicit run/spend budget are missing, record the blocker instead of consuming unspecified usage. |
| [ ] | T40 | Write a short pilot result and choose the next smallest change. | Report reference usefulness, adoption, tokens, latency, and limitations. Choose fix/retest, proceed to TypeScript, or re-scope; do not label the pilot a passed confirmatory benchmark. |

**Gate G:** T41 onward stays deferred until the pilot supports spending further effort. No full 300+ run benchmark is implied by completing this queue's PHP tasks.

Benchmark reporting uses [REPORT-TEMPLATE](../benchmark/REPORT-TEMPLATE.md). Split the reporting portion of T38 into these tasks before any paid pilot run; they add no automatic run authorization:

| Done | ID | Small task | Done when |
|---|---|---|---|
| [ ] | T38a | Define the study manifest and record/replay one run's usage, transcript, and evaluator outcome. | A synthetic run round-trips with IDs, versions, limits, and failure accounting; no model call required. |
| [ ] | T38b | Implement artifact extraction to per-attempt CSV. | Fixtures prove token totals, missing-usage handling, failures, and retry inclusion rules; originals remain traceable. |
| [ ] | T38c | Implement task-weighted aggregates and Markdown report generation. | Known synthetic ratios/counts reproduce; pilot reports mark confirmatory gates unevaluated; tables link to evidence. |

T40 produces `benchmark/results/<study-id>/report.md` from the template and computed artifacts. Before T49, add **T48a — implement and validate paired intervals and preregistered gate reporting** on synthetic fixtures, including incomplete blocks and degenerate cases. Reuse the pilot reporting pipeline. Run analysis locally before considering more model trials.

Optional follow-up after T40: **T40a — author and validate a thin Rivet skill** using the skill-creation workflow. Keep it in a distributable asset directory, test local discovery/explicit invocation in the target hosts under an agreed usage budget, and compare its instructions against the CLI contract. Do not install it into user-wide directories by default. This is not a prerequisite to the snippet MVP and is a separately recorded benchmark treatment.

## H. Deferred second-language and validation queue

These are still separate tasks, activated only after Gate G. Read the corresponding language/benchmark sections when needed. Split further before implementation if a row exceeds one coherent change.

| Done | ID | Small task | Done when |
|---|---|---|---|
| [ ] | T41 | Add TypeScript/TSX authored fixtures, gold spans, and extension/grammar dispatch. | `.ts`, `.tsx`, and declaration-file cases use the intended grammar; unsupported extensions are explicit. |
| [ ] | T42 | Extract TypeScript named definitions and signatures. | Named functions/classes/methods and remaining supported kinds match gold; split remaining kind families into subtasks if needed. |
| [ ] | T43 | Extract TypeScript scopes, uses, and imports, including TSX expressions/component names. | Top-level/anonymous-container uses, interpolation, aliases, and shadowing match gold; literal JSX text is excluded. |
| [ ] | T44 | Resolve supported direct relative imports/exports. | Local named/default bindings resolve uniquely; re-exports, package/path aliases, and ambiguous paths remain unresolved. |
| [ ] | T45 | Add TypeScript receiver hints and query integration. | Supported typed/new/this hints are scoped; refs and context work through the existing generic path without language branches in core. |
| [ ] | T46 | Exercise the pinned real TypeScript fixture and run two-language contract checks. | Known limitations are documented; full/incremental query bytes and coverage hold for both languages. |
| [ ] | T47 | Compare source estimates against the pilot model's tokenizer and rendered outputs. | Record error distributions on PHP/TS, Unicode, and minified samples; avoid silently changing budget semantics. |
| [ ] | T48 | Prepare held-out task checks and preregistration in small batches. | Corpus, endpoints, sample-size rationale, limits, retry rules, and projected spend are fixed before confirmatory runs. Creating a large corpus is split into batch subtasks. |
| [ ] | T49 | Execute/analyze the confirmatory benchmark only under an explicit run/spend budget. | Results report passed gates, failure, or inconclusive—not a selected successful-run comparison. Keep this operational job separate from ordinary coding sessions. |

Release tasks are deliberately not expanded yet. If results justify release, break [RELEASING](RELEASING.md) into separate packaging, ownership/license, CI/platform, artifact-test, and publication tasks. No publication is authorized by this planning document.

## Session handoff

Each session updates the checkpoint at the top and ends with a compact report:

```text
Task: Txx — done / partial / blocked
Changed: paths and the concrete behavior added
Checked: actual command(s), pass/fail
Remaining: only unfinished scope or a concrete blocker
Next: one task or subtask ID
```

Do not repeat the entire architecture or completed history. Preserve uncommitted work; initialize version control as a separate explicit setup action if needed, without renaming the workspace or publishing anything.
