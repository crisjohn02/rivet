Implement SN1 from docs/TASKS.md only. Read AGENTS.md first, then the SN1 row; `docs/AGENT-SNIPPET.md`; `benchmark/results/pilot-01/RESULT.md` "Decision" item 1; `benchmark/results/T37-local/report.md` "Output size: JSON against text"; `crates/rivet-cli/src/snippet.rs`; and `crates/rivet-cli/tests/snippet_json.rs`. Nothing else.

SN1 — Change the managed snippet to recommend the compact text output instead of `--json`.

In pilot-01, every one of arm C's rivet calls used the default text output and none used `--json`, although the snippet says "Add `--json` for structured results." T37 measured the text form at 18% to 32% of the JSON's size for `symbol` and `refs`, and 51% to 69% for `context`. The user has approved changing the benchmark treatment so the snippet matches the cheaper form agents actually use.

Requirements:

1. Change exactly one sentence of the managed block. Replace "Add `--json` for structured results." with:
   "The default text output is compact and meant for you to read; `--json` emits the full machine contract at several times the size, so reserve it for scripts that parse the result."
   Change nothing else in the block: not the other sentences, the markers, the whitespace, or the final newline. Report the before and after text of that one line, and the full diff of the block, so the orchestrator can confirm only it changed.
2. Make the same change in `docs/AGENT-SNIPPET.md`, inside its fenced block. This task may edit that file, the one exception to the docs rule, and only that sentence in it. The existing drift test, which extracts the block from the document and compares it byte for byte with `rivet snippet`, must pass.
3. `docs/AGENT-SNIPPET.md` says "Changes require a new recorded snippet hash." Record the new SHA-256 of the shipped bytes. If a test asserts the old hash `0d5a7d0a2195199ecdefd96a23da4ba6e4653d15830b0109f76c1d096be6119f`, update it; and add one sentence to `docs/AGENT-SNIPPET.md`, outside the fenced block, recording the new hash, the date 2026-09-23, and that pilot-01 used the previous one. Report the new hash.
4. `rivet init --write-snippet` in a file that already holds the old block must replace it with the new one, which is existing managed-block behaviour; add a test proving an old-block file is updated to the new block, with text outside it preserved.
5. The text form must keep carrying every signal the snippet mentions, since the snippet now steers agents to it: the `?` on name-only matches, totals and next offsets on paged lists, coverage and skipped files, and context segment forms. Confirm each is present in the text output with a short test or a quoted example.
6. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, and `python3 tests/gold/check_gold.py`. Paste real results.
7. Update docs/TASKS.md: tick SN1 and update "Last checks" only. Do NOT change "Last completed task" or "Next task". Do not run git.

Report contradictions rather than silently resolving them.

Finish with the AGENTS.md final block.
