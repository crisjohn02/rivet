Implement T38a, T38b and T38c from docs/TASKS.md together, as one coherent harness. Read AGENTS.md first, then those three rows and the T38, T39 and T40 rows for context; `docs/BENCHMARK.md` in full; `benchmark/REPORT-TEMPLATE.md`; `benchmark/corpus.toml`; `docs/AGENT-SNIPPET.md`; and `docs/INTEGRATION.md` for how Claude Code loads `CLAUDE.md`. Nothing else.

This task builds and tests the pilot harness. It makes NO model calls: every test runs against a fake agent. A separate task, T38, is defining the pilot's tasks in parallel, and the paid runs are T39.

## Decisions already made by the user, which you must implement

- **Arms:** B and C only, per `docs/BENCHMARK.md` "Configurations". Both arms get the config-B prompt, verbatim from that document, appended to the system prompt. C additionally gets rivet and exactly the managed snippet, installed by running `rivet init --write-snippet --snippet-file CLAUDE.md` in the workspace.
- **Harness:** Claude Code headless, `claude -p`, authenticated by the user's own login. There is no API key, so `--bare` is unavailable. Isolate each run with flags instead:
  - `--setting-sources project`, so the user's own settings, plugins and hooks are not loaded;
  - `--strict-mcp-config` with no `--mcp-config`, so no MCP servers load;
  - `--disable-slash-commands`, so no skills load;
  - `--no-session-persistence`;
  - `--output-format stream-json --verbose`, for a complete transcript with usage;
  - `--model claude-opus-5-5`, pinned, never an alias;
  - `--settings '{"effortLevel":"high"}'`, pinned;
  - `--max-budget-usd` per run, from the task's limits.
  Record `claude --version`, and every flag, in each run record.
- **Tools are read-only.** The pilot's tasks are navigation tasks with structured answers, so no run may edit files or execute the project's code. Allow only `Read`, `Grep`, `Glob`, and `Bash` restricted to `rg`, `ls`, `find` and `wc`, plus `Bash(rivet:*)` in arm C only. Deny `Edit`, `Write`, `NotebookEdit`, `WebFetch`, `WebSearch`, and any subagent or task tool. In arm B, additionally deny `Bash(rivet:*)` explicitly, and keep rivet off `PATH`. `docs/BENCHMARK.md` requires denial at the runner boundary, not just removal from `PATH`. After every B run, scan its transcript for any rivet invocation and flag the run contaminated if one is found.
- **Budget:** a hard total cap of $25 across the whole study. Before each run, if spent-so-far plus that run's `max_budget_usd` would exceed the cap, stop the study and report; never start a run that could breach it. Spent-so-far is the sum of each run's reported `total_cost_usd`, which under a subscription login is Claude Code's estimate; label it that way.
- **Privacy.** The corpus is private and this repository is public. Tasks, prompts, gold answers, transcripts and per-run records contain private code, so they live OUTSIDE the repository. Tasks are read from `$RIVET_PILOT_TASKS_DIR`, and run artifacts are written to `$RIVET_PILOT_RUNS_DIR`, both required with no default. The committed code, schema docs and tests must contain nothing from the corpus. The only committed study output is the aggregate report, and it may name tasks only by opaque ID and category.

## The task format, fixed now so T38 can write tasks in parallel

Each task is a directory `$RIVET_PILOT_TASKS_DIR/<task-id>/` holding:
- `task.toml` with `id`, `project` (for example `timesheet`), `category` (one of `locate`, `trace`, `callers`, `tests`, `dependencies`), `check` (one of `exact_symbol`, `set_f1`, `accepted_path`), `f1_threshold` for `set_f1`, and `[limits]` with `max_budget_usd`, `max_turns` and `wall_seconds`.
- `prompt.md`, the task text given identically to both arms. It ends by requiring a final fenced `json` code block that is the answer.
- `gold.json`, the expected answer, in the shape its check type defines.

Define the three check types precisely in a committed schema document, `benchmark/runner/TASK-FORMAT.md`:
- `exact_symbol`: the answer names one file and one symbol, and passes when both match gold after normalizing path separators and a leading `./`.
- `set_f1`: the answer is a list of file-and-symbol items, and passes when F1 against the gold set is at least `f1_threshold`. Report precision, recall and F1.
- `accepted_path`: the answer is an ordered call path, and passes when it equals any of gold's accepted paths.
An answer that is missing or unparseable fails, with its tokens still counted.

## Requirements

1. **T38a — the study manifest and one replayed run.** A study is `benchmark/studies/<study-id>/study.toml`, committed and public-safe, listing opaque task IDs, arms, trials, the randomization seed, the model, the flags and the budget cap. The runner, `benchmark/runner/run_study.py`, executes the study: fresh workspace per run, randomized arm order within each task and trial block using the seed, the agent run, the check, and a run record. A synthetic run must round-trip end to end with IDs, versions, limits and failure accounting, using a fake agent selected by `RIVET_PILOT_CLAUDE_BIN`, a script you write that emits a canned stream-json transcript. No model call.
2. **Workspace preparation, per run.** Copy the pinned corpus copy from `$RIVET_CORPUS_DIR/<project>/` into a fresh directory under `$RIVET_PILOT_RUNS_DIR`, excluding any existing `.rivet/`. Normalize repository-provided agent instructions identically in both arms: remove `CLAUDE.md`, `AGENTS.md` and a project `.claude/` directory, and record the original files' hashes. Then apply the arm: C runs `rivet init --write-snippet --snippet-file CLAUDE.md`; B does nothing. Never write to `$RIVET_CORPUS_DIR` itself.
3. **T38b — extraction to a per-attempt CSV.** Parse each stream-json transcript into one row with the columns in `docs/BENCHMARK.md` "Collected artifacts", filling what the transcript supports: token totals, cached and uncached input, output, tool calls total and by name, rivet invocations by command, rivet errors by exit code, wall time, cost, pass, termination reason and the contamination flag. Where a column cannot be derived, write it as unavailable, never an inferred value. Fallbacks, meaning text-tool use within the next two tool calls on the same identifier or file, need an extractor you document as heuristic. Test with fixture transcripts covering token totals, missing usage, a timeout, an unparseable answer and a contaminated B run.
4. **T38c — aggregates and the report.** From the CSV, compute task-weighted means per arm, the C over B ratio of input tokens, pass rates, and wall time and cost, plus a per-task table. Generate `report.md` from `benchmark/REPORT-TEMPLATE.md`, marking every confirmatory gate unevaluated, since `docs/BENCHMARK.md` forbids treating the pilot as the confirmatory study. Test that known synthetic inputs reproduce known ratios and counts.
5. **Failures count.** Per `docs/BENCHMARK.md`, timeouts, crashes, invalid answers and exhausted limits are failures, with their consumed tokens included. Classify provider and infrastructure failures separately, and implement the symmetric retry rule: rerun the whole affected task and trial block once, preserving both transcripts.
6. Tests run with plain `python3`, standard library only, and must not call the network or the real `claude`. Add a test target that `python3 tests/gold/check_gold.py` does not need to know about, for example `python3 benchmark/runner/test_runner.py`, and state how to run it.
7. Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace`, `python3 tests/gold/check_gold.py`, and your runner tests. Paste real results.
8. Update docs/TASKS.md: tick T38a, T38b and T38c, and update "Last checks" only. Do NOT change "Last completed task" or "Next task", and do not tick T38. Do not touch other docs. Do not run git.

Report contradictions rather than silently resolving them.

Finish with the AGENTS.md final block, with Task: T38a–T38c.
