# Contributing to rivet

Thank you for considering it. This document covers how to work on `rivet` in a way that keeps it small, fast, boring, deterministic, and honest.

## Before you start

Read the [specification](rivet-agent-native-codebase-cli-spec.md), particularly §5 (Design Principles), §11 (Reference Resolution), and §36 (Most Important Constraint), then the [implementation plan](docs/IMPLEMENTATION-PLAN.md). Most design discussions are settled there.

Development setup is in [docs/BUILDING.md](docs/BUILDING.md).

## What we accept

- Bug fixes, especially determinism, freshness, and coverage or binding bugs. These are contract violations and take priority.
- New languages that follow [docs/ADDING-A-LANGUAGE.md](docs/ADDING-A-LANGUAGE.md) completely, including the annotated coverage/binding tests and fixture.
- Improvements to resolution heuristics within the v0.1 scope, with before/after numbers on a fixture.
- Performance work with a Criterion bench showing the change.
- Documentation fixes.

## What we do not accept

- Anything that calls a model, requires a network, or reads an API key.
- Features that answer "what should I change" or "is this good". See spec §36.
- Type inference, generics resolution, or interface-to-implementation mapping before the v0.2 design pass.
- New commands before the Milestone 6 benchmark has passed its preregistered gates, unless the spec is amended first.
- Output changes without a corresponding snapshot update and a CHANGELOG entry distinguishing schema changes (additive/breaking) from behavior corrections.

## Pull requests

- One change per PR. Small PRs get reviewed; large ones wait.
- CI must pass: fmt, clippy with `-D warnings`, all tests, snapshot tests.
- If a snapshot changed, say in the PR description whether the change is additive or breaking and why.
- If you touched `rivet snippet` text or `--help` output, note that the benchmark treatment has changed.
- Describe what you measured. "Faster" needs a bench. "More accurate" needs fixture counts per resolution tier.

## Determinism rules

These must be enforced by implementation tests:

- Never let `HashMap` iteration order reach output. Sort, or use `BTreeMap`.
- Every list is sorted by an explicit key before leaving `rivet-index`.
- No timestamps, durations, random values, or refresh-work counters in navigation output. Administrative work reports may differ between runs.
- Token estimation is a pure function of bytes.

## Honesty rules

- Every reference, call, and caller carries a `resolution` tier. Never emit one without it.
- Never upgrade a tier without a heuristic that justifies it. `name_match` that happens to be right is still `name_match`.
- Coverage and binding tests must pass. Candidate mode preserves supported identifier uses; reference mode excludes uses bound to other declarations. Never claim runtime completeness or use raw text hits as a semantic gold set.

## Commit messages

Imperative subject under 72 characters, blank line, body explaining why. Reference the spec section when the change implements or alters one, for example `Implement freshness check (spec §12.4)`.

## License

`rivet` is dual-licensed under MIT and Apache-2.0. By submitting a contribution you agree that it is licensed under both, at the user's option, without additional terms. This is the standard Rust ecosystem arrangement and matches Section 5 of the Apache license. No contributor license agreement is required.

## Questions

Open a discussion rather than an issue for design questions. Open an issue for bugs, with the repository (or a minimal reproduction), the command, the expected output, and the actual output. For determinism bugs, include both differing outputs.
