#!/usr/bin/env python3
"""Study runner (T38a; confirmatory studies T48a).

    python3 benchmark/runner/run_study.py validate   benchmark/studies/<id>/study.toml
    python3 benchmark/runner/run_study.py plan       benchmark/studies/<id>/study.toml
    python3 benchmark/runner/run_study.py run        benchmark/studies/<id>/study.toml [--dry-run]
    python3 benchmark/runner/run_study.py hash-tasks benchmark/studies/<id>/study.toml

`validate` checks the manifest, every task and every gold answer against its
own check. `plan` prints the schedule and worst-case spend. Only `run` starts
agent sessions, and it spends model usage unless `RIVET_PILOT_CLAUDE_BIN`
points at a fake agent; every record of such a run is marked `dry_run`, and a
confirmatory study then also needs `--dry-run`. `hash-tasks` prints the
`tasks_sha256` of the listed tasks. For a confirmatory study, `validate`,
`plan` and `run` first refuse unless the preregistration and the tasks match
their frozen hashes, and `run` also needs the frozen rivet binary.

Required environment (no defaults; nothing private lives in the repository):

    RIVET_PILOT_TASKS_DIR   task directories, see TASK-FORMAT.md
    RIVET_PILOT_RUNS_DIR    where workspaces, transcripts and records go
    RIVET_CORPUS_DIR        the pinned corpus copies, one directory per project
    RIVET_PILOT_RIVET_BIN   the rivet binary for arm C (run only)
    RIVET_PILOT_CLAUDE_BIN  optional: harness binary, default `claude`

Exit codes: 0 finished, 1 error, 2 invalid manifest or task (including every
confirmatory refusal), 3 stopped by the budget guard. Standard library only.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import shlex
import shutil
import signal
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True  # keep __pycache__ out of the repository
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import checks  # noqa: E402
import sandbox  # noqa: E402
import study as studylib  # noqa: E402
from transcript import (  # noqa: E402
    Transcript,
    contamination_scan,
    isolation_violations,
    provider_error,
)

INSTRUCTION_FILES = ("AGENTS.md", "CLAUDE.md", "CLAUDE.local.md")
INSTRUCTION_DIRS = (".claude",)
KILL_GRACE_SECONDS = 5.0
RUNNER_VERSION = "t38abc-1"

EXIT_OK, EXIT_ERROR, EXIT_INVALID, EXIT_BUDGET = 0, 1, 2, 3


def utc_now() -> str:
    return _dt.datetime.now(_dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def require_env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise studylib.StudyError(f"{name} is required and has no default")
    return os.path.abspath(value)


def write_json(path: str, value) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        handle.write("\n")
    os.replace(tmp, path)


def is_within(path: str, parent: str) -> bool:
    path, parent = os.path.realpath(path), os.path.realpath(parent)
    return path == parent or path.startswith(parent + os.sep)


# ---- workspace -------------------------------------------------------------


def tree_hash(root: str) -> tuple[str, int]:
    """sha256 over sorted (relative path, kind, content hash), skipping
    `.rivet/` and `.git/` (the harness's own `git status` may touch the
    index). Symlinks hash their target text, never followed."""
    entries = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = sorted(d for d in dirnames if d not in (".rivet", ".git"))
        rel_dir = os.path.relpath(dirpath, root)
        for name in sorted(filenames) + sorted(d for d in dirnames if os.path.islink(os.path.join(dirpath, d))):
            full = os.path.join(dirpath, name)
            rel = os.path.normpath(os.path.join(rel_dir, name)).replace(os.sep, "/")
            if os.path.islink(full):
                entries.append(f"L {rel} {os.readlink(full)}")
            elif os.path.isfile(full):
                entries.append(f"F {rel} {studylib.sha256_file(full)}")
    return studylib.sha256_bytes("\n".join(sorted(entries)).encode()), len(entries)


def copy_corpus(source: str, dest: str) -> None:
    def ignore(_dir, names):
        return [n for n in names if n == ".rivet"]

    shutil.copytree(source, dest, symlinks=True, ignore=ignore)


def normalize_instructions(workspace: str) -> list[dict]:
    """Removes repository agent instructions at any depth, identically in
    both arms, returning what was removed with content hashes."""
    removed = []
    for dirpath, dirnames, filenames in os.walk(workspace):
        dirnames.sort()
        rel_dir = os.path.relpath(dirpath, workspace)
        for name in list(dirnames):
            if name in INSTRUCTION_DIRS:
                full = os.path.join(dirpath, name)
                rel = os.path.normpath(os.path.join(rel_dir, name)).replace(os.sep, "/")
                if os.path.islink(full):
                    removed.append({"path": rel, "kind": "symlink", "sha256": studylib.sha256_bytes(os.readlink(full).encode())})
                    os.unlink(full)
                else:
                    digest, count = tree_hash(full)
                    removed.append({"path": rel + "/", "kind": "dir", "sha256": digest, "files": count})
                    shutil.rmtree(full)
                dirnames.remove(name)
        for name in sorted(filenames):
            if name in INSTRUCTION_FILES:
                full = os.path.join(dirpath, name)
                rel = os.path.normpath(os.path.join(rel_dir, name)).replace(os.sep, "/")
                if os.path.islink(full):
                    removed.append({"path": rel, "kind": "symlink", "sha256": studylib.sha256_bytes(os.readlink(full).encode())})
                else:
                    removed.append({"path": rel, "kind": "file", "sha256": studylib.sha256_file(full)})
                os.unlink(full)
    return sorted(removed, key=lambda r: r["path"])


def root_escape_problem(workspace: str) -> str | None:
    """rivet discovers its root by walking up to `.rivet/` or `.git`. A
    workspace without `.git` whose ancestor has either would let `rivet init`
    write outside the workspace."""
    if os.path.exists(os.path.join(workspace, ".git")):
        return None
    parent = os.path.dirname(os.path.realpath(workspace))
    while True:
        for marker in (".git", ".rivet"):
            if os.path.exists(os.path.join(parent, marker)):
                return f"ancestor {os.path.join(parent, marker)} would capture rivet's root"
        up = os.path.dirname(parent)
        if up == parent:
            return None
        parent = up


def git_head(workspace: str) -> str | None:
    if not os.path.exists(os.path.join(workspace, ".git")):
        return None
    try:
        out = subprocess.run(
            ["git", "-C", workspace, "rev-parse", "HEAD"], capture_output=True, text=True, timeout=30, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


GIT_IDENTITY = {
    "GIT_AUTHOR_NAME": "rivet pilot",
    "GIT_AUTHOR_EMAIL": "pilot@rivet.invalid",
    "GIT_AUTHOR_DATE": "2000-01-01T00:00:00+0000",
    "GIT_COMMITTER_NAME": "rivet pilot",
    "GIT_COMMITTER_EMAIL": "pilot@rivet.invalid",
    "GIT_COMMITTER_DATE": "2000-01-01T00:00:00+0000",
}
GIT_COMMIT_MESSAGE = "rivet pilot workspace"


def reset_git(workspace: str) -> str:
    """Replaces the workspace's `.git` with a fresh repository holding one
    commit of the prepared tree, with a fixed identity and date and no user
    or system git configuration, so `git status` is clean in both arms and
    the commit is a function of the tree alone. Returns the new HEAD."""
    dot_git = os.path.join(workspace, ".git")
    if os.path.islink(dot_git) or os.path.isfile(dot_git):
        os.unlink(dot_git)
    elif os.path.isdir(dot_git):
        shutil.rmtree(dot_git)
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": workspace,
        "LC_ALL": "C",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        **GIT_IDENTITY,
    }
    config = ["-c", "commit.gpgsign=false", "-c", f"core.hooksPath={os.devnull}", "-c", "core.autocrlf=false", "-c", "core.fsmonitor=false"]
    subprocess.run(["git", "-C", workspace, "init", "-q", "--template=", "-b", "main"], env=env, capture_output=True, timeout=60, check=False)
    if os.path.isdir(os.path.join(workspace, ".rivet")):
        # The cache must stay out of the commit (rivet init's .gitignore
        # entry does this); a committed cache would differ from B's tree
        # and could carry an index.
        ignored = subprocess.run(["git", "-C", workspace, "check-ignore", "-q", ".rivet/"], env=env, capture_output=True, timeout=60, check=False)
        if ignored.returncode != 0:
            raise studylib.StudyError(".rivet/ is not ignored by the workspace .gitignore; refusing to commit the cache")
    for args in (
        [*config, "add", "-A"],
        [*config, "commit", "-q", "--allow-empty", "-m", GIT_COMMIT_MESSAGE],
    ):
        done = subprocess.run(["git", "-C", workspace, *args], env=env, capture_output=True, text=True, timeout=600, check=False)
        if done.returncode != 0:
            raise studylib.StudyError(f"git {args[-1] if args else ''} failed while resetting the workspace: {done.stderr.strip()}")
    status = subprocess.run(["git", "-C", workspace, "status", "--porcelain"], env=env, capture_output=True, text=True, timeout=120, check=False)
    if status.returncode != 0 or status.stdout.strip():
        raise studylib.StudyError(f"reset workspace git status is not clean: {status.stdout.strip()[:200]}")
    head = subprocess.run(["git", "-C", workspace, "rev-parse", "HEAD"], env=env, capture_output=True, text=True, timeout=30, check=False)
    return head.stdout.strip()


def tool_version(binary: str, args: list[str]) -> str | None:
    try:
        out = subprocess.run([binary, *args], capture_output=True, text=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


# ---- one attempt -------------------------------------------------------------


class Context:
    def __init__(self, study: dict, corpus_manifest: str):
        self.study = study
        # Resolved: the sandbox profile only matches real paths.
        self.tasks_dir = os.path.realpath(require_env("RIVET_PILOT_TASKS_DIR"))
        self.runs_root = os.path.realpath(require_env("RIVET_PILOT_RUNS_DIR"))
        self.corpus_dir = os.path.realpath(require_env("RIVET_CORPUS_DIR"))
        self.claude_bin = os.environ.get("RIVET_PILOT_CLAUDE_BIN") or "claude"
        self.claude_is_override = bool(os.environ.get("RIVET_PILOT_CLAUDE_BIN"))
        self.corpus_manifest = os.path.abspath(corpus_manifest)
        self.projects = studylib.load_corpus_projects(self.corpus_manifest)
        for private in (self.tasks_dir, self.corpus_dir):
            if is_within(self.runs_root, private):
                raise studylib.StudyError(f"RIVET_PILOT_RUNS_DIR may not lie inside {private}")
        for private in (self.tasks_dir, self.runs_root, self.corpus_dir):
            if is_within(private, studylib.REPO_ROOT):
                raise studylib.StudyError(f"{private} lies inside the public repository; private data must stay outside")
        self.study_dir = os.path.join(self.runs_root, study["study_id"])
        self.workspaces_dir = os.path.join(self.study_dir, "workspaces")
        self.tools_dir = os.path.join(self.study_dir, "tools")
        if not sandbox.available():
            raise studylib.StudyError(f"{sandbox.SANDBOX_EXEC} is required to confine agent processes")
        # Static read denials, identical in both arms: the manifest's paths
        # plus the task and corpus directories. The runs directory is handled
        # per attempt (everything but the attempt's workspace and the tools).
        self.deny_paths = sorted({sandbox.resolve(p) for p in study["sandbox"]["deny_read_paths"]} | {self.tasks_dir, self.corpus_dir})
        self.deny_regexes = list(study["sandbox"]["deny_read_regexes"])
        for label, path in (("RIVET_PILOT_RUNS_DIR", self.runs_root),):
            rule = sandbox.denied_by(path, self.deny_paths, self.deny_regexes)
            if rule:
                raise studylib.StudyError(f"{label} {path} falls under the denied read rule {rule}; the agent could not read its workspace")
        claude_real = sandbox.which_real(self.claude_bin)
        if claude_real is not None:
            rule = sandbox.denied_by(claude_real, self.deny_paths, self.deny_regexes)
            if rule:
                raise studylib.StudyError(f"the harness binary {claude_real} falls under the denied read rule {rule}")
        self.claude_real = claude_real
        self.tasks = {}
        for task_id in study["tasks"]:
            task = checks.load_task(self.tasks_dir, task_id)
            if task["project"] not in self.projects:
                raise checks.TaskError(f"{task_id}: project {task['project']!r} is not in {self.corpus_manifest}")
            task["hashes"] = {name: studylib.sha256_file(p) for name, p in task["paths"].items()}
            self.tasks[task_id] = task


def run_probe(ctx: Context, arm: str, task: dict, workspace: str, profile_path: str, env: dict, versions: dict) -> dict:
    """Pre-flight: inside the attempt's exact profile, cwd and environment,
    confirm the workspace is readable and every protected location is not;
    in arm B confirm that no `rivet` can be executed while other programs
    can; in arm C confirm the pinned rivet runs."""
    ok = [("list_workspace", ["/bin/ls", "-a", workspace])]
    denied = [
        ("read_gold", ["/bin/cat", os.path.realpath(task["paths"]["gold.json"])]),
        ("list_tasks_dir", ["/bin/ls", ctx.tasks_dir]),
        ("list_corpus_dir", ["/bin/ls", ctx.corpus_dir]),
        ("list_study_dir", ["/bin/ls", ctx.study_dir]),
        ("list_workspaces_dir", ["/bin/ls", ctx.workspaces_dir]),
        ("read_ledger", ["/bin/cat", os.path.join(ctx.study_dir, "ledger.jsonl")]),
    ]
    for path in ctx.deny_paths:
        if os.path.exists(path) and path not in (ctx.tasks_dir, ctx.corpus_dir):
            denied.append((f"list_denied:{path}", ["/bin/ls", path]))
    if arm == "B":
        ok.append(("exec_other_program", ["/bin/echo", "ok"]))
        denied.append(("exec_rivet_stand_in", [versions["exec_probe"], "probe"]))
        if versions.get("rivet_bin"):
            denied.append(("exec_pinned_rivet", [versions["rivet_bin"], "--version"]))
    else:
        ok.append(("exec_pinned_rivet", [versions["rivet_bin"], "--version"]))
    return sandbox.probe(profile_path, workspace, env, ok, denied)


def run_harness(argv, env, cwd, prompt, transcript_path, stderr_path, wall_seconds, max_turns):
    """Runs the harness with the prompt on stdin, streaming stdout to the
    transcript. Enforces wall time and the turn limit by killing the whole
    process group. Returns a dict of what happened."""
    outcome = {"launch_error": None, "exit_code": None, "killed_for": None, "wall_clock_seconds": None}
    turn_ids: set[str] = set()
    killed = threading.Event()
    reason = {"value": None}
    started = time.monotonic()
    try:
        with open(stderr_path, "wb") as err:
            proc = subprocess.Popen(
                argv,
                cwd=cwd,
                env=env,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=err,
                start_new_session=True,
            )
    except OSError as error:
        outcome["launch_error"] = f"{type(error).__name__}: {error}"
        outcome["wall_clock_seconds"] = time.monotonic() - started
        open(transcript_path, "wb").close()
        return outcome

    def kill(why):
        if killed.is_set():
            return
        reason["value"] = why
        killed.set()
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            return

        def hard_kill():
            try:
                proc.wait(timeout=KILL_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    pass

        threading.Thread(target=hard_kill, daemon=True).start()

    def reader():
        with open(transcript_path, "wb") as out:
            for line in proc.stdout:
                out.write(line)
                out.flush()
                try:
                    event = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    continue
                if isinstance(event, dict) and event.get("type") == "assistant":
                    message = event.get("message") or {}
                    mid = message.get("id") if isinstance(message, dict) else None
                    turn_ids.add(mid if isinstance(mid, str) else f"<anon-{len(turn_ids)}>")
                    if len(turn_ids) > max_turns:
                        kill("max_turns")

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    try:
        proc.stdin.write(prompt.encode("utf-8"))
        proc.stdin.close()
    except (BrokenPipeError, OSError):
        pass
    deadline = started + wall_seconds
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            kill("wall_timeout")
            break
        try:
            proc.wait(timeout=min(remaining, 0.25))
            break
        except subprocess.TimeoutExpired:
            if killed.is_set():
                break
    proc.wait()
    # Anything the harness left running in its process group (a background
    # shell, a stuck tool) must not outlive the attempt.
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    thread.join(timeout=30)
    outcome["exit_code"] = proc.returncode
    outcome["killed_for"] = reason["value"]
    outcome["wall_clock_seconds"] = time.monotonic() - started
    return outcome


def classify(outcome: dict, t: Transcript) -> dict:
    """Termination reason and the infrastructure flag (TASK-FORMAT.md
    "Failure classification")."""
    if outcome.get("setup_error"):
        return {"termination_reason": "setup_failed", "infrastructure_failure": True, "infrastructure_reason": "setup_failed"}
    if outcome.get("launch_error"):
        return {"termination_reason": "launch_failed", "infrastructure_failure": True, "infrastructure_reason": "launch_failed"}
    if outcome.get("runner_interrupted"):
        return {"termination_reason": "runner_interrupted", "infrastructure_failure": True, "infrastructure_reason": "runner_interrupted"}
    if outcome.get("killed_for") == "wall_timeout":
        return {"termination_reason": "wall_timeout", "infrastructure_failure": False, "infrastructure_reason": None}
    if outcome.get("killed_for") == "max_turns":
        return {"termination_reason": "max_turns", "infrastructure_failure": False, "infrastructure_reason": None}
    marker = provider_error(t)
    if marker is not None:
        return {"termination_reason": "provider_error", "infrastructure_failure": True, "infrastructure_reason": marker}
    result = t.result
    if result is not None:
        subtype = str(result.get("subtype", ""))
        if subtype == "success" and not result.get("is_error"):
            return {"termination_reason": "completed", "infrastructure_failure": False, "infrastructure_reason": None}
        if subtype == "error_max_turns":
            return {"termination_reason": "max_turns", "infrastructure_failure": False, "infrastructure_reason": None}
        if "budget" in subtype:
            return {"termination_reason": "max_budget", "infrastructure_failure": False, "infrastructure_reason": None}
        return {"termination_reason": "agent_error", "infrastructure_failure": False, "infrastructure_reason": None}
    if t.init is None:
        return {"termination_reason": "no_session", "infrastructure_failure": True, "infrastructure_reason": "harness exited before its init event"}
    return {"termination_reason": "agent_crash", "infrastructure_failure": False, "infrastructure_reason": None}


def execute_attempt(ctx: Context, ledger: studylib.Ledger, block: dict, arm: str, attempt: int, order: int, versions: dict) -> dict:
    study = ctx.study
    task = ctx.tasks[block["task_id"]]
    limits = task["limits"]
    aid = studylib.attempt_id(block["task_id"], block["trial"], arm, attempt)
    run_dir = os.path.join(ctx.study_dir, "runs", aid)
    workspace = os.path.join(ctx.study_dir, "workspaces", aid)
    if os.path.exists(run_dir) or os.path.exists(workspace):
        raise studylib.StudyError(f"{aid}: run directory already exists; refusing to overwrite evidence")
    os.makedirs(run_dir)
    record = {
        "schema_version": 1,
        "runner_version": RUNNER_VERSION,
        "study_id": study["study_id"],
        "study_manifest_sha256": study["sha256"],
        "attempt_id": aid,
        "run_id": studylib.run_id(block["task_id"], block["trial"], arm),
        "block_id": block["block_id"],
        "task_id": block["task_id"],
        "project": task["project"],
        "category": task["category"],
        "check": task["check"],
        "task_hashes": task["hashes"],
        "arm": arm,
        "trial": block["trial"],
        "attempt": attempt,
        "execution_order": order,
        "block_arm_order": block["arm_order"],
        "limits": limits,
        "model_id": study["model"],
        "harness_binary": ctx.claude_bin,
        "harness_binary_is_override": ctx.claude_is_override,
        # A replaced harness is a rehearsal: its records never count as results.
        "dry_run": ctx.claude_is_override,
        "harness_version": versions["claude"],
        "settings_hash": studylib.settings_hash(study),
        "tool_policy_hash": studylib.tool_policy_hash(study, arm),
        "repository_commit": ctx.projects[task["project"]].get("commit"),
        "cost_label": "claude_code_estimate",
        "started_at": utc_now(),
    }
    outcome: dict = {}
    t = Transcript([])
    transcript_path = os.path.join(run_dir, "transcript.jsonl")
    stderr_path = os.path.join(run_dir, "stderr.txt")
    ledger.start(aid, limits["max_budget_usd"])
    try:
        source = os.path.join(ctx.corpus_dir, task["project"])
        if not os.path.isdir(source):
            raise studylib.StudyError(f"corpus copy {source} does not exist")
        if is_within(workspace, source):
            raise studylib.StudyError("workspace would lie inside the corpus copy")
        os.makedirs(os.path.dirname(workspace), exist_ok=True)
        copy_corpus(source, workspace)
        record["workspace"] = workspace
        record["corpus_tree_sha256"], record["corpus_tree_files"] = tree_hash(workspace)
        head = git_head(workspace)
        record["workspace_git_head"] = head
        if head is not None and head != record["repository_commit"]:
            raise studylib.StudyError(f"corpus copy HEAD {head} differs from the pinned commit {record['repository_commit']}")
        problem = root_escape_problem(workspace)
        if problem:
            raise studylib.StudyError(problem)
        record["removed_instructions"] = normalize_instructions(workspace)
        record["instruction_policy"] = "remove AGENTS.md, CLAUDE.md, CLAUDE.local.md and .claude/ at every depth, both arms"
        bin_dir = None
        if arm == "C":
            rivet_bin = versions["rivet_bin"]
            # The pinned copy's directory holds only `rivet`; it goes first
            # on arm C's PATH.
            bin_dir = ctx.tools_dir
            init = subprocess.run(
                [rivet_bin, "init", "--write-snippet", "--snippet-file", "CLAUDE.md", "--json"],
                cwd=workspace,
                capture_output=True,
                text=True,
                timeout=120,
                check=False,
            )
            record["rivet_init"] = {"exit_code": init.returncode, "stdout": init.stdout, "stderr": init.stderr}
            if init.returncode != 0:
                raise studylib.StudyError(f"rivet init failed with exit {init.returncode}")
            snippet_path = os.path.join(workspace, "CLAUDE.md")
            if not os.path.isfile(snippet_path) or not os.path.isdir(os.path.join(workspace, ".rivet")):
                raise studylib.StudyError("rivet init did not create CLAUDE.md and .rivet/ in the workspace")
            with open(snippet_path, "rb") as handle:
                installed = handle.read()
            if installed != versions["snippet_bytes"]:
                raise studylib.StudyError("CLAUDE.md is not exactly the managed snippet")
            cache_entries = sorted(os.listdir(os.path.join(workspace, ".rivet")))
            if cache_entries != ["config.toml"]:
                raise studylib.StudyError(f"the C workspace .rivet/ holds {cache_entries}; runs must start cold")
            record["snippet_hash"] = studylib.sha256_bytes(installed)
            record["tool_version"] = versions["rivet"]
            record["rivet_binary_sha256"] = versions["rivet_sha256"]
        else:
            record["snippet_hash"] = None
            record["tool_version"] = None
            record["rivet_binary_sha256"] = None
        # One fresh commit of the prepared tree: git status is clean in both
        # arms, whatever normalization and `rivet init` changed.
        record["workspace_git_reset_head"] = reset_git(workspace)
        record["workspace_pre_sha256"], _ = tree_hash(workspace)
        env = studylib.child_env(os.environ, arm, versions.get("rivet_source"), bin_dir)
        record["child_env_names"] = sorted(env)
        record["child_path"] = env["PATH"].split(os.pathsep)
        record["anthropic_api_key_set"] = "ANTHROPIC_API_KEY" in env
        argv = studylib.harness_argv(study, arm, ctx.claude_bin, limits["max_budget_usd"])
        record["argv"] = argv
        record["prompt_sha256"] = studylib.sha256_bytes(task["prompt"].encode("utf-8"))
        profile_path = os.path.join(run_dir, "sandbox.sb")
        profile = sandbox.build_profile(
            ctx.deny_paths, ctx.deny_regexes, ctx.runs_root, [workspace, ctx.tools_dir], deny_rivet_exec=(arm == "B")
        )
        with open(profile_path, "w", encoding="utf-8") as handle:
            handle.write(profile)
        record["sandbox_profile_sha256"] = studylib.sha256_bytes(profile.encode("utf-8"))
        record["sandbox_deny_paths"] = ctx.deny_paths
        record["sandbox_deny_regexes"] = ctx.deny_regexes
        record["sandbox_probe"] = run_probe(ctx, arm, task, workspace, profile_path, env, versions)
        if not record["sandbox_probe"]["passed"]:
            failed = [c["name"] for c in record["sandbox_probe"]["checks"] if not c["passed"]]
            raise studylib.StudyError(f"sandbox pre-flight probe failed: {', '.join(failed)}")
        if sandbox.which_real(ctx.claude_bin, env["PATH"]) is None:
            outcome = {"launch_error": f"harness binary {ctx.claude_bin!r} not found", "wall_clock_seconds": 0.0}
            open(transcript_path, "wb").close()
            open(stderr_path, "wb").close()
        else:
            outcome = run_harness(
                sandbox.wrap(profile_path, argv),
                env,
                workspace,
                task["prompt"],
                transcript_path,
                stderr_path,
                limits["wall_seconds"],
                limits["max_turns"],
            )
    except (studylib.StudyError, OSError, subprocess.SubprocessError) as error:
        # Setup problems (a wrong commit, rivet reachable from arm B, a
        # snippet mismatch, a full disk) are integrity failures that would
        # repeat for every run: record them and stop the study.
        message = f"{type(error).__name__}: {error}"
        record["outcome"] = {"setup_error": message}
        record.update(classify(record["outcome"], t))
        record["cost_usd"] = None
        record["pass"] = None
        record["finished_at"] = utc_now()
        ledger.end(aid, None, launched=False)
        write_json(os.path.join(run_dir, "record.json"), record)
        raise studylib.StudyError(f"{aid}: setup failed, study stopped: {message}") from error
    t = Transcript.from_file(transcript_path)
    record["outcome"] = outcome
    record["wall_clock_seconds"] = outcome.get("wall_clock_seconds")
    record.update(classify(outcome, t))
    cost = (t.result or {}).get("total_cost_usd")
    cost = float(cost) if isinstance(cost, (int, float)) and not isinstance(cost, bool) else None
    record["cost_usd"] = cost
    ledger.end(aid, cost, launched=not outcome.get("launch_error"))
    record["transcript_sha256"] = studylib.sha256_file(transcript_path)
    record["transcript_invalid_lines"] = t.invalid_lines
    record["isolation_violations"] = isolation_violations(t, study["model"], study["tools"]) if t.events else ["no_transcript"]
    evaluation = checks.evaluate(task["check"], task["gold"], t.final_text(), task["f1_threshold"])
    record["evaluation"] = evaluation
    if record["infrastructure_failure"]:
        record["pass"] = None
    else:
        record["pass"] = bool(evaluation["passed"] and record["termination_reason"] == "completed")
    rivet_dir_after = os.path.isdir(os.path.join(workspace, ".rivet"))
    if os.path.isdir(workspace):
        record["workspace_post_sha256"], _ = tree_hash(workspace)
        record["workspace_modified"] = record.get("workspace_pre_sha256") not in (None, record["workspace_post_sha256"])
    else:
        record["workspace_post_sha256"] = None
        record["workspace_modified"] = None
    if arm == "B":
        evidence = contamination_scan(t, workspace)
        if rivet_dir_after:
            evidence.append({"kind": "workspace_rivet_dir"})
        record["contamination_evidence"] = evidence
        record["contaminated"] = bool(evidence)
    else:
        # The C workspace, including the `.rivet/` index the run built, is
        # kept in place as evidence for the positions rivet returned.
        record["contamination_evidence"] = []
        record["contaminated"] = None
    record["finished_at"] = utc_now()
    write_json(os.path.join(run_dir, "check.json"), evaluation)
    write_json(os.path.join(run_dir, "record.json"), record)
    return record


# ---- the study -------------------------------------------------------------


def gather_versions(ctx: Context) -> dict:
    versions = {"claude": tool_version(ctx.claude_bin, ["--version"])}
    # A stand-in executable named `rivet` (a copy of /bin/echo) lets the
    # arm-B probe test the exec rule whether or not arm C runs.
    probe_dir = os.path.join(ctx.tools_dir, "exec-probe")
    os.makedirs(probe_dir, exist_ok=True)
    stand_in = os.path.join(probe_dir, "rivet")
    if not os.path.exists(stand_in):
        shutil.copyfile("/bin/echo", stand_in)
        os.chmod(stand_in, 0o755)
    versions["exec_probe"] = stand_in
    if "C" in ctx.study["arms"]:
        rivet_bin = require_env("RIVET_PILOT_RIVET_BIN")
        if not (os.path.isfile(rivet_bin) and os.access(rivet_bin, os.X_OK)):
            raise studylib.StudyError(f"RIVET_PILOT_RIVET_BIN {rivet_bin} is not an executable file")
        # Pin a copy inside the study's tools directory: the binary the agent
        # runs must not sit under a denied path (for example a build tree in
        # rivet's own repository), and a copy cannot change under the study.
        source_sha = studylib.sha256_file(rivet_bin)
        if ctx.study["kind"] == "confirmatory" and source_sha != ctx.study["confirmatory"]["rivet_binary_sha256"]:
            # The preflight checked it too; this closes the gap before the copy.
            studylib.check_rivet_binary(ctx.study, rivet_bin)
        pinned = os.path.join(ctx.tools_dir, "rivet")
        os.makedirs(ctx.tools_dir, exist_ok=True)
        if os.path.exists(pinned):
            if studylib.sha256_file(pinned) != source_sha:
                raise studylib.StudyError(f"{pinned} differs from RIVET_PILOT_RIVET_BIN; a study keeps one rivet binary")
        else:
            shutil.copyfile(rivet_bin, pinned)
            os.chmod(pinned, 0o755)
        rivet_bin = pinned
        versions["rivet_bin"] = pinned
        versions["rivet_source"] = os.path.realpath(require_env("RIVET_PILOT_RIVET_BIN"))
        versions["rivet"] = tool_version(rivet_bin, ["--version"])
        versions["rivet_sha256"] = source_sha
        snippet = subprocess.run([rivet_bin, "snippet"], capture_output=True, timeout=60, check=False)
        if snippet.returncode != 0 or not snippet.stdout:
            raise studylib.StudyError("`rivet snippet` failed; cannot pin the managed snippet")
        versions["snippet_bytes"] = snippet.stdout
        versions["snippet_hash"] = studylib.sha256_bytes(snippet.stdout)
    return versions


def load_existing(ctx: Context) -> dict[str, dict]:
    records = {}
    runs = os.path.join(ctx.study_dir, "runs")
    if os.path.isdir(runs):
        for name in sorted(os.listdir(runs)):
            path = os.path.join(runs, name, "record.json")
            if os.path.exists(path):
                with open(path, encoding="utf-8") as handle:
                    records[name] = json.load(handle)
    return records


def recover_interrupted(ctx: Context, ledger: studylib.Ledger, records: dict) -> None:
    """An attempt the ledger started but that has no record (the runner
    died) gets a record classified `runner_interrupted`; its cost is taken
    from its transcript if one exists, else charged at the worst case."""
    for aid in sorted(ledger.started):
        if aid in records:
            continue
        run_dir = os.path.join(ctx.study_dir, "runs", aid)
        transcript_path = os.path.join(run_dir, "transcript.jsonl")
        t = Transcript.from_file(transcript_path)
        cost = (t.result or {}).get("total_cost_usd")
        cost = float(cost) if isinstance(cost, (int, float)) and not isinstance(cost, bool) else None
        if aid not in ledger.ended:
            ledger.end(aid, cost, launched=True)
        task_id, trial, arm, attempt = aid.rsplit(".", 3)
        os.makedirs(run_dir, exist_ok=True)
        if not os.path.exists(transcript_path):
            open(transcript_path, "wb").close()
        task = ctx.tasks.get(task_id, {})
        record = {
            "schema_version": 1,
            "runner_version": RUNNER_VERSION,
            "study_id": ctx.study["study_id"],
            "attempt_id": aid,
            "run_id": f"{task_id}.{trial}.{arm}",
            "block_id": f"{task_id}.{trial}",
            "task_id": task_id,
            "project": task.get("project"),
            "category": task.get("category"),
            "check": task.get("check"),
            "arm": arm,
            "trial": int(trial[1:]),
            "attempt": int(attempt[1:]),
            "execution_order": None,
            "limits": task.get("limits"),
            "model_id": ctx.study["model"],
            # For a confirmatory study, refuse_mixed_dry_run guarantees that
            # the interrupted invocation had this invocation's harness.
            "dry_run": ctx.claude_is_override,
            "outcome": {"runner_interrupted": True},
            "termination_reason": "runner_interrupted",
            "infrastructure_failure": True,
            "infrastructure_reason": "runner_interrupted",
            "cost_usd": cost,
            "cost_label": "claude_code_estimate",
            "pass": None,
            "transcript_sha256": studylib.sha256_file(transcript_path),
            "contaminated": (bool(contamination_scan(t)) if arm == "B" else None),
            "contamination_evidence": contamination_scan(t) if arm == "B" else [],
        }
        write_json(os.path.join(run_dir, "record.json"), record)
        records[aid] = record


def record_dry_run(record: dict):
    """A record's dry-run flag; records older than T48a carry only
    `harness_binary_is_override`, which is the same fact."""
    if "dry_run" in record:
        return record["dry_run"]
    return record.get("harness_binary_is_override")


def refuse_mixed_dry_run(ctx: Context) -> None:
    """A confirmatory study directory holds either rehearsal records or real
    ones, never both: a real run resuming a rehearsal would skip the
    rehearsed attempts and inherit the rehearsal's ledger."""
    earlier = []
    manifest_path = os.path.join(ctx.study_dir, "manifest.json")
    if os.path.exists(manifest_path):
        with open(manifest_path, encoding="utf-8") as handle:
            earlier.append(("manifest.json", record_dry_run(json.load(handle))))
    for aid, record in sorted(load_existing(ctx).items()):
        earlier.append((aid, record_dry_run(record)))
    for name, flag in earlier:
        if flag is not ctx.claude_is_override:
            raise studylib.FreezeError(
                f"refused: dry-run check failed: {ctx.study_dir} already holds {name} with dry_run={flag}, "
                f"and this run has dry_run={ctx.claude_is_override}; use a separate RIVET_PILOT_RUNS_DIR for a rehearsal"
            )


