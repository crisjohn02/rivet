#!/usr/bin/env python3
"""Task-weighted aggregates and the Markdown report: pilot (T38c) and
confirmatory (T48a) studies.

    python3 benchmark/runner/report.py RUNS_CSV --study benchmark/studies/<id>/study.toml \
        --out-dir benchmark/results/<id> [--corpus-manifest benchmark/corpus.toml]

Writes `report.md` (from benchmark/REPORT-TEMPLATE.md), `summary.json` and
`per-task.csv` into the output directory. The outputs name tasks only by
opaque ID, category and project, and contain only aggregates; no transcript
text reaches them. For a pilot every confirmatory gate is `NOT EVALUATED`:
BENCHMARK.md forbids treating a pilot as the confirmatory study. For a
confirmatory study the report applies the frozen inclusion and validity
rules, a paired cluster bootstrap and the preregistered gates
(TASK-FORMAT.md "Confirmatory analysis"). Identical inputs give
byte-identical outputs. Standard library only.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import math
import os
import random
import re
import sys
from fractions import Fraction

sys.dont_write_bytecode = True  # keep __pycache__ out of the repository
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import study as studylib  # noqa: E402

ANALYSIS_VERSION = "t38c-1"
TEMPLATE = os.path.join(studylib.REPO_ROOT, "benchmark", "REPORT-TEMPLATE.md")
MISSING = ("unavailable", "not_applicable", "")
LIMIT_REASONS = ("wall_timeout", "max_turns", "max_budget")
CRASH_REASONS = ("agent_crash", "agent_error")
SAFE_TEXT_RE = re.compile(r"^[A-Za-z0-9_.:+-]*$")


class ReportError(RuntimeError):
    pass


def number(value: str):
    if value in MISSING:
        return None
    if value == "true":
        return 1.0
    if value == "false":
        return 0.0
    try:
        return float(value)
    except ValueError:
        return None


def read_rows(path: str) -> tuple[list[dict], str]:
    with open(path, "rb") as handle:
        data = handle.read()
    rows = list(csv.DictReader(io.StringIO(data.decode("utf-8"))))
    for row in rows:
        for column in ("task_id", "category", "project", "config", "block_id", "run_id", "attempt_id"):
            if not SAFE_TEXT_RE.match(row.get(column, "")):
                raise ReportError(f"{column} value {row.get(column)!r} is not an opaque token; refusing to publish it")
    return rows, studylib.sha256_bytes(data)


def mean(values: list[float]):
    return sum(values) / len(values) if values else None


METRICS = {
    "input_tokens_total": "input_tokens_total",
    "success": "pass",
    "wall_clock_seconds": "wall_clock_seconds",
    "tool_calls_total": "tool_calls_total",
    "output_tokens": "output_tokens",
    "cost_usd": "cost_usd_claude_code_estimate",
}


def task_means(rows: list[dict], arms: list[str], mean_fn=mean) -> dict:
    """{metric: {task: {arm: {"mean", "n", "missing"}}}} over included rows.
    A run whose value is unavailable is counted as missing, never as zero."""
    out: dict = {}
    tasks = sorted({r["task_id"] for r in rows})
    for metric, column in METRICS.items():
        out[metric] = {}
        for task in tasks:
            out[metric][task] = {}
            for arm in arms:
                selected = [r for r in rows if r["task_id"] == task and r["config"] == arm and r["analysis_inclusion"] == "included"]
                values = [number(r[column]) for r in selected]
                present = [v for v in values if v is not None]
                out[metric][task][arm] = {"mean": mean_fn(present), "n": len(present), "missing": len(values) - len(present)}
    return out


def arm_summary(per_task: dict, arms: list[str], mean_fn=mean) -> dict:
    """Task-weighted means: every task with a value counts once. `paired`
    restricts to tasks with a value in every arm, which is what the C/B
    comparison uses."""
    out = {}
    for metric, tasks in per_task.items():
        entry = {"all_tasks": {}, "paired": {}, "paired_tasks": []}
        for arm in arms:
            values = [cell[arm]["mean"] for cell in tasks.values() if cell[arm]["mean"] is not None]
            entry["all_tasks"][arm] = {"mean": mean_fn(values), "tasks": len(values)}
        paired = sorted(t for t, cell in tasks.items() if all(cell[a]["mean"] is not None for a in arms))
        entry["paired_tasks"] = paired
        for arm in arms:
            entry["paired"][arm] = mean_fn([tasks[t][arm]["mean"] for t in paired])
        entry["missing_runs"] = {arm: sum(cell[arm]["missing"] for cell in tasks.values()) for arm in arms}
        out[metric] = entry
    return out


def ratio(numerator, denominator):
    """C/B, or None with a reason for an undefined ratio."""
    if numerator is None or denominator is None:
        return None, "no paired tasks"
    if denominator == 0:
        return None, "zero denominator"
    return numerator / denominator, None


def comparisons(summary: dict) -> dict:
    out = {}
    for metric, entry in summary.items():
        b, c = entry["paired"].get("B"), entry["paired"].get("C")
        if metric == "success":
            diff = None if b is None or c is None else (c - b) * 100.0
            out[metric] = {"difference_pp": diff, "paired_tasks": len(entry["paired_tasks"])}
            continue
        value, why = ratio(c, b)
        out[metric] = {
            "ratio_c_over_b": value,
            "undefined_reason": why,
            "change_pct": None if value is None else (value - 1.0) * 100.0,
            "reduction_pct": None if value is None else (1.0 - value) * 100.0,
            "paired_tasks": len(entry["paired_tasks"]),
        }
    return out


def accounting(rows: list[dict], study: dict, arms: list[str]) -> dict:
    out = {}
    for arm in arms:
        mine = [r for r in rows if r["config"] == arm]
        included = [r for r in mine if r["analysis_inclusion"] == "included"]
        scheduled = {f"{t}.t{n}.{arm}" for t in study["tasks"] for n in range(1, study["trials"] + 1)}
        accounted = {r["run_id"] for r in included}
        out[arm] = {
            "scheduled": len(scheduled),
            "attempts": len(mine),
            "accounted": len(scheduled & accounted),
            "successful": sum(1 for r in included if r["pass"] == "true"),
            "evaluator_failures": sum(
                1 for r in included if r["termination_reason"] == "completed" and r["pass"] == "false" and r["answer_status"] == "ok"
            ),
            "limit_reached": sum(1 for r in included if r["termination_reason"] in LIMIT_REASONS),
            "crash_or_invalid_answer": sum(
                1
                for r in included
                if r["termination_reason"] in CRASH_REASONS
                or (r["termination_reason"] == "completed" and r["answer_status"] != "ok")
            ),
            "infrastructure_failures": sum(1 for r in mine if r["infrastructure_failure"] == "true"),
            "rerun_attempts": sum(1 for r in mine if r["attempt"] == "2"),
            "missing": len(scheduled - accounted),
            "contaminated": sum(1 for r in mine if r["contaminated"] == "true"),
            "isolation_violations": sum(1 for r in mine if r["isolation_violations"] not in ("none", "")),
            "unscheduled_runs": len(accounted - scheduled),
        }
    return out


def adoption(rows: list[dict]) -> dict:
    included = [r for r in rows if r["config"] == "C" and r["analysis_inclusion"] == "included"]
    by_command: dict[str, int] = {}
    by_exit: dict[str, int] = {}
    invoking = 0
    known = 0
    fallbacks = 0
    fallbacks_known = 0
    for r in included:
        commands = r["rivet_invocations_by_command"]
        if commands not in MISSING:
            known += 1
            parsed = json.loads(commands)
            if parsed:
                invoking += 1
            for key, value in parsed.items():
                by_command[key] = by_command.get(key, 0) + value
        if r["rivet_errors_by_exit"] not in MISSING:
            for key, value in json.loads(r["rivet_errors_by_exit"]).items():
                by_exit[key] = by_exit.get(key, 0) + value
        if r["rivet_to_text_fallbacks"] not in MISSING:
            fallbacks_known += 1
            fallbacks += int(r["rivet_to_text_fallbacks"])
    return {
        "eligible_runs": len(included),
        "runs_with_known_commands": known,
        "runs_invoking_rivet": invoking,
        "invocations_by_command": dict(sorted(by_command.items())),
        "errors_by_exit": dict(sorted(by_exit.items())),
        "text_fallbacks": fallbacks,
        "runs_with_known_fallbacks": fallbacks_known,
    }


def compute(rows: list[dict], study: dict, csv_sha: str) -> dict:
    arms = sorted(study["arms"])
    per_task = task_means(rows, arms)
    summary = arm_summary(per_task, arms)
    known_costs = [number(r["cost_usd_claude_code_estimate"]) for r in rows]
    return {
        "analysis_version": ANALYSIS_VERSION,
        "study_id": study["study_id"],
        "study_kind": "pilot",
        "inputs": {"runs_csv_sha256": csv_sha, "study_manifest_sha256": study["sha256"]},
        "arms": arms,
        "weighting": "equal task weighting over included attempts; comparisons use tasks with a value in every arm",
        "per_task": per_task,
        "arm_means": summary,
        "comparisons": comparisons(summary) if set(arms) == {"B", "C"} else {},
        "accounting": accounting(rows, study, arms),
        "adoption_c": adoption(rows) if "C" in arms else None,
        "spend": {
            "reported_usd_claude_code_estimate": sum(v for v in known_costs if v is not None),
            "attempts_with_unknown_cost": sum(1 for v in known_costs if v is None),
            "cap_usd": study["budget_cap_usd"],
        },
        "gates": {
            "efficiency_point_estimate": "not_evaluated",
            "evidence_of_reduction": "not_evaluated",
            "success_non_inferiority": "not_evaluated",
            "study_integrity": "not_evaluated",
            "reason": "pilot study; docs/BENCHMARK.md forbids treating the pilot as the confirmatory study",
        },
        "intervals": "not computed (pilot; paired intervals are T48a)",
    }


# ---- confirmatory analysis (T48a) ------------------------------------------

# The analysis this script implements. A manifest whose analysis_version
# differs makes the outcome `invalid`.
CONFIRMATORY_ANALYSIS_VERSION = "t48a-1"
QUALITY_MARGIN_PP = -5.0  # quality gate: lower bound on C - B must exceed this
EFFICIENCY_LIMIT = 1.0  # efficiency gate upper_bound_below_1: upper bound on C/B below this
UPPER_PERCENT = 95
LOWER_PERCENT = 5
VERDICTS = {
    "pass": "gates passed",
    "fail_quality": "gates not met (success non-inferiority failed)",
    "inconclusive": "inconclusive (efficiency gate not met)",
    "invalid": "invalid study",
}


def fmean(values: list[float]):
    """The exactly rounded mean (`math.fsum`), so results do not depend on
    the Python version's float summation."""
    return math.fsum(values) / len(values) if values else None


