# PF2: `--no-refresh` queries slower than refreshed ones; cached diagnostic detail

Read `AGENTS.md` first and follow it. Then read only:

- `docs/TASKS.md`, the checkpoint and the PF1 row.
- `docs/ARCHITECTURE.md`, the refresh, snapshot and read-only-open sections.
- `docs/OUTPUT-CONTRACT.md`, `diagnostics` and the `--no-refresh` wording.
- `crates/rivet-cli/src/` (argument handling, refresh orchestration, the query commands) and
  `crates/rivet-store/src/lib.rs` (open modes).
- `tests/real/RESULTS.md` and `tests/real/manifest.toml` for how Hono is fetched and measured.

Work in this worktree only. Do not commit; the reviewer commits. Do not touch `~/ssr`,
`~/rivet-corpus` or `~/pilot-runs-rivet`, and do not run `claude` or any model. You may run
`tests/real/fetch.sh` (network only for the pinned Hono commit).

## Findings to fix

1. **`--no-refresh` is slower.** On Hono v4.13.9 (release build, warm cache), `rivet symbol
   'src/request.ts#HonoRequest.queries' --json` takes about 0.18–0.20 s with the default refresh
   and 0.25–0.28 s with `--no-refresh`, which should do strictly less work. A no-change
   `rivet index` takes about 45 ms. Measure first (e.g. `--timing` if the command supports it, or
   instrument locally without committing the instrumentation), find where the extra time goes on
   the read-only path, and fix it. Then report where the remaining ~180 ms of a refreshed query
   goes (a breakdown, not a fix) so the orchestrator can decide whether a follow-up is worth it.
2. **Cached diagnostics lose detail.** For a file that failed to parse, `rivet index` reports the
   real location (for example `"} at byte 2885"`), but a query answered from the stored snapshot
   reports `"stored parse error"`. Check OUTPUT-CONTRACT: if it requires the same detail, persist
   and return it. If that needs a store schema change, bump `INDEX_FORMAT_VERSION` following the
   existing older-format rule. If the contract allows the generic text, document the difference
   in OUTPUT-CONTRACT instead and say which you chose and why.

## Constraints

- Output is byte-identical to before for every query, in both refresh modes, apart from the
  diagnostic detail in finding 2 if you change it. Determinism and snapshot semantics are unchanged:
  `--no-refresh` must still never write, and must still refuse an incompatible or missing store
  exactly as now.
- No new dependency. No change to budget or ranking.

## Tests

- A test that `--no-refresh` performs no write (the store's bytes and mtime are unchanged) and
  returns the same bytes as a refreshed query on an unchanged tree, on both fixtures.
- For finding 2, a test that the cached and fresh diagnostics agree (or that the documented
  difference holds).
- Timing is not asserted in tests. Record before/after medians of 5 runs on Hono for `symbol`,
  `refs`, and `context` in both modes, and a no-change `index`, in the report and as a dated PF2
  addendum in `tests/real/RESULTS.md`.

## Checks

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 tests/gold/check_gold.py
python3 benchmark/runner/test_runner.py
python3 tests/real/test_check_real.py
cargo build --release && tests/real/fetch.sh && python3 tests/real/check_real.py --only determinism,incremental
```

Then add a ticked PF2 row after PF1 in `docs/TASKS.md` and update the checkpoint.

## Report

- the cause and fix for finding 1, with before/after timings;
- the breakdown of a refreshed query's time;
- the choice for finding 2;
- goldens changed; check results; contradictions.
