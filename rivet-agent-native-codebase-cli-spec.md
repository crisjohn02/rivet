# RIVET — Agent-Native Codebase CLI
## Product & Technical Specification

**Status:** Draft v0.3  
**Working name:** `rivet`  
**Primary implementation language:** Rust  
**Primary purpose:** Low-level, deterministic codebase tooling designed for use by AI coding harnesses and agents.

**Changes in v0.3:** reconciled freshness, reference coverage, context budgets, and JSON contracts; specified atomic index updates and bounded failures; selected fixed context ranking for the MVP; added implementation acceptance gates. [OUTPUT-CONTRACT](docs/OUTPUT-CONTRACT.md) owns wire formats, [ARCHITECTURE](docs/ARCHITECTURE.md) owns storage and refresh behavior, and [IMPLEMENTATION-PLAN](docs/IMPLEMENTATION-PLAN.md) owns build order and acceptance checks. This spec owns product scope. Examples here are illustrative; the output contract is normative for serialization.

**Changes in v0.2:** added Related Work (§6), Symbol Identity (§10), Reference Resolution (§11), Harness Integration (§30); made index freshness a requirement (§12); specified the context ranking and budget algorithm (§16); tightened the benchmark protocol (§32); made the MVP definition a single section (§31); resolved several open questions; removed duplicated command lists.

---

# 1. Overview

`rivet` is a low-level command-line toolkit for codebase exploration, structural navigation, context retrieval, and eventually code editing.

It is designed primarily for **machine consumers**, especially AI coding harnesses such as Codex, Claude Code, and other autonomous or semi-autonomous software engineering agents.

The core idea:

> Current coding agents rely heavily on human-oriented Unix tools such as `grep`, `sed`, `awk`, `find`, `cat`, and raw file reads. These tools are extremely useful, but they are line-oriented and text-oriented rather than code-structure-oriented.

`rivet` provides deterministic primitives that expose source code using structural concepts:

- symbols and definitions
- references, callers, callees
- imports and types
- tests
- dependency relationships
- token-budgeted source context
- structural diffs and impact relationships (later)

`rivet` does **not** contain an AI agent. It does **not** reason about code. It returns structured, deterministic repository facts that an external agent reasons over.

---

# 2. Problem Statement

AI coding agents currently perform workflows similar to:

```text
search for a symbol
read a file
read neighboring files
search references
read definitions
search callers
inspect imports
read tests
modify files
run tests
read large error output
search again
edit again
```

using `grep`, `rg`, `sed`, `awk`, `find`, `cat`, `head`, `tail`, and `git diff`.

These are excellent general-purpose utilities, but they are not optimized for the economics of LLM-based coding agents. The main inefficiencies are:

1. **Excessive file reads**
2. **Excessive token consumption**
3. **Repeated repository rediscovery**
4. **Too many tool calls**
5. **Line-oriented output instead of semantic output**
6. **Large diffs and compiler/test logs being injected into model context**
7. **Fragile line-based editing**
8. **Lack of a unified machine-readable interface**
9. **Repeated parsing of the same repository**
10. **No explicit token-budget-aware context retrieval**

A less obvious inefficiency is **false positives**. `rg launch` returns every occurrence of the word, including comments, strings, and unrelated methods with the same name. The agent then spends reads and tokens ruling them out.

---

# 3. Goals

`rivet` should:

- reduce the number of tool calls needed to understand code
- reduce source lines and tokens sent into an LLM context window
- provide deterministic structural information
- be honest about the confidence of what it reports (see §11)
- expose stable JSON output for machine consumers
- work without any LLM dependency, network, or cloud account
- keep its index fresh automatically (see §12.4)
- have very fast CLI startup
- support multiple programming languages behind one abstraction
- remain useful independently of any specific AI vendor or harness
- compose cleanly with standard Unix tooling
- provide compact human-readable output by default and structured output through `--json`
- support explicit token or output budgets where appropriate

---

# 4. Non-Goals

`rivet` should not initially:

- contain an LLM or generate explanations
- make architectural judgments or decide whether code is good or bad
- replace compilers, language servers, Git, or test runners
- act as an autonomous coding agent or provide conversational interfaces
- infer product requirements
- modify code through probabilistic generation
- require a cloud service

Commands that do **not** belong in `rivet`:

```bash
rivet explain UserService
rivet fix this bug
rivet improve architecture
rivet write feature
```

Those are agent-level responsibilities. The correct level of abstraction is:

```bash
rivet symbol UserService.create
rivet refs UserService.create
rivet callers UserService.create
rivet context UserService.create --tokens 3000
```

---

# 5. Design Principles

## 5.1 Deterministic

Given the same source snapshot, tool and grammar versions, configuration, and query options, navigation commands return byte-identical output. Administrative commands (`init`, `index`) report the work performed, so their counters may differ between runs. Explicit timing is also excluded.

This requires explicit ordering everywhere. SQLite does not guarantee row order, so every result list is sorted by an explicit key (see §20.2). No probabilistic model is involved.

## 5.2 Machine-First

Output is optimized for consumption by coding harnesses. Human readability is still useful, but machine stability is more important.

## 5.3 Structured Before Textual

Prefer:

```json
{
  "id": "app/Services/SurveyService.php#App\\Services\\SurveyService::launch",
  "name": "launch",
  "qualified_name": "App\\Services\\SurveyService::launch",
  "kind": "method",
  "file": "app/Services/SurveyService.php",
  "start_line": 82,
  "end_line": 141
}
```

over large unstructured text output.

## 5.4 Honest About Uncertainty

Without a compiler or language server, some answers are heuristic. `rivet` labels each fact with how it was derived rather than presenting guesses as certainties. An agent that knows a reference is only a name match can verify it; an agent that is misled cannot.

## 5.5 Token-Aware

Where source context is returned, the caller can specify a maximum budget and `rivet` returns the highest-value context that fits.

## 5.6 Fast Startup

Commands may be invoked hundreds of times during an agent session. Startup latency must remain low.

## 5.7 Local First

The default mode requires no network, no cloud account, no API key, and no external AI model.

## 5.8 Incremental and Snapshot-Consistent

Queries refresh automatically and return positions and source from one committed index snapshot. Default content hashing detects edits even when file size and mtime are preserved. A filesystem scan is not an atomic filesystem snapshot: concurrent edits can occur after a read. The response identifies its snapshot and verification mode; detectable races trigger a bounded retry (§12.4).

## 5.9 Composable

```bash
rivet refs SurveyService.launch --json | jq '.references[].file'
```

## 5.10 Language-Agnostic Core

Language-specific behavior is implemented behind a common abstraction.

---

# 6. Related Work