def run_study(ctx: Context, out=sys.stdout) -> int:
    study = ctx.study
    if study["kind"] == "confirmatory":
        refuse_mixed_dry_run(ctx)
    os.makedirs(ctx.study_dir, exist_ok=True)
    ledger = studylib.Ledger(os.path.join(ctx.study_dir, "ledger.jsonl"), study["budget_cap_usd"], study["per_run_reserve_usd"])
    versions = gather_versions(ctx)
    blocks = studylib.schedule(study)
    manifest = {
        "study_id": study["study_id"],
        "study_manifest_sha256": study["sha256"],
        "runner_version": RUNNER_VERSION,
        "harness_version": versions["claude"],
        "harness_binary_is_override": ctx.claude_is_override,
        "dry_run": ctx.claude_is_override,
        "rivet_version": versions.get("rivet"),
        "rivet_binary_sha256": versions.get("rivet_sha256"),
        "snippet_hash": versions.get("snippet_hash"),
        "settings_hash": studylib.settings_hash(study),
        "tool_policy_hash": {arm: studylib.tool_policy_hash(study, arm) for arm in study["arms"]},
        "argv_by_arm": {
            arm: sandbox.wrap("<attempt>/sandbox.sb", studylib.harness_argv(study, arm, ctx.claude_bin, 0.0)[:-1] + ["<task max_budget_usd>"])
            for arm in study["arms"]
        },
        "sandbox": {"deny_paths": ctx.deny_paths, "deny_regexes": ctx.deny_regexes, "runs_root": ctx.runs_root, "tools_dir": ctx.tools_dir},
        "tasks": {tid: {"project": t["project"], "category": t["category"], "check": t["check"], "limits": t["limits"], "hashes": t["hashes"]} for tid, t in ctx.tasks.items()},
        "schedule": blocks,
        "budget_cap_usd": study["budget_cap_usd"],
        "per_run_reserve_usd": study["per_run_reserve_usd"],
    }
    write_json(os.path.join(ctx.study_dir, "manifest.json"), manifest)
    records = load_existing(ctx)
    recover_interrupted(ctx, ledger, records)
    order = max([r.get("execution_order") or 0 for r in records.values()] + [0])

    def budget_line(extra: float) -> str:
        return (
            f"charged so far ${ledger.charged_usd():.4f} (reported Claude Code estimate ${ledger.reported_usd():.4f}, "
            f"unknown costs at worst case) + next ${extra:.4f} vs cap ${study['budget_cap_usd']:.2f}"
        )

    for block in blocks:
        task = ctx.tasks[block["task_id"]]
        budget = task["limits"]["max_budget_usd"]
        for attempt in (1, 2):
            if attempt == 2:
                # Symmetric retry rule: rerun the whole block once when any
                # of its first attempts failed for infrastructure reasons.
                first = [records.get(studylib.attempt_id(block["task_id"], block["trial"], arm, 1)) for arm in block["arm_order"]]
                if not any(r and r.get("infrastructure_failure") for r in first):
                    break
            ids = {arm: studylib.attempt_id(block["task_id"], block["trial"], arm, attempt) for arm in block["arm_order"]}
            pending = [arm for arm in block["arm_order"] if ids[arm] not in records]
            if not pending:
                continue
            need = len(pending) * ledger.worst_case(budget)
            if ledger.charged_usd() + need > study["budget_cap_usd"] + 1e-9:
                reason = f"budget guard: block {block['block_id']} attempt {attempt} not started; " + budget_line(need)
                ledger.stop(reason)
                print(reason, file=sys.stderr)
                return EXIT_BUDGET
            for arm in pending:
                if not ledger.allows(budget):
                    reason = f"budget guard: {ids[arm]} not started; " + budget_line(ledger.worst_case(budget))
                    ledger.stop(reason)
                    print(reason, file=sys.stderr)
                    return EXIT_BUDGET
                order += 1
                record = execute_attempt(ctx, ledger, block, arm, attempt, order, versions)
                records[record["attempt_id"]] = record
                print(
                    f"{record['attempt_id']}: {record['termination_reason']} pass={record['pass']} "
                    f"cost={record['cost_usd']} contaminated={record.get('contaminated')}",
                    file=out,
                )
    print(f"study {study['study_id']} finished; " + budget_line(0.0), file=out)
    return EXIT_OK


