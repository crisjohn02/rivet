"""macOS `sandbox-exec` confinement of every agent process (T38a review fix).

Every harness launch is `sandbox-exec -f <profile> <claude argv>`. The profile
allows everything by default, then denies reads of every protected path, then
re-allows the attempt's own workspace and the pinned rivet copy. SBPL is
last-match-wins (verified on macOS 27.0: a later `allow file-read*` overrides
an earlier `deny` for a nested subpath), and a directory whose ancestor is
denied cannot even be entered unless each ancestor's *metadata* is allowed,
so the ancestors between the runs directory and the workspace get
`allow file-read-metadata (literal ...)` only: they can be stat'ed, not
listed. Arm B also gets `deny process-exec` for any path ending in `/rivet`.

Paths are resolved with `os.path.realpath` before they enter the profile;
`/tmp` is `/private/tmp`, and an unresolved path silently matches nothing.
Standard library only.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess

SANDBOX_EXEC = "/usr/bin/sandbox-exec"
RIVET_EXEC_REGEX = r"/rivet$"
DENIED_MARKER = "Operation not permitted"


class SandboxError(RuntimeError):
    pass


def available() -> bool:
    return os.path.exists(SANDBOX_EXEC) and os.access(SANDBOX_EXEC, os.X_OK)


def resolve(path: str) -> str:
    return os.path.realpath(os.path.expanduser(path))


def _quote(path: str) -> str:
    if '"' in path or "\\" in path or "\n" in path:
        raise SandboxError(f"path {path!r} cannot be written into a sandbox profile")
    return f'"{path}"'


def _regex(pattern: str) -> str:
    if '"' in pattern or "\n" in pattern:
        raise SandboxError(f"regex {pattern!r} cannot be written into a sandbox profile")
    return f'#"{pattern}"'


def is_under(path: str, parent: str) -> bool:
    return path == parent or path.startswith(parent.rstrip("/") + "/")


def denied_by(path: str, deny_paths: list[str], deny_regexes: list[str]) -> str | None:
    """The first static rule that would deny reading `path` (resolved)."""
    real = resolve(path)
    for deny in deny_paths:
        if is_under(real, deny):
            return deny
    for pattern in deny_regexes:
        if re.search(pattern, real):
            return pattern
    return None


def ancestors_between(top: str, leaf: str) -> list[str]:
    """`top` and every directory strictly between it and `leaf`."""
    out = []
    current = os.path.dirname(leaf)
    while is_under(current, top):
        out.append(current)
        if current == top:
            break
        current = os.path.dirname(current)
    return sorted(out, key=len)


def build_profile(
    deny_paths: list[str],
    deny_regexes: list[str],
    runs_root: str,
    allow_paths: list[str],
    deny_rivet_exec: bool,
) -> str:
    """The SBPL text. `deny_paths`, `runs_root` and `allow_paths` must
    already be resolved. `allow_paths` must lie under `runs_root`."""
    lines = ["(version 1)", "(allow default)"]
    for path in sorted(set(deny_paths)):
        lines.append(f"(deny file-read* (subpath {_quote(path)}))")
    for pattern in sorted(set(deny_regexes)):
        lines.append(f"(deny file-read* (regex {_regex(pattern)}))")
    lines.append(f"(deny file-read* (subpath {_quote(runs_root)}))")
    metadata = set()
    for path in allow_paths:
        if not is_under(path, runs_root) or path == runs_root:
            raise SandboxError(f"allowed path {path} is not inside the runs directory")
        metadata.update(ancestors_between(runs_root, path))
    for path in sorted(metadata):
        lines.append(f"(allow file-read-metadata (literal {_quote(path)}))")
    for path in sorted(set(allow_paths)):
        lines.append(f"(allow file-read* (subpath {_quote(path)}))")
    if deny_rivet_exec:
        lines.append(f"(deny process-exec (regex {_regex(RIVET_EXEC_REGEX)}))")
    return "\n".join(lines) + "\n"


def wrap(profile_path: str, argv: list[str]) -> list[str]:
    return [SANDBOX_EXEC, "-f", profile_path, *argv]


def _run(profile_path: str, argv: list[str], cwd: str, env: dict) -> dict:
    try:
        done = subprocess.run(wrap(profile_path, argv), cwd=cwd, env=env, capture_output=True, text=True, timeout=60, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return {"argv": argv, "exit": None, "denied": False, "stderr": f"{type(error).__name__}: {error}"}
    return {
        "argv": argv,
        "exit": done.returncode,
        "denied": done.returncode != 0 and DENIED_MARKER in done.stderr,
        "stderr": done.stderr.strip()[-400:],
    }


def probe(profile_path: str, cwd: str, env: dict, expect_ok: list[tuple[str, list[str]]], expect_denied: list[tuple[str, list[str]]]) -> dict:
    """Runs each probe inside the exact profile the agent will get. A probe
    expected to be denied passes only when it fails with the sandbox's
    "Operation not permitted"; one expected to work passes only on exit 0.
    Returns {"passed": bool, "checks": [...]}."""
    checks = []
    for name, argv in expect_ok:
        result = _run(profile_path, argv, cwd, env)
        result.update(name=name, expect="ok", passed=result["exit"] == 0)
        checks.append(result)
    for name, argv in expect_denied:
        result = _run(profile_path, argv, cwd, env)
        result.update(name=name, expect="denied", passed=bool(result["denied"]))
        checks.append(result)
    return {"passed": all(c["passed"] for c in checks), "checks": checks}


def which_real(binary: str, path_value: str | None = None) -> str | None:
    found = binary if os.sep in binary else shutil.which(binary, path=path_value)
    return os.path.realpath(found) if found and os.path.exists(found) else None
