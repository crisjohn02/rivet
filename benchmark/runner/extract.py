#!/usr/bin/env python3
"""Per-attempt CSV extraction (T38b).

    python3 benchmark/runner/extract.py "$RIVET_PILOT_RUNS_DIR/<study-id>" [--out runs.csv]

Reads every `runs/<attempt-id>/record.json` and its `transcript.jsonl`,
verifies the transcript still has the hash its record names (so each row
traces to an unaltered original), re-derives the transcript metrics, applies
the frozen retry and contamination inclusion rules, and writes one row per
attempt to `runs.csv` (default: inside the study directory, which is
private). Columns are documented in TASK-FORMAT.md "runs.csv". A value the
evidence does not support is `unavailable`; a column that does not apply to
the arm is `not_applicable`. Nothing is zero-filled. Standard library only.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import os
import sys

sys.dont_write_bytecode = True  # keep __pycache__ out of the repository
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import study as studylib  # noqa: E402
from transcript import Transcript, contamination_scan, rivet_metrics  # noqa: E402

UNAVAILABLE = "unavailable"
NOT_APPLICABLE = "not_applicable"

COLUMNS = [
    "run_id",
    "attempt_id",
    "block_id",
    "task_id",
    "project",
    "category",
    "check",
    "repository_commit",
    "config",
    "trial",
    "attempt",
    "execution_order",
    "analysis_inclusion",
    "inclusion_reason",
    "model_id",
    "models_observed",
    "harness_version",
    "settings_hash",
    "tool_policy_hash",
    "tool_version",
    "snippet_hash",
    "max_budget_usd",
    "max_turns",
    "wall_seconds_limit",
    "pass",
    "answer_status",
    "precision",
    "recall",
    "f1",
    "termination_reason",
    "infrastructure_failure",
    "infrastructure_reason",
    "contaminated",
    "contamination_evidence",
    "isolation_violations",
    "usage_source",
    "input_tokens_total",
    "input_tokens_cached",
    "input_tokens_uncached",
    "input_tokens_cache_creation",
    "output_tokens",
    "num_turns",
    "tool_calls_total",
    "tool_calls_by_name",
    "permission_denials",
    "edits",
    "failed_edits",
    "workspace_modified",
    "wall_clock_seconds",
    "harness_duration_seconds",
    "indexing_seconds",
    "cost_usd_claude_code_estimate",
    "rivet_invocations_by_command",
    "rivet_errors_by_exit",
    "refresh_mode",
    "rivet_to_text_fallbacks",
    "snapshot_mismatches",
    "started_at",
    "transcript_path",
    "transcript_sha256",
]


class ExtractError(RuntimeError):
    pass


def fmt(value) -> str:
    if value is None:
        return UNAVAILABLE
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, float):
        return f"{value:.6f}"
    if isinstance(value, (dict, list)):
        return json.dumps(value, sort_keys=True, separators=(",", ":"))
    return str(value)


def inclusion(records: list[dict]) -> dict[str, tuple[bool, str]]:
    """The frozen inclusion rules (TASK-FORMAT.md "Retry and inclusion").

    Per task/trial block: when any first attempt is an infrastructure
    failure, the whole block's first attempts are superseded by its second
    attempts. Infrastructure failures are never task outcomes; contaminated
    B runs are excluded from the primary analysis and reported.
    """
    by_block: dict[str, dict[int, list[dict]]] = {}
    for record in records:
        by_block.setdefault(record["block_id"], {}).setdefault(int(record["attempt"]), []).append(record)
    out = {}
    for _block, attempts in sorted(by_block.items()):
        first = attempts.get(1, [])
        second = attempts.get(2, [])
        retried = any(r.get("infrastructure_failure") for r in first)
        for record in first:
            aid = record["attempt_id"]
            if retried and second:
                out[aid] = (False, "superseded_by_block_retry")
            elif record.get("infrastructure_failure"):
                out[aid] = (False, "infrastructure_failure_retry_missing")
            elif retried:
                out[aid] = (True, "block_retry_missing")
            else:
                out[aid] = (True, "first_attempt")
        for record in second:
            aid = record["attempt_id"]
            if record.get("infrastructure_failure"):
                out[aid] = (False, "infrastructure_failure_after_retry")
            else:
                out[aid] = (True, "block_retry")
        for n, extra in attempts.items():
            if n not in (1, 2):
                for record in extra:
                    out[record["attempt_id"]] = (False, "unexpected_attempt_number")
    for record in records:
        included, reason = out[record["attempt_id"]]
        if included and record.get("contaminated"):
            out[record["attempt_id"]] = (False, "contaminated")
    return out


def row_for(study_dir: str, record: dict, included: bool, reason: str) -> dict:
    run_dir = os.path.join(study_dir, "runs", record["attempt_id"])
    transcript_path = os.path.join(run_dir, "transcript.jsonl")
    if not os.path.exists(transcript_path):
        raise ExtractError(f"{record['attempt_id']}: transcript.jsonl is missing")
    digest = studylib.sha256_file(transcript_path)
    if record.get("transcript_sha256") and digest != record["transcript_sha256"]:
        raise ExtractError(f"{record['attempt_id']}: transcript hash differs from its record; the original was altered")
    t = Transcript.from_file(transcript_path)
    arm = record["arm"]
    usage = t.usage()
    base, cached, created = usage["input_tokens"], usage["cache_read_input_tokens"], usage["cache_creation_input_tokens"]
    total = base + cached + created if None not in (base, cached, created) else None
    uncached = base + created if None not in (base, created) else None
    evaluation = record.get("evaluation") or {}
    limits = record.get("limits") or {}
    result = t.result or {}
    edits, failed_edits = t.edits()
    duration = result.get("duration_ms")
    turns = result.get("num_turns") if isinstance(result.get("num_turns"), int) else (t.assistant_turns() if t.events else None)
    row = {
        "run_id": record.get("run_id"),
        "attempt_id": record["attempt_id"],
        "block_id": record["block_id"],
        "task_id": record["task_id"],
        "project": record.get("project"),
        "category": record.get("category"),
        "check": record.get("check"),
        "repository_commit": record.get("repository_commit"),
        "config": arm,
        "trial": record.get("trial"),
        "attempt": record.get("attempt"),
        "execution_order": record.get("execution_order"),
        "analysis_inclusion": "included" if included else "excluded",
        "inclusion_reason": reason,
        "model_id": record.get("model_id"),
        "models_observed": "|".join(t.models_observed()) or None,
        "harness_version": record.get("harness_version"),
        "settings_hash": record.get("settings_hash"),
        "tool_policy_hash": record.get("tool_policy_hash"),
        "tool_version": record.get("tool_version") if arm == "C" else NOT_APPLICABLE,
        "snippet_hash": record.get("snippet_hash") if arm == "C" else NOT_APPLICABLE,
        "max_budget_usd": limits.get("max_budget_usd"),
        "max_turns": limits.get("max_turns"),
        "wall_seconds_limit": limits.get("wall_seconds"),
        "pass": record.get("pass"),
        "answer_status": evaluation.get("answer_status"),
        "precision": evaluation.get("precision") if record.get("check") == "set_f1" else NOT_APPLICABLE,
        "recall": evaluation.get("recall") if record.get("check") == "set_f1" else NOT_APPLICABLE,
        "f1": evaluation.get("f1") if record.get("check") == "set_f1" else NOT_APPLICABLE,
        "termination_reason": record.get("termination_reason"),
        "infrastructure_failure": bool(record.get("infrastructure_failure")),
        "infrastructure_reason": record.get("infrastructure_reason") or NOT_APPLICABLE,
        "contaminated": record.get("contaminated") if arm == "B" else NOT_APPLICABLE,
        "contamination_evidence": (
            "|".join(sorted({e["kind"] for e in record.get("contamination_evidence") or []})) or "none"
        )
        if arm == "B"
        else NOT_APPLICABLE,
        "isolation_violations": "|".join(record.get("isolation_violations") or []) or "none",
        "usage_source": usage["usage_source"],
        "input_tokens_total": total,
        "input_tokens_cached": cached,
        "input_tokens_uncached": uncached,
        "input_tokens_cache_creation": created,
        "output_tokens": usage["output_tokens"],
        "num_turns": turns,
        "tool_calls_total": len(t.tool_uses) if t.events else None,
        "tool_calls_by_name": t.tool_calls_by_name() if t.events else None,
        "permission_denials": t.permission_denials(),
        "edits": edits if t.events else None,
        "failed_edits": failed_edits if t.events else None,
        "workspace_modified": record.get("workspace_modified"),
        "wall_clock_seconds": record.get("wall_clock_seconds"),
        "harness_duration_seconds": duration / 1000.0 if isinstance(duration, (int, float)) else None,
        # rivet indexes lazily inside the agent's first query and reports no
        # timing in query output, so indexing time is not separable.
        "indexing_seconds": None if arm == "C" else NOT_APPLICABLE,
        "cost_usd_claude_code_estimate": record.get("cost_usd"),
        "started_at": record.get("started_at"),
        "transcript_path": os.path.relpath(transcript_path, study_dir).replace(os.sep, "/"),
        "transcript_sha256": digest,
        # No extractor compares returned spans with stored hashes yet.
        "snapshot_mismatches": None if arm == "C" else NOT_APPLICABLE,
    }
    if arm == "C":
        metrics = rivet_metrics(t) if t.events else None
        row["rivet_invocations_by_command"] = metrics["rivet_invocations_by_command"] if metrics else None
        row["rivet_errors_by_exit"] = metrics["rivet_errors_by_exit"] if metrics else None
        row["refresh_mode"] = metrics["refresh_mode"] if metrics else None
        row["rivet_to_text_fallbacks"] = metrics["rivet_to_text_fallbacks"] if metrics else None
    else:
        for column in ("rivet_invocations_by_command", "rivet_errors_by_exit", "refresh_mode", "rivet_to_text_fallbacks"):
            row[column] = NOT_APPLICABLE
        # Re-scan independently of the runner so a record cannot hide it.
        if contamination_scan(t, record.get("workspace")) and not record.get("contaminated"):
            raise ExtractError(f"{record['attempt_id']}: transcript shows rivet use but the record is not flagged contaminated")
    return {column: fmt(row[column]) for column in COLUMNS}


def load_records(study_dir: str) -> list[dict]:
    runs = os.path.join(study_dir, "runs")
    if not os.path.isdir(runs):
        raise ExtractError(f"{runs} does not exist")
    records = []
    for name in sorted(os.listdir(runs)):
        path = os.path.join(runs, name, "record.json")
        if not os.path.exists(path):
            raise ExtractError(f"{name}: record.json is missing; run the runner to recover interrupted attempts")
        with open(path, encoding="utf-8") as handle:
            record = json.load(handle)
        if record.get("attempt_id") != name:
            raise ExtractError(f"{name}: record attempt_id {record.get('attempt_id')!r} does not match its directory")
        records.append(record)
    return records


def sort_key(record: dict):
    order = record.get("execution_order")
    return (order is None, order if isinstance(order, int) else 0, record["attempt_id"])


def extract(study_dir: str) -> str:
    """Returns the CSV text for a study directory."""
    records = sorted(load_records(study_dir), key=sort_key)
    rules = inclusion(records)
    buffer = io.StringIO()
    writer = csv.DictWriter(buffer, fieldnames=COLUMNS, lineterminator="\n")
    writer.writeheader()
    for record in records:
        included, reason = rules[record["attempt_id"]]
        writer.writerow(row_for(study_dir, record, included, reason))
    return buffer.getvalue()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="extract runs.csv from a pilot study directory (T38b)")
    parser.add_argument("study_dir")
    parser.add_argument("--out")
    args = parser.parse_args(argv)
    try:
        text = extract(args.study_dir)
    except ExtractError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    out = args.out or os.path.join(args.study_dir, "runs.csv")
    with open(out, "w", encoding="utf-8", newline="") as handle:
        handle.write(text)
    print(f"wrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