def validate(study: dict, corpus_manifest: str, out=sys.stdout) -> int:
    tasks_dir = require_env("RIVET_PILOT_TASKS_DIR")
    projects = studylib.load_corpus_projects(corpus_manifest)
    failures = 0
    for task_id in study["tasks"]:
        try:
            task = checks.load_task(tasks_dir, task_id)
            if task["project"] not in projects:
                raise checks.TaskError(f"project {task['project']!r} is not in the corpus manifest")
            result = checks.gold_self_check(task)
            if not result["passed"]:
                raise checks.TaskError(f"gold does not pass its own check: {result['detail']}")
            print(f"ok   {task_id} {task['category']} {task['check']} limits={json.dumps(task['limits'], sort_keys=True)}", file=out)
        except checks.TaskError as error:
            failures += 1
            print(f"FAIL {task_id}: {error}", file=out)
    return EXIT_INVALID if failures else EXIT_OK


def plan(ctx: Context, out=sys.stdout) -> int:
    total = 0.0
    for block in studylib.schedule(ctx.study):
        task = ctx.tasks[block["task_id"]]
        cost = task["limits"]["max_budget_usd"] + ctx.study["per_run_reserve_usd"]
        total += cost * len(block["arm_order"])
        print(f"{block['block_id']}: {' then '.join(block['arm_order'])}  worst case ${cost * len(block['arm_order']):.2f}", file=out)
    print(f"worst case without retries ${total:.2f} vs cap ${ctx.study['budget_cap_usd']:.2f}", file=out)
    for arm in ctx.study["arms"]:
        argv = studylib.harness_argv(ctx.study, arm, ctx.claude_bin, 0.0)[:-1] + ["<task max_budget_usd>"]
        wrapped = sandbox.wrap("<runs>/<attempt>/sandbox.sb", argv)
        print(f"arm {arm}: " + " ".join(shlex.quote(a) for a in wrapped) + " < prompt.md", file=out)
    print("sandbox read denials (both arms): " + ", ".join(ctx.deny_paths + ctx.deny_regexes), file=out)
    print(f"  plus {ctx.runs_root} except the attempt's workspace and {ctx.tools_dir}; arm B also denies exec of */rivet", file=out)
    return EXIT_OK


