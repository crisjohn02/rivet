# Instructions for implementation agents

You are implementing `rivet` one task at a time under an orchestrator that reviews your work adversarially. Read this file, then `docs/TASKS.md` (the checkpoint table and your assigned task row), then only the document sections that row lists. Do not reread the whole spec.

## Scope rules

- Implement exactly the assigned task ID. Do not start the next task, add speculative abstractions, or refactor unrelated code.
- A command that is not yet implemented must fail clearly (non-zero exit, message naming the task that will implement it). Never emit placeholder success or empty results that look complete.
- Do not edit files under `docs/` except: tick the task's checkbox and update the "Current checkpoint" table in `docs/TASKS.md`. Do not edit the spec, README, or contracts; if you find a contradiction, report it in your final message instead.
- Do not run `git commit`, `git push`, or change git config. The orchestrator commits after review.
- Do not install system packages or modify anything outside this repository.
- Keep every crate `publish = false`. Package names are local design names.

## Environment

- Rust is at `~/.cargo/bin`. If `cargo` is not found, run `export PATH="$HOME/.cargo/bin:$PATH"` first.
- Use the stable toolchain already installed. Do not install additional toolchains or targets.
- Run `cargo fmt --all` before finishing. `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` must pass when the task's acceptance says so.

## Documents that own decisions

| Question | Read |
|---|---|
| Product scope, commands, resolution tiers, context rules | `rivet-agent-native-codebase-cli-spec.md`, only the sections your task row cites |
| Exact JSON fields, ordering, exit codes | `docs/OUTPUT-CONTRACT.md` (normative) |
| Storage schema, refresh, concurrency | `docs/ARCHITECTURE.md` |
| Language adapter boundary | `docs/ADDING-A-LANGUAGE.md` |
| Build layout and test locations | `docs/BUILDING.md` |

## Final message format

End with exactly this block, filled in truthfully. If a check failed, say so with the output; do not claim success you did not observe.

```text
Task: Txx — done / partial / blocked
Changed: paths and the concrete behavior added
Checked: actual command(s) run, pass/fail, key output lines
Remaining: unfinished scope or a concrete blocker, or "none"
Contradictions found in docs: list, or "none"
Next: one task or subtask ID
```