`rivet` sits in a crowded space. Knowing what exists sharpens the positioning and avoids reinventing solved problems.

| Tool | What it does | Relationship to `rivet` |
|---|---|---|
| **ripgrep / grep** | Fast text search. | The baseline. Near-perfect recall for an identifier, zero structure. `rivet refs` only justifies itself by precision and structure, not recall. |
| **[universal-ctags](https://docs.ctags.io/en/latest/man/ctags.1.html)** | Multi-language tags, including reference tags for supported constructs. | Definition lookup is established functionality; reference support varies by parser. Rivet must earn its value through usable relationships and bounded context. |
| **ast-grep** | Tree-sitter structural search and rewrite, written in Rust. | Proves the Rust plus Tree-sitter stack. Pattern-based and stateless; no persistent index or graph. Its structural rewrite is relevant prior art for §18. |
| **GitHub stack-graphs** | Incremental, Tree-sitter-based name resolution without a compiler. | Addresses syntax-based name resolution in §11. Shows it is feasible and that per-language rule sets are large. Candidate resolution layer for a later version. |
| **[Aider repo map](https://aider.chat/docs/repomap.html)** | Repository map with graph ranking and a token budget. | Direct precedent for `rivet context`. Graph ranking is a later experiment against the fixed-priority MVP baseline (§16.3). |
| **Serena** | LSP-backed MCP server exposing navigation and editing to agents. | The alternative path that `rivet` argues against for v0.1 (see §19). A natural comparison target in the benchmark. |
| **SCIP / LSIF** | Precise code intelligence index formats from Sourcegraph. | Require per-language indexers and usually a build. Possible import or export format later. |
| **Kythe** | Build-integrated, compiler-precise code graph. | Shows the precision ceiling. Far too heavy for a local CLI. |
| **Language servers (LSP)** | Precise semantics per language. | Discussed in §19. Optional enrichment later. |

Positioning: `rivet` combines structural extraction, token-budgeted context, and a persistent relationship graph, exposed as a fast CLI with a stable JSON contract and explicit confidence labels.

---

# 7. Proposed Technology Stack

## Core Language

Rust: native single-binary distribution, low startup latency, strong performance and memory efficiency, safe concurrency, excellent CLI and parsing ecosystems, and precedent in ripgrep and ast-grep.

Tree-sitter grammars compile as C. Cross-compilation for release targets should be set up in CI from the first milestone, following ripgrep's approach.

## Parsing

Tree-sitter: parse source files, locate syntax nodes, identify symbols, imports, and calls, determine source ranges, support structural extraction.

## Persistence

SQLite: symbol index, file metadata, relationships, repository metadata, incremental indexing state.

## Likely Rust Libraries

```text
clap
serde
serde_json
tree-sitter
rusqlite
rayon
ignore
anyhow
thiserror
```

Possible later dependencies:

```text
tokio
notify
```

---

# 8. High-Level Architecture

```text
                ┌───────────────────────┐
                │      rivet CLI        │
                └───────────┬───────────┘
                            │
                 ┌──────────▼──────────┐
                 │     rivet-core      │
                 │  common data model  │
                 └──────┬───────┬──────┘
                        │       │
           ┌────────────▼──┐  ┌─▼──────────────┐
           │ rivet-parser  │  │  rivet-index   │
           │  Tree-sitter  │  │ relationships  │
           └───────┬───────┘  └───────┬────────┘
                   │                  │
        ┌──────────▼──────────┐  ┌────▼──────────┐
        │   rivet-languages   │  │  rivet-store  │
        │   PHP / TypeScript  │  │    SQLite     │
        └─────────────────────┘  └───────────────┘
```

Possible workspace:

```text
rivet/
├── Cargo.toml
├── crates/
│   ├── rivet-cli/
│   ├── rivet-core/
│   ├── rivet-parser/
│   ├── rivet-index/
│   ├── rivet-store/
│   └── rivet-languages/
│       └── src/
│           ├── typescript/
│           └── php/
└── tests/
```

---

# 9. Language Abstraction

Language adapters return owned named definitions, identifier uses, import/lexical scope facts, and receiver hints. The generic index resolves cross-file declaration links; adapter hints are not final resolution tiers. This preserves enough evidence to re-resolve unchanged files without reparsing them.

[ADDING-A-LANGUAGE](docs/ADDING-A-LANGUAGE.md) owns the conceptual trait, grammar variants, supported syntax, case rules, and fixture requirements. PHP is first; TypeScript and TSX follow. Other languages are deferred until after the benchmark gate.

---

# 10. Symbol Identity and Addressing

Agents need a stable way to name a symbol, and `rivet` needs a deterministic answer when a name is ambiguous.

## 10.1 Canonical ID

Every named symbol has a canonical ID:

```text
<repo-relative path>#<language-native qualified name>
```

Examples:

```text
app/Services/SurveyService.php#App\Services\SurveyService::launch
src/services/survey.ts#SurveyService.launch
internal/survey/service.go#Service.Launch
```

Every JSON result that mentions a symbol includes `id`, `name`, `qualified_name`, `kind`, `file`, and `language`. Paths always use `/` regardless of platform.

Encode literal `%` and `#` within the path and qualified-name components as `%25` and `%23` before joining with `#`. Encoding is decoded only after splitting, and canonical IDs are matched before query normalization. IDs are stable across line-only edits, but not file moves or renames. For duplicate qualified names in a file, suffix every member with a one-based ordinal in `(start_byte, end_byte, kind)` order, including `#1`; insertion or removal may change these IDs. Anonymous definitions are not addressable symbols in v0.1; their uses remain indexed with the nearest named container or null.

## 10.2 Accepted Query Forms

All symbol-taking commands accept any of:

| Form | Example | Notes |
|---|---|---|
| Short name | `launch` | Matches any symbol with that name. |
| Dotted path | `SurveyService.launch` | Language-neutral. `rivet` normalizes `::`, `->`, `\`, `/`, and `.` to the same separator when matching. |
| Native qualified name | `App\Services\SurveyService::launch` | |
| File and line | `app/Services/SurveyService.php:82` | Resolves to the innermost symbol enclosing that line. Useful when the agent starts from a stack trace or a diff. |
| Canonical ID | `app/Services/SurveyService.php#App\Services\SurveyService::launch` | Always unique. |

## 10.3 Ambiguity

If a query matches more than one symbol, `rivet` exits with code 5 and returns the candidate list, so the agent can pick deterministically without a second search:

```json
{
  "error": "ambiguous_symbol",
  "message": "query 'SurveyService.launch' matched 2 symbols",
  "total": 2,
  "truncated": false,
  "next_offset": null,
  "candidates": [
    { "id": "app/Services/SurveyService.php#App\\Services\\SurveyService::launch", "kind": "method", "file": "app/Services/SurveyService.php", "start_line": 82 },
    { "id": "app/Legacy/SurveyService.php#App\\Legacy\\SurveyService::launch", "kind": "method", "file": "app/Legacy/SurveyService.php", "start_line": 14 }
  ],
  "hint": "Re-run with one of the candidate ids."
}
```

v0.1 deliberately has no `--all` or `--first`: an agent must select a returned canonical ID. Candidate lists support `--limit` and `--offset`, with `total` and `truncated`. `file:line` uses a repository-relative path and a positive line parsed from the final colon. If several innermost symbols share that line, return ambiguity instead of guessing. Dotted/native queries match complete trailing name components; short names match whole identifiers using language-specific case rules.

---

# 11. Reference Resolution Without a Compiler

This is the hardest part of the project and the largest risk to the hypothesis. It deserves to be stated plainly.

## 11.1 The Problem

Tree-sitter provides syntax, not semantics. It can report "here is a call to a method named `launch` on some receiver." It cannot, by itself, report that `$this->service->launch()` in a controller refers to `SurveyService::launch` rather than some other class's `launch`. In PHP and TypeScript, with interfaces, dependency injection containers, and dynamic dispatch, that gap is most of the difficulty.

## 11.2 Where `rivet refs` Can Beat `rg`

`rg launch` finds text containing that spelling, including non-references, but misses differently spelled aliases and runtime-generated names. Rivet can improve navigation in three ways:

1. **Separating literal text from code uses.** Comments and literal string portions are excluded; interpolated expressions remain code.
2. **Attaching structure.** The containing symbol, the reference kind, and the receiver expression. These require language-specific extraction but not a full compiler.
3. **Filtering out same-name members on unrelated types.** Hard. This is the part the examples in this document promise and the part that requires real resolution work.

Items 1 and 2 are the guaranteed value of v0.1. Item 3 is delivered incrementally and always labeled.

## 11.3 Resolution Tiers

Every reference, caller, and callee carries a `resolution` field. A tier describes evidence for a declaration link, not certainty about the implementation called at runtime.

| Tier | Meaning | Examples |
|---|---|---|
| `exact` | A supported lexical binding identifies one declaration without receiver/type assumptions. | A direct named import whose module/export is resolved; a lexically bound named function with shadowing accounted for. |
| `scoped` | A visible receiver/type/namespace heuristic identifies one declaration. | `$this->launch()`, `self::launch()`, or a receiver explicitly typed `SurveyService`. Dynamic dispatch can still select another implementation. |
| `name_match` | Spelling alone is evidence, or binding is unsupported/ambiguous. | An untyped receiver, a dynamic binding, or a name unique in the index but not lexically bound. |

Human output marks `name_match` results with `?`. `--min-resolution exact|scoped|name_match` defaults to `name_match`, except for the `symbol` call lists, which default to `scoped` and count the name-only rows they leave out (§14). An empty result never proves that no runtime references exist.

## 11.4 v0.1 Receiver Heuristics

Apply the following only within supported lexical scopes:

1. `this` / `self` / `$this` inside a class body → `scoped`.
2. A local assigned from `new Foo(...)` before the use, with no intervening reassignment or uncertain control flow → `scoped`.
3. A visible explicit parameter/property/variable type → `scoped`.
4. A direct import with a supported module/export lookup and no shadowing → `exact` for the imported declaration; member access still requires receiver evidence.
5. Unique spelling across the index → `name_match`, never an automatic tier upgrade.

Reject conflicting hints and multiple candidates. No inferred types, inheritance traversal for binding, interface-to-implementation mapping, PHP late-static dispatch resolution, or TypeScript path-alias/re-export/package resolution in v0.1. Declared supertypes are stored, but only reference-mode exclusion (§11.5) reads them; they never bind a use or change a tier. Unsupported forms stay visible as candidates; the language support matrix in [ADDING-A-LANGUAGE](docs/ADDING-A-LANGUAGE.md) defines the boundary.

## 11.5 Coverage and Reference Modes

`refs` defaults to `--mode references`: include uses linked to the target and unresolved uses with the same name; exclude uses confidently linked to another declaration. Aliased imports contribute references through their binding, even when their spelling differs from the target name.

**Evidence-based exclusion (LR2).** In `--mode references`, an unresolved use whose name matches the target is also excluded when evidence shows it cannot refer to the target. There are exactly two kinds of evidence:

1. *Use form incompatible with the target kind.* The table below is derived from the forms the PHP extractor records (a "receiver" is `->`, `?->`, or `::`):

   | Target | Compatible unresolved use forms |
   |---|---|
   | method | `call` or `read` with a receiver (`__get` can dispatch a property read to a method; a `write` stays excluded, since `__set` never invokes a same-named method) |
   | function | `call` without a receiver; `import` |
   | property | `read` or `write` with a receiver |
   | class constant or enum case | `read` with a receiver |
   | global constant | `import` |
   | class, interface, enum, trait | `type` (including `new`, `instanceof`, `extends`/`implements`, the class before `::`); `import` |
   | namespace | every form |

   Any other form is excluded. `unknown` is never excluded: the extractor records bare constant names, qualified constant names, and a trait `use` inside a class body that way. `assignment` is never recorded by the PHP extractor and is never excluded. Rule 1 does not tell a class-constant read from an instance-property read with a receiver (both are `read`), so neither excludes the other. The extractor records no callable strings or callable arrays (single-quoted strings and literal string portions are not code), so no such form exists to keep or exclude.

2. *Receiver class known, with no possible common instance.* A member or scoped use whose receiver class R was determined by a receiver rule (§11.4: `$this` or `self` inside a class body, a trusted `new` assignment, an explicit typed receiver, or a class named before `::`), under exactly the trust conditions that rule applies before binding, is excluded only when no object could be an instance of both R and the target's declaring class-like T. Rationale: exclusion must be sound; an excluded use cannot dispatch to the target under the stated assumption.

   Let `up(X)` be X plus its transitive declared supertypes (`extends`/`implements`, from the stored hierarchy), and let the *subtypes of T* be every class-like X with T in `up(X)`: indexed classes, interfaces, and enums, including T itself, and every anonymous class, whose header supertypes are recorded for this purpose although it is not a symbol. Two names are the same class when they share an indexed declaration or their qualified names are equal under ASCII case-insensitive comparison. R need not be indexed: a spelling no indexed class-like has keeps the qualified name PHP's compile-time name resolution gives it. R and T are related, and the use is kept, when:
   - (a) R is in `up(X)` for some subtype X of T; or
   - (b) R is not indexed and some subtype X of T has an unindexed link in its ancestor closure, since the unknown part of X's chain might contain R (so `Model $m; $m->save()` is kept for `App\User::save` when `User` reaches a vendor class).

   Assumption: an unindexed (vendor) class never extends or implements an indexed class, so an indexed T is in `up(X)` only for an indexed or anonymous X. Never exclude, in addition, when:
   - T is, or may be, a trait (a trait is class-kind; only a declaration header that starts with `class`, `final`, `abstract`, or `readonly` after its attributes proves a class), or R is an indexed declaration that may be a trait: trait use is not tracked;
   - the receiver rule refuses to determine a class (conflicting or rebound hints, a union type, `static::`, `parent::`, `self` as a declared type, an anonymous class, an unattributed namespace);
   - R's name has no qualified name (two imports claim its alias, or its namespace is unattributed), or an indexed class-like with that qualified name exists that the rule could not select uniquely;
   - any declared supertype in the snapshot, of a named or an anonymous class, has no qualified name: it could name any class, so rule 2 excludes nothing in that snapshot.

Exclusion is not resolution: an excluded use stays unresolved, is never bound, and keeps its tier; `--mode candidates` lists it exactly as before. A use bound to the target is never excluded. Exclusion applies after the kind and resolution filters and before pagination; `refs` reports the per-evidence counts as `by_exclusion` (OUTPUT-CONTRACT). `context` callers and `symbol.called_by` use reference-mode matching and inherit the rule; `symbol.calls` (containment) is unaffected.

`--mode candidates` is the lexical audit mode: also include every extracted identifier use with the target's normalized unqualified name, including those bound elsewhere. Such unrelated candidates are reported as `name_match` relative to the queried symbol, with their actual `resolved_target` retained for inspection. Apply kind/tier filters before pagination; a call site appears once, with kind `call`.

There is no claim of universal semantic recall or a superset of raw `rg` output. Definitions are not references; comments, literal text, ignored files, unsupported syntax, aliases, and dynamic names make that comparison invalid. Coverage tests use hand-labeled identifier-use spans and declaration bindings in supported files. Candidate mode must cover all labeled same-name use spans; reference mode must include gold target links and exclude gold links to other definitions. Track precision and recall separately by tier. Report skipped or parse-failed files through `coverage`, rather than interpreting a partial index as a complete negative answer.

Interpolated expressions inside strings/templates are code and must be extracted; literal portions are not. Rename safety and runtime completeness remain outside the MVP.

---

# 12. Repository Index

## 12.1 Layout

```text
.rivet/
├── index.db
└── config.toml
```

`rivet init` creates the directory and adds `.rivet/` to `.gitignore` if not present.

## 12.2 Stored Data

**Files:** path, language, mtime, size, content hash, parse status.

**Symbols:** id, name, qualified name, kind, file, start and end byte, start and end line, parent symbol, language, signature text.

**Relationships:** `defines`, `references`, `calls`, `imports`, `extends`, `implements`, `contains`, `tested_by`. Reference and call links carry a `resolution` tier (§11.3). The MVP stores named symbols, their containment, import bindings, and identifier uses (including calls and type positions). Parent relationships and reference-based test context are derived from these records. Declared `extends`/`implements` supertypes are stored (T36d) and read only by reference-mode exclusion (§11.5), never to resolve or bind a use; inheritance traversal for binding and a separate `tested_by` graph are deferred. Top-level uses have a nullable containing symbol and are never discarded.

## 12.3 Incremental Indexing

Only files whose content or parser fingerprint changed are reparsed. Store the exact parsed source bytes so positions, hashes, and returned source agree. Deleted or newly excluded files lose their facts. Config, ignore rules, enabled languages, grammar/extractor versions, and resolver versions participate in invalidation.

Retain unresolved uses and import/scope facts independently of resolved links. For the MVP, recompute all declaration bindings whenever indexed content changes; this handles new duplicates, removed definitions, aliases, and changed imports correctly. Selective re-resolution is a measured optimization for later.

Publish file metadata, source, symbols, uses, resolved links, diagnostics, and a deterministic snapshot digest in one transaction. A failed refresh leaves the previous complete snapshot intact and returns an error; it does not silently answer from that snapshot. See [ARCHITECTURE](docs/ARCHITECTURE.md).

## 12.4 Automatic Freshness (Requirement)

Every query refreshes before answering:

1. Walk all eligible files using repository-local ignore rules. `git status` is not a replacement: it describes changes against Git, not against Rivet's last snapshot.
2. Default `--freshness content`: hash eligible files, compare hashes, and parse only changed content. `--freshness metadata` is an explicit faster mode that trusts size/mtime and can miss preserved-metadata edits.
3. Recheck observed file metadata and the eligible path set before commit. Retry the whole refresh once on a detected race; continued mutation returns `repository_changed` (exit 9).
4. Read all query data and source from one committed snapshot. Never combine old index positions with fresh filesystem reads.

No filesystem walk promises a simultaneous view of all live files or detects every possible concurrent writer. The contract is consistency with the bytes actually indexed, plus content verification of an unchanged working tree, not a lock on the user's editor.

`--no-refresh` is explicit cached access. It requires a compatible existing index, sets `freshness: cached`, and is incompatible with `--freshness`. No query silently chooses it. Content hashing cost must be measured; the old unconditional 50 ms freshness claim is removed. Counters for refresh work belong to `index`, not deterministic query output.

---

# 13. `rivet index`

Build or update the repository index.

```bash
rivet index
rivet index --force
rivet index --json
rivet index --languages php,typescript
```

Possible output:

```text
Indexed 4,821 files
32,418 symbols
18,220 relationships

Updated: 312
Unchanged: 4,509

Elapsed: 421 ms
```

Requirements: respect `.gitignore`, ignore `.git` and `.rivet`, ignore common dependency directories by default (§26), avoid reparsing unchanged files.

---

# 14. `rivet symbol`

Locate and describe a symbol.

```bash
rivet symbol SurveyService.launch
```

Human output:

```text
SurveyService.launch
method
app/Services/SurveyService.php:82-141

signature:
  public function launch(Survey $survey, LaunchOptions $options): LaunchResult

calls:
  SurveyValidator.validate
  AllocationEngine.allocate
  Survey.save

called by: (+1 name-only not listed)
  SurveyController.launch
  LaunchSurveyJob.handle
```

Both call lists default to `--min-resolution scoped`: they list `exact` and `scoped` rows and leave out name-only (`name_match`) rows, such as a `QueueWorker.dispatch` caller seen only by its spelling. Each list counts the name-only rows it leaves out, as `(+N name-only not listed)` on its heading in human output and as `hidden_name_match` in JSON, so a list whose rows are all name-only never reads as empty. `--min-resolution name_match` lists those rows too, marked `?`. `refs` and `context` keep name-only results by default.

JSON uses a versioned envelope containing `index`, `symbol`, nullable signature/doc/parent fields, and independently paginated `calls` / `called_by` lists. See [OUTPUT-CONTRACT](docs/OUTPUT-CONTRACT.md). Call lists require Milestone 3; earlier development builds do not claim final MVP support.

Options:

```text
--json
--source            include the full symbol body
--signature-only    omit calls and called_by; conflicts with --source
--min-resolution exact|scoped|name_match   call lists; default scoped
--limit N
--offset N
```

---

# 15. `rivet refs`

Return references to a symbol.

```bash
rivet refs SurveyService.launch
```

```text
3 references  (2 scoped, 1 name_match)

app/Http/Controllers/SurveyController.php:91   SurveyController.launch   call
app/Jobs/LaunchSurveyJob.php:44                LaunchSurveyJob.handle    call
tests/Feature/SurveyLaunchTest.php:118         SurveyLaunchTest.test_launch   call ?
```

JSON output per reference: `file`, `line`, `column`, byte range, nullable `containing_symbol` (symbol object), `ref_kind`, `resolution`, nullable `resolved_target` (symbol ID), and nullable `receiver`. See the normative [output contract](docs/OUTPUT-CONTRACT.md).

Reference kinds: `call`, `type`, `import`, `assignment`, `read`, `write`, `unknown`. The MVP may classify only `call`, `import`, `type`, and `unknown`.

Options:

```text
--json
--min-resolution exact|scoped|name_match
--kind call,type,...
--mode references|candidates
--limit N
--offset N
```

---

# 16. `rivet context`

Expected to be the most important command.

> Return the smallest useful structural context around a symbol within a specified token budget.

```bash
rivet context SurveyService.launch --tokens 3000
```

## 16.1 Output

A sequence of source segments, each with metadata:

```text
── app/Services/SurveyService.php:82-141  SurveyService.launch  [target, full]
<source>

── app/Models/Survey.php:12-19  Survey  [type, signature]
class Survey extends Model
{
    public string $status;
    public function save(): bool { … }
}

── app/Services/AllocationEngine.php:40-81  AllocationEngine.allocate  [callee, full]
<source>

── tests/Feature/SurveyLaunchTest.php:100-167  SurveyLaunchTest  [test, signature]
...

estimated_tokens: 2814 / 3000  (tokenizer: utf8-bytes-v1; budget_scope: source)
```

JSON: `segments[]` with a nested `symbol`, `form`, `reason`, `resolution`, `estimated_tokens`, and `source`. The output contract defines their exact shapes.

## 16.2 Collapsed Form

The largest token saving available is returning a symbol as its **signature only**: declaration line(s), doc comment, and for containers the member signatures, with bodies replaced by `{ … }`. A collapsed class tells the agent what exists and how to call it at a fraction of the cost of its body.

Every candidate has two representations, full and signature, with independent token estimates. Partial-body extraction (returning "the 18 relevant lines" of a symbol) is out of scope for the MVP; it is hard to make deterministic and hard for an agent to trust.

```text
--collapse auto      default: full if it fits, else signature, else skip
--collapse always    signatures only, except the target
--collapse never     full bodies only
```

## 16.3 Ranking

Start with fixed priorities, no PageRank. Collect supported declaration links up to `--depth` hops (1 or 2); name-only links do not expand context by default. A target has depth 0. Direct type definitions, callees, callers, and imports are depth 1; parent is also eligible at depth 1. A reference from a file matching configured test globs supplies reason `test`; this is a source relationship, not test coverage or proof of execution.

Use this ascending integer tuple:

```text
(depth, reason_priority, resolution_priority, file_utf8_bytes, start_byte, id)
reason_priority: target=0, type=1, callee=2, test=3, caller=4, import=5, parent=6, second_degree=7
resolution_priority: exact=0, scoped=1, name_match=2
```

The target is always first. At depth 2 the reason is `second_degree`, and resolution is the weakest link in the selected path. Deduplicate candidates by ID, retaining the best tuple. Apply include/exclude flags before traversal; disabled relationships cannot reappear through another hop. Use only direct imports consumed within the candidate symbol, not every dependency in its file. No sibling expansion through containment. PageRank is a later ablation, requiring evidence that it improves utility.

## 16.4 Budget Fitting

`--tokens` bounds the **estimated source payload**, not JSON metadata, human headers, or a model's actual token count (§23). All successful responses satisfy the declared estimate. No partial source truncation in the MVP.

1. Try the target's full body. If it does not fit, try its signature for `auto` or `always`; for `never`, fail with `budget_too_small` (exit 8). If the allowed target form still cannot fit, return the error with `required_tokens`.
2. Visit other candidates in rank order. `auto` tries full, then signature; `always` tries signature only; `never` tries full only. Skip a candidate if no allowed form fits and continue to later smaller candidates.
3. A full emitted source span suppresses candidates wholly contained within it. Avoid duplicate source bodies from parent/child overlap; a related ancestor may use a nonoverlapping signature, otherwise skip it. Signature summaries omit member declarations already emitted separately. Report omissions by budget, overlap, and limit.
4. Emit at most `--limit` segments (default 50, target included). Hard traversal caps are 1,000 unique candidates and 10,000 examined uses. Traverse frontier symbols in tuple order and uses by `(file, start_byte, end_byte, ref_kind)`; stop before exceeding a cap and report `candidate_limit_reached`. Do not claim an exhaustive omitted total if traversal was cut short.

The result is deterministic for a snapshot and options. `--collapse always` still prefers the target in full if it fits. The builder counts estimates on the final emitted representations, after overlap removal.

## 16.5 Options

```text
--tokens N
--depth 1|2
--include-tests / --exclude-tests
--include-callers / --exclude-callers
--include-callees / --exclude-callees
--collapse auto|always|never
--limit N             maximum segments, including target
--json
```

---

# 17. Future Commands

None of these are in the MVP. They are listed once here; §35 refers back rather than repeating them.

## 17.1 `rivet callers` / `rivet callees`

Symbols that invoke, or are invoked by, the target. Both are projections of data already collected for `rivet symbol` and carry `resolution` per edge. Cheap once Milestone 3 lands.

## 17.2 `rivet tests`

Tests that reference the target, found by references first and by naming convention second, each labeled with how it was found.

## 17.3 `rivet imports` / `rivet types`

Import graph for a file or symbol; type definitions a symbol depends on.

## 17.4 `rivet slice`

A program-oriented slice: imports, types, target, direct helpers, constants, and errors raised. Later-stage.

## 17.5 `rivet impact`

Deterministic graph fan-out from a symbol:

```json
{
  "symbol": "app/Models/Survey.php#App\\Models\\Survey::$status",
  "direct_references": 14,
  "affected_symbols": [ "...", "..." ],
  "affected_tests": [ "...", "..." ]
}
```

It exposes graph relationships. It does not produce risk scores.

## 17.6 `rivet diff --semantic`

Structural summary of a change set instead of a raw diff:

```text
17 files changed
6 symbols modified
1 public signature changed
3 tests added

SurveyValidator.validate
  body modified
  calls new function validateQuota
SurveyController.create
  catches new QuotaExceeded
Survey.status
  enum gained PAUSED
```

Raw diff remains available through Git or `rivet diff --raw`.

## 17.7 `rivet errors`, `rivet repo`

Structured compiler and test output; repository overview. Speculative.

---

# 18. Editing

Editing is deferred until the read and navigation primitives are stable and benchmarked.

Long-term editing prefers structural operations over line-number replacement:

```bash
rivet rename User.emailAddress User.email
rivet edit SurveyService.launch --replace-body-from patch.php
```

Requirements: AST-aware, fail safely, validate the expected target before writing, preserve formatting, never silently modify ambiguous targets, optionally run a formatter, always expose the exact resulting diff. ast-grep's rewrite engine is relevant prior art.

---

# 19. Why LSP Is Not Required for v0.1

LSP provides precise semantics but adds: server installation per language, process lifecycle, initialization and workspace configuration, server-specific behavior, startup latency, and dependency management. Serena (§6) takes this path and is the right comparison for whether the cost is worth it.

v0.1 extracts what it can from the filesystem, Tree-sitter, and the index, and labels its confidence (§11). LSP may later become an optional enrichment layer that upgrades `name_match` edges to `exact`.

Version ladder:

```text
v0.1  index, symbol, refs, context; incremental indexing; automatic freshness; one or two languages
v0.2  callers, callees, tests; interface and trait resolution; more languages
v0.3  impact, semantic diff
v0.4  optional LSP enrichment
v0.5  structural editing
```

---

# 20. Output Contract

## 20.1 JSON

Every MVP command supports `--json`, including `init` and `snippet`. Each response has `schema_version: 1`. Success is exactly one UTF-8 JSON object and a newline on stdout. Failure is exactly one JSON error object on stderr and no stdout. Progress is suppressed in JSON mode; diagnostics live in the response. Help/version remain text. `--json-lines` is deferred.

[OUTPUT-CONTRACT](docs/OUTPUT-CONTRACT.md) is authoritative for fields, nullability, errors, counts, and ordering. Additive object fields are allowed; changes to existing types, enum sets, ordering, or semantics require a schema bump after release.

## 20.2 Ordering

References and call sites sort by file bytes, byte offset, kind, and target ID; symbols/candidates by file bytes, start byte, and ID. Context segments retain ranking order (§16.3). Suggestions sort by edit distance then name bytes. JSON keys have a fixed serializer order.

## 20.3 Errors

Errors have stable `error`, `message`, and `hint` fields and the exit codes below. Partial repository coverage is not a fatal query error unless the explicitly selected target file is unavailable or unparseable. Successful partial answers expose coverage and diagnostics. Machine consumers must check them before treating absence as evidence.

## 20.4 Size Discipline

`--limit` defaults to 50 (1–1,000). `--offset` defaults to 0 and applies to refs, ambiguity candidates, and independently to each symbol call list. Counts are computed after filters but before pagination. Context uses a segment limit and source budget, not an offset. A list reports its total and whether any elements were omitted. Source retrieval is opt-in except for budgeted context. Source size is also bounded by the indexed file-size limit.

---

# 21. Exit Codes

```text
0  success
1  general failure
2  invalid arguments
3  repository or index unavailable
4  symbol not found
5  ambiguous symbol (candidates returned, see §10.3)
6  parse failure
7  unsupported language
8  context budget too small
9  repository changed during refresh
```

Exact values may change before release; after release they are stable.

---

# 22. Configuration

`.rivet/config.toml`:

```toml
[index]
respect_gitignore = true
exclude = ["vendor/**", "node_modules/**", "dist/**", "build/**"]
max_file_size_kb = 1024
freshness = "content"                 # content | metadata

[languages]
enabled = ["php", "typescript"]

[context]
default_token_budget = 4000
include_tests = true
test_globs = ["tests/**", "**/*.test.ts", "**/*.spec.ts"]
max_depth = 2
collapse = "auto"

[output]
default_limit = 50
```

Command-line arguments override repository configuration, which overrides built-in defaults. Reject unknown keys and invalid values. Language overrides apply to the current invocation; the effective indexing configuration is fingerprinted and a later different configuration invalidates the cache. No global configuration in v0.1.

---

# 23. Token Estimation

For the first prototype use a fully specified baseline: `estimate(source) = ceil(UTF8_byte_length(source) / 3)`, with empty source estimated as zero. Segment estimates are summed; report `tokenizer: "utf8-bytes-v1"` and `budget_scope: "source"`. This is a deterministic budget unit, not a guaranteed upper bound for any model tokenizer. Metadata and formatting overhead are excluded and must be measured separately in the benchmark.

Before release, compare it with the benchmark model's tokenizer on representative PHP and TypeScript samples, including Unicode and minified files; record error percentiles and total rendered output tokens. Any future estimator uses a different explicit ID and updates snapshots; changing budget semantics after schema freeze is breaking. Do not advertise a hard model-token cap without an actual tokenizer and full-response accounting.

---

# 24. Performance Goals

Aspirational targets, measured on a 5,000-file repository:

```text
metadata-mode freshness, no changes    < 50 ms
content-mode freshness, no changes     measure bytes read and p50/p95; no pre-measurement SLA
cached symbol query                    < 20 ms (plus freshness check)
cached reference query                 < 50 ms (plus freshness check)
context query, 3,000 source units      < 150 ms (plus freshness check)
incremental reindex, one file changed  < 100 ms
```

Report end-to-end latency as well as these component costs on pinned hardware, release builds, corpus file count/bytes, and cold/warm cache conditions. Include initial indexing, no-change refresh, one-file edit, declaration deletion, branch switch, and a high-fanout symbol. Also record index size and peak memory. These are aspirations, not release claims; correctness gates take priority.

---

# 25. Repository Discovery

Walk upward from the working directory and select the nearest ancestor containing `.rivet/` or a `.git` file/directory; stop at the first such boundary. This supports Git worktrees and avoids selecting an outer project's cache. A query auto-creates `.rivet/` only at a Git root and never edits instruction files or `.gitignore`. Outside Git, `init` explicitly establishes the current directory as root.

`init` is idempotent: preserve existing configuration and append `.rivet/` to the root `.gitignore` only if needed. `--write-snippet` updates a marked managed block in the sole existing `AGENTS.md` or `CLAUDE.md`; when neither exists, create `AGENTS.md`. When both exist, require `--snippet-file AGENTS.md|CLAUDE.md` (exit 2 otherwise). Validate before writing; preserve text outside the block and reject malformed/duplicate blocks. Explicit snippet destinations are root-relative and limited to these two files in v0.1. An explicit --snippet-file overrides auto-selection and creates the selected file if missing.

# 26. Ignoring Files

Always exclude `.git/` and `.rivet/`. Default dependency/build exclusions are `node_modules/`, `vendor/`, `dist/`, `build/`, `target/`, and `coverage/`. Configured exclusions are additional. Walk regular files only; skip symlinks and nested Git repositories/submodules in v0.1. Hidden files are eligible unless excluded. Use repository-local `.gitignore` rules at every level and `.git/info/exclude` when present; disable global ignore configuration for reproducibility. `respect_gitignore = false` disables Git ignore rules, not mandatory or configured exclusions. Untracked files are eligible. Do not execute Git or repository hooks during scanning.

UTF-8 paths and UTF-8 source are the supported v0.1 input domain. Report non-UTF-8 paths with escaped-byte diagnostics; never lossy-convert them into colliding IDs. Skip invalid UTF-8 source, binary files, and files over `max_file_size_kb * 1024` bytes, recording coverage. Generated code follows the same rules and can be excluded explicitly.

# 27. Security and Failure Boundaries

Parsing and indexing execute no source, scripts, hooks, package installation, or network requests. Reject symlinked `.rivet`, database/config files, and write destinations; output paths must remain within the selected root. Open regular source files without following symlinks and validate file identity around reads. Bound file reads even if a file grows after stat; never open FIFOs/devices. Preserve permissions when updating managed text files.

A syntax-error or parser-resource-limit file contributes no symbols/uses in v0.1; remove its previous facts and report diagnostics. Other files remain queryable with partial coverage. Targeting that file by path/ID returns exit 6. Files unreadable because of I/O/permissions fail refresh with exit 3 instead of silently preserving old facts. Tree-sitter recovery must not cause old facts to be mislabeled as current.

Parser work must have cooperative cancellation/resource bounds, initially one million visited syntax nodes and 500,000 extracted uses per file, plus the file-size limit. Native crashes cannot be converted into normal file diagnostics; process isolation is future hardening. Deterministic limits avoid wall-clock-dependent query results. Cancellation rolls back unpublished work. Database lock waits are bounded to 5 seconds, then exit 3 with a retry hint.

---

# 28. Compatibility

Initial build and test targets:

```text
Linux x86_64
Linux ARM64
macOS ARM64
macOS x86_64
```

Windows has a planned CI build; runtime support must be backed by recorded tests. Cross-platform behavior is planned from the start:

- all path handling through `std::path`; output paths normalized to `/`
- line counting tolerant of CRLF
- case-insensitive filesystem awareness in the file table
- a Windows build in CI even before it is manually tested

---

# 29. Installation

```text
cargo install <verified-package-name> --bin rivet
```

Package ownership and release URLs are unverified. This is an installation template, not an available release; see [INSTALL](docs/INSTALL.md).

Prebuilt releases:

```text
rivet-linux-x86_64
rivet-linux-aarch64
rivet-darwin-aarch64
rivet-darwin-x86_64
```

Package managers later.

---

# 30. Harness Integration

The consumers are Claude Code, Codex, and similar harnesses. A tool they do not know exists, or do not know when to prefer, delivers nothing. This is part of the MVP because the benchmark depends on it.

## 30.1 LLM-Readable Help

`rivet --help` and `rivet help <command>` are written for a model reader: under 40 lines, examples first, and an explicit "use this instead of `rg` when…" line per command.

## 30.2 Drop-In Snippet

`rivet snippet` prints a short block suitable for `CLAUDE.md`, `AGENTS.md`, or a skill file, describing the three navigation commands and when to reach for them. `rivet init --write-snippet` manages one idempotent block under the rules in §25. The benchmark's `rivet` configuration uses exactly this snippet and nothing more, so results reflect what a real user would get.

## 30.3 Self-Describing Errors

Every error carries a `hint` with the next command to try (§20.3). Ambiguity returns the candidates. Symbol-not-found suggests the closest names. The goal is that an agent recovers in one step rather than falling back to `rg`.

## 30.4 Why CLI Before MCP

Both target harnesses call shell tools natively, and a CLI lets the benchmark's control and treatment configurations use the same tool-calling mechanism. An MCP server is a thin wrapper to add later; it is excluded from the MVP.

## 30.5 User Setup and Optional Skills

[INTEGRATION](docs/INTEGRATION.md) defines the normal user workflow: install the binary in the agent's execution environment, install the managed block into the selected project's `AGENTS.md` or `CLAUDE.md`, and use the existing coding session normally. An explicit `--snippet-file` overrides automatic selection and creates the selected file if missing. Navigation maintains a writable local cache under the host's ordinary permissions.

An optional instruction-only skill can expose an explicit Rivet workflow after the PHP pilot; it uses the same commands in the current agent session. It adds no model, daemon, or separate agent. The default benchmark remains snippet-only; skill variants need separate evaluation. No skill installation command or MCP implementation is added to the MVP.

---

# 31. MVP Scope and Milestones

This section is the single definition of the MVP.

## 31.1 Required Commands

```text
rivet init
rivet index
rivet symbol
rivet refs
rivet context
rivet snippet
```

## 31.2 Required Infrastructure

- Rust workspace with CI cross-builds for §28 targets
- Tree-sitter integration
- SQLite index with incremental updates and automatic freshness (§12.4)
- `.gitignore`-aware traversal
- canonical symbol IDs and ambiguity handling (§10)
- resolution tiers on every edge (§11)
- stable, explicitly ordered JSON (§20)
- PHP first, then TypeScript/TSX; exact support boundary in ADDING-A-LANGUAGE.md

## 31.3 Explicitly Excluded

```text
LSP · semantic diff · impact analysis · structural editing · rename
daemon mode · MCP server · embedded AI · vector embeddings · cloud services
partial-body context extraction · type inference beyond §11.4
PageRank · JSON Lines · multi-target --all/--first · JavaScript support
```

## 31.4 Milestones

| # | Milestone | Success criterion |
|---|---|---|
| 0 | Contract fixtures and dependency spike | Executable JSON examples, pinned grammar/toolchain choices, and tiny PHP/TSX grammar smoke tests; see IMPLEMENTATION-PLAN.md |
| 1 | Repository scanner: root detection, `.gitignore`, language detection, hashing, SQLite persistence, freshness check | `rivet index` tracks files incrementally; a query after an edit reflects the edit |
| 2 | Symbol extraction: classes, functions, methods, interfaces, structs, enums, modules; canonical IDs | `rivet symbol Foo` locates symbols; ambiguous queries return candidates |
| 3 | References and relationships with resolution tiers; coverage and binding tests (§11.5) pass | `rivet refs Foo` returns labeled references; `calls` and `called_by` populate |
| 4 | Context builder: ranking, collapse, budget fitting | `rivet context Foo --tokens 3000` returns relevant source within the estimate |
| 5 | Harness snippet and help text | A fresh Claude Code or Codex session with the snippet uses `rivet` unprompted on a navigation task |
| 6 | Benchmark (§32) | Report passed gates, failure/re-scope, or an explicit inconclusive result |

Only after Milestone 6 passes the preregistered gates does the command set expand; inconclusive results do not unlock expansion.

## 31.5 First Language

Use PHP for the first end-to-end slice and TypeScript/TSX for the second. Establish tiny authored fixtures first; select and pin real benchmark repositories before the pilot. Run a small pilot before adding the second language and the full benchmark. No additional languages until the two-language result is measured. [TASKS](docs/TASKS.md) breaks implementation into one-task sessions with explicit checkpoints and usage gates.

---

# 32. Benchmark Plan

[The benchmark runbook](docs/BENCHMARK.md) is the protocol authority. Begin with a PHP pilot to validate checks and estimate cost/variance, then use held-out tasks for the confirmatory run.

## 32.1 Configurations

| Config | Tools/instructions | Role |
|---|---|---|
| A | Standard shell and harness-native tools, default instructions | Secondary control |
| B | A plus a fixed efficient-search prompt | Primary control |
| C | B plus Rivet and the standard snippet | Primary treatment |
| D | C plus a request to try Rivet first | Optional exploratory adoption arm |

Pin model, harness, settings, repository commits, tool/snippet versions, and resource limits. Use fresh isolated workspaces and cold Rivet caches. Hide evaluators/gold answers from agents. Randomize arm order in task/trial blocks; record every run, including failures and provider outages under a symmetric retry rule.

## 32.2 Tasks and Trials

At least 20 objectively checked tasks across PHP and TypeScript repositories, five trials per task per required configuration (300 runs minimum). Categories include bug fixing/location, rename, test lookup, call-path tracing, validation, and small features. Syntax-aware gold bindings—not text absence alone—validate rename tasks. Pilot results determine whether the minimum sample is adequate for the quality margin; an underpowered result is inconclusive.

## 32.3 Preregistered Decision

Primary comparison is C/B mean total input tokens across all assigned runs, equally weighting tasks. Proposed gates: ratio ≤ 0.70 with upper one-sided 95% bound < 1.00; task-success difference C−B lower one-sided 95% bound > −0.05. Both gates must pass. Use the paired task-cluster analysis in the runbook. Successful-run-only comparisons are secondary, as are A comparisons and tool-call/wall-clock metrics.

## 32.4 Metrics and Attribution

Capture provider token usage (cached/uncached where available), task outcomes, tool calls, wall time including initial indexing, errors, adoption, fallbacks, and snapshot-consistency evidence. Distinguish index/source mismatches from edits after indexing. Command-use correlations are descriptive; identifying the value of `context` versus navigation needs a separately randomized ablation. Report task/language breakdowns, confidence intervals, total cost, and corpus limitations.

---

# 33. Core Hypothesis

> AI coding agents perform software-engineering tasks more efficiently when given deterministic, confidence-labeled structural codebase primitives than when relying mainly on line-oriented Unix text tools.

Specifically:

> A small set of agent-native navigation commands reduces tool calls, source reads, and input tokens while maintaining task success, and does so beyond what a better `rg` prompt alone achieves.

The second clause is why configuration B exists.

---

# 34. Product Positioning

`rivet` is not an AI coding agent, an IDE, an LLM wrapper, a RAG framework, or a chatbot.

It is closer to: ripgrep plus Tree-sitter plus a persistent code index plus graph queries plus token-aware source extraction, with explicit confidence labels, designed for AI coding harnesses.

> `rivet` is a machine-first CLI toolkit for fast structural code navigation and token-efficient source retrieval.

---

# 35. Long-Term Vision

A mature `rivet` exposes the MVP commands (§31.1) plus the future commands (§17) and eventually structural editing (§18). A coding harness then uses raw text tools only when raw text access is actually necessary.

```text
LLM
 ↓
coding harness
 ↓
rivet
 ↓
Tree-sitter / index / Git / optional LSP
 ↓
source repository
```

The model remains responsible for reasoning. `rivet` is the efficient, honest interface between the model and the repository.

---

# 36. Most Important Constraint

Do not let `rivet` become an agent.

Whenever a proposed feature starts answering "What should I change?", "Why is this bad?", or "How should I redesign this?", it belongs outside `rivet`.

`rivet` answers: Where is this symbol? What references it, and how sure are we? What does it call? What calls it? What source is structurally relevant? What changed structurally? What tests reference it?

These are deterministic repository facts, labeled with how they were derived.

---

# 37. Open Questions

Settled for v0.1: local `.rivet/` storage; PHP then TypeScript/TSX; fixed relationship ranking; no PageRank, JSON Lines, or multi-target queries; content verification by default with explicitly labeled faster/cached modes; strict source-estimate budgets; tests inferred only from references in configured test paths.

Remaining experiments, not blockers to the first slice:

1. Does context/navigation outperform configuration B at comparable task success?
2. What precision and candidate coverage do the scoped heuristics achieve on the chosen real corpora?
3. Is content hashing acceptable at the intended repository sizes? Report end-to-end costs before relaxing the default.
4. How inaccurate is `utf8-bytes-v1` across the selected corpora and actual rendered outputs?
5. Does graph ranking improve over fixed priorities enough to justify its complexity?
6. Would an optional LSP or stack-graphs layer provide measurable benefit over the existing tools?

Pre-coding dependency and publication decisions are tracked in [IMPLEMENTATION-PLAN](docs/IMPLEMENTATION-PLAN.md); package ownership and release URLs must be verified before publishing install commands as runnable instructions.

---

# 38. Working Philosophy

The project should remain:

```text
small
fast
boring
deterministic
honest
local
composable
machine-first
language-aware
agent-agnostic
```

The objective is not to make the CLI intelligent. The objective is to make the agent require less effort to see and manipulate the codebase correctly.
