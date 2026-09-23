"""Pilot task loading and the three answer checks (T38a).

The normative definition of the task directory and of each check type is
`benchmark/runner/TASK-FORMAT.md`; this module implements it and must not
diverge from it. Standard library only.
"""

from __future__ import annotations

import json
import math
import os
import re
import tomllib

CATEGORIES = ("locate", "trace", "callers", "tests", "dependencies")
CHECK_TYPES = ("exact_symbol", "set_f1", "accepted_path")
TASK_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")
PROJECT_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$")

# The final answer is the last fenced block whose info string is exactly
# `json` (case-insensitive), in the harness's final result text.
FENCE_RE = re.compile(r"```[ \t]*json[ \t]*\r?\n(.*?)\r?\n?[ \t]*```", re.DOTALL | re.IGNORECASE)


class TaskError(ValueError):
    """A task directory does not satisfy TASK-FORMAT.md."""


def normalize_path(path: str) -> str:
    """Normalizes a file path for comparison: `\\` becomes `/`, and any
    leading `./` segments are removed. Nothing else is changed."""
    path = path.strip().replace("\\", "/")
    while path.startswith("./"):
        path = path[2:]
    return path


def normalize_symbol(symbol: str) -> str:
    """Symbols compare exactly after trimming surrounding whitespace."""
    return symbol.strip()


def _item(value, where: str) -> tuple[str, str]:
    if not isinstance(value, dict):
        raise TaskError(f"{where}: expected an object with `file` and `symbol`")
    file, symbol = value.get("file"), value.get("symbol")
    if not isinstance(file, str) or not isinstance(symbol, str) or not file.strip() or not symbol.strip():
        raise TaskError(f"{where}: `file` and `symbol` must be non-empty strings")
    return (normalize_path(file), normalize_symbol(symbol))


def _items(value, where: str) -> list[tuple[str, str]]:
    if not isinstance(value, list):
        raise TaskError(f"{where}: expected a list")
    return [_item(v, f"{where}[{i}]") for i, v in enumerate(value)]


def parse_gold(check: str, gold) -> object:
    """Validates a gold answer and returns its normalized form."""
    if not isinstance(gold, dict):
        raise TaskError("gold.json: expected a JSON object")
    if check == "exact_symbol":
        return _item(gold, "gold.json")
    if check == "set_f1":
        items = _items(gold.get("items"), "gold.json items")
        if not items:
            raise TaskError("gold.json items: the gold set must not be empty")
        return frozenset(items)
    if check == "accepted_path":
        paths = gold.get("accepted_paths")
        if not isinstance(paths, list) or not paths:
            raise TaskError("gold.json accepted_paths: expected a non-empty list of paths")
        out = []
        for i, path in enumerate(paths):
            steps = _items(path, f"gold.json accepted_paths[{i}]")
            if not steps:
                raise TaskError(f"gold.json accepted_paths[{i}]: a path must not be empty")
            out.append(tuple(steps))
        return tuple(out)
    raise TaskError(f"unknown check type {check!r}")


def parse_answer(check: str, answer) -> object:
    """Validates an agent answer's shape; raises TaskError when it is invalid."""
    if not isinstance(answer, dict):
        raise TaskError("answer: expected a JSON object")
    if check == "exact_symbol":
        return _item(answer, "answer")
    if check == "set_f1":
        return frozenset(_items(answer.get("items"), "answer items"))
    if check == "accepted_path":
        return tuple(_items(answer.get("path"), "answer path"))
    raise TaskError(f"unknown check type {check!r}")


def extract_answer_json(text: str | None):
    """Returns (status, value). status is `ok`, `missing` or `unparseable`."""
    if not isinstance(text, str):
        return "missing", None
    blocks = FENCE_RE.findall(text)
    if not blocks:
        return "missing", None
    try:
        return "ok", json.loads(blocks[-1])
    except (json.JSONDecodeError, ValueError):
        return "unparseable", None


def evaluate(check: str, gold_raw, final_text: str | None, f1_threshold: float | None = None) -> dict:
    """Runs one check. Returns a JSON-serializable evaluator result.

    `answer_status` is one of `ok`, `missing`, `unparseable`, `invalid_shape`.
    Precision, recall and F1 are reported only for `set_f1` and are `null`
    otherwise. A missing, unparseable or invalid answer never passes.
    """
    gold = parse_gold(check, gold_raw)
    result = {
        "check": check,
        "answer_status": None,
        "passed": False,
        "precision": None,
        "recall": None,
        "f1": None,
        "f1_threshold": f1_threshold if check == "set_f1" else None,
        "detail": "",
    }
    status, value = extract_answer_json(final_text)
    if status != "ok":
        result["answer_status"] = status
        result["detail"] = f"final fenced json block {status}"
        if check == "set_f1":
            result.update(precision=0.0, recall=0.0, f1=0.0)
        return result
    try:
        answer = parse_answer(check, value)
    except TaskError as error:
        result["answer_status"] = "invalid_shape"
        result["detail"] = str(error)
        if check == "set_f1":
            result.update(precision=0.0, recall=0.0, f1=0.0)
        return result
    result["answer_status"] = "ok"
    if check == "exact_symbol":
        result["passed"] = answer == gold
    elif check == "set_f1":
        if f1_threshold is None:
            raise TaskError("set_f1 needs f1_threshold")
        hit = len(answer & gold)
        # An empty answer has no precision to measure; it is scored 0.
        precision = hit / len(answer) if answer else 0.0
        recall = hit / len(gold)
        f1 = 0.0 if precision + recall == 0 else 2 * precision * recall / (precision + recall)
        result.update(precision=precision, recall=recall, f1=f1)
        result["passed"] = f1 >= f1_threshold
    else:
        result["passed"] = answer in gold
    return result


