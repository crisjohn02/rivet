# Working on rivet

`rivet` is a machine-first CLI for structural code navigation and token-budgeted
source retrieval, built for AI coding harnesses. Read
[the specification](rivet-agent-native-codebase-cli-spec.md) for scope and
[docs/TASKS.md](docs/TASKS.md) for what is done and what is next.

## How this project is built

**Claude orchestrates and reviews. OpenCode implements.** Do not write feature
code directly. One task per OpenCode session, reviewed and committed by Claude
before the next starts. This split exists so every change gets an adversarial
read from a different model before it lands.

### The loop

```bash
orchestration/run-task.sh T20          # OpenCode implements one task, prints its report
# ... Claude reviews: read the diff, run the checks, probe the binary by hand ...
git add -A && git commit               # only after the review passes
git push
```

Requirements on this machine: `opencode` on PATH (authenticated), a Rust
toolchain, and a C compiler for the Tree-sitter grammars.

### Claude's review, every task

Never trust the implementor's report. Independently:

1. Read the whole diff, not just the files the report names.
2. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `python3 tests/gold/check_gold.py`.
3. Build the binary and exercise the new behavior by hand against the fixture, including the cases the task's tests do not cover. Several real defects were caught only this way (anonymous-class members leaking as symbols, an order-sensitive config fingerprint being sorted).
4. Use a private `CARGO_TARGET_DIR` when reviewing a worktree so a stale shared binary cannot make a broken build look green.
5. Fold any defect found into the *next* task's prompt as a "review fix" preamble rather than patching it silently.

### Parallel tasks

Only when tasks are genuinely independent. Most of the queue is chained.

```bash
orchestration/run-task-worktree.sh T20   # own worktree + branch task/T20
orchestration/merge-task.sh T20          # merge into main, then verify
```

Each worktree builds into its own `target/`. Expect small conflicts in
`docs/TASKS.md` and module doc comments; resolve by keeping both ticks.
T13, T14, and T17 ran this way successfully.

### Writing a task prompt

Prompts live in `orchestration/prompts/`. A good one names: the exact task ID,
which document sections to read and nothing else, the concrete API to build,
the specific tests to write including adversarial cases, the checks to run, and
the instruction to report contradictions rather than silently resolving them.
`AGENTS.md` carries the standing rules the implementor reads first.

## Non-negotiables

- **Determinism.** Same snapshot and options means byte-identical output. Sort every list explicitly; `HashMap` order must never reach output.
- **Honesty.** Every reference carries a resolution tier. Never upgrade a tier without a rule that justifies it. `name_match` that happens to be right is still `name_match`.
- **No model, no network.** Nothing in the binary calls an LLM or requires a network.
- **Scope.** `rivet` answers "where is this and what references it", never "what should I change". See spec §36.

## Repository layout

```text
crates/rivet-cli/         arguments, refresh orchestration, output, exit codes
crates/rivet-core/        owned records: spans, ids, kinds, config, walk, source
crates/rivet-parser/      Tree-sitter driver and grammar dispatch
crates/rivet-languages/   per-language extraction (PHP first, TypeScript next)
crates/rivet-index/       query resolution, binding rules, ranking
crates/rivet-store/       SQLite schema, atomic publish, snapshot reads
tests/fixtures/php/       authored fixture; gold spans in tests/gold/
orchestration/            task prompts and the OpenCode runner scripts
```

`docs/OUTPUT-CONTRACT.md` is normative for JSON. `docs/ARCHITECTURE.md` owns
storage and refresh. The spec owns product scope.
