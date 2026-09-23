# Pilot Task Format, Run Records and runs.csv

This is the normative format for the B/C study harness: pilot studies (T38a–T38c) and confirmatory studies (T48a). `checks.py`, `run_study.py`, `extract.py` and `report.py` implement it. The protocol it serves is [BENCHMARK](../../docs/BENCHMARK.md). Nothing in this directory may contain corpus content: tasks, prompts, gold answers, transcripts and per-run records are private and live outside the repository.

## Task directory

Each task is `$RIVET_PILOT_TASKS_DIR/<task-id>/`. The task ID is opaque: `[A-Za-z0-9][A-Za-z0-9_-]*`, at most 64 characters, with no dots. The directory holds three files.

`task.toml`:

```toml
id = "p01"                 # must equal the directory name
project = "timesheet"      # a [[project]] name in benchmark/corpus.toml
category = "locate"        # locate | trace | callers | tests | dependencies
check = "exact_symbol"     # exact_symbol | set_f1 | accepted_path
# f1_threshold = 0.8       # required for set_f1, in (0, 1]; forbidden otherwise

[limits]
max_budget_usd = 1.50      # passed as --max-budget-usd; positive number
max_turns = 30             # enforced by the runner; positive integer
wall_seconds = 600         # enforced by the runner; positive number
```

`prompt.md` is the exact task text. The runner sends it byte-for-byte on the harness's stdin, identically in both arms. It must end by requiring a final fenced `json` code block that is the answer.

`gold.json` is the expected answer in the shape its check type defines below. `run_study.py validate` loads every task and feeds each gold answer through its own check. A task whose gold does not pass is invalid.

## Answers and check types

The answer is the **last** fenced block whose info string is `json` in the harness's final result text. When there is no result event, the last assistant text is used. The answer status is:

- `missing` when there is no such block;
- `unparseable` when the block is not JSON;
- `invalid_shape` when the JSON does not have the shape below;
- `ok` otherwise.

Any status other than `ok` fails. The run's tokens are still counted. Extra top-level keys in an answer are ignored.