def hash_tasks(study: dict, out=sys.stdout) -> int:
    """Prints the listed tasks' `tasks_sha256` (TASK-FORMAT.md). It checks
    nothing else, so it can compute the value a new manifest freezes."""
    digest = studylib.tasks_sha256(require_env("RIVET_PILOT_TASKS_DIR"), study["tasks"])
    print(digest, file=out)
    if study["kind"] == "confirmatory":
        same = digest == study["confirmatory"]["tasks_sha256"]
        print(f"manifest tasks_sha256 {'matches' if same else 'differs: ' + study['confirmatory']['tasks_sha256']}", file=sys.stderr)
    return EXIT_OK


def confirmatory_preflight(study: dict, command: str, dry_run: bool, out=sys.stdout) -> None:
    """The refusals of a confirmatory study (TASK-FORMAT.md "Confirmatory
    studies"), before anything is written. Raises FreezeError."""
    prereg = studylib.check_preregistration(study)
    tasks = studylib.check_tasks(study, require_env("RIVET_PILOT_TASKS_DIR"))
    print(f"frozen: preregistration {prereg['path']} sha256 {prereg['sha256']}, tracked and unmodified at HEAD", file=out)
    print(f"frozen: tasks sha256 {tasks}", file=out)
    if command != "run":
        return
    if os.environ.get("RIVET_PILOT_CLAUDE_BIN") and not dry_run:
        raise studylib.FreezeError(
            "refused: dry-run check failed: RIVET_PILOT_CLAUDE_BIN replaces the harness, so every record is a dry run; "
            "a confirmatory study needs --dry-run to run that way"
        )
    binary = studylib.check_rivet_binary(study, require_env("RIVET_PILOT_RIVET_BIN"))
    print(f"frozen: rivet binary sha256 {binary}", file=out)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="rivet study runner (T38a, T48a)")
    parser.add_argument("command", choices=["validate", "plan", "run", "hash-tasks"])
    parser.add_argument("study")
    parser.add_argument("--corpus-manifest", default=studylib.CORPUS_TOML)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="run: acknowledge that RIVET_PILOT_CLAUDE_BIN replaces the harness (required for a confirmatory study)",
    )
    args = parser.parse_args(argv)
    try:
        study = studylib.load_study(args.study)
    except (studylib.StudyError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return EXIT_INVALID
    try:
        if args.command == "hash-tasks":
            return hash_tasks(study)
        if args.command == "run" and args.dry_run and not os.environ.get("RIVET_PILOT_CLAUDE_BIN"):
            raise studylib.FreezeError("refused: dry-run check failed: --dry-run without RIVET_PILOT_CLAUDE_BIN would run the real harness")
        if study["kind"] == "confirmatory":
            confirmatory_preflight(study, args.command, args.dry_run)
        if args.command == "validate":
            return validate(study, args.corpus_manifest)
        ctx = Context(study, args.corpus_manifest)
        if args.command == "plan":
            return plan(ctx)
        return run_study(ctx)
    except checks.TaskError as error:
        print(f"error: {error}", file=sys.stderr)
        return EXIT_INVALID
    except studylib.FreezeError as error:
        print(f"error: {error}", file=sys.stderr)
        return EXIT_INVALID
    except studylib.StudyError as error:
        print(f"error: {error}", file=sys.stderr)
        return EXIT_ERROR


if __name__ == "__main__":
    sys.exit(main())
