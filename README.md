# rivet

**Agent-native codebase CLI.** Fast, deterministic structural code navigation and token-efficient source retrieval for AI coding harnesses.

> **Status:** pre-implementation. This repository currently holds the specification and supporting documents. Nothing builds yet. The commands below describe intended behavior.

---

## What it is

Coding agents such as Claude Code and Codex explore repositories with `rg`, `cat`, `sed`, and `find`. Those tools are line-oriented. `rivet` exposes the same repository as **symbols, references, callers, callees, and token-budgeted context**, with stable JSON output and an explicit confidence label on every fact it reports.

`rivet` contains no model and does no reasoning. It answers questions like:

- Where is this symbol?
- What references it, and how sure are we?
- What does it call? What calls it?
- What is the smallest useful source context around it that fits in 3,000 tokens?

Start implementation with [the plan and acceptance checks](docs/IMPLEMENTATION-PLAN.md). The main unproven question is whether these primitives outperform an efficient text-tool workflow at comparable task success.

For small coding sessions, use the [task queue and current checkpoint](docs/TASKS.md). Start with T01 and complete one task per session.

## Quick start (intended)

```bash
# install (see docs/INSTALL.md)
# Package name and release URLs are not finalized yet.
# Build from source once the Cargo workspace exists (docs/BUILDING.md).

# in a repository
rivet init                       # creates .rivet/, adds it to .gitignore
rivet index                      # first full index; later queries refresh automatically

rivet symbol SurveyService.launch
rivet refs SurveyService.launch --json
rivet context SurveyService.launch --tokens 3000

# tell your agent about it
rivet init --write-snippet       # manages a block in AGENTS.md or CLAUDE.md
```

## Commands

| Command | Purpose | MVP |
|---|---|---|
| `rivet init` | Create `.rivet/` and the default config | yes |
| `rivet index` | Build or force-rebuild the index | yes |
| `rivet symbol <query>` | Locate and describe a symbol: location, signature, calls, callers | yes |
| `rivet refs <query>` | Likely references to a symbol, each labeled `exact`, `scoped`, or `name_match` | yes |
| `rivet context <query> --tokens N` | Ranked source segments around a symbol within a token budget | yes |
| `rivet snippet` | Print the drop-in block for `CLAUDE.md` / `AGENTS.md` | yes |
| `rivet callers`, `rivet callees`, `rivet tests` | Graph projections | v0.2 |
| `rivet impact`, `rivet diff --semantic` | Structural change analysis | v0.3 |
| `rivet rename`, `rivet edit` | Structural editing | v0.5 |

Every command accepts `--json`. Reference lists default to 50 results; use `--offset` to page. `refs --mode candidates` expands to same-name identifier uses for auditing. Context budgets bound an estimate of source text, not the full response or actual model tokens. Symbol queries accept a short name, a dotted path, a language-native qualified name, `file:line`, or a canonical ID. Ambiguous queries return the candidate list and exit 5.

## Example

```text
$ rivet refs SurveyService.launch
3 references  (2 scoped, 1 name_match)

app/Http/Controllers/SurveyController.php:91   SurveyController.launch        call
app/Jobs/LaunchSurveyJob.php:44                LaunchSurveyJob.handle         call
tests/Feature/SurveyLaunchTest.php:118         SurveyLaunchTest.test_launch   call ?
```

The trailing `?` marks a name-only match. `rivet` uses Tree-sitter, not a compiler, so it labels how each fact was derived instead of presenting guesses as certainties. See [Reference Resolution](rivet-agent-native-codebase-cli-spec.md#11-reference-resolution-without-a-compiler) in the spec.

## Design in one paragraph

Rust single binary. Tree-sitter for parsing. SQLite for a persistent, incrementally updated index under `.rivet/`. Queries hash eligible source content by default and return source and positions from one committed snapshot. Faster metadata-only and cached modes are explicit. Navigation output has deterministic ordering and bytes for the same snapshot, versions, and options. No network, no API keys, no model.

## Documentation

| Document | Contents |
|---|---|
| [Specification](rivet-agent-native-codebase-cli-spec.md) | Product and technical spec, Draft v0.3 |
| [docs/IMPLEMENTATION-PLAN.md](docs/IMPLEMENTATION-PLAN.md) | Pre-coding review, build order, decisions, and acceptance checks |
| [docs/TASKS.md](docs/TASKS.md) | Small implementation tasks, completion checks, usage gates, and session handoff |
| [docs/INTEGRATION.md](docs/INTEGRATION.md) | Normal Codex/Claude Code setup, optional skills, and integration acceptance |
| [docs/INSTALL.md](docs/INSTALL.md) | Installing from prebuilt releases or source; setting up a repository |
| [docs/BUILDING.md](docs/BUILDING.md) | Building, testing, linting, cross-compiling |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate layout, query data flow, index schema, freshness algorithm |
| [docs/OUTPUT-CONTRACT.md](docs/OUTPUT-CONTRACT.md) | JSON shapes, ordering rules, error objects, versioning policy |
| [docs/ADDING-A-LANGUAGE.md](docs/ADDING-A-LANGUAGE.md) | How to implement the `Language` trait for a new grammar |
| [docs/BENCHMARK.md](docs/BENCHMARK.md) | Benchmark runbook: configurations, tasks, protocol, report |
| [benchmark/REPORT-TEMPLATE.md](benchmark/REPORT-TEMPLATE.md) | Empty pilot/confirmatory report template: metrics, gates, evidence, limitations |
| [docs/AGENT-SNIPPET.md](docs/AGENT-SNIPPET.md) | The text `rivet snippet` prints |
| [docs/RELEASING.md](docs/RELEASING.md) | Release checklist and versioning policy |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute |
| [CHANGELOG.md](CHANGELOG.md) | Changes by version |

## Non-goals

`rivet` will not explain code, judge architecture, fix bugs, or act as an agent. If a proposed feature answers "what should I change?", it belongs in the harness, not here.

## License

Dual-licensed under either of

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option. Anyone may use, modify, redistribute, and sell `rivet` or derivatives of it under whichever license they prefer. Contributions are accepted under the same dual license (see [CONTRIBUTING.md](CONTRIBUTING.md#license)).
