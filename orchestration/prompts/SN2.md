# SN2: a lean managed snippet

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint table and the SN1 row.
- `orchestration/prompts/SN1.md`, for how the snippet, its hash history and the upgrade-in-place
  tests work.
- `orchestration/review-notes/token-economy-audit-2026-09-24.md` (TE1), lever 1.
- `docs/AGENT-SNIPPET.md`, `crates/rivet-cli/src/snippet.rs` and `crates/rivet-cli/tests/snippet_json.rs`.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model.

## Why

- **Cost.** TE1 measured the snippet at 683 tokens per model request, re-sent on every request.
  That is 5.4% of the with-rivet arm's input tokens on the held-out project. It cancelled nearly
  all of the saving from fewer requests.
- **Unused material.** Agents used rivet in only a few ways, and much of the current text covers
  paging, ambiguity handling and `--json`, which they did not use or should not use.
- **Estimate.** A snippet of about 150 tokens saves an estimated 1,600 to 2,200 tokens per run.
  This is an estimate, not evidence.

## Build

Replace the managed block's content with exactly this text, markers included:

```markdown
<!-- rivet:start -->
## rivet (code navigation)

- `rivet symbol <name>`: definition, signature, calls and callers.
- `rivet refs <name>`: references with their containing symbols.
- `rivet context <name> --tokens 1500`: the symbol plus related source; a `signature` segment is not the full body.

Names may be short, `Class.method`, a canonical ID, or `file:line`. Results refresh automatically. `?` marks a name-only match: verify it. rivet does not see comments, strings, dynamic calls, framework wiring or unsupported files, so an empty or short result is not proof of absence; use text search there.
<!-- rivet:end -->
```

- **Byte-identical.** The block in `snippet.rs` and in `docs/AGENT-SNIPPET.md` must match byte
  for byte, as SN1 required.
- **Hash history.** Record the new SHA-256 and keep every previous hash in the history, so that
  `rivet init --write-snippet` still upgrades each older block in place. That covers the pilot-01
  block (`0d5a7d0a…`) and the SN1 block (`18195306…`).
- **Upgrade tests.**
  - An SN1-era block, with surrounding user text, upgrades in place to the new block, and the
    surrounding text is preserved.
  - The pilot-01-era block still upgrades.
- **Counts.** Report the new block's byte length and an estimate of its tokens at about 4
  characters per token, and against the old block.
- **Accuracy check.** Every command form and flag the text names must exist. Verify this against
  `--help`, and verify that `context`'s `--tokens` accepts 1500.
- **Report problems; don't fix them silently.** If any clause in this text is false for the
  current binary, report it and do not change the text on your own. Examples: `?` does not appear
  in some output, or `Class.method` is not accepted.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
python3 benchmark/runner/test_runner.py
```

Then add and tick an SN2 row in `docs/TASKS.md` and update the checkpoint.

## Report

- the new hash and its length;
- the upgrade tests;
- every golden changed;
- the check results;
- any clause that is not accurate.