def _positive(value, where: str, integer: bool) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or (integer and not isinstance(value, int)):
        raise TaskError(f"{where}: expected a positive {'integer' if integer else 'number'}")
    if not math.isfinite(value) or value <= 0:
        raise TaskError(f"{where}: must be positive")
    return value


def load_task(tasks_dir: str, task_id: str) -> dict:
    """Loads and validates `$RIVET_PILOT_TASKS_DIR/<task_id>/`.

    The returned dict holds the parsed `task.toml` fields plus `prompt` (the
    exact prompt text), `gold` (the raw gold JSON) and `paths` (the three
    files, which the runner hashes).
    """
    if not TASK_ID_RE.match(task_id):
        raise TaskError(f"task id {task_id!r} is not an opaque id ([A-Za-z0-9_-], at most 64)")
    root = os.path.join(tasks_dir, task_id)
    paths = {name: os.path.join(root, name) for name in ("task.toml", "prompt.md", "gold.json")}
    for name, path in paths.items():
        if not os.path.isfile(path):
            raise TaskError(f"{task_id}: missing {name}")
    with open(paths["task.toml"], "rb") as handle:
        try:
            meta = tomllib.load(handle)
        except tomllib.TOMLDecodeError as error:
            raise TaskError(f"{task_id}/task.toml: {error}") from error
    if meta.get("id") != task_id:
        raise TaskError(f"{task_id}/task.toml: id {meta.get('id')!r} does not match its directory")
    project = meta.get("project")
    if not isinstance(project, str) or not PROJECT_RE.match(project):
        raise TaskError(f"{task_id}/task.toml: project must be a plain name")
    if meta.get("category") not in CATEGORIES:
        raise TaskError(f"{task_id}/task.toml: category must be one of {', '.join(CATEGORIES)}")
    check = meta.get("check")
    if check not in CHECK_TYPES:
        raise TaskError(f"{task_id}/task.toml: check must be one of {', '.join(CHECK_TYPES)}")
    threshold = meta.get("f1_threshold")
    if check == "set_f1":
        if isinstance(threshold, bool) or not isinstance(threshold, (int, float)) or not 0 < threshold <= 1:
            raise TaskError(f"{task_id}/task.toml: set_f1 needs f1_threshold in (0, 1]")
        threshold = float(threshold)
    elif threshold is not None:
        raise TaskError(f"{task_id}/task.toml: f1_threshold is only valid for set_f1")
    limits = meta.get("limits")
    if not isinstance(limits, dict):
        raise TaskError(f"{task_id}/task.toml: missing [limits]")
    out_limits = {
        "max_budget_usd": float(_positive(limits.get("max_budget_usd"), f"{task_id} limits.max_budget_usd", False)),
        "max_turns": int(_positive(limits.get("max_turns"), f"{task_id} limits.max_turns", True)),
        "wall_seconds": float(_positive(limits.get("wall_seconds"), f"{task_id} limits.wall_seconds", False)),
    }
    with open(paths["prompt.md"], encoding="utf-8") as handle:
        prompt = handle.read()
    if not prompt.strip():
        raise TaskError(f"{task_id}/prompt.md is empty")
    with open(paths["gold.json"], encoding="utf-8") as handle:
        try:
            gold = json.load(handle)
        except json.JSONDecodeError as error:
            raise TaskError(f"{task_id}/gold.json: {error}") from error
    parse_gold(check, gold)
    return {
        "id": task_id,
        "project": project,
        "category": meta["category"],
        "check": check,
        "f1_threshold": threshold,
        "limits": out_limits,
        "prompt": prompt,
        "gold": gold,
        "paths": paths,
    }


def gold_self_check(task: dict) -> dict:
    """Feeds the gold answer back through its own check, as a final fenced
    block. Every valid task must pass this."""
    gold = task["gold"]
    if task["check"] == "accepted_path":
        answer = {"path": gold["accepted_paths"][0]}
    else:
        answer = gold
    text = "gold\n```json\n" + json.dumps(answer) + "\n```\n"
    return evaluate(task["check"], gold, text, task["f1_threshold"])