def bound_index(replicates: int, percent: int) -> int:
    """`ceil(percent / 100 * R) - 1`, computed in integer arithmetic."""
    return -(-percent * replicates // 100) - 1


def schedule_blocks(rows: list[dict], study: dict) -> list[dict]:
    """Every scheduled task/trial block, sorted by task ID then trial, with
    each arm's includable attempt or the reason it has none. A block is
    included only when both arms have one; an excluded block leaves both
    arms. The retry rule itself was applied by extract.py."""
    arms = study["arms"]
    trials = study["trials"]
    grouped: dict = {}
    for row in rows:
        if row["config"] not in arms:
            raise ReportError(f"{row['attempt_id']}: arm {row['config']!r} is not in the study")
        if not re.fullmatch(r"[1-9][0-9]*", row["trial"]) or not re.fullmatch(r"[1-9][0-9]*", row["attempt"]):
            raise ReportError(f"{row['attempt_id']}: trial and attempt must be positive integers")
        trial = int(row["trial"])
        if trial > trials:
            raise ReportError(f"{row['attempt_id']}: trial {trial} is not in the study's schedule of {trials} trials")
        if row["block_id"] != f"{row['task_id']}.t{trial}":
            raise ReportError(f"{row['attempt_id']}: block_id {row['block_id']!r} does not match its task and trial")
        grouped.setdefault((row["task_id"], trial), {}).setdefault(row["config"], []).append(row)
    out = []
    for task in sorted(study["tasks"]):
        for trial in range(1, trials + 1):
            per_arm = grouped.get((task, trial), {})
            reasons: dict = {}
            values: dict = {}
            for arm in arms:
                attempts = sorted(per_arm.get(arm, []), key=lambda r: (int(r["attempt"]), r["attempt_id"]))
                included = [r for r in attempts if r["analysis_inclusion"] == "included"]
                if len(included) > 1:
                    raise ReportError(f"block {task}.t{trial} arm {arm} has {len(included)} included attempts; the retry rule includes one")
                if not attempts:
                    reasons[arm] = "missing_run"
                elif not included:
                    reasons[arm] = attempts[-1]["inclusion_reason"] or "excluded"
                elif number(included[0]["input_tokens_total"]) is None:
                    reasons[arm] = "missing_usage"
                elif included[0]["pass"] not in ("true", "false"):
                    reasons[arm] = "missing_pass"
                else:
                    values[arm] = {
                        "attempt_id": included[0]["attempt_id"],
                        "tokens": number(included[0]["input_tokens_total"]),
                        "pass": 1.0 if included[0]["pass"] == "true" else 0.0,
                    }
            for arm, reason in reasons.items():
                if not SAFE_TEXT_RE.match(reason):
                    raise ReportError(f"block {task}.t{trial} arm {arm}: inclusion reason {reason!r} is not an opaque token")
            out.append(
                {
                    "block_id": f"{task}.t{trial}",
                    "task_id": task,
                    "trial": trial,
                    "included": not reasons,
                    "reasons": reasons,
                    "arms": values if not reasons else {},
                }
            )
    return out


def task_values(blocks: list[dict]) -> dict:
    """Per task, over its included blocks only: mean input tokens and pass
    rate per arm. Tasks without an included block have no values."""
    out = {}
    for task in sorted({b["task_id"] for b in blocks}):
        included = [b for b in blocks if b["task_id"] == task and b["included"]]
        entry = {"included_blocks": len(included), "scheduled_blocks": sum(1 for b in blocks if b["task_id"] == task)}
        if included:
            for arm in ("B", "C"):
                entry[arm] = {
                    "tokens": fmean([b["arms"][arm]["tokens"] for b in included]),
                    "success": fmean([b["arms"][arm]["pass"] for b in included]),
                }
        out[task] = entry
    return out


def estimates(values: dict, tasks: list[str]) -> dict:
    """The two primary estimates over `tasks` (a replicate may repeat a
    task), each task weighted once per appearance: ratio = mean of task C
    means / mean of task B means (+inf when that B mean is 0), and the
    success difference in percentage points."""
    b = fmean([values[t]["B"]["tokens"] for t in tasks])
    c = fmean([values[t]["C"]["tokens"] for t in tasks])
    pb = fmean([values[t]["B"]["success"] for t in tasks])
    pc = fmean([values[t]["C"]["success"] for t in tasks])
    return {
        "b_mean_input_tokens": b,
        "c_mean_input_tokens": c,
        "ratio": math.inf if b == 0 else c / b,
        "b_success_rate": pb,
        "c_success_rate": pc,
        "success_diff_pp": 100.0 * (pc - pb),
    }


def bootstrap_draws(strata: dict[str, list[str]], seed: int, replicates: int):
    """Yields each replicate's drawn tasks. One `random.Random(seed)` drives
    every draw: per replicate, strata in sorted name order; per stratum of n
    tasks sorted by ID, n draws of `tasks[rng.randrange(n)]`. Each stratum
    keeps its size."""
    rng = random.Random(seed)
    order = [sorted(strata[name]) for name in sorted(strata)]
    for _ in range(replicates):
        drawn = []
        for members in order:
            n = len(members)
            drawn.extend(members[rng.randrange(n)] for _ in range(n))
        yield drawn


def bootstrap(values: dict, strata: dict[str, list[str]], seed: int, replicates: int) -> dict:
    """Paired cluster bootstrap: a drawn task brings all of its included
    blocks in both arms, so the replicate estimates are the estimates over
    the drawn tasks' means. Bounds are order statistics of the sorted
    replicates (TASK-FORMAT.md "Paired cluster bootstrap")."""
    ratios, diffs = [], []
    zero = 0
    for drawn in bootstrap_draws(strata, seed, replicates):
        est = estimates(values, drawn)
        if math.isinf(est["ratio"]):
            zero += 1
        ratios.append(est["ratio"])
        diffs.append(est["success_diff_pp"])
    ratios.sort()
    diffs.sort()
    upper, lower = bound_index(replicates, UPPER_PERCENT), bound_index(replicates, LOWER_PERCENT)
    return {
        "method": "paired cluster bootstrap: resample tasks with replacement within language strata; each drawn task brings all its included blocks in both arms",
        "rng": "python random.Random(analysis_seed); per replicate, strata in sorted name order, n draws of tasks_sorted_by_id[rng.randrange(n)] per stratum",
        "seed": seed,
        "replicates": replicates,
        "strata": {name: sorted(tasks) for name, tasks in sorted(strata.items())},
        "ratio_upper_bound_index": upper,
        "success_diff_lower_bound_index": lower,
        "ratio_upper_bound": ratios[upper],
        "success_diff_lower_bound_pp": diffs[lower],
        "zero_denominator_replicates": zero,
    }


def task_languages(rows: list[dict], corpus_manifest: str) -> tuple[dict, dict]:
    """(language, project) of every task with rows. The language is the
    task project's single `languages` entry in the corpus manifest; a
    project with more than one language is an error for now."""
    projects = studylib.load_corpus_projects(corpus_manifest)
    named: dict[str, set] = {}
    for row in rows:
        named.setdefault(row["task_id"], set()).add(row["project"])
    languages, owners = {}, {}
    for task, names in sorted(named.items()):
        if len(names) != 1:
            raise ReportError(f"task {task}: rows name {len(names)} projects")
        project = next(iter(names))
        entry = projects.get(project)
        if entry is None:
            raise ReportError(f"task {task}: project {project!r} is not in {corpus_manifest}")
        langs = entry.get("languages")
        if not isinstance(langs, list) or len(langs) != 1 or not isinstance(langs[0], str) or not langs[0] or not SAFE_TEXT_RE.match(langs[0]):
            raise ReportError(
                f"project {project!r} has languages {langs!r}; bootstrap strata need exactly one language per project"
            )
        languages[task] = langs[0]
        owners[task] = project
    return languages, owners


def successful_only(rows: list[dict]) -> dict:
    """Secondary: the input-token ratio over passing runs only, with equal
    task weighting over tasks that have a passing run in both arms."""
    per: dict = {}
    for row in rows:
        if row["analysis_inclusion"] == "included" and row["pass"] == "true" and number(row["input_tokens_total"]) is not None:
            per.setdefault(row["task_id"], {}).setdefault(row["config"], []).append(number(row["input_tokens_total"]))
    paired = sorted(t for t, arms in per.items() if "B" in arms and "C" in arms)
    b = fmean([fmean(per[t]["B"]) for t in paired])
    c = fmean([fmean(per[t]["C"]) for t in paired])
    value, why = ratio(c, b)
    return {"ratio_c_over_b": value, "undefined_reason": why, "paired_tasks": len(paired), "b_mean": b, "c_mean": c}


def group_estimates(values: dict, groups: dict[str, list[str]]) -> dict:
    """Secondary: equal-task-weighted estimates within each category or
    language."""
    out = {}
    for name, tasks in sorted(groups.items()):
        est = estimates(values, sorted(tasks))
        out[name] = {
            "tasks": len(tasks),
            "b_mean_input_tokens": est["b_mean_input_tokens"],
            "c_mean_input_tokens": est["c_mean_input_tokens"],
            "ratio_c_over_b": None if math.isinf(est["ratio"]) else est["ratio"],
            "b_success_rate": est["b_success_rate"],
            "c_success_rate": est["c_success_rate"],
            "success_diff_pp": est["success_diff_pp"],
        }
    return out


def compute_confirmatory(rows: list[dict], study: dict, csv_sha: str, corpus_manifest: str) -> tuple[dict, list[dict]]:
    """The confirmatory summary and the rows as analysed (rows of excluded
    blocks marked excluded)."""
    conf = study["confirmatory"]
    arms = ["B", "C"]
    blocks = schedule_blocks(rows, study)
    languages, owners = task_languages(rows, corpus_manifest)
    values = task_values(blocks)
    included_tasks = sorted(t for t, v in values.items() if v["included_blocks"] > 0)
    excluded = [b for b in blocks if not b["included"]]
    kept = {v["attempt_id"] for b in blocks if b["included"] for v in b["arms"].values()}
    analysis_rows = [dict(r, analysis_inclusion="included" if r["attempt_id"] in kept else "excluded") for r in rows]

    reasons = []
    dry = sum(1 for r in rows if r.get("dry_run") == "true")
    unknown = sum(1 for r in rows if r.get("dry_run") not in ("true", "false"))
    if dry:
        reasons.append({"reason": "dry_run", "detail": f"{dry} of {len(rows)} rows are dry runs (RIVET_PILOT_CLAUDE_BIN replaced the harness)"})
    if unknown:
        reasons.append({"reason": "dry_run_unknown", "detail": f"{unknown} of {len(rows)} rows do not record whether they are dry runs"})
    if conf["analysis_version"] != CONFIRMATORY_ANALYSIS_VERSION:
        reasons.append(
            {
                "reason": "analysis_version_mismatch",
                "detail": f"manifest analysis_version {conf['analysis_version']}, script {CONFIRMATORY_ANALYSIS_VERSION}",
            }
        )
    minimum = conf["min_complete_trials_per_task"]
    short = [(t, values[t]["included_blocks"]) for t in sorted(values) if values[t]["included_blocks"] < minimum]
    if short:
        reasons.append(
            {
                "reason": "too_few_complete_trials",
                "detail": "; ".join(f"{t}: {n} included blocks" for t, n in short) + f" (minimum {minimum})",
            }
        )
    limit = conf["max_excluded_block_fraction"]
    # Compared exactly, as the decimal written in the manifest.
    if Fraction(len(excluded), len(blocks)) > Fraction(repr(limit)):
        reasons.append(
            {
                "reason": "too_many_excluded_blocks",
                "detail": f"{len(excluded)} of {len(blocks)} scheduled blocks excluded; the limit is {repr(limit)}",
            }
        )
    point = estimates(values, included_tasks) if included_tasks else None
    if point is None:
        reasons.append({"reason": "no_included_blocks", "detail": "no task has an included block"})
    elif point["b_mean_input_tokens"] == 0:
        reasons.append({"reason": "zero_denominator", "detail": "the B mean input tokens of the point estimate is 0"})

    strata: dict[str, list[str]] = {}
    for task in included_tasks:
        strata.setdefault(languages[task], []).append(task)
    boot = bootstrap(values, strata, conf["analysis_seed"], conf["bootstrap_replicates"]) if included_tasks else None

    gates = {
        "efficiency_gate": conf["efficiency_gate"],
        "efficiency_rule": "one-sided 95% upper bound on C/B < 1.00",
        "quality_rule": "one-sided 95% lower bound on C - B > -5 percentage points",
        "point_estimate_at_most_0_70": "not_a_gate",
        "evaluated": not reasons,
        "efficiency_pass": None,
        "quality_pass": None,
        "not_evaluated_reason": ", ".join(r["reason"] for r in reasons) or None,
    }
    if reasons:
        outcome = "invalid"
    else:
        gates["quality_pass"] = boot["success_diff_lower_bound_pp"] > QUALITY_MARGIN_PP
        gates["efficiency_pass"] = boot["ratio_upper_bound"] < EFFICIENCY_LIMIT
        outcome = "fail_quality" if not gates["quality_pass"] else "inconclusive" if not gates["efficiency_pass"] else "pass"

    primary = {
        "weighting": "equal task weighting over included blocks; failures count as pass = 0 with their tokens",
        "tasks": included_tasks,
        "b_mean_input_tokens": None,
        "c_mean_input_tokens": None,
        "ratio_c_over_b": None,
        "ratio_undefined_reason": None,
        "reduction_pct": None,
        "b_success_rate": None,
        "c_success_rate": None,
        "success_diff_pp": None,
    }
    if point is not None:
        primary.update({k: point[k] for k in ("b_mean_input_tokens", "c_mean_input_tokens", "b_success_rate", "c_success_rate", "success_diff_pp")})
        if math.isinf(point["ratio"]):
            primary["ratio_undefined_reason"] = "zero denominator"
        else:
            primary["ratio_c_over_b"] = point["ratio"]
            primary["reduction_pct"] = (1.0 - point["ratio"]) * 100.0
    else:
        primary["ratio_undefined_reason"] = "no included blocks"

    per_task = task_means(analysis_rows, arms, mean_fn=fmean)
    arm_means = arm_summary(per_task, arms, mean_fn=fmean)
    categories: dict[str, set] = {}
    for row in rows:
        categories.setdefault(row["task_id"], set()).add(row["category"])
    for task, names in categories.items():
        if len(names) != 1:
            raise ReportError(f"task {task}: rows name {len(names)} categories")
    by_category: dict[str, list[str]] = {}
    for task in included_tasks:
        by_category.setdefault(next(iter(categories[task])), []).append(task)
    known_costs = [number(r["cost_usd_claude_code_estimate"]) for r in rows]
    summary = {
        "analysis_version": CONFIRMATORY_ANALYSIS_VERSION,
        "manifest_analysis_version": conf["analysis_version"],
        "study_id": study["study_id"],
        "study_kind": "confirmatory",
        "dry_run": bool(dry),
        "inputs": {
            "runs_csv_sha256": csv_sha,
            "study_manifest_sha256": study["sha256"],
            "preregistration": conf["preregistration"],
            "preregistration_sha256": conf["preregistration_sha256"],
            "rivet_binary_sha256": conf["rivet_binary_sha256"],
            "tasks_sha256": conf["tasks_sha256"],
        },
        "arms": arms,
        "outcome": outcome,
        "validity": {"valid": not reasons, "reasons": reasons},
        "blocks": {
            "scheduled": len(blocks),
            "included": len(blocks) - len(excluded),
            "excluded": len(excluded),
            "max_excluded_block_fraction": limit,
            "min_complete_trials_per_task": minimum,
            "included_by_task": {t: values[t]["included_blocks"] for t in sorted(values)},
        },
        "exclusions": [
            {"block_id": b["block_id"], "task_id": b["task_id"], "trial": b["trial"], "reasons": dict(sorted(b["reasons"].items()))}
            for b in excluded
        ],
        "primary": primary,
        "bootstrap": boot,
        "gates": gates,
        "per_task_primary": {
            t: {
                "project": owners.get(t),
                "language": languages.get(t),
                "category": next(iter(categories[t])) if t in categories else None,
                "included_blocks": values[t]["included_blocks"],
                "B": values[t].get("B"),
                "C": values[t].get("C"),
            }
            for t in sorted(values)
        },
        "per_task": per_task,
        "secondary": {
            "label": "secondary: point estimates only, no interval, no gate",
            "arm_means": arm_means,
            "comparisons": comparisons(arm_means),
            "successful_runs_only_input_tokens": successful_only(analysis_rows),
            "by_category": group_estimates(values, by_category),
            "by_language": group_estimates(values, strata),
        },
        "accounting": accounting(analysis_rows, study, arms),
        "adoption_c": adoption(analysis_rows),
        "spend": {
            "reported_usd_claude_code_estimate": math.fsum(v for v in known_costs if v is not None),
            "attempts_with_unknown_cost": sum(1 for v in known_costs if v is None),
            "cap_usd": study["budget_cap_usd"],
        },
    }
    return summary, analysis_rows


def strict_json(value):
    """JSON without NaN or Infinity: an infinite bound is the string "+inf"."""
    if isinstance(value, float) and math.isinf(value):
        return "+inf" if value > 0 else "-inf"
    if isinstance(value, dict):
        return {k: strict_json(v) for k, v in value.items()}
    if isinstance(value, list):
        return [strict_json(v) for v in value]
    return value


# ---- formatting ------------------------------------------------------------


def f_num(value, digits=0, suffix=""):
    if value is None:
        return "unavailable"
    return f"{value:,.{digits}f}{suffix}"


def f_pct(value):
    return "unavailable" if value is None else f"{value:+.1f}%"


def f_rate(value):
    return "unavailable" if value is None else f"{value * 100:.0f}%"


def set_row(text: str, first_cell: str, cells: list[str]) -> str:
    pattern = re.compile(r"^\| " + re.escape(first_cell) + r" \|.*$", re.MULTILINE)
    if len(pattern.findall(text)) != 1:
        raise ReportError(f"template row {first_cell!r} not found exactly once; the template changed")
    line = "| " + " | ".join([first_cell] + cells) + " |"
    return pattern.sub(lambda _m: line, text, count=1)


def replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        raise ReportError(f"template text {old[:50]!r} not found exactly once; the template changed")
    return text.replace(old, new)


def rewrite_links(text: str, template_dir: str, out_dir: str) -> str:
    def fix(match):
        target = match.group(2)
        if re.match(r"^[a-z]+:", target) or target.startswith("#"):
            return match.group(0)
        path, _, anchor = target.partition("#")
        absolute = os.path.normpath(os.path.join(template_dir, path))
        new = os.path.relpath(absolute, out_dir).replace(os.sep, "/")
        return f"{match.group(1)}({new}{'#' + anchor if anchor else ''})"

    return re.sub(r"(\[[^\]]*\])\(([^)\s]+)\)", fix, text)


def render(summary: dict, rows: list[dict], study: dict, template_text: str, template_dir: str, out_dir: str) -> str:
    # Template links are relative to the template; rewrite them for the
    # destination before any generated (already destination-relative) link
    # is inserted.
    text = rewrite_links(template_text, template_dir, out_dir)
    comp = summary["comparisons"]
    means = summary["arm_means"]
    acc = summary["accounting"]
    tok, succ, wall = comp.get("input_tokens_total", {}), comp.get("success", {}), comp.get("wall_clock_seconds", {})
    dates = sorted(r["started_at"][:10] for r in rows if r["started_at"] not in MISSING)
    c_rows = [r for r in rows if r["config"] == "C"]

    def uniq(column, source=rows):
        values = sorted({r[column] for r in source if r[column] not in MISSING})
        return ", ".join(values) if values else "unavailable"

    text = replace_once(text, "# Rivet Benchmark Report — STUDY_ID", f"# Rivet Benchmark Report — {study['study_id']}")
    status_line = next(line for line in text.splitlines() if line.startswith("> **Status: NOT RUN.**"))
    text = replace_once(
        text,
        status_line,
        "> **Status: PILOT.** Pilot observations only; every confirmatory gate is not evaluated. "
        "Numerical tables are generated by `benchmark/runner/report.py` from the private `runs.csv` and must not be edited by hand. "
        f"Follow the [benchmark runbook]({os.path.relpath(os.path.join(studylib.REPO_ROOT, 'docs', 'BENCHMARK.md'), out_dir).replace(os.sep, '/')}).",
    )
    text = replace_once(
        text,
        "**Supported public statement:** None. No benchmark has run.",
        "**Supported public statement:** None. This is a small pilot for usability, variance and cost; it supports no savings claim.",
    )
    scheduled = sum(a["scheduled"] for a in acc.values())
    accounted = sum(a["accounted"] for a in acc.values())
    failed = sum(a["accounted"] - a["successful"] for a in acc.values())
    invalid = sum(a["contaminated"] for a in acc.values())
    reduction = tok.get("reduction_pct")
    text = set_row(text, "Study type / run dates", [f"pilot; {dates[0]} to {dates[-1]}" if dates else "pilot; no runs"])
    text = set_row(text, "Rivet version, commit, binary hash", [f"{uniq('tool_version', c_rows)}; commit and binary hash in the private manifest.json"])
    text = set_row(text, "Model identifier / harness version", [f"{uniq('model_id')} / Claude Code {uniq('harness_version')}"])
    projects = uniq("project")
    text = set_row(
        text,
        "Repositories, languages, tasks, trials",
        [f"{projects}; {len(study['tasks'])} tasks; {study['trials']} trial(s) per arm"],
    )
    text = set_row(
        text,
        "Scheduled / completed / failed / invalidated runs",
        [f"{scheduled} / {accounted} / {failed} / {invalid} (plus {sum(a['rerun_attempts'] for a in acc.values())} rerun attempts)"],
    )
    text = set_row(
        text,
        "Mean input-token reduction",
        [
            f"{f_num(reduction, 1, '%')} (C/B = {f_num(tok.get('ratio_c_over_b'), 3)} over {tok.get('paired_tasks', 0)} paired tasks; pilot point estimate, no interval)"
            if reduction is not None
            else f"unavailable ({tok.get('undefined_reason', 'no data')})"
        ],
    )
    text = set_row(
        text,
        "Task success difference (C − B)",
        [f"{f_num(succ.get('difference_pp'), 1)} percentage points over {succ.get('paired_tasks', 0)} paired tasks (pilot)"],
    )
    text = set_row(text, "End-to-end wall-time change", [f"{f_pct(wall.get('change_pct'))} (pilot)"])
    text = set_row(text, "Verdict", ["pilot observation — confirmatory gates not evaluated"])

    spend = summary["spend"]
    text = set_row(text, "Study manifest, artifact checksums", [f"`benchmark/studies/{study['study_id']}/study.toml` sha256 `{study['sha256'][:16]}`; runs.csv sha256 `{summary['inputs']['runs_csv_sha256'][:16]}`"])
    text = set_row(text, "Instructions, snippet/help hashes", [f"config-B prompt appended in both arms; snippet sha256 {uniq('snippet_hash', c_rows)}"])
    text = set_row(text, "Model settings, context limits, cache policy", [f"settings hash {uniq('settings_hash')}; harness default cache policy, identical in both arms"])
    text = set_row(text, "Run ordering / randomization seed", [f"seed {study['seed']}; arm order randomized within each task/trial block"])
    limits = []
    for column, label in (("max_budget_usd", "USD"), ("max_turns", "turns"), ("wall_seconds_limit", "s wall")):
        values = sorted({number(r[column]) for r in rows if number(r[column]) is not None})
        if values:
            limits.append(f"{f_num(values[0], 2 if column == 'max_budget_usd' else 0)}–{f_num(values[-1], 2 if column == 'max_budget_usd' else 0)} {label}")
    text = set_row(text, "Per-run token / time / tool-call limits", ["; ".join(limits) + " (per task, equal across arms); no tool-call limit" if limits else "unavailable"])
    text = set_row(
        text,
        "Study run/spend cap and actual spend",
        [f"${spend['cap_usd']:.2f} cap; ${spend['reported_usd_claude_code_estimate']:.2f} reported (Claude Code estimate, not billing); {spend['attempts_with_unknown_cost']} attempts with unknown cost"],
    )
    text = set_row(
        text,
        "Extractor/report code revision and exact reproduction commands",
        [f"analysis {ANALYSIS_VERSION}; `extract.py <study dir>` then `report.py runs.csv --study <study.toml> --out-dir <dir>`"],
    )
    text = set_row(text, "Raw transcripts, usage records, evaluator artifacts", ["private, outside the repository (per-attempt directories under the runs directory); not published"])

    # Task and run accounting.
    categories = sorted({(r["project"], r["category"]) for r in rows})
    lines = []
    for project, category in categories:
        tasks = sorted({r["task_id"] for r in rows if r["project"] == project and r["category"] == category})
        sched = " / ".join(f"{a}: {len(tasks) * study['trials']}" for a in summary["arms"])
        accd = " / ".join(
            f"{a}: {len({r['run_id'] for r in rows if r['task_id'] in tasks and r['config'] == a and r['analysis_inclusion'] == 'included'})}"
            for a in summary["arms"]
        )
        lines.append(f"| {project} | {category} | {len(tasks)} | {study['trials']} | {sched} | {accd} |")
    text = replace_once(text, "| NOT SET | — | — | — | — | — |", "\n".join(lines) if lines else "| no runs | — | — | — | — | — |")

    def outcome_cells(key):
        return ["—", str(acc["B"][key]) if "B" in acc else "—", str(acc["C"][key]) if "C" in acc else "—", "—"]

    for label, key in (
        ("Successful tasks", "successful"),
        ("Evaluator failures", "evaluator_failures"),
        ("Time/token/tool-call limit reached", "limit_reached"),
        ("Agent crash / invalid final answer", "crash_or_invalid_answer"),
        ("Infrastructure failures", "infrastructure_failures"),
        ("Rerun attempts", "rerun_attempts"),
    ):
        text = set_row(text, label, outcome_cells(key))
    text = set_row(
        text,
        "Missing / contaminated runs",
        ["—"] + [f"{acc[a]['missing']} / {acc[a]['contaminated']}" if a in acc else "—" for a in ("B", "C")] + ["—"],
    )

    # Primary results: paired task-weighted means (tasks with a value in both arms).
    for label, metric, fmt_value, compare in (
        (
            "Mean total input tokens / run",
            "input_tokens_total",
            f_num,
            f"Ratio: {f_num(tok.get('ratio_c_over_b'), 3)}; reduction: {f_num(tok.get('reduction_pct'), 1)}%",
        ),
        ("Task success rate", "success", f_rate, f"Difference: {f_num(succ.get('difference_pp'), 1)} percentage points"),
        ("Mean end-to-end wall time / run", "wall_clock_seconds", lambda v: f_num(v, 1, " s"), f"Change: {f_pct(wall.get('change_pct'))}"),
        ("Mean tool calls / run", "tool_calls_total", lambda v: f_num(v, 1), f"Change: {f_pct(comp.get('tool_calls_total', {}).get('change_pct'))}"),
        ("Mean output tokens / run", "output_tokens", f_num, f"Change: {f_pct(comp.get('output_tokens', {}).get('change_pct'))}"),
    ):
        paired = means.get(metric, {}).get("paired", {})
        text = set_row(text, label, ["—", fmt_value(paired.get("B")), fmt_value(paired.get("C")), compare, "not computed (pilot)"])
    text = set_row(text, "Efficiency point estimate", ["C/B ≤ 0.70", f"C/B = {f_num(tok.get('ratio_c_over_b'), 3)} (pilot)", "NOT EVALUATED (pilot)"])
    text = set_row(text, "Evidence of reduction", ["One-sided 95% upper bound on C/B < 1.00", "not computed (pilot)", "NOT EVALUATED (pilot)"])
    text = set_row(
        text,
        "Success non-inferiority",
        ["One-sided 95% lower bound on C−B > −5 percentage points", f"difference {f_num(succ.get('difference_pp'), 1)} pp; bound not computed", "NOT EVALUATED (pilot)"],
    )
    integrity = f"{sum(a['missing'] for a in acc.values())} missing, {sum(a['contaminated'] for a in acc.values())} contaminated, {sum(a['isolation_violations'] for a in acc.values())} with isolation notes"
    text = set_row(text, "Study integrity", ["Valid accounting; no unresolved snapshot-consistency bug", integrity, "NOT EVALUATED (pilot)"])

    # Per-task table.
    per = summary["per_task"]
    task_lines = []
    for task in sorted(per["input_tokens_total"]):
        meta = next(r for r in rows if r["task_id"] == task)
        tb = per["input_tokens_total"][task].get("B", {}).get("mean")
        tc = per["input_tokens_total"][task].get("C", {}).get("mean")
        red, _ = ratio(tc, tb)
        wb = per["wall_clock_seconds"][task].get("B", {}).get("mean")
        wc = per["wall_clock_seconds"][task].get("C", {}).get("mean")
        wr, _ = ratio(wc, wb)
        task_lines.append(
            "| "
            + " | ".join(
                [
                    task,
                    f"{meta['project']} / {meta['category']}",
                    f_rate(per["success"][task].get("B", {}).get("mean")),
                    f_rate(per["success"][task].get("C", {}).get("mean")),
                    f_num(tb),
                    f_num(tc),
                    f_num(None if red is None else (1 - red) * 100, 1, "%"),
                    f_pct(None if wr is None else (wr - 1) * 100),
                ]
            )
            + " |"
        )
    task_block = "\n".join(task_lines) if task_lines else "| no runs | — | — | — | — | — | — | — |"
    task_block += "\n\nComplete per-task/arm table: [per-task.csv](per-task.csv). All estimates and accounting: [summary.json](summary.json)."
    text = replace_once(text, "| NOT RUN | — | — | — | — | — | — | — |", task_block)

    ad = summary["adoption_c"] or {}
    if ad:
        text = set_row(text, "Runs invoking Rivet", [f"{ad['runs_invoking_rivet']} / {ad['eligible_runs']}", "Fraction of all eligible C runs"])
        cmds = ad["invocations_by_command"]
        text = set_row(
            text,
            "symbol / refs / context invocations",
            [f"{cmds.get('symbol', 0)} / {cmds.get('refs', 0)} / {cmds.get('context', 0)} (other: {sum(v for k, v in cmds.items() if k not in ('symbol', 'refs', 'context'))})", "Command traces"],
        )
        errs = ", ".join(f"exit {k}: {v}" for k, v in ad["errors_by_exit"].items()) or "none"
        text = set_row(text, "Errors by exit code", [errs, "Heuristic attribution from harness tool results"])
        text = set_row(text, "Text-tool fallbacks", [f"{ad['text_fallbacks']} over {ad['runs_with_known_fallbacks']} runs", "Heuristic next-two-tool-calls rule, TASK-FORMAT.md"])
        text = set_row(text, "Snapshot-consistency incidents", ["unavailable", "No extractor yet"])
        text = set_row(text, "Partial-coverage responses", ["unavailable", "Coverage metadata not extracted"])
    return text


def f_bound(value, digits: int) -> str:
    if value is None:
        return "unavailable"
    if math.isinf(value):
        return "+inf" if value > 0 else "-inf"
    return f"{value:.{digits}f}"


def f_pp(value) -> str:
    return "unavailable" if value is None else f"{value:+.1f}"


def render_confirmatory(summary: dict, rows: list[dict], study: dict, template_text: str, template_dir: str, out_dir: str) -> str:
    """report.md of a confirmatory study. `rows` are the analysed rows
    (rows of excluded blocks marked excluded)."""
    text = rewrite_links(template_text, template_dir, out_dir)
    conf = study["confirmatory"]
    prim, boot, gates = summary["primary"], summary["bootstrap"] or {}, summary["gates"]
    outcome, dry = summary["outcome"], summary["dry_run"]
    reasons = summary["validity"]["reasons"]
    reason_names = ", ".join(r["reason"] for r in reasons)
    blocks = summary["blocks"]
    acc = summary["accounting"]
    sec = summary["secondary"]
    comp = sec["comparisons"]
    wall = comp.get("wall_clock_seconds", {})
    runbook = os.path.relpath(os.path.join(studylib.REPO_ROOT, "docs", "BENCHMARK.md"), out_dir).replace(os.sep, "/")
    dates = sorted(r["started_at"][:10] for r in rows if r["started_at"] not in MISSING)
    c_rows = [r for r in rows if r["config"] == "C"]
    ub, lb = boot.get("ratio_upper_bound"), boot.get("success_diff_lower_bound_pp")
    if dry:
        gate_outcome = "NOT EVALUATED (dry run)"
    elif reasons:
        gate_outcome = "NOT EVALUATED (invalid study)"
    else:
        gate_outcome = None

    def uniq(column, source=rows):
        values = sorted({r[column] for r in source if r[column] not in MISSING})
        return ", ".join(values) if values else "unavailable"

    text = replace_once(
        text,
        "# Rivet Benchmark Report — STUDY_ID",
        f"# Rivet Benchmark Report — {study['study_id']}" + (" (DRY RUN)" if dry else ""),
    )
    status_line = next(line for line in text.splitlines() if line.startswith("> **Status: NOT RUN.**"))
    generated = (
        f"Numerical tables and gate outcomes are generated by `benchmark/runner/report.py` (analysis {CONFIRMATORY_ANALYSIS_VERSION}) "
        f"from the private `runs.csv` and must not be edited by hand. Follow the [benchmark runbook]({runbook})."
    )
    if dry:
        status = (
            "> **Status: DRY RUN.** At least one run replaced the harness through `RIVET_PILOT_CLAUDE_BIN`. "
            "This rehearses the confirmatory pipeline and is not a result: the outcome is `invalid` and no gate is evaluated. " + generated
        )
    else:
        status = (
            f"> **Status: CONFIRMATORY — outcome `{outcome}`.** Gates are evaluated only as preregistered in "
            f"`{conf['preregistration']}`. " + generated
        )
    text = replace_once(text, status_line, status)
    statements = {
        "pass": "Pending review. Both preregistered gates passed; write the scoped statement by hand from the gate rows below and link this report.",
        "fail_quality": "None. The success non-inferiority gate failed.",
        "inconclusive": "None. The efficiency gate was not met, so the result is inconclusive.",
        "invalid": f"None. The study is invalid ({reason_names}).",
    }
    text = replace_once(
        text,
        "**Supported public statement:** None. No benchmark has run.",
        f"**Supported public statement:** {statements[outcome]}",
    )
    scheduled = sum(a["scheduled"] for a in acc.values())
    accounted = sum(a["accounted"] for a in acc.values())
    failed = sum(a["accounted"] - a["successful"] for a in acc.values())
    languages = sorted({v["language"] for v in summary["per_task_primary"].values() if v["language"]})
    text = set_row(
        text,
        "Study type / run dates",
        [("confirmatory (DRY RUN)" if dry else "confirmatory") + (f"; {dates[0]} to {dates[-1]}" if dates else "; no runs")],
    )
    text = set_row(
        text,
        "Rivet version, commit, binary hash",
        [f"{uniq('tool_version', c_rows)}; frozen binary sha256 `{conf['rivet_binary_sha256'][:16]}`; commit in the preregistration"],
    )
    text = set_row(text, "Model identifier / harness version", [f"{uniq('model_id')} / Claude Code {uniq('harness_version')}"])
    text = set_row(
        text,
        "Repositories, languages, tasks, trials",
        [f"{uniq('project')}; {', '.join(languages) or 'unavailable'}; {len(study['tasks'])} tasks; {study['trials']} trial(s) per arm"],
    )
    text = set_row(
        text,
        "Scheduled / completed / failed / invalidated runs",
        [
            f"{scheduled} / {accounted} / {failed} / {scheduled - accounted} "
            f"({blocks['excluded']} of {blocks['scheduled']} blocks excluded from both arms; plus {sum(a['rerun_attempts'] for a in acc.values())} rerun attempts)"
        ],
    )
    reduction = prim["reduction_pct"]
    text = set_row(
        text,
        "Mean input-token reduction",
        [
            f"{f_num(reduction, 1, '%')} (C/B = {f_num(prim['ratio_c_over_b'], 3)} over {len(prim['tasks'])} tasks; one-sided 95% upper bound on C/B {f_bound(ub, 3)})"
            if reduction is not None
            else f"unavailable ({prim['ratio_undefined_reason']})"
        ],
    )
    text = set_row(
        text,
        "Task success difference (C − B)",
        [f"{f_pp(prim['success_diff_pp'])} percentage points over {len(prim['tasks'])} tasks; one-sided 95% lower bound {f_pp(lb)} pp"],
    )
    text = set_row(text, "End-to-end wall-time change", [f"{f_pct(wall.get('change_pct'))} (secondary, point estimate)"])
    verdict = VERDICTS[outcome] + (f" ({reason_names})" if reasons else "")
    text = set_row(text, "Verdict", [("DRY RUN — " if dry else "") + verdict])

    spend = summary["spend"]
    text = set_row(
        text,
        "Frozen preregistration and revision",
        [f"`{conf['preregistration']}` sha256 `{conf['preregistration_sha256'][:16]}`; the runner refuses to start unless it is committed and unmodified at HEAD"],
    )
    text = set_row(text, "Study manifest, artifact checksums", [f"`benchmark/studies/{study['study_id']}/study.toml` sha256 `{study['sha256'][:16]}`; runs.csv sha256 `{summary['inputs']['runs_csv_sha256'][:16]}`"])
    text = set_row(text, "Prompts, setup/check versions, task hashes", [f"tasks sha256 `{conf['tasks_sha256'][:16]}` (TASK-FORMAT.md \"tasks_sha256\")"])
    text = set_row(text, "Instructions, snippet/help hashes", [f"config-B prompt appended in both arms; snippet sha256 {uniq('snippet_hash', c_rows)}"])
    text = set_row(text, "Model settings, context limits, cache policy", [f"settings hash {uniq('settings_hash')}; harness default cache policy, identical in both arms"])
    text = set_row(
        text,
        "Run ordering / randomization seed",
        [f"seed {study['seed']}; arm order randomized within each task/trial block; bootstrap seed {conf['analysis_seed']}"],
    )
    limits = []
    for column, label in (("max_budget_usd", "USD"), ("max_turns", "turns"), ("wall_seconds_limit", "s wall")):
        values = sorted({number(r[column]) for r in rows if number(r[column]) is not None})
        if values:
            limits.append(f"{f_num(values[0], 2 if column == 'max_budget_usd' else 0)}–{f_num(values[-1], 2 if column == 'max_budget_usd' else 0)} {label}")
    text = set_row(text, "Per-run token / time / tool-call limits", ["; ".join(limits) + " (per task, equal across arms); no tool-call limit" if limits else "unavailable"])
    text = set_row(
        text,
        "Study run/spend cap and actual spend",
        [f"${spend['cap_usd']:.2f} cap; ${spend['reported_usd_claude_code_estimate']:.2f} reported (Claude Code estimate, not billing); {spend['attempts_with_unknown_cost']} attempts with unknown cost"],
    )
    text = set_row(
        text,
        "Extractor/report code revision and exact reproduction commands",
        [
            f"analysis {CONFIRMATORY_ANALYSIS_VERSION} (manifest: {conf['analysis_version']}); "
            "`extract.py <study dir>` then `report.py runs.csv --study <study.toml> --out-dir <dir>`"
        ],
    )
    text = set_row(text, "Raw transcripts, usage records, evaluator artifacts", ["private, outside the repository (per-attempt directories under the runs directory); not published"])

    # Task and run accounting: runs of excluded blocks are not accounted.
    per_task_meta = summary["per_task_primary"]
    lines = []
    for project, category in sorted({(r["project"], r["category"]) for r in rows}):
        tasks = sorted({r["task_id"] for r in rows if r["project"] == project and r["category"] == category})
        language = per_task_meta[tasks[0]]["language"]
        sched = " / ".join(f"{a}: {len(tasks) * study['trials']}" for a in summary["arms"])
        accd = " / ".join(
            f"{a}: {len({r['run_id'] for r in rows if r['task_id'] in tasks and r['config'] == a and r['analysis_inclusion'] == 'included'})}"
            for a in summary["arms"]
        )
        lines.append(f"| {language} / {project} | {category} | {len(tasks)} | {study['trials']} | {sched} | {accd} |")
    silent = sorted(t for t, v in per_task_meta.items() if v["project"] is None)
    if silent:
        sched = " / ".join(f"{a}: {len(silent) * study['trials']}" for a in summary["arms"])
        lines.append(f"| no runs | — | {len(silent)} | {study['trials']} | {sched} | " + " / ".join(f"{a}: 0" for a in summary["arms"]) + " |")
    text = replace_once(text, "| NOT SET | — | — | — | — | — |", "\n".join(lines) if lines else "| no runs | — | — | — | — | — |")

    def outcome_cells(key):
        return ["—", str(acc["B"][key]), str(acc["C"][key]), "—"]

    for label, key in (
        ("Successful tasks", "successful"),
        ("Evaluator failures", "evaluator_failures"),
        ("Time/token/tool-call limit reached", "limit_reached"),
        ("Agent crash / invalid final answer", "crash_or_invalid_answer"),
        ("Infrastructure failures", "infrastructure_failures"),
        ("Rerun attempts", "rerun_attempts"),
    ):
        text = set_row(text, label, outcome_cells(key))
    text = set_row(
        text,
        "Missing / contaminated runs",
        ["—"] + [f"{acc[a]['missing']} / {acc[a]['contaminated']}" for a in ("B", "C")] + ["—"],
    )

    # Primary results (equal task weighting over included blocks) and the
    # secondary point estimates.
    means = sec["arm_means"]
    text = set_row(
        text,
        "Mean total input tokens / run",
        [
            "—",
            f_num(prim["b_mean_input_tokens"]),
            f_num(prim["c_mean_input_tokens"]),
            f"Ratio: {f_num(prim['ratio_c_over_b'], 3)}; reduction: {f_num(prim['reduction_pct'], 1)}%",
            f"two-sided not computed; one-sided 95% upper bound on C/B: {f_bound(ub, 3)}",
        ],
    )
    text = set_row(
        text,
        "Task success rate",
        [
            "—",
            f_rate(prim["b_success_rate"]),
            f_rate(prim["c_success_rate"]),
            f"Difference: {f_num(prim['success_diff_pp'], 1)} percentage points",
            f"two-sided not computed; one-sided 95% lower bound on C − B: {f_pp(lb)} pp",
        ],
    )
    for label, metric, fmt_value in (
        ("Mean end-to-end wall time / run", "wall_clock_seconds", lambda v: f_num(v, 1, " s")),
        ("Mean tool calls / run", "tool_calls_total", lambda v: f_num(v, 1)),
        ("Mean output tokens / run", "output_tokens", f_num),
    ):
        paired = means.get(metric, {}).get("paired", {})
        text = set_row(
            text,
            label,
            ["—", fmt_value(paired.get("B")), fmt_value(paired.get("C")), f"Change: {f_pct(comp.get(metric, {}).get('change_pct'))}", "secondary; point estimate only"],
        )

    def gate_cell(passed):
        return gate_outcome or ("PASS" if passed else "FAIL")

    text = set_row(
        text,
        "Efficiency point estimate",
        ["C/B ≤ 0.70 (not a gate of this study)", f"C/B = {f_num(prim['ratio_c_over_b'], 3)}", f"NOT A GATE (efficiency_gate = {conf['efficiency_gate']})"],
    )
    text = set_row(
        text,
        "Evidence of reduction",
        ["One-sided 95% upper bound on C/B < 1.00", f"upper bound {f_bound(ub, 3)}", gate_cell(gates["efficiency_pass"])],
    )
    text = set_row(
        text,
        "Success non-inferiority",
        [
            "One-sided 95% lower bound on C−B > −5 percentage points",
            f"difference {f_pp(prim['success_diff_pp'])} pp; lower bound {f_pp(lb)} pp",
            gate_cell(gates["quality_pass"]),
        ],
    )
    integrity = (
        f"{blocks['excluded']} of {blocks['scheduled']} blocks excluded (limit {repr(conf['max_excluded_block_fraction'])}); "
        f"fewest included blocks per task {min(blocks['included_by_task'].values())} (minimum {blocks['min_complete_trials_per_task']}); "
        "snapshot consistency unchecked (no extractor)"
    )
    text = set_row(
        text,
        "Study integrity",
        ["Valid accounting; no unresolved snapshot-consistency bug", integrity, f"INVALID ({reason_names})" if reasons else "VALID (accounting)"],
    )

    notes = [
        f"**Gates evaluated** (`efficiency_gate = \"{conf['efficiency_gate']}\"`): efficiency passes when the one-sided 95% upper bound on C/B is below 1.00; "
        "success non-inferiority passes when the one-sided 95% lower bound on C − B is above −5 percentage points. "
        f"The template's C/B ≤ 0.70 point-estimate row is not a gate of this study. Outcome: `{outcome}`."
    ]
    if boot:
        strata = "; ".join(f"{name}: {len(tasks)} tasks" for name, tasks in boot["strata"].items())
        notes.append(
            f"**Bootstrap:** paired cluster bootstrap over tasks within language strata ({strata}), {boot['replicates']} replicates, seed {boot['seed']}; "
            f"the upper bound is the sorted C/B replicate at index {boot['ratio_upper_bound_index']} and the lower bound the sorted C − B replicate at index "
            f"{boot['success_diff_lower_bound_index']} (TASK-FORMAT.md \"Paired cluster bootstrap\"). "
            f"Replicates with a zero B denominator, counted as C/B = +inf: {boot['zero_denominator_replicates']}."
        )
    else:
        notes.append("**Bootstrap:** not computed; no task has an included block.")
    if reasons:
        notes.append("**Validity: invalid.** " + " ".join(f"`{r['reason']}`: {r['detail']}." for r in reasons))
    else:
        notes.append("**Validity:** valid; no validity rule was triggered.")
    excl = summary["exclusions"]
    if excl:
        listed = ", ".join(
            f"`{e['block_id']}` (" + "; ".join(f"{arm}: {why}" for arm, why in e["reasons"].items()) + ")" for e in excl
        )
        notes.append(f"**Excluded blocks** (removed from both arms; {len(excl)} of {blocks['scheduled']} scheduled): {listed}.")
    else:
        notes.append(f"**Excluded blocks:** none of {blocks['scheduled']} scheduled.")
    succ_only = sec["successful_runs_only_input_tokens"]
    notes.append(
        "**Secondary results** (point estimates only; no interval, no gate): "
        f"successful-run-only input-token C/B {f_num(succ_only['ratio_c_over_b'], 3)} over {succ_only['paired_tasks']} tasks with a passing run in both arms; "
        + "; ".join(
            f"{label} C/B {f_num(comp.get(metric, {}).get('ratio_c_over_b'), 3)} over {comp.get(metric, {}).get('paired_tasks', 0)} tasks"
            for label, metric in (
                ("cost (Claude Code estimate)", "cost_usd"),
                ("tool calls", "tool_calls_total"),
                ("wall clock", "wall_clock_seconds"),
                ("output tokens", "output_tokens"),
            )
        )
        + "."
    )
    text = replace_once(text, "Use the actual frozen thresholds if different", "\n\n".join(notes) + "\n\nUse the actual frozen thresholds if different")

    # Per-task table and the secondary category/language aggregates.
    per = summary["per_task"]
    task_lines = []
    for task in sorted(per_task_meta):
        meta = per_task_meta[task]
        tb = (meta["B"] or {}).get("tokens")
        tc = (meta["C"] or {}).get("tokens")
        red, _ = ratio(tc, tb)
        wb = per.get("wall_clock_seconds", {}).get(task, {}).get("B", {}).get("mean")
        wc = per.get("wall_clock_seconds", {}).get(task, {}).get("C", {}).get("mean")
        wr, _ = ratio(wc, wb)
        task_lines.append(
            "| "
            + " | ".join(
                [
                    task,
                    f"{meta['language'] or 'unavailable'} / {meta['category'] or 'unavailable'}",
                    f_rate((meta["B"] or {}).get("success")),
                    f_rate((meta["C"] or {}).get("success")),
                    f_num(tb),
                    f_num(tc),
                    f_num(None if red is None else (1 - red) * 100, 1, "%"),
                    f_pct(None if wr is None else (wr - 1) * 100),
                ]
            )
            + " |"
        )
    task_block = "\n".join(task_lines) if task_lines else "| no runs | — | — | — | — | — | — | — |"
    task_block += (
        "\n\nPer-task values use each task's included blocks only. Complete per-task/arm table: [per-task.csv](per-task.csv). "
        "All estimates, bounds, exclusions and accounting: [summary.json](summary.json)."
    )
    for title, key in (("category", "by_category"), ("language stratum", "by_language")):
        task_block += (
            f"\n\n**Secondary, by {title}** (equal task weighting over included blocks; point estimates only):\n\n"
            "| Group | Tasks | B success | C success | B mean input | C mean input | C/B |\n|---|---|---|---|---|---|---|"
        )
        for name, g in sec[key].items():
            task_block += (
                f"\n| {name} | {g['tasks']} | {f_rate(g['b_success_rate'])} | {f_rate(g['c_success_rate'])} | "
                f"{f_num(g['b_mean_input_tokens'])} | {f_num(g['c_mean_input_tokens'])} | {f_num(g['ratio_c_over_b'], 3)} |"
            )
        if not sec[key]:
            task_block += "\n| none | — | — | — | — | — | — |"
    text = replace_once(text, "| NOT RUN | — | — | — | — | — | — | — |", task_block)

    ad = summary["adoption_c"]
    text = set_row(text, "Runs invoking Rivet", [f"{ad['runs_invoking_rivet']} / {ad['eligible_runs']}", "Fraction of the C runs in included blocks"])
    cmds = ad["invocations_by_command"]
    text = set_row(
        text,
        "symbol / refs / context invocations",
        [f"{cmds.get('symbol', 0)} / {cmds.get('refs', 0)} / {cmds.get('context', 0)} (other: {sum(v for k, v in cmds.items() if k not in ('symbol', 'refs', 'context'))})", "Command traces"],
    )
    errs = ", ".join(f"exit {k}: {v}" for k, v in ad["errors_by_exit"].items()) or "none"
    text = set_row(text, "Errors by exit code", [errs, "Heuristic attribution from harness tool results"])
    text = set_row(text, "Text-tool fallbacks", [f"{ad['text_fallbacks']} over {ad['runs_with_known_fallbacks']} runs", "Heuristic next-two-tool-calls rule, TASK-FORMAT.md"])
    text = set_row(text, "Snapshot-consistency incidents", ["unavailable", "No extractor yet"])
    text = set_row(text, "Partial-coverage responses", ["unavailable", "Coverage metadata not extracted"])
    return text


def per_task_csv(summary: dict, rows: list[dict]) -> str:
    buffer = io.StringIO()
    writer = csv.writer(buffer, lineterminator="\n")
    header = ["task_id", "project", "category", "config", "metric", "task_mean", "runs_with_value", "runs_missing"]
    writer.writerow(header)
    for task in sorted(summary["per_task"]["input_tokens_total"]):
        meta = next(r for r in rows if r["task_id"] == task)
        for arm in summary["arms"]:
            for metric in METRICS:
                cell = summary["per_task"][metric][task][arm]
                writer.writerow(
                    [task, meta["project"], meta["category"], arm, metric, "unavailable" if cell["mean"] is None else f"{cell['mean']:.6f}", cell["n"], cell["missing"]]
                )
    return buffer.getvalue()


def generate(runs_csv: str, study_path: str, out_dir: str, template: str = TEMPLATE, corpus_manifest: str = studylib.CORPUS_TOML) -> dict:
    study = studylib.load_study(study_path)
    rows, csv_sha = read_rows(runs_csv)
    unknown = sorted({r["task_id"] for r in rows} - set(study["tasks"]))
    if unknown:
        raise ReportError(f"runs.csv has tasks not in the study: {unknown}")
    with open(template, encoding="utf-8") as handle:
        template_text = handle.read()
    if study["kind"] == "confirmatory":
        summary, analysed = compute_confirmatory(rows, study, csv_sha, corpus_manifest)
        os.makedirs(out_dir, exist_ok=True)
        report = render_confirmatory(summary, analysed, study, template_text, os.path.dirname(os.path.abspath(template)), os.path.abspath(out_dir))
        outputs = {
            "report.md": report,
            "summary.json": json.dumps(strict_json(summary), indent=2, sort_keys=True, allow_nan=False) + "\n",
            "per-task.csv": per_task_csv(summary, analysed),
        }
    else:
        summary = compute(rows, study, csv_sha)
        os.makedirs(out_dir, exist_ok=True)
        report = render(summary, rows, study, template_text, os.path.dirname(os.path.abspath(template)), os.path.abspath(out_dir))
        outputs = {
            "report.md": report,
            "summary.json": json.dumps(summary, indent=2, sort_keys=True) + "\n",
            "per-task.csv": per_task_csv(summary, rows),
        }
    for name, content in outputs.items():
        with open(os.path.join(out_dir, name), "w", encoding="utf-8", newline="") as handle:
            handle.write(content)
    return summary


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="pilot and confirmatory aggregates and report.md (T38c, T48a)")
    parser.add_argument("runs_csv")
    parser.add_argument("--study", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--template", default=TEMPLATE)
    parser.add_argument("--corpus-manifest", default=studylib.CORPUS_TOML, help="confirmatory: the task projects' languages (bootstrap strata)")
    args = parser.parse_args(argv)
    try:
        generate(args.runs_csv, args.study, args.out_dir, args.template, args.corpus_manifest)
    except (ReportError, studylib.StudyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"wrote {os.path.join(args.out_dir, 'report.md')}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
