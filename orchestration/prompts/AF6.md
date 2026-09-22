Implement AF6 from docs/TASKS.md only. This is a review-fix task, not new scope. Read AGENTS.md first, then the AF6 row; `docs/ARCHITECTURE.md` "Refresh and invalidation" and the `--force` sentence in "Concurrency and source consistency"; `docs/OUTPUT-CONTRACT.md` "Administrative commands" (the `index --json` counts, especially "`updated` counts ... all current rows for `--force`"); spec §13; `crates/rivet-cli/src/refresh.rs` including its debug-only `RIVET_DEBUG_REPARSED` and resource-limit hooks; `crates/rivet-cli/src/index.rs`; and the `publish` path in `crates/rivet-store/src/lib.rs`. Nothing else.

AF6 — Make `index --force` actually rebuild.

The defect, reproduced by the orchestrator on the current binary. `rivet index --force` on an unchanged file reports `updated: 1`, yet the debug-only `RIVET_DEBUG_REPARSED` hook records no reparse at all, while the same hook records an edited file on a normal refresh, so the hook works. `--force` clears the tables and then reinserts each unchanged file's previously stored symbols, uses and scopes. It therefore reports every row as regenerated without regenerating anything.

Why it matters. ARCHITECTURE describes `index --force` as a rebuild of the disposable cache, and it is the only recovery tool when a cache's facts are suspect but no fingerprint changed. As it stands it cannot recover such a cache, and its `updated` count claims work it did not do. T32 left one such case in place deliberately: it did not bump `fact-schema`, so an index built before T32 that holds a clean file with more than 500,000 uses keeps facts that current rules would reject. Only a real `--force` can repair that.

Requirements:

1. Under `--force`, read every eligible file's bytes from disk afresh and reparse it, regenerating all of its symbols, uses and scopes and re-resolving every binding. Reuse nothing stored from the previous snapshot. Keep it inside the single writer transaction T31 established, in the pseudocode's order.
2. `updated` must then be true: every current file row really was regenerated, which is what the contract's "all current rows for `--force`" means.
3. The digest of a forced index must equal the digest of a normal index of the same content and configuration. ARCHITECTURE: "Identical supported content/configuration yields the same digest regardless of update history."
4. `--force` must still refuse to overwrite a database from a newer unsupported format, as ARCHITECTURE requires, and must never touch user source or configuration.
5. A normal refresh is unchanged. The authored fixture's `index --json` and query output must be byte-identical to before, digest included, and gold must verify 51 entries.
6. Tests, driving the real binary. Cover:
   a. The reparse hook records every eligible file under `--force`, and none under a no-change normal refresh.
   b. Recovery. Index a repository, then tamper with stored facts directly through SQLite without changing any fingerprint, for example by deleting a use row or rewriting a symbol's name. A normal refresh keeps the tampered facts, which proves the tamper survives reuse. `--force` then restores exactly the facts of a fresh index, compared row for row across symbols, uses and bindings.
   c. The T32 case. Index a file with default limits, so it has facts. Then run `--force` with the debug use limit lowered below that file's use count. The file must now be recorded as `resource_limit` with no facts.
   d. The forced digest equals a normal index's digest on the authored fixture.
   e. A newer-format database is still refused by `--force` with exit 3 and left byte-for-byte unchanged.
7. Report the cost of `--force` against a normal no-change refresh on the authored fixture. A number is enough.
8. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `python3 tests/gold/check_gold.py`. Paste real results with exact per-crate counts.
9. Update docs/TASKS.md: tick AF6 and update "Last checks" only. Do NOT change "Last completed task" or "Next task". Do not touch other docs. Do not run git.

Report contradictions rather than silently resolving them.

Finish with the AGENTS.md final block.
