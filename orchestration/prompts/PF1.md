Implement PF1 from docs/TASKS.md only. This is a performance fix found while pinning the benchmark corpus, not new scope. Read AGENTS.md first, then the PF1 row; `docs/ARCHITECTURE.md` "Refresh and invalidation" (the pseudocode is the specification) and "Minimal logical schema"; spec §12.3 and §12.4; `crates/rivet-cli/src/refresh.rs`; and `crates/rivet-store/src/lib.rs`, its schema and publish path. Nothing else.

PF1 — Make a no-change refresh cheap.

The defect, reproduced by the orchestrator with a release build on the pinned private PHP corpus, which has 2,815 files seen, 1,219 PHP files indexed, 11,336 symbols, 125,299 uses and 30,751 bindings:

| Operation | Time |
|---|---|
| Cold build, `.rivet/` deleted first | 3.9 s |
| No-change refresh, reporting `updated: 0` | 29.6 s |
| `index --force` | about 17 s |

A refresh that changes nothing costs seven times a cold build. Every query refreshes first, so every `symbol`, `refs` and `context` call on a real repository takes about half a minute, which makes rivet unusable and would dominate the T37 benchmark.

Two leads, neither confirmed. Profile before fixing:
1. The schema declares `symbols.parent_id REFERENCES symbols(id) ON DELETE SET NULL`, with no index whose leading column is `parent_id`. Every deleted symbol therefore makes SQLite scan the whole `symbols` table for children. With 11,336 symbols, deleting and reinserting all of them is about 128 million row checks. That fits both slow cases, since `--force` deletes everything and a cold build deletes nothing. Every other foreign key has an index on its referencing column.
2. The ARCHITECTURE pseudocode rewrites facts only for changed files, and clears and re-resolves bindings only "if content, membership, or resolver fingerprint changed". The current refresh appears to rewrite every file's facts and every binding on every refresh, changed or not; T22 chose to re-resolve all uses on every refresh. On a no-change refresh the pseudocode does nothing but walk, read and hash, compare, recheck, and commit.

Requirements:

1. Profile first. Measure where a no-change refresh on the corpus copy actually spends its time, split into walk, read and hash, load stored facts, staging writes, resolution, recheck, digest and commit, and report the split before and after. Do not optimise by guesswork.
2. A no-change refresh must write no fact rows: no symbol, use, scope or binding row deleted, inserted or updated, and no re-resolution. Per the pseudocode, facts are replaced only for changed files, and bindings are cleared and recomputed only when content, membership or the resolver fingerprint changed. Keep T22's correctness: when anything does change, every persisted use is still re-resolved, so a change in one file still updates bindings in unchanged files. The T22 tests must still pass unchanged.
3. Index the unindexed foreign key. Add the index additively, for example with `CREATE INDEX IF NOT EXISTS` when the store opens, so an existing cache gains it without a rebuild. Decide whether this needs an index-format version bump, remembering that AF6 found a plain refresh refuses any format mismatch with exit 3, so a bump would force every existing user to run `--force`. Justify your choice.
4. Keep every guarantee. T31's single writer transaction, the pre-commit recheck and race retry, and pinned snapshot reads all hold. The snapshot digest of a no-change refresh equals the previous one, and a changed file still gets exactly T31's behaviour. `--force` still regenerates everything, as AF6 requires; it simply should no longer pay the quadratic delete.
5. Output does not change. `index`, `symbol`, `refs` and `context` output on the authored fixture must be byte-identical, digest included, and on the corpus copy the digest and the counts of symbols, uses and bindings must be identical before and after the fix. Gold must verify 51 authored entries, and with `RIVET_PRIVATE_GOLD_DIR=/Users/cris/rivet-corpus/gold RIVET_CORPUS_DIR=/Users/cris/rivet-corpus` it must also verify the 28 private entries.
6. Tests, deterministic and not dependent on wall-clock time:
   a. A no-change refresh performs no fact-row writes. Assert this with a count that does not depend on timing, for example SQLite's `total_changes` over the refresh, or a debug-only counter in the store, and show that the count is zero, or bounded by a small constant for meta, on a repository of several files.
   b. A one-file edit rewrites only that file's fact rows, plus re-resolved bindings, and nothing for other files.
   c. The T22 cross-file cases still hold: adding a competing declaration in a new file still changes a binding in an untouched file.
   d. The index on `symbols.parent_id` exists on a fresh store and on a store that predates the fix.
   e. Determinism and digest equality across a no-change refresh.
7. Measure and report after the fix, with a release build on `/Users/cris/rivet-corpus/fluent`, the median of 5 runs of: cold build, no-change refresh, a one-file edit plus refresh, and `--force`. For the edit, change a file in a copy of the corpus, never the corpus itself, then restore it. Run rivet only against the copies under `/Users/cris/rivet-corpus/`, NEVER against anything under `/Users/cris/ssr/`. Record nothing from those projects' code in this repository; the numbers alone are fine.
8. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `python3 tests/gold/check_gold.py`. Paste real results with exact per-crate counts.
9. Update docs/TASKS.md: tick PF1 and update "Last checks" only. Do NOT change "Last completed task" or "Next task". Do not touch other docs. Do not run git.

Report contradictions rather than silently resolving them.

Finish with the AGENTS.md final block.
