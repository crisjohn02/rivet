#!/usr/bin/env python3
"""A fake `claude -p` for runner tests. It never calls a model or a network.

Selected through RIVET_PILOT_CLAUDE_BIN. Behavior comes from a plan file:

    FAKE_CLAUDE_PLAN   JSON {"<task>:<arm>": [step, ...]}; call n of that key
                       uses step n (the last step repeats). A step is
                       {"transcript": "<file in testdata/transcripts>",
                        "turns": N, "sleep": S, "exit": E,
                        "mkdir_rivet": bool, "stderr": "..."}.
    FAKE_CLAUDE_STATE  directory for per-key call counters.
    FAKE_CLAUDE_LOG    JSON-lines log of every invocation (argv, cwd, PATH,
                       whether rivet resolves on PATH, env names, stdin),
                       plus one {"key", "probes"} line per step with
                       "reads"/"execs" sandbox probes.

The task is read from a `fake-task: <id>` line in the prompt on stdin; the
arm is C exactly when `--allowedTools` includes Bash(rivet:*).
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> int:
    argv = sys.argv[1:]
    if argv == ["--version"]:
        print("0.0.0 (Fake Claude)")
        return 0
    prompt = sys.stdin.read()
    match = re.search(r"fake-task: (\S+)", prompt)
    task = match.group(1) if match else "unknown"
    allowed = argv[argv.index("--allowedTools") + 1] if "--allowedTools" in argv else ""
    arm = "C" if "Bash(rivet:*)" in allowed else "B"
    key = f"{task}:{arm}"
    state = os.environ.get("FAKE_CLAUDE_STATE")
    count = 0
    if state:
        os.makedirs(state, exist_ok=True)
        counter = os.path.join(state, key.replace(":", "_"))
        if os.path.exists(counter):
            with open(counter) as handle:
                count = int(handle.read())
        with open(counter, "w") as handle:
            handle.write(str(count + 1))
    log = os.environ.get("FAKE_CLAUDE_LOG")
    if log:
        with open(log, "a") as handle:
            handle.write(
                json.dumps(
                    {
                        "argv": argv,
                        "cwd": os.getcwd(),
                        "path": os.environ.get("PATH", ""),
                        "rivet_on_path": shutil.which("rivet"),
                        "env_names": sorted(os.environ),
                        "stdin": prompt,
                        "key": key,
                        "call": count,
                    },
                    sort_keys=True,
                )
                + "\n"
            )
    plan = {}
    if os.environ.get("FAKE_CLAUDE_PLAN"):
        with open(os.environ["FAKE_CLAUDE_PLAN"]) as handle:
            plan = json.load(handle)
    steps = plan.get(key) or [{"transcript": "ok_exact.jsonl"}]
    step = steps[min(count, len(steps) - 1)]
    if step.get("stderr"):
        sys.stderr.write(step["stderr"])
    # Sandbox probes from inside the agent process: {"reads": [path, ...],
    # "execs": [[argv...], ...]}; "{{WORKSPACE}}" expands to the cwd.
    probes = {}
    for path in step.get("reads", []):
        path = path.replace("{{WORKSPACE}}", os.getcwd())
        try:
            with open(path, "rb") as handle:
                handle.read(1)
            probes[path] = "ok"
        except PermissionError:
            probes[path] = "denied"
        except OSError as error:
            probes[path] = f"error:{error.errno}"
    for argv_probe in step.get("execs", []):
        argv_probe = [a.replace("{{WORKSPACE}}", os.getcwd()) for a in argv_probe]
        try:
            done = subprocess.run(argv_probe, capture_output=True, timeout=30)
            probes[" ".join(argv_probe)] = f"exit:{done.returncode}"
        except PermissionError:
            probes[" ".join(argv_probe)] = "denied"
        except OSError as error:
            probes[" ".join(argv_probe)] = f"error:{error.errno}"
    if probes and log:
        with open(log, "a") as handle:
            handle.write(json.dumps({"key": key, "probes": probes}, sort_keys=True) + "\n")
    if step.get("mkdir_rivet"):
        os.makedirs(os.path.join(os.getcwd(), ".rivet"), exist_ok=True)
    for n in range(int(step.get("turns", 0))):
        event = {
            "type": "assistant",
            "message": {
                "id": f"msg_turn_{n}",
                "model": "claude-opus-5-5",
                "content": [{"type": "text", "text": f"thinking {n}"}],
                "usage": {"input_tokens": 10, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0, "output_tokens": 1},
            },
        }
        print(json.dumps(event), flush=True)
        time.sleep(0.05)
    if step.get("transcript"):
        with open(os.path.join(HERE, "transcripts", step["transcript"])) as handle:
            for line in handle:
                print(line.rstrip("\n").replace("{{WORKSPACE}}", os.getcwd()), flush=True)
    if step.get("sleep"):
        time.sleep(float(step["sleep"]))
    return int(step.get("exit", 0))


if __name__ == "__main__":
    sys.exit(main())
