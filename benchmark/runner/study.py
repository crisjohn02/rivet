"""Study manifest, run schedule, budget ledger and harness command (T38a),
and the freezing checks of a confirmatory study (T48a).

A study is `benchmark/studies/<study-id>/study.toml` (committed, public-safe:
opaque task IDs and settings only). See TASK-FORMAT.md "study.toml" and
"Confirmatory studies". Standard library only.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
import re
import shutil
import stat
import subprocess
import tomllib

from checks import TASK_ID_RE

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BENCHMARK_DOC = os.path.join(REPO_ROOT, "docs", "BENCHMARK.md")
CORPUS_TOML = os.path.join(REPO_ROOT, "benchmark", "corpus.toml")

STUDY_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
SUPPORTED_ARMS = ("B", "C")
MODEL_ALIASES = {"opus", "sonnet", "haiku", "fable", "default", "best", "opusplan"}
MODEL_RE = re.compile(r"^claude-[a-z]+-\d[a-z0-9.-]*$")

# Flags every run must carry (the user's isolation decisions). The runner
# refuses a manifest whose `harness.flags` omit any of them.
REQUIRED_FLAGS = (
    ("-p",),
    ("--setting-sources", "project"),
    ("--strict-mcp-config",),
    ("--disable-slash-commands",),
    ("--no-session-persistence",),
    ("--output-format", "stream-json"),
    ("--verbose",),
)
# Flags the runner adds itself; a manifest may not smuggle them in.
RUNNER_OWNED_FLAGS = {
    "--model",
    "--settings",
    "--append-system-prompt",
    "--system-prompt",
    "--tools",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--max-budget-usd",
    "--mcp-config",
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--add-dir",
    "--plugin-dir",
    "--agents",
    "--resume",
    "--continue",
}
COMMON_DENIED = ("Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "Agent", "Task")
RIVET_ALLOW = "Bash(rivet:*)"
# Environment variables that mark the parent process as a Claude Code
# session; a nested harness must not inherit them.
STRIPPED_ENV = ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CLAUDE_CODE_SSE_PORT")

STUDY_KINDS = ("pilot", "confirmatory")
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")
# The only efficiency gate the analysis implements (TASK-FORMAT.md
# "Gates and outcome"): the one-sided 95% upper bound on C/B is below 1.00.
EFFICIENCY_GATES = ("upper_bound_below_1",)
MIN_BOOTSTRAP_REPLICATES = 1000
CONFIRMATORY_KEYS = (
    "preregistration",
    "preregistration_sha256",
    "rivet_binary_sha256",
    "tasks_sha256",
    "analysis_version",
    "analysis_seed",
    "bootstrap_replicates",
    "min_complete_trials_per_task",
    "max_excluded_block_fraction",
    "efficiency_gate",
)


class StudyError(ValueError):
    """The manifest or the environment does not allow a safe study run."""


class FreezeError(StudyError):
    """A confirmatory study does not match its frozen preregistration, tasks
    or rivet binary, or was asked to run in a way it must refuse."""


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_hash(value) -> str:
    return sha256_bytes(json.dumps(value, sort_keys=True, separators=(",", ":")).encode())


def config_b_prompt_from_doc(path: str = BENCHMARK_DOC) -> str | None:
    """The fenced block after "Config B prompt, verbatim:" in BENCHMARK.md."""
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as handle:
        text = handle.read()
    match = re.search(r"Config B prompt, verbatim:\s*```text\n(.*?)\n```", text, re.DOTALL)
    return match.group(1) if match else None


def _string_list(value, where: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(v, str) and v for v in value):
        raise StudyError(f"{where}: expected a list of non-empty strings")
    return list(value)


def _is_int(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _confirmatory_table(table, trials: int) -> dict:
    """Validates the `[confirmatory]` table (TASK-FORMAT.md "Confirmatory
    studies"). Every key is required and no other key is accepted, so a
    misspelt frozen setting cannot be silently ignored."""
    if not isinstance(table, dict):
        raise StudyError('study.toml: kind = "confirmatory" requires a [confirmatory] table')
    unknown = sorted(set(table) - set(CONFIRMATORY_KEYS))
    if unknown:
        raise StudyError(f"study.toml: [confirmatory] has unknown keys: {', '.join(unknown)}")
    missing = [key for key in CONFIRMATORY_KEYS if key not in table]
    if missing:
        raise StudyError(f"study.toml: [confirmatory] is missing {', '.join(missing)}")
    prereg = table["preregistration"]
    if (
        not isinstance(prereg, str)
        or not prereg
        or prereg.startswith("/")
        or "\\" in prereg
        or any(part in ("", ".", "..") for part in prereg.split("/"))
    ):
        raise StudyError("study.toml: confirmatory.preregistration must be a repository-relative path without '.', '..' or empty components")
    for key in ("preregistration_sha256", "rivet_binary_sha256", "tasks_sha256"):
        value = table[key]
        if not isinstance(value, str) or not HEX64_RE.match(value):
            raise StudyError(f"study.toml: confirmatory.{key} must be 64 lowercase hex digits")
    version = table["analysis_version"]
    if not isinstance(version, str) or not version.strip():
        raise StudyError("study.toml: confirmatory.analysis_version must be a non-empty string")
    seed = table["analysis_seed"]
    # random.Random uses the absolute value of an integer seed, so a negative
    # seed would silently equal its positive twin.
    if not _is_int(seed) or seed < 0:
        raise StudyError("study.toml: confirmatory.analysis_seed must be a non-negative integer")
    replicates = table["bootstrap_replicates"]
    if not _is_int(replicates) or replicates < MIN_BOOTSTRAP_REPLICATES:
        raise StudyError(f"study.toml: confirmatory.bootstrap_replicates must be an integer >= {MIN_BOOTSTRAP_REPLICATES}")
    minimum = table["min_complete_trials_per_task"]
    if not _is_int(minimum) or not 1 <= minimum <= trials:
        raise StudyError(f"study.toml: confirmatory.min_complete_trials_per_task must be an integer in [1, trials = {trials}]")
    fraction = table["max_excluded_block_fraction"]
    if (
        isinstance(fraction, bool)
        or not isinstance(fraction, (int, float))
        or not math.isfinite(fraction)
        or not 0 <= fraction < 1
    ):
        raise StudyError("study.toml: confirmatory.max_excluded_block_fraction must be a number in [0, 1)")
    gate = table["efficiency_gate"]
    if gate not in EFFICIENCY_GATES:
        raise StudyError(f"study.toml: confirmatory.efficiency_gate must be one of {', '.join(EFFICIENCY_GATES)}")
    return {
        "preregistration": prereg,
        "preregistration_sha256": table["preregistration_sha256"],
        "rivet_binary_sha256": table["rivet_binary_sha256"],
        "tasks_sha256": table["tasks_sha256"],
        "analysis_version": version,
        "analysis_seed": seed,
        "bootstrap_replicates": replicates,
        "min_complete_trials_per_task": minimum,
        "max_excluded_block_fraction": float(fraction),
        "efficiency_gate": gate,
    }


def load_study(path: str) -> dict:
    """Loads and validates a study manifest. Raises StudyError."""
    with open(path, "rb") as handle:
        try:
            raw = tomllib.load(handle)
        except tomllib.TOMLDecodeError as error:
            raise StudyError(f"{path}: {error}") from error
    if raw.get("schema_version") != 1:
        raise StudyError("study.toml: schema_version must be 1")
    study_id = raw.get("study_id")
    if not isinstance(study_id, str) or not STUDY_ID_RE.match(study_id):
        raise StudyError("study.toml: study_id must be an opaque id")
    if os.path.basename(os.path.dirname(os.path.abspath(path))) != study_id:
        raise StudyError(f"study.toml: study_id {study_id!r} must match its directory name")
    kind = raw.get("kind")
    if kind not in STUDY_KINDS:
        raise StudyError('study.toml: kind must be "pilot" or "confirmatory"')
    tasks =_string_list(raw.get("tasks", []), "tasks") if raw.get("tasks") else []
    if not tasks:
        raise StudyError("study.toml: tasks is empty; T38 defines the pilot task IDs")
    for task in tasks:
        if not TASK_ID_RE.match(task):
            raise StudyError(f"study.toml: task id {task!r} is not opaque")
    if len(set(tasks)) != len(tasks):
        raise StudyError("study.toml: duplicate task id")
    arms = _string_list(raw.get("arms"), "arms")
    if sorted(arms) != sorted(set(arms)) or not set(arms) <= set(SUPPORTED_ARMS) or not arms:
        raise StudyError(f"study.toml: arms must be distinct values from {SUPPORTED_ARMS}")
    trials = raw.get("trials")
    if isinstance(trials, bool) or not isinstance(trials, int) or trials < 1:
        raise StudyError("study.toml: trials must be a positive integer")
    seed = raw.get("seed")
    if isinstance(seed, bool) or not isinstance(seed, int):
        raise StudyError("study.toml: seed must be an integer")
    cap = raw.get("budget_cap_usd")
    if isinstance(cap, bool) or not isinstance(cap, (int, float)) or cap <= 0:
        raise StudyError("study.toml: budget_cap_usd must be positive")
    reserve = raw.get("per_run_reserve_usd", 0.0)
    if isinstance(reserve, bool) or not isinstance(reserve, (int, float)) or reserve < 0:
        raise StudyError("study.toml: per_run_reserve_usd must be >= 0")
    if raw.get("retry_rule") != "rerun_block_once":
        raise StudyError("study.toml: retry_rule must be \"rerun_block_once\"")
    confirmatory = None
    if kind == "confirmatory":
        if arms != ["B", "C"]:
            raise StudyError('study.toml: a confirmatory study must have arms = ["B", "C"] exactly')
        confirmatory = _confirmatory_table(raw.get("confirmatory"), trials)
    elif "confirmatory" in raw:
        raise StudyError('study.toml: [confirmatory] is only valid with kind = "confirmatory"')

    harness = raw.get("harness")
    if not isinstance(harness, dict):
        raise StudyError("study.toml: missing [harness]")
    model = harness.get("model")
    if not isinstance(model, str) or model in MODEL_ALIASES or not MODEL_RE.match(model):
        raise StudyError("study.toml: harness.model must be a full pinned model name, never an alias")
    settings = harness.get("settings")
    try:
        settings_obj = json.loads(settings) if isinstance(settings, str) else None
    except json.JSONDecodeError:
        settings_obj = None
    if not isinstance(settings_obj, dict):
        raise StudyError("study.toml: harness.settings must be a JSON object string")
    flags = _string_list(harness.get("flags"), "harness.flags")
    for required in REQUIRED_FLAGS:
        if not any(flags[i : i + len(required)] == list(required) for i in range(len(flags))):
            raise StudyError(f"study.toml: harness.flags must include {' '.join(required)}")
    for flag in flags:
        if flag.split("=", 1)[0] in RUNNER_OWNED_FLAGS or flag == "--bare":
            raise StudyError(f"study.toml: harness.flags may not contain {flag}; the runner owns it")
    tools = _string_list(harness.get("tools"), "harness.tools")
    if sorted(tools) != sorted({"Read", "Grep", "Glob", "Bash"}):
        raise StudyError("study.toml: harness.tools must be exactly Read, Grep, Glob, Bash")
    prompt = harness.get("append_system_prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        raise StudyError("study.toml: harness.append_system_prompt is required")
    prompt = prompt.rstrip("\n")
    doc_prompt = config_b_prompt_from_doc()
    if doc_prompt is not None and doc_prompt != prompt:
        raise StudyError("study.toml: append_system_prompt differs from the BENCHMARK.md config-B prompt")

    arm_cfg = raw.get("arms_config")
    if not isinstance(arm_cfg, dict):
        raise StudyError("study.toml: missing [arms_config.<arm>] tables")
    arms_out = {}
    for arm in arms:
        cfg = arm_cfg.get(arm)
        if not isinstance(cfg, dict):
            raise StudyError(f"study.toml: missing [arms_config.{arm}]")
        allowed = _string_list(cfg.get("allowed_tools"), f"arms_config.{arm}.allowed_tools")
        denied = _string_list(cfg.get("disallowed_tools"), f"arms_config.{arm}.disallowed_tools")
        rivet = cfg.get("rivet")
        if rivet is not (arm == "C"):
            raise StudyError(f"study.toml: arms_config.{arm}.rivet must be {arm == 'C'}")
        for tool in COMMON_DENIED:
            if tool not in denied:
                raise StudyError(f"study.toml: arms_config.{arm} must deny {tool}")
        if arm == "B":
            if RIVET_ALLOW not in denied:
                raise StudyError("study.toml: arm B must deny Bash(rivet:*) explicitly")
            if any("rivet" in tool for tool in allowed):
                raise StudyError("study.toml: arm B may not allow any rivet tool")
        else:
            if RIVET_ALLOW not in allowed:
                raise StudyError("study.toml: arm C must allow Bash(rivet:*)")
            if any("rivet" in tool for tool in denied):
                raise StudyError("study.toml: arm C may not deny rivet")
            if cfg.get("snippet_file") != "CLAUDE.md":
                raise StudyError("study.toml: arm C snippet_file must be CLAUDE.md")
        for tool in allowed:
            base = tool.split("(", 1)[0]
            if base not in tools:
                raise StudyError(f"study.toml: arms_config.{arm} allows {tool}, outside harness.tools")
        arms_out[arm] = {"allowed_tools": allowed, "disallowed_tools": denied, "rivet": rivet}
    if set(arms) == {"B", "C"}:
        b_allowed = set(arms_out["B"]["allowed_tools"])
        c_allowed = set(arms_out["C"]["allowed_tools"]) - {RIVET_ALLOW}
        if b_allowed != c_allowed:
            raise StudyError("study.toml: arms B and C must allow the same tools apart from Bash(rivet:*)")
        if set(arms_out["B"]["disallowed_tools"]) - {RIVET_ALLOW} != set(arms_out["C"]["disallowed_tools"]):
            raise StudyError("study.toml: arms B and C must deny the same tools apart from Bash(rivet:*)")
    sandbox = raw.get("sandbox")
    if not isinstance(sandbox, dict):
        raise StudyError("study.toml: missing [sandbox]")
    deny_paths = sandbox.get("deny_read_paths")
    if not isinstance(deny_paths, list) or not all(isinstance(p, str) and (p.startswith("/") or p.startswith("~/")) for p in deny_paths):
        raise StudyError("study.toml: sandbox.deny_read_paths must be a list of absolute or ~/ paths")
    deny_regexes = sandbox.get("deny_read_regexes")
    if not isinstance(deny_regexes, list) or not all(isinstance(p, str) and p.startswith("^/") for p in deny_regexes):
        raise StudyError("study.toml: sandbox.deny_read_regexes must be a list of regexes anchored at ^/")
    for pattern in deny_regexes:
        try:
            re.compile(pattern)
        except re.error as error:
            raise StudyError(f"study.toml: sandbox regex {pattern!r}: {error}") from error
    if sandbox.get("deny_rivet_exec_in_b") is not True:
        raise StudyError("study.toml: sandbox.deny_rivet_exec_in_b must be true")
    with open(path, "rb") as handle:
        manifest_sha = sha256_bytes(handle.read())
    return {
        "path": os.path.abspath(path),
        "sha256": manifest_sha,
        "study_id": study_id,
        "kind": kind,
        "confirmatory": confirmatory,
        "tasks": tasks,
        "arms": arms,
        "trials": trials,
        "seed": seed,
        "budget_cap_usd": float(cap),
        "per_run_reserve_usd": float(reserve),
        "retry_rule": "rerun_block_once",
        "model": model,
        "settings": settings,
        "flags": flags,
        "tools": tools,
        "append_system_prompt": prompt,
        "arms_config": arms_out,
        "sandbox": {"deny_read_paths": list(deny_paths), "deny_read_regexes": list(deny_regexes), "deny_rivet_exec_in_b": True},
    }


def arm_order(seed: int, task: str, trial: int, arms: list[str]) -> list[str]:
    """The randomized arm order of one task/trial block. Each block's order
    depends only on (seed, task, trial), so it is reproducible and does not
    shift when tasks are added."""
    digest = hashlib.sha256(f"{seed}:{task}:{trial}".encode()).digest()
    rng = random.Random(int.from_bytes(digest[:8], "big"))
    order = sorted(arms)
    rng.shuffle(order)
    return order


def schedule(study: dict) -> list[dict]:
    """Blocks in execution order: trial-major, then manifest task order."""
    blocks = []
    for trial in range(1, study["trials"] + 1):
        for task in study["tasks"]:
            blocks.append(
                {
                    "block_id": f"{task}.t{trial}",
                    "task_id": task,
                    "trial": trial,
                    "arm_order": arm_order(study["seed"], task, trial, study["arms"]),
                }
            )
    return blocks


def run_id(task: str, trial: int, arm: str) -> str:
    return f"{task}.t{trial}.{arm}"


def attempt_id(task: str, trial: int, arm: str, attempt: int) -> str:
    return f"{run_id(task, trial, arm)}.a{attempt}"


def harness_argv(study: dict, arm: str, binary: str, max_budget_usd: float) -> list[str]:
    """The exact argv for one run. The prompt goes on stdin: `--allowedTools`
    and `--tools` are variadic, so a positional prompt after them would be
    read as a tool name."""
    cfg = study["arms_config"][arm]
    return [
        binary,
        *study["flags"],
        "--model",
        study["model"],
        "--settings",
        study["settings"],
        "--append-system-prompt",
        study["append_system_prompt"],
        "--tools",
        ",".join(study["tools"]),
        "--allowedTools",
        ",".join(cfg["allowed_tools"]),
        "--disallowedTools",
        ",".join(cfg["disallowed_tools"]),
        "--max-budget-usd",
        format_usd(max_budget_usd),
    ]


def format_usd(value: float) -> str:
    text = f"{value:.4f}".rstrip("0").rstrip(".")
    return text or "0"


def settings_hash(study: dict) -> str:
    """Hash of everything held constant across arms."""
    return canonical_hash(
        {
            "flags": study["flags"],
            "model": study["model"],
            "settings": study["settings"],
            "tools": study["tools"],
            "append_system_prompt": study["append_system_prompt"],
        }
    )


def tool_policy_hash(study: dict, arm: str) -> str:
    return canonical_hash(study["arms_config"][arm])


def sanitized_path(path_value: str, rivet_bin: str | None) -> list[str]:
    """PATH entries with every directory that holds a `rivet` executable
    removed, as is the directory of the configured rivet binary."""
    banned = set()
    if rivet_bin:
        banned.add(os.path.realpath(os.path.dirname(os.path.abspath(rivet_bin))))
    out = []
    for entry in path_value.split(os.pathsep):
        if not entry:
            continue
        real = os.path.realpath(entry)
        if real in banned:
            continue
        candidate = os.path.join(entry, "rivet")
        if os.path.exists(candidate) and os.access(candidate, os.X_OK):
            continue
        if entry not in out:
            out.append(entry)
    return out


def child_env(base: dict, arm: str, rivet_bin: str | None, bin_dir: str | None) -> dict:
    """The harness environment. Removes every RIVET_* variable (so no task,
    corpus or run location reaches the agent) and the parent-session markers,
    and rebuilds PATH: arm B has no directory containing `rivet`; arm C has
    only `bin_dir/rivet` in front."""
    env = {k: v for k, v in base.items() if not k.startswith("RIVET_") and k not in STRIPPED_ENV}
    entries = sanitized_path(base.get("PATH", ""), rivet_bin)
    if arm == "C":
        if not bin_dir:
            raise StudyError("arm C needs a rivet bin directory")
        entries = [bin_dir] + entries
    env["PATH"] = os.pathsep.join(entries)
    if arm == "B" and shutil.which("rivet", path=env["PATH"]) is not None:
        raise StudyError("arm B PATH still resolves rivet; refusing to run")
    if arm == "C":
        found = shutil.which("rivet", path=env["PATH"])
        if found is None or os.path.realpath(found) != os.path.realpath(os.path.join(bin_dir, "rivet")):
            raise StudyError("arm C PATH does not resolve the pinned rivet first")
    return env


# ---- budget ledger -------------------------------------------------------


class Ledger:
    """Append-only spend ledger, `$RIVET_PILOT_RUNS_DIR/<study>/ledger.jsonl`.

    Spent-so-far is the sum over started attempts of the reported
    `total_cost_usd` (Claude Code's own estimate under a subscription login).
    An attempt whose cost is unknown (killed, crashed, runner interrupted)
    is charged its worst case, `max_budget_usd + per_run_reserve_usd`. An
    attempt whose harness process never started is charged nothing. The
    reserve covers Claude Code checking `--max-budget-usd` between turns, so
    a run can end slightly above its own budget.
    """

    def __init__(self, path: str, cap: float, reserve: float):
        self.path = path
        self.cap = cap
        self.reserve = reserve
        self.started: dict[str, float] = {}
        self.ended: dict[str, float | None] = {}
        self.unlaunched: set[str] = set()
        if os.path.exists(path):
            with open(path, encoding="utf-8") as handle:
                for line in handle:
                    if not line.strip():
                        continue
                    entry = json.loads(line)
                    if entry.get("event") == "start":
                        self.started[entry["attempt_id"]] = float(entry["max_budget_usd"])
                    elif entry.get("event") == "end":
                        self.ended[entry["attempt_id"]] = entry.get("cost_usd")
                        if entry.get("launched") is False:
                            self.unlaunched.add(entry["attempt_id"])

    def _append(self, entry: dict) -> None:
        with open(self.path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())

    def reported_usd(self) -> float:
        return sum(c for c in self.ended.values() if isinstance(c, (int, float)))

    def charged_usd(self) -> float:
        total = 0.0
        for attempt, budget in self.started.items():
            cost = self.ended.get(attempt)
            if attempt in self.unlaunched:
                continue  # the harness never started, so nothing was spent
            if isinstance(cost, (int, float)):
                total += cost
            else:
                total += budget + self.reserve
        return total

    def worst_case(self, max_budget_usd: float) -> float:
        return max_budget_usd + self.reserve

    def allows(self, max_budget_usd: float) -> bool:
        return self.charged_usd() + self.worst_case(max_budget_usd) <= self.cap + 1e-9

    def start(self, attempt: str, max_budget_usd: float) -> None:
        if not self.allows(max_budget_usd):
            raise StudyError("budget guard: refusing to start an attempt that could exceed the cap")
        self.started[attempt] = max_budget_usd
        self._append({"event": "start", "attempt_id": attempt, "max_budget_usd": max_budget_usd})

    def end(self, attempt: str, cost_usd: float | None, launched: bool = True) -> None:
        self.ended[attempt] = cost_usd
        if not launched:
            self.unlaunched.add(attempt)
        self._append({"event": "end", "attempt_id": attempt, "cost_usd": cost_usd, "launched": launched})

    def stop(self, reason: str) -> None:
        self._append({"event": "stopped", "reason": reason, "charged_usd": round(self.charged_usd(), 6)})


def load_corpus_projects(path: str = CORPUS_TOML) -> dict:
    with open(path, "rb") as handle:
        raw = tomllib.load(handle)
    return {p["name"]: p for p in raw.get("project", []) if isinstance(p, dict) and "name" in p}


# ---- confirmatory freezing (T48a) ----------------------------------------


def tasks_hash_text(tasks_dir: str, tasks: list[str]) -> str:
    """The text `tasks_sha256` hashes: one line per regular file under each
    listed task directory, `<task-id>/<relative path>\\t<sha256 of the
    bytes>\\n`, sorted by the UTF-8 bytes of that path. It holds no absolute
    path. A symlink or any other non-regular entry is refused rather than
    skipped, because the runner would read through a symlink that the hash
    did not cover."""
    entries = []
    for task in tasks:
        root = os.path.join(tasks_dir, task)
        if os.path.islink(root) or not os.path.isdir(root):
            raise StudyError(f"task directory {task}/ is missing or not a plain directory")
        for dirpath, dirnames, filenames in os.walk(root):
            for name in sorted(dirnames) + sorted(filenames):
                full = os.path.join(dirpath, name)
                rel = f"{task}/" + os.path.relpath(full, root).replace(os.sep, "/")
                mode = os.lstat(full).st_mode
                if stat.S_ISDIR(mode):
                    continue
                if not stat.S_ISREG(mode):
                    raise StudyError(f"{rel} is not a regular file (symlinks and special files are not hashed)")
                if any(c in rel for c in "\t\n\r"):
                    raise StudyError(f"{rel!r}: a tab or newline in a task file name would break tasks_sha256")
                try:
                    key = rel.encode("utf-8")
                except UnicodeEncodeError as error:
                    raise StudyError(f"{rel!r}: task file names must be UTF-8") from error
                entries.append((key, f"{rel}\t{sha256_file(full)}\n"))
    entries.sort(key=lambda entry: entry[0])
    return "".join(line for _key, line in entries)


def tasks_sha256(tasks_dir: str, tasks: list[str]) -> str:
    return sha256_bytes(tasks_hash_text(tasks_dir, tasks).encode("utf-8"))


def _git(repo: str, *args: str) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env["GIT_OPTIONAL_LOCKS"] = "0"  # a check must not rewrite the index
    try:
        return subprocess.run(
            ["git", "--literal-pathspecs", "-C", repo, *args], capture_output=True, timeout=60, check=False, env=env
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise FreezeError(f"refused: preregistration git check failed: cannot run git: {error}") from error


def manifest_repository(study: dict) -> str:
    """The top level of the git repository that holds the manifest."""
    done = _git(os.path.dirname(study["path"]), "rev-parse", "--show-toplevel")
    if done.returncode != 0:
        raise FreezeError(f"refused: preregistration git check failed: {study['path']} is not inside a git repository")
    return os.path.realpath(done.stdout.decode("utf-8", "replace").strip())


def check_preregistration(study: dict) -> dict:
    """The preregistration exists, has the manifest's sha256, is tracked by
    git and is byte-identical to its HEAD version, with nothing staged."""
    conf = study["confirmatory"]
    rel = conf["preregistration"]
    expected = conf["preregistration_sha256"]
    repo = manifest_repository(study)
    path = os.path.join(repo, rel)
    if not os.path.isfile(path):
        raise FreezeError(
            f"refused: preregistration hash check failed: {rel} does not exist in {repo}; "
            f"manifest preregistration_sha256 {expected}, file sha256 none"
        )
    actual = sha256_file(path)
    if actual != expected:
        raise FreezeError(
            f"refused: preregistration hash check failed for {rel}: "
            f"manifest preregistration_sha256 {expected}, file sha256 {actual}"
        )
    if _git(repo, "ls-files", "--error-unmatch", "--", rel).returncode != 0:
        raise FreezeError(
            f"refused: preregistration git check failed: {rel} is not tracked by git in {repo}; "
            f"manifest preregistration_sha256 {expected}, file sha256 {actual}"
        )
    blob = _git(repo, "cat-file", "blob", f"HEAD:{rel}")
    if blob.returncode != 0:
        raise FreezeError(
            f"refused: preregistration git check failed: {rel} is not in HEAD (staged but not committed); "
            f"manifest preregistration_sha256 {expected}, file sha256 {actual}, HEAD sha256 none"
        )
    head = sha256_bytes(blob.stdout)
    status = _git(repo, "status", "--porcelain", "--untracked-files=all", "--", rel)
    lines = status.stdout.decode("utf-8", "replace").strip()
    if status.returncode != 0 or lines or head != actual:
        raise FreezeError(
            f"refused: preregistration git check failed: {rel} is modified relative to HEAD "
            f"(git status: {lines or 'clean'}); HEAD sha256 {head}, file sha256 {actual}"
        )
    return {"path": rel, "repository": repo, "sha256": actual}


def check_tasks(study: dict, tasks_dir: str) -> str:
    expected = study["confirmatory"]["tasks_sha256"]
    actual = tasks_sha256(tasks_dir, study["tasks"])
    if actual != expected:
        raise FreezeError(
            f"refused: tasks hash check failed: manifest tasks_sha256 {expected}, "
            f"the listed tasks in RIVET_PILOT_TASKS_DIR hash to {actual}"
        )
    return actual


def check_rivet_binary(study: dict, rivet_bin: str) -> str:
    expected = study["confirmatory"]["rivet_binary_sha256"]
    if not os.path.isfile(rivet_bin):
        raise FreezeError(
            f"refused: rivet binary check failed: RIVET_PILOT_RIVET_BIN {rivet_bin} is not a file; "
            f"manifest rivet_binary_sha256 {expected}, binary sha256 none"
        )
    actual = sha256_file(rivet_bin)
    if actual != expected:
        raise FreezeError(
            f"refused: rivet binary check failed: manifest rivet_binary_sha256 {expected}, "
            f"RIVET_PILOT_RIVET_BIN {rivet_bin} sha256 {actual}"
        )
    return actual
