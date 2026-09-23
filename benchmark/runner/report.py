#!/usr/bin/env python3
"""Task-weighted pilot aggregates and the Markdown report (T38c).

    python3 benchmark/runner/report.py RUNS_CSV --study benchmark/studies/<id>/study.toml \
        --out-dir benchmark/results/<id>

Writes `report.md` (from benchmark/REPORT-TEMPLATE.md), `summary.json` and
`per-task.csv` into the output directory. The outputs name tasks only by
opaque ID, category and project, and contain only aggregates; no transcript
text reaches them. Every confirmatory gate is `NOT EVALUATED`: BENCHMARK.md
forbids treating a pilot as the confirmatory study. Identical inputs give
byte-identical outputs. Standard library only.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import os
import re
import sys

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


def task_means(rows: list[dict], arms: list[str]) -> dict:
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
                out[metric][task][arm] = {"mean": mean(present), "n": len(present), "missing": len(values) - len(present)}
    return out


def arm_summary(per_task: dict, arms: list[str]) -> dict:
    """Task-weighted means: every task with a value counts once. `paired`
    restricts to tasks with a value in every arm, which is what the C/B
    comparison uses."""
    out = {}
    for metric, tasks in per_task.items():
        entry = {"all_tasks": {}, "paired": {}, "paired_tasks": []}
        for arm in arms:
            values = [cell[arm]["mean"] for cell in tasks.values() if cell[arm]["mean"] is not None]
            entry["all_tasks"][arm] = {"mean": mean(values), "tasks": len(values)}
        paired = sorted(t for t, cell in tasks.items() if all(cell[a]["mean"] is not None for a in arms))
        entry["paired_tasks"] = paired
        for arm in arms:
            entry["paired"][arm] = mean([tasks[t][arm]["mean"] for t in paired])
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


def generate(runs_csv: str, study_path: str, out_dir: str, template: str = TEMPLATE) -> dict:
    study = studylib.load_study(study_path)
    rows, csv_sha = read_rows(runs_csv)
    unknown = sorted({r["task_id"] for r in rows} - set(study["tasks"]))
    if unknown:
        raise ReportError(f"runs.csv has tasks not in the study: {unknown}")
    summary = compute(rows, study, csv_sha)
    with open(template, encoding="utf-8") as handle:
        template_text = handle.read()
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
    parser = argparse.ArgumentParser(description="pilot aggregates and report.md (T38c)")
    parser.add_argument("runs_csv")
    parser.add_argument("--study", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--template", default=TEMPLATE)
    args = parser.parse_args(argv)
    try:
        generate(args.runs_csv, args.study, args.out_dir, args.template)
    except (ReportError, studylib.StudyError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"wrote {os.path.join(args.out_dir, 'report.md')}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