An *item* is `{"file": "<repository-relative path>", "symbol": "<symbol>"}`, and both values must be non-empty strings. Paths are normalized before comparison: `\` becomes `/`, and every leading `./` is removed. Nothing else changes: case, absolute paths and `..` are compared as given. Symbols compare exactly after trimming surrounding whitespace.

| Check | Answer | Gold | Passes when |
|---|---|---|---|
| `exact_symbol` | an item | an item | file and symbol both equal gold |
| `set_f1` | `{"items": [item, ...]}` | `{"items": [item, ...]}`, non-empty | F1 ≥ `f1_threshold` |
| `accepted_path` | `{"path": [item, ...]}` | `{"accepted_paths": [[item, ...], ...]}`, each path non-empty | the path equals one accepted path exactly, in order and length |

For `set_f1`, both sides are sets of normalized items, so duplicates collapse. Precision is |A∩G|/|A|, recall is |A∩G|/|G|, and F1 is their harmonic mean, F1 = 2·|A∩G| / (|A| + |G|), which is 0 when |A∩G| is 0. An empty answer, and any answer that does not have status `ok`, scores precision, recall and F1 of 0. The pass/fail decision is exact: F1 is computed as a rational number and compared with `f1_threshold` read as the exact decimal written in `task.toml`, so an F1 of exactly 4/5 meets `0.8`. All three values are reported as floats rounded from the exact values.

## study.toml

A study is `benchmark/studies/<study-id>/study.toml`. It is committed and public-safe, and `study_id` must equal its directory name. [pilot-01](../studies/pilot-01/study.toml) is the pilot's manifest. The runner refuses a manifest that breaks any of these rules:

- `kind` is `"pilot"` or `"confirmatory"`, and `tasks` is a non-empty list of distinct opaque IDs. A confirmatory study must also satisfy [Confirmatory studies](#confirmatory-studies).
- `arms` is a subset of `["B", "C"]`, and `trials` is at least 1. `seed` is an integer. `retry_rule = "rerun_block_once"`.
- `budget_cap_usd` is positive, and `per_run_reserve_usd` is at least 0.
- `harness.model` is a full model name, never an alias.
- `harness.settings` is a JSON object string.
- `harness.flags` includes `-p`, `--setting-sources project`, `--strict-mcp-config`, `--disable-slash-commands`, `--no-session-persistence`, `--output-format stream-json` and `--verbose`. It must not contain `--bare`, `--mcp-config`, a permission bypass, or any flag the runner sets itself.
- `harness.tools` is exactly `Read, Grep, Glob, Bash`.
- `harness.append_system_prompt` equals the "Config B prompt, verbatim" block in BENCHMARK.md, byte for byte.
- `[arms_config.B]` has `rivet = false`, denies `Bash(rivet:*)` and allows nothing that mentions rivet.
- `[arms_config.C]` has `rivet = true` and `snippet_file = "CLAUDE.md"`, and allows `Bash(rivet:*)`.
- Both arms deny `Edit, Write, NotebookEdit, WebFetch, WebSearch, Agent, Task`. Their allow and deny lists must be identical apart from `Bash(rivet:*)`.
- `[sandbox]` has `deny_read_paths` (absolute or `~/` paths), `deny_read_regexes` (anchored at `^/`) and `deny_rivet_exec_in_b = true`. The section is part of the manifest, so it is hashed into `study_manifest_sha256`.
- A `[confirmatory]` table appears only with `kind = "confirmatory"`.

### Confirmatory studies

A confirmatory study keeps every rule above, and adds these:

- `arms` is exactly `["B", "C"]`.
- It has a `[confirmatory]` table. Every key is required, and no other key is accepted, so a misspelt frozen setting cannot be silently ignored.

```toml
[confirmatory]
preregistration = "benchmark/preregistration.md"   # relative to the git top level of the manifest's repository; no ".", ".." or empty parts
preregistration_sha256 = "<64 lowercase hex>"      # sha256 of the preregistration's bytes
rivet_binary_sha256 = "<64 lowercase hex>"         # sha256 of the rivet binary arm C runs
tasks_sha256 = "<64 lowercase hex>"                # see tasks_sha256 below
analysis_version = "t48a-1"                        # must equal report.py's CONFIRMATORY_ANALYSIS_VERSION, or the outcome is invalid
analysis_seed = 20260927                           # integer >= 0 (random.Random ignores a seed's sign)
bootstrap_replicates = 10000                       # integer >= 1000
min_complete_trials_per_task = 3                   # integer in [1, trials]
max_excluded_block_fraction = 0.10                 # number in [0, 1)
efficiency_gate = "upper_bound_below_1"            # the only accepted value
```

**`tasks_sha256`.** It is the sha256 of the UTF-8 text with one line per regular file under each listed task directory, `<task-id>/<relative path>\t<sha256 of the file bytes>\n`, with `/` as the separator. The lines are sorted by the UTF-8 bytes of `<task-id>/<relative path>` across all tasks (so `a-b/` sorts before `a/`), and the text holds no absolute path. Every regular file counts, including one the runner never reads, such as `.DS_Store`. A symlink or any other non-regular entry, including a symlinked task directory, is refused rather than skipped, because the runner would read through a symlink that the hash did not cover. A file name with a tab or newline, or one that is not UTF-8, is refused too. `study.tasks_sha256` implements it, and `run_study.py hash-tasks <study.toml>` prints it (see Runner).

**Refusals.** `validate`, `plan` and `run` refuse a confirmatory study, with exit 2 and before anything is written, unless all of these hold:

| Check | Holds when | The refusal prints |
|---|---|---|
| preregistration hash | the file exists and its sha256 equals `preregistration_sha256` | the manifest hash and the file's hash (or `none`) |
| preregistration git | the file is tracked (`git ls-files --error-unmatch`), exists in `HEAD`, is byte-identical to its `HEAD` blob, and `git status --porcelain` shows nothing for it (nothing staged either), in the repository that holds the manifest | the file's hash, and the manifest hash or the `HEAD` blob's hash (`none` when the file is not in `HEAD`) |
| tasks hash | the listed tasks in `$RIVET_PILOT_TASKS_DIR` hash to `tasks_sha256` | the manifest hash and the computed hash |
| rivet binary (`run` only) | `RIVET_PILOT_RIVET_BIN` hashes to `rivet_binary_sha256`; checked again when the runner pins its copy | the manifest hash and the binary's hash |
| dry run (`run` only) | `RIVET_PILOT_CLAUDE_BIN` is unset, or `--dry-run` is given | which check failed |

Each refusal message begins `refused: <check> check failed`.

**Dry runs.** When `RIVET_PILOT_CLAUDE_BIN` is set, the harness is a replacement, and every record (and `manifest.json`) has `dry_run = true`. For any study, `run --dry-run` without `RIVET_PILOT_CLAUDE_BIN` is refused, because it would run the real harness. A confirmatory study also refuses to `run` when its study directory already holds a `manifest.json` or a record whose dry-run flag differs from this run's. A real run would otherwise resume a rehearsal, skip the attempts the rehearsal recorded, and inherit its ledger. A rehearsal therefore uses its own `RIVET_PILOT_RUNS_DIR`, with the real manifest and tasks. `report.py` never evaluates a gate from a dry-run row (see [Confirmatory analysis](#confirmatory-analysis)).

## Runner

```bash
python3 benchmark/runner/run_study.py validate   benchmark/studies/<id>/study.toml   # tasks + gold self-check
python3 benchmark/runner/run_study.py plan       benchmark/studies/<id>/study.toml   # schedule, worst case, argv
python3 benchmark/runner/run_study.py run        benchmark/studies/<id>/study.toml   # spends usage
python3 benchmark/runner/run_study.py run        benchmark/studies/<id>/study.toml --dry-run   # with RIVET_PILOT_CLAUDE_BIN
python3 benchmark/runner/run_study.py hash-tasks benchmark/studies/<id>/study.toml   # prints tasks_sha256
```

`hash-tasks` needs only `RIVET_PILOT_TASKS_DIR`. It prints the digest on stdout and checks nothing else, so it can compute the value a new manifest freezes. For a confirmatory manifest it also says on stderr whether the digest matches `tasks_sha256`.

The runner needs four environment variables, none of which has a default:

- `RIVET_PILOT_TASKS_DIR`
- `RIVET_PILOT_RUNS_DIR`
- `RIVET_CORPUS_DIR`
- `RIVET_PILOT_RIVET_BIN` (for arm C)

`RIVET_PILOT_CLAUDE_BIN` optionally replaces `claude`, which is how the tests use a fake agent. Every record of such a run is a dry run (see [Confirmatory studies](#confirmatory-studies)). The runner refuses to start if any of the private directories lies inside this repository. Exit codes:

| Code | Meaning |
|---|---|
| 0 | Finished |
| 1 | Error, including a setup integrity failure |
| 2 | Invalid manifest or task, or a confirmatory refusal |
| 3 | Stopped by the budget guard |

A second `run` resumes. Attempts that already have a record are skipped. An attempt that was started but has no record (the runner died) is recorded as `runner_interrupted`.

**Schedule.** Runs are grouped into blocks, one per task and trial. Blocks run trial-major, then in manifest task order. Within a block, the arm order is a shuffle seeded by `sha256("<seed>:<task>:<trial>")`, so each block's order depends only on its own identity. The schedule is written to `manifest.json`.

**Study directory layout.** `$RIVET_PILOT_RUNS_DIR/<study>/` holds four trees, kept apart so the sandbox can deny everything except one workspace:

| Path | Holds | Agent read access |
|---|---|---|
| `runs/<attempt-id>/` | records, transcripts, profiles | denied |
| `workspaces/<attempt-id>/` | the agent's working copy | only its own |
| `tools/rivet` | the pinned rivet copy, made once per study and hash-checked against `RIVET_PILOT_RIVET_BIN` | readable in both arms; executable only in C |
| `tools/exec-probe/rivet` | a copy of `/bin/echo` for the arm-B exec probe | readable; exec denied in B |
| `ledger.jsonl`, `manifest.json` | spend ledger, study manifest | denied |

**Workspace, per attempt.** The runner prepares a fresh workspace for every attempt:

1. It copies `$RIVET_CORPUS_DIR/<project>/` to `$RIVET_PILOT_RUNS_DIR/<study>/workspaces/<attempt-id>/`, preserving symlinks. Every `.rivet/` directory is excluded, and the source is never written.
2. It hashes the copied tree and records it as `corpus_tree_sha256`. If the copy has `.git`, its `HEAD` must equal the `commit` pinned in `benchmark/corpus.toml`.
3. If the workspace has no `.git`, no ancestor directory may contain `.git` or `.rivet`, because rivet's root discovery would otherwise escape the workspace.
4. It normalizes instructions identically in both arms. `AGENTS.md`, `CLAUDE.md`, `CLAUDE.local.md` and every `.claude/` directory are removed at any depth. The path and sha256 of each one is recorded.
5. Arm C then runs `rivet init --write-snippet --snippet-file CLAUDE.md --json`. The resulting `CLAUDE.md` must equal `rivet snippet` byte for byte, and `.rivet/` must hold only `config.toml`, so the run starts cold. Arm B does nothing.
6. It replaces the workspace's `.git` with a fresh repository holding one commit of the prepared tree (see Git state). If `.rivet/` exists but is not ignored, the attempt aborts rather than committing the cache.
7. It hashes the workspace again (`workspace_pre_sha256`, excluding `.rivet/` and `.git/`), and again after the run. A difference sets `workspace_modified`.
8. It writes the attempt's sandbox profile to `runs/<attempt-id>/sandbox.sb` and runs the pre-flight probe (see Sandbox).

Any setup failure stops the study. Such a failure is an integrity problem that would repeat on every run.

**Git state.** The replacement repository is made with `git init --template= -b main`, then `git add -A` and one commit. User and system git configuration are ignored (`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` set to `/dev/null`), and hooks, gpg signing, autocrlf and fsmonitor are disabled. The author and committer are fixed (`rivet pilot <pilot@rivet.invalid>`, `2000-01-01T00:00:00+0000`), and the message is `rivet pilot workspace`. The commit is therefore a function of the prepared tree alone, and the runner records it as `workspace_git_reset_head`. The runner checks that `git status --porcelain` is empty. What Claude Code's git context shows the agent:

| Arm | Branch | Status | History | Tracked tree |
|---|---|---|---|---|
| B | `main` | clean | one commit, `rivet pilot workspace` | the normalized corpus, without `CLAUDE.md` |
| C | `main` | clean | one commit, `rivet pilot workspace` | also `CLAUDE.md` (the snippet) and the `.gitignore` line `rivet init` appended; `.rivet/` is ignored and untracked |

The commit hashes differ between arms because the trees differ. The original history is gone from the workspace, since the pin was already verified against the original `HEAD`.

**Sandbox.** Every agent process runs as `/usr/bin/sandbox-exec -f runs/<attempt-id>/sandbox.sb <claude argv>`, and the runner refuses to start without `sandbox-exec`. The profile (`sandbox.build_profile`) is:

```text
(version 1)
(allow default)
(deny file-read* (subpath "<each deny path>"))          ; manifest paths + tasks dir + corpus dir
(deny file-read* (regex #"<each manifest regex>"))
(deny file-read* (subpath "<runs dir>"))
(allow file-read-metadata (literal "<runs dir>"))       ; each ancestor of the workspace and tools,
(allow file-read-metadata (literal "<runs>/<study>"))   ;   stat only, so they can be entered
(allow file-read-metadata (literal "<runs>/<study>/workspaces"))
(allow file-read* (subpath "<runs>/<study>/tools"))
(allow file-read* (subpath "<runs>/<study>/workspaces/<attempt-id>"))
(deny process-exec (regex #"/rivet$"))                  ; arm B only
```

How the profile behaves (verified on macOS 27.0):

- **Resolved paths only.** Every path is resolved with `os.path.realpath` (`/tmp` is `/private/tmp`), because an unresolved path silently matches nothing.
- **Last match wins.** A later `allow` overrides an earlier `deny`.
- **Ancestors are stat-only.** Entering a directory needs metadata access to each ancestor. The ancestors can therefore be entered, but not listed or read.
- **Reads resolve symlinks.** A symlink to a denied path is denied, including from child processes.
- **Exec rule.** It matches the resolved executable path, so a symlink with another name that points at a `rivet` is blocked too.

Read rules are identical in both arms; arm B adds only the exec rule.

Before a study starts, the runner refuses if the runs directory, or the harness binary's resolved path, falls under a static deny rule. Pinning rivet into `tools/` keeps the binary C executes outside every denied path.

**Pre-flight probe.** Before each launch, the runner executes probes inside the attempt's exact profile, with the same cwd and environment:

| Probe | Must |
|---|---|
| `/bin/ls -a <workspace>` | succeed |
| `cat` of the task's own `gold.json` | be denied |
| `ls` of the tasks directory, corpus directory, study directory and `workspaces/` | be denied |
| `cat` of `ledger.jsonl` | be denied |
| `ls` of every manifest deny path that exists | be denied |
| B: `/bin/echo` | succeed |
| B: the `tools/exec-probe/rivet` stand-in, and the pinned `tools/rivet` | have their exec denied |
| C: `tools/rivet --version` | succeed |

"Denied" means a non-zero exit with the sandbox's "Operation not permitted". Any mismatch aborts the study before the agent starts. Every probe's result is logged in `record.json` as `sandbox_probe`.

**Harness command.** The argv is built by `study.harness_argv`, and the runner wraps it in `sandbox-exec` as above:

```text
claude <harness.flags> --model <model> --settings <settings> --append-system-prompt <config-B prompt>
       --tools Read,Grep,Glob,Bash --allowedTools <arm allow list> --disallowedTools <arm deny list>
       --max-budget-usd <task max_budget_usd>            (prompt.md on stdin)
```

The prompt goes on stdin because `--tools` and `--allowedTools` are variadic, so a positional prompt after them would be parsed as a tool name.

**Environment.** Every `RIVET_*` variable, and the parent-session markers `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT` and `CLAUDE_CODE_SSE_PORT`, are removed from the harness environment. `PATH` is rebuilt without any directory holding a `rivet` executable. Arm B must then fail to resolve `rivet`, or the attempt aborts. Arm C prepends the study's `tools/` directory, which holds only the pinned `rivet`.

**Limits.** The runner enforces the limits itself:

- `wall_seconds`: it kills the harness's whole process group, with SIGTERM and then SIGKILL after 5 seconds.
- `max_turns`: it kills the harness as soon as the stream shows more than that many distinct assistant messages. Claude Code 2.1.280 lists no `--max-turns` flag.
- Budget: `--max-budget-usd` is Claude Code's own limit.

After the harness exits, the runner kills whatever is left in its process group.

**Budget guard.** The ledger `ledger.jsonl` records a `start` and an `end` for every attempt. *Charged* spend is calculated per attempt:

| Attempt | Charged |
|---|---|
| Reported `total_cost_usd` | That cost. Under a subscription login this is Claude Code's estimate, labelled `claude_code_estimate`, not billing. |
| Cost unknown | `max_budget_usd + per_run_reserve_usd` (worst case) |
| Harness never started | 0 |

Before each block, the runner requires charged + Σ(pending runs' `max_budget_usd + per_run_reserve_usd`) ≤ `budget_cap_usd`. Before each run, it requires charged + `max_budget_usd + per_run_reserve_usd` ≤ `budget_cap_usd`. If either check fails, it writes a `stopped` entry and exits 3. The reserve exists because Claude Code checks its budget between turns, so a run can end above its own `--max-budget-usd`.

**Failure classification.** Every attempt gets one `termination_reason`:

| Reason | Infrastructure | Rule |
|---|---|---|
| `setup_failed` | yes | workspace/arm setup raised (the study stops) |
| `launch_failed` | yes | the harness binary could not be executed |
| `runner_interrupted` | yes | the runner died during the attempt |
| `provider_error` | yes | the result event is an error, or an assistant text block *begins* with a harness error prefix (`API Error`, `Invalid API key`, `OAuth token has expired`, `Please run /login`), and that text matches `API Error: 429/5xx`, `overloaded_error`, `rate_limit_error`, `"type": "api_error"`, `authentication_error` or a login/key message. Agent prose and tool output are never scanned. |
| `no_session` | yes | the harness exited without its `system/init` event |
| `wall_timeout` | no | killed by the runner at `wall_seconds` |
| `max_turns` | no | killed by the runner, or result subtype `error_max_turns` |
| `max_budget` | no | result subtype containing `budget` |
| `agent_error` | no | any other result event that is not a clean `success` |
| `agent_crash` | no | init seen, no result event, not killed |
| `completed` | no | result subtype `success`, not an error |

`pass` is true only for `completed` with a passing check. It is false for every other task failure, and unavailable (null) for infrastructure failures, which are not task outcomes.

**Retry and inclusion (frozen).** If any first attempt in a block is an infrastructure failure, the whole block (every arm) is rerun once as attempt 2. Both attempts' directories are kept. `extract.py` assigns `analysis_inclusion` as follows:

| Situation | Inclusion | `inclusion_reason` |
|---|---|---|
| Attempt 1 of a block without a retry | included | `first_attempt` |
| Attempt 1 of a retried block | excluded | `superseded_by_block_retry` |
| Attempt 2 | included | `block_retry` |
| Attempt 2 that is itself an infrastructure failure | excluded | `infrastructure_failure_after_retry` |
| Attempt 1 infrastructure failure with no attempt 2 (the study stopped) | excluded | `infrastructure_failure_retry_missing` |
| Attempt 1 run whose block retry never ran | included | `block_retry_missing` |
| Any B run flagged contaminated | excluded, reported in accounting | `contaminated` |

Task failures are always included, with their tokens.

**Run directory.** `$RIVET_PILOT_RUNS_DIR/<study>/runs/<attempt-id>/` holds the following:

- `transcript.jsonl`: the raw stream-json, never rewritten.
- `stderr.txt`
- `check.json`: the evaluator result.
- `record.json`
- `sandbox.sb`: the exact profile.

The study directory also holds `manifest.json` (versions, hashes, per-arm argv, schedule, task limits and hashes) and `ledger.jsonl`. The attempt ID is `<task>.t<trial>.<arm>.a<attempt>`, the run ID omits `.a<attempt>`, and the block ID is `<task>.t<trial>`. `record.json` holds:

- IDs, the harness and rivet versions, the rivet binary sha256, and the snippet hash;
- `harness_binary_is_override` and `dry_run`, both true when `RIVET_PILOT_CLAUDE_BIN` replaced the harness;
- `settings_hash`, which covers everything held constant across arms;
- `tool_policy_hash`, which covers the arm's allow and deny lists;
- the full argv, the names (not values) of the child environment variables, and the child `PATH`;
- limits, removed-instruction hashes, workspace hashes and `workspace_git_reset_head`, `rivet init` output, the outcome, the classification, cost, the evaluation, contamination evidence and isolation notes;
- `sandbox_profile_sha256`, the resolved `sandbox_deny_paths` and `sandbox_deny_regexes`, and `sandbox_probe`.

**Isolation notes.** These are recorded, not hidden. The init event's model must equal the pinned model, and its `tools` must be a subset of `harness.tools`. Its `mcp_servers` must be empty and its `apiKeySource` must be `none`. Any other model seen in usage is listed as `other_models:` (Claude Code makes small auxiliary requests).

## runs.csv

`python3 benchmark/runner/extract.py "$RIVET_PILOT_RUNS_DIR/<study>"` writes one row per attempt, including failures and retries, to `<study>/runs.csv`, which stays private. Before using a transcript, it checks that the transcript's sha256 still equals the one in its record. It independently re-runs the contamination scan on B transcripts and refuses a record that under-reports contamination.

Two values mark a gap, and neither is ever zero-filled:

- `unavailable`: the evidence does not support a value.
- `not_applicable`: the column does not apply to the arm.

Booleans are `true`/`false`, floats have six decimals, and maps are compact sorted JSON.

**Identity, schedule and inclusion**

| Column | Derivation |
|---|---|
| `run_id`, `attempt_id`, `block_id`, `task_id`, `trial`, `attempt`, `execution_order` | runner schedule; `execution_order` is the global start order |
| `project`, `category`, `check` | `task.toml` |
| `repository_commit` | `benchmark/corpus.toml` pin, verified against the copy's `HEAD` when it has `.git` |
| `config` | arm, `B` or `C` |
| `analysis_inclusion`, `inclusion_reason` | frozen rule above |

**Versions and settings**

| Column | Derivation |
|---|---|
| `model_id` | pinned model passed to `--model` |
| `models_observed` | models seen in assistant messages and `modelUsage` |
| `harness_version` | `claude --version` of the binary used |
| `dry_run` | the record's `dry_run`; for a record older than T48a, its `harness_binary_is_override` |
| `settings_hash`, `tool_policy_hash` | sha256 of the canonical JSON described above |
| `tool_version`, `snippet_hash` | C: `rivet --version`, and the sha256 of the installed `CLAUDE.md`. B: not applicable. |
| `max_budget_usd`, `max_turns`, `wall_seconds_limit` | task limits |

**Outcome**

| Column | Derivation |
|---|---|
| `pass` | see Failure classification; unavailable for infrastructure failures |
| `answer_status` | `ok`, `missing`, `unparseable` or `invalid_shape` |
| `precision`, `recall`, `f1` | `set_f1` only; not applicable otherwise |
| `termination_reason`, `infrastructure_failure`, `infrastructure_reason` | classification table |
| `contaminated`, `contamination_evidence` | B only: the evidence kinds (heuristic, below) |
| `isolation_violations` | isolation notes, or `none` |

**Tokens.** All values are provider-reported. `usage_source` names the source used, in this order of preference:

1. `result.modelUsage`, summed over every model.
2. `result.usage`.
3. `assistant_messages_partial`: per-message usage, de-duplicated by message ID (stream-json repeats a message's usage on each content block), used for a run with no result event.
4. `none`.

| Column | Derivation |
|---|---|
| `input_tokens_total` | uncached input + cache creation + cache reads; unavailable if any part is missing |
| `input_tokens_cached` | cache-read input tokens |
| `input_tokens_uncached` | uncached input + cache-creation input tokens |
| `input_tokens_cache_creation` | cache-creation input tokens, shown separately |
| `output_tokens` | output tokens |

**Tools and time**

| Column | Derivation |
|---|---|
| `num_turns` | result `num_turns`, else the distinct assistant messages |
| `tool_calls_total`, `tool_calls_by_name` | `tool_use` blocks, de-duplicated by ID |
| `permission_denials` | length of the result's `permission_denials`; unavailable if the key is absent |
| `edits`, `failed_edits` | `Edit`/`MultiEdit`/`NotebookEdit`/`Write` tool uses, and those whose result is an error. All are denied in the pilot, so they measure attempts only. |
| `workspace_modified` | pre- and post-run workspace tree hashes differ (excluding `.rivet/` and `.git/`) |
| `wall_clock_seconds` | runner-measured, from launch to exit (includes harness startup) |
| `harness_duration_seconds` | result `duration_ms` / 1000 |
| `indexing_seconds` | C: unavailable, because rivet indexes lazily inside the agent's first query and reports no timing there. B: not applicable. |
| `cost_usd_claude_code_estimate` | result `total_cost_usd`: Claude Code's estimate, not billing |

**Rivet (C only; B not applicable)**

| Column | Derivation |
|---|---|
| `rivet_invocations_by_command` | heuristic, below |
| `rivet_errors_by_exit` | heuristic, below |
| `refresh_mode` | the distinct `index.freshness` values found in rivet JSON output inside tool results; unavailable when the agent used no `--json` |
| `rivet_to_text_fallbacks` | heuristic, below |
| `snapshot_mismatches` | unavailable: no extractor compares returned spans with stored hashes yet |

**Evidence**

| Column | Derivation |
|---|---|
| `started_at` | UTC start time |
| `transcript_path`, `transcript_sha256` | the original transcript, relative to the study directory |

### Heuristic extractors

These are heuristics. Do not present their counts as exact.

- **Rivet invocations.** A Bash command is split on shell operators (`;`, `&&`, `||`, `|`, `&`, newlines, `$(`, backticks, parentheses) and shell-split into words. This is not full shell grammar. A word is an executed `rivet` when its basename is `rivet` and it appears in one of these positions:
  - first, after `VAR=value` assignments and the wrappers `command env exec nice nohup time timeout xargs sudo`;
  - after `-exec`, `-execdir` or `-ok`;
  - after `--pre` or `--pre=`.

  The subcommand is the first non-flag argument. The query is the next non-flag argument, skipping the values of `--tokens --offset --limit --mode --snippet-file --kind --depth --root`.
- **Exit codes.** Each Bash call that invokes rivet adds one count per rivet error to `rivet_errors_by_exit`, keyed by exit code. A call is read in this order:
  1. **A single simple rivet command** whose failed result begins `Exit code N` counts N once. Its error text is the same error and is not counted again.
  2. **Otherwise, the result text.** Each rivet human error found in it is counted, whether or not the call failed. rivet writes errors to stderr, which bypasses `| head` and is kept alongside a following `; rg`. The form is from `error_text` in `crates/rivet-cli/src/human.rs`:
     - a line that starts `rivet <command>: <message>`, where `<command>` is `index`, `init`, `symbol`, `refs` or `context`;
     - immediately followed by a line starting `hint: `.

     `<command>` must be the subcommand of one of the call's rivet invocations, and each invocation takes at most one error, in text order. The human text does not print the error code. So the code is read from the message, or from the first line after the hint, and mapped to its exit by OUTPUT-CONTRACT "Errors":

     | Exit | Code | Message, or first line after the hint |
     |---|---|---|
     | 5 | `ambiguous_symbol` | `query '<q>' matched <N> symbols`, or `candidates:` |
     | 4 | `symbol_not_found` | `query '<q>' matched no symbols` or `no symbol encloses <q>`, or `did you mean:` |
     | 6 | `parse_failure` | `<path> is not indexed: it has a syntax error` or `…: it exceeds a parser resource limit`, or `detail: ` |
     | 7 | `unsupported_language` | `<path> is <language>, which is not indexed` or `<path> is not in a supported language` |
     | 8 | `budget_too_small` | `the context target needs at least <N> estimated tokens, but the budget is <M>`, or `required_tokens: <N> (budget_tokens: <M>)` |
     | 9 | `repository_changed` | `the repository changed while it was being indexed…` |
     | 2 | `invalid_arguments` | `invalid value for ` …, … ` cannot be combined with ` …, or `` `--offset` is not supported by `rivet context` `` |
     | 3 | `repository_unavailable` | `no committed index at ` …, `the cache has no committed snapshot`, `incompatible cache: ` …, or `<path> is not indexed: ` for a binary, oversize or non-UTF-8 file |

     An error in this form whose message matches no row counts as `unattributed`. This covers `general` and messages that wrap an I/O, lock or config error.
  3. **A failed call with no matched error text** counts once as `unattributed`, as before, since the shell's status may belong to another program. When the text accounts for any error, the call's status is not counted again.

  A call without a result counts as `no_result`. Text that does not start a line never matches, so `rg` output (`path:line:…`) that quotes rivet or mentions `error` does not count. These are not read: `--json` error objects, and clap's own argument errors (`error: …`, exit 2).
- **Fallbacks.** This is BENCHMARK.md's "text-tool use within the next two tool calls on the same identifier/file". A rivet call counts one fallback when either of the next two tool calls matches the rivet query. The following call qualifies only if it is:
  - `Read`, `Grep` or `Glob`;
  - Bash that does not invoke rivet.

  Its input must contain one of these targets:
  - the rivet query as a whole word;
  - any `::`, `.`, `\`, `->` or `#` component of the query that is at least 3 characters;
  - the file, for a `file:line` query.

  Files that appear only in rivet's output are not targets, so reading a file rivet pointed to is not a fallback.
- **Contamination (arm B).** The scan is conservative, and any one of these kinds flags the run:

  | Kind | Evidence |
  |---|---|
  | `transcript_invocation` | a Bash command executes rivet (parser above) |
  | `transcript_mention` | a Bash command contains the word `rivet`, after the workspace path is replaced |
  | `transcript_rivet_output` | a tool result contains rivet's JSON snapshot marker, `"snapshot":"blake3:` |
  | `workspace_rivet_dir` | `.rivet/` exists in the B workspace after the run |

  A B run that merely greps the corpus for the word `rivet` is therefore flagged. The evidence is kept in `record.json` for audit.

## Report

The report is generated from `runs.csv`:

```bash
python3 benchmark/runner/report.py "$RIVET_PILOT_RUNS_DIR/<study>/runs.csv" \
    --study benchmark/studies/<study>/study.toml --out-dir benchmark/results/<study>
```

It writes `report.md` from [REPORT-TEMPLATE](../REPORT-TEMPLATE.md), plus `summary.json` and `per-task.csv`. These outputs hold only aggregates, keyed by opaque task ID, category and project. The generator refuses any ID, category or project value that is not a plain token. The metrics are computed as follows:

- **Task means.** Each task's mean per arm is taken over its included attempts that have a value. Missing values are counted separately and are never zero.
- **Arm means.** Each arm's mean is the mean of its task means, with equal task weighting.
- **Comparisons.** C/B comparisons use the tasks that have a value in both arms. The comparisons are:
  - the input-token ratio and reduction, `100 × (1 − C/B)`;
  - the success difference, in percentage points;
  - the relative change in wall time, tool calls and output tokens.

  A zero or missing denominator is reported as undefined, with its reason.

For a pilot, every confirmatory gate is `NOT EVALUATED (pilot)` and no interval is computed. Identical inputs give byte-identical outputs.

### Confirmatory analysis

For a confirmatory manifest, `report.py` applies the following. `--corpus-manifest` (default `benchmark/corpus.toml`) supplies each project's languages. All means are exactly rounded (`math.fsum`), so a result does not depend on the Python version's float summation.

**Inclusion.** The frozen retry rule is already applied by `extract.py` (see "Retry and inclusion"). Failures of any kind (timeout, crash, invalid answer, a limit reached) count as `pass = 0`, with the tokens they consumed. A scheduled task/trial block is *included* only when each arm has exactly one row with `analysis_inclusion = included`, an available `input_tokens_total`, and a `pass` of `true` or `false`. Otherwise the block is *excluded* from **both** arms. The reason is recorded per arm:

| Reason | Meaning |
|---|---|
| `missing_run` | no row at all for that arm (for example, the study stopped) |
| the latest attempt's `inclusion_reason` | no included row: `infrastructure_failure_after_retry`, `infrastructure_failure_retry_missing`, `contaminated`, ... |
| `missing_usage` | the included row has no `input_tokens_total` |
| `missing_pass` | the included row has no pass value |

Every excluded block is listed, with its reasons, in `summary.json` `exclusions` and in `report.md`. A row outside the manifest's schedule (an unknown task, a trial beyond `trials`, a mismatched `block_id`), a task whose rows disagree on project or category, or two included attempts for one block and arm is an input error, and no report is written.

**Validity.** The outcome is `invalid`, and no gate is evaluated, when any of these holds. Every reason that applies is reported, in this order:

| Reason | Holds when |
|---|---|
| `dry_run` | some row has `dry_run = true` |
| `dry_run_unknown` | some row does not record `dry_run` as `true` or `false` |
| `analysis_version_mismatch` | the manifest's `analysis_version` differs from `report.py`'s `CONFIRMATORY_ANALYSIS_VERSION` (`t48a-1`) |
| `too_few_complete_trials` | some task has fewer than `min_complete_trials_per_task` included blocks |
| `too_many_excluded_blocks` | excluded blocks ÷ scheduled blocks exceeds `max_excluded_block_fraction`, compared exactly as the decimal written in the manifest (1 of 8 does not exceed `0.125`) |
| `no_included_blocks` | no task has an included block, so no estimate exists |
| `zero_denominator` | the point estimate's B mean input tokens is 0 |

**Point estimates.** They use equal task weighting over the tasks with at least one included block. For each such task and arm, the task mean input tokens (`input_tokens_total`) and the pass rate are taken over its included blocks. Then:

- `ratio` = (mean over tasks of the C task means) ÷ (mean over tasks of the B task means);
- `success_diff_pp` = 100 × (mean over tasks of the C pass rate − mean over tasks of the B pass rate).

**Paired cluster bootstrap.** This is the exact procedure, so a result can be reproduced by hand:

1. The strata are languages. A task's language is the single entry of its project's `languages` in the corpus manifest. A project with more than one language (or none) is an error for now. Each stratum holds the tasks that have an included block, sorted by task ID.
2. One `rng = random.Random(analysis_seed)` (Python's Mersenne Twister) drives every draw.
3. For each of the `bootstrap_replicates` replicates: for each stratum in sorted name order, with n tasks, draw n times `tasks[rng.randrange(n)]`. Each stratum keeps its size.
4. A drawn task brings all of its included blocks in both arms. The replicate's `ratio` and `success_diff_pp` are the two point estimates over the drawn tasks, with each draw weighted once. When a replicate's B mean is 0, its ratio is `+inf`, which is conservative. Such replicates are counted in `zero_denominator_replicates`.
5. Upper bound on the ratio: sort the replicate ratios ascending and take index `ceil(0.95 × R) − 1`. Lower bound on the success difference: sort ascending and take index `ceil(0.05 × R) − 1`. Both indices use integer arithmetic. With R = 10000 they are 9499 and 499; with R = 20 they are 18 and 0.

The bootstrap is still computed for an invalid study, when a task has an included block, so a dry run exercises it. Only the gates are withheld.

**Gates and outcome.** The only efficiency gate is `efficiency_gate = "upper_bound_below_1"`:

- `quality_pass` = lower bound > −5.0 percentage points;
- `efficiency_pass` = upper bound < 1.00 (`+inf` fails).

The outcome is `invalid` under the validity rules; otherwise `fail_quality` when quality fails; otherwise `inconclusive` when efficiency fails; otherwise `pass`. The template's "C/B ≤ 0.70" point-estimate row is reported as `NOT A GATE`, because the manifest's gate wins, and the report states which gates were evaluated.

**summary.json** holds `analysis_version` (the script's), `manifest_analysis_version`, `dry_run`, `inputs` (runs.csv, manifest, preregistration, binary and tasks hashes), `outcome`, `validity`, `blocks` (scheduled, included, excluded, and included blocks per task), `exclusions`, `primary` (the point estimates), `bootstrap` (seed, replicates, strata, both indices, both bounds, and `zero_denominator_replicates`), `gates`, `per_task_primary`, `secondary`, `accounting`, `adoption_c` and `spend`. It is strict JSON: an infinite bound is the string `"+inf"`. Gate pass values are `null` when no gate is evaluated.

**Secondary results.** These are point estimates only, labelled secondary, with no interval and no gate. All of them use included blocks only:

- the successful-run-only input-token ratio, over tasks with a passing run in both arms;
- cost (Claude Code estimate), tool-call, wall-clock and output-token ratios, over tasks with a value in both arms (missing values are counted, never zero);
- per-task, per-category and per-language tables;
- adoption and fallbacks, over the C runs of included blocks.

**report.md.** The title and status say `CONFIRMATORY` and the outcome. A report with any dry-run row is titled `(DRY RUN)`, its status says `DRY RUN`, and its gate cells read `NOT EVALUATED (dry run)`. An invalid study's gate cells read `NOT EVALUATED (invalid study)`. The template's "Two-sided 95% interval" column says that no two-sided interval is computed, and gives the one-sided bound instead.

## Tests

```bash
python3 benchmark/runner/test_runner.py        # standard library only; add -v for names
```

The tests never call a model, the network or the real `claude`. Every `run` in them sets `RIVET_PILOT_CLAUDE_BIN`, so no refusal test could reach a real harness even if the refusal were missing. Confirmatory tests build their own throwaway git repository for the manifest and preregistration. Agent runs use `testdata/fake_claude.py`, selected via `RIVET_PILOT_CLAUDE_BIN`, which replays the canned stream-json transcripts in `testdata/transcripts/`. The fake agent runs inside the same `sandbox-exec` profile, and it can attempt reads and execs from inside it, so the tests prove the confinement from the agent's side. Arm C uses `testdata/fake_rivet.py`. The synthetic tasks and corpus are in `testdata/`. The tests create the instruction files and the stale `.rivet/` at runtime, so this repository carries none.

## Known limitations

- **No fresh home directory.** The harness authenticates with the user's own login, so each run cannot get a fresh home directory as BENCHMARK.md asks. Isolation relies on `--setting-sources project`, `--strict-mcp-config`, `--disable-slash-commands`, `--no-session-persistence` and the removal of project instructions. The init event is checked and recorded.
- **Remaining rivet evasions.** Arm B's rivet denial is the OS exec rule on any `*/rivet`, plus the `Bash(rivet:*)` deny rule, the absence of rivet from `PATH`, and the post-run transcript and workspace scan. A rivet binary copied under a different file name would evade the exec rule. Arm B cannot create one, because it has no write or copy tool.
- **Parent directory names are visible.** The sandbox denies reads, not writes, outside the workspace; the tools prevent writes. The workspace's ancestors can be stat'ed, but not listed.
- **Untested harness paths.** Two denied paths may be ones Claude Code itself uses at runtime. They are untestable without a model call, and are reported rather than weakened:
  - `^/private/tmp/claude-` also covers the per-user scratch area Claude Code keeps under `/private/tmp/claude-<uid>/`.
  - `~/.claude/projects` is where Claude Code keeps per-project state.
