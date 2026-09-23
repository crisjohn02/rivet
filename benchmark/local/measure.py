#!/usr/bin/env python3
"""T37: measure rivet itself on the pinned private corpus. No model, no agent.

Run through `benchmark/local/measure.sh`, which builds the release binary and
passes its path. Everything here uses the Python standard library only.

Privacy. The corpus is private and this repository is public. Every project is
measured in a scratch copy under a fresh temporary directory, never in
`$RIVET_CORPUS_DIR` itself, and the copy is deleted at the end. The results
file records only project names, pinned commits, counts, byte sizes, times,
and the generic query name chosen from `GENERIC_NAMES`. Canonical IDs, file
paths, source text, and matched lines are used in memory to drive the queries
and are never written out. `scan_results_for_leaks` checks the finished
results against every path and symbol name the run saw before writing.

Timing. Each timed sample is the wall-clock time of one `rivet` process,
measured with `time.perf_counter_ns` around `/usr/bin/time -l rivet ...`, so
it includes process start and the `time` wrapper (about a millisecond). Peak
memory is the `maximum resident set size` line `/usr/bin/time -l` prints.
The OS file cache is warm: every project is walked and indexed once, untimed,
before any timed run, and nothing purges the cache (that needs root).
"""

from __future__ import annotations

import datetime
import hashlib
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# Generic method names common to Laravel code. The common-name and high-fanout
# queries are chosen from this fixed list only, so the chosen name is safe to
# record. Order is the tie-break order.
GENERIC_NAMES = [
    "handle",
    "get",
    "save",
    "create",
    "update",
    "delete",
    "store",
    "show",
    "index",
    "find",
    "toArray",
    "execute",
]
# The common-name query needs "many declarations": at least this many.
MIN_DECLARATIONS = 5
TOKEN_BUDGETS = [1000, 4000, 16000, 64000]
EXCLUDED_DIRS = {"node_modules", "vendor", "dist", "build", "target", "coverage", ".git", ".rivet"}


def log(message: str) -> None:
    print(f"[measure] {message}", file=sys.stderr, flush=True)


def summary(values: list[float]) -> dict:
    return {
        "median": round(statistics.median(values), 2),
        "min": round(min(values), 2),
        "max": round(max(values), 2),
        "runs": len(values),
    }


def estimate(n_bytes: int) -> int:
    """rivet's utf8-bytes-v1 estimate, ceil(bytes / 3). Not a real tokenizer."""
    return math.ceil(n_bytes / 3)


class Rivet:
    def __init__(self, binary: str, cwd: Path):
        self.binary = binary
        self.cwd = cwd

    def run(self, args: list[str], timed: bool = True) -> dict:
        """One rivet process. Returns exit code, stdout, stderr, wall ms, RSS."""
        cmd = (["/usr/bin/time", "-l"] if timed else []) + [self.binary] + args
        start = time.perf_counter_ns()
        proc = subprocess.run(
            cmd, cwd=self.cwd, stdin=subprocess.DEVNULL, capture_output=True
        )
        wall_ms = (time.perf_counter_ns() - start) / 1e6
        stderr = proc.stderr
        rss = None
        if timed:
            match = re.search(rb"(\d+)\s+maximum resident set size", stderr)
            if match is None:
                raise RuntimeError(f"no RSS line from /usr/bin/time for {args[:1]}")
            rss = int(match.group(1))
            # /usr/bin/time writes its report after rivet's own stderr; cut it.
            cut = re.search(rb"\s*[\d.]+ real\s+[\d.]+ user\s+[\d.]+ sys\n", stderr)
            stderr = stderr[: cut.start()] if cut else stderr
        return {
            "code": proc.returncode,
            "stdout": proc.stdout,
            "stderr": stderr,
            "wall_ms": wall_ms,
            "rss": rss,
        }

    def json(self, args: list[str], expect: int = 0) -> dict:
        result = self.run(args + ["--json"], timed=False)
        if result["code"] != expect:
            raise RuntimeError(f"rivet {args[0]} exited {result['code']}, expected {expect}")
        return json.loads(result["stdout"] if expect == 0 else result["stderr"])


def repeat(rivet: Rivet, args: list[str], runs: int, expect: int = 0, before=None) -> dict:
    """Times `runs` runs of one command, calling `before()` untimed first."""
    walls, rsss, digests, stderr_empty = [], [], set(), True
    last = None
    for index in range(runs):
        if before is not None:
            before(index)
        result = rivet.run(args)
        if result["code"] != expect:
            raise RuntimeError(
                f"rivet {args[0]} exited {result['code']}, expected {expect}: "
                f"{result['stderr'][:200]!r}"
            )
        walls.append(result["wall_ms"])
        rsss.append(result["rss"] / 1e6)
        digests.add(hashlib.sha256(result["stdout"]).hexdigest())
        if "--json" in args and expect == 0 and result["stderr"]:
            stderr_empty = False
        last = result
    out = {"wall_ms": summary(walls), "max_rss_mb": summary(rsss)}
    if "--json" in args and expect == 0:
        out["json_stderr_empty"] = stderr_empty
    # `index` output describes work done (it carries elapsed_ms) and is exempt
    # from byte identity (OUTPUT-CONTRACT "Transport and common rules").
    out["stdout_identical_across_runs"] = None if args[0] == "index" else len(digests) == 1
    out["_last"] = last
    return out


def strip_private(value):
    """Drops the `_last` raw output kept for in-memory use."""
    if isinstance(value, dict):
        return {k: strip_private(v) for k, v in value.items() if not k.startswith("_")}
    if isinstance(value, list):
        return [strip_private(v) for v in value]
    return value


def candidates_for(rivet: Rivet, name: str) -> list[str]:
    result = rivet.run(["symbol", name, "--limit", "1000", "--no-refresh", "--json"], timed=False)
    if result["code"] == 0:
        return [json.loads(result["stdout"])["symbol"]["id"]]
    if result["code"] == 5:
        data = json.loads(result["stderr"])
        if data["truncated"]:
            raise RuntimeError("more than 1000 declarations; raise the page size")
        return [candidate["id"] for candidate in data["candidates"]]
    if result["code"] == 4:
        return []
    raise RuntimeError(f"symbol lookup exited {result['code']}")


def select_targets(rivet: Rivet, seen_names: set[str], seen_ids: set[str]) -> dict:
    """Deterministic, untimed selection over GENERIC_NAMES (cached reads)."""
    per_name = []
    for name in GENERIC_NAMES:
        ids = candidates_for(rivet, name)
        best_total, best_id = -1, None
        for symbol_id in ids:
            seen_ids.add(symbol_id)
            data = rivet.json(["refs", symbol_id, "--limit", "1", "--no-refresh"])
            symbol = data["symbol"]
            seen_names.update([symbol["qualified_name"], symbol["file"]])
            if data["total"] > best_total:
                best_total, best_id = data["total"], symbol_id
        per_name.append({"name": name, "declarations": len(ids), "best_refs": best_total, "_id": best_id})

    eligible = [row for row in per_name if row["declarations"] >= MIN_DECLARATIONS and row["best_refs"] > 0]
    if not eligible:
        raise RuntimeError("no generic name has enough declarations and uses")
    common = max(eligible, key=lambda row: (row["best_refs"], row["declarations"]))

    most_declared = max(per_name, key=lambda row: row["declarations"])

    fanout = None
    for row in per_name:
        if row["_id"] is None or row["best_refs"] <= 0:
            continue
        data = rivet.json(["context", row["_id"], "--no-refresh"])
        explored = len(data["segments"]) + sum(data["omitted"].values())
        key = (data["candidate_limit_reached"], explored)
        if fanout is None or key > fanout["_key"]:
            fanout = {"name": row["name"], "_id": row["_id"], "_key": key}

    return {
        "rule": (
            f"Among GENERIC_NAMES, the common-name query uses the name with at least "
            f"{MIN_DECLARATIONS} declarations whose best single declaration has the most "
            f"`refs` results (references mode); its target is that declaration. The "
            f"ambiguous symbol query uses the name with the most declarations. The "
            f"high-fanout context query uses, among each name's most-referenced "
            f"declaration, the one whose depth-2 context explores the most unique "
            f"candidates (segments plus omitted), preferring candidate_limit_reached. "
            f"Ties go to earlier list order, then earlier candidate order."
        ),
        "per_name": [{k: v for k, v in row.items() if not k.startswith("_")} for row in per_name],
        "common": common,
        "most_declared": most_declared,
        "fanout": fanout,
    }


def php_bytes(project: Path) -> dict:
    """Sizes of tracked `.php` files outside default-excluded directories.

    This is the content a content-mode refresh reads and hashes (only
    enabled-language files are hashed). Tracked files only; the scratch copy
    is a clean checkout, so that matches the walk.
    """
    listing = subprocess.run(
        ["git", "-c", "core.hooksPath=/dev/null", "ls-files", "-z"],
        cwd=project, capture_output=True, check=True, stdin=subprocess.DEVNULL,
    ).stdout.split(b"\0")
    count, total = 0, 0
    for raw in listing:
        if not raw.endswith(b".php"):
            continue
        parts = raw.split(b"/")
        if any(part.decode("utf-8", "replace") in EXCLUDED_DIRS for part in parts[:-1]):
            continue
        path = project / raw.decode("utf-8", "surrogateescape")
        if path.is_file() and not path.is_symlink():
            count += 1
            total += path.stat().st_size
    return {"files": count, "bytes": total}


def dir_bytes(path: Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def source_bytes(command: str, data: dict) -> int:
    if command == "context":
        return sum(len(segment["source"].encode("utf-8")) for segment in data["segments"])
    if command == "symbol":
        return len(data.get("source", "").encode("utf-8"))
    return 0


def format_row(rivet: Rivet, command: str, target: str, extra: list[str]) -> dict:
    args = [command, target] + extra + ["--no-refresh"]
    as_json = rivet.run(args + ["--json"], timed=False)
    as_text = rivet.run(args, timed=False)
    if as_json["code"] != 0 or as_text["code"] != 0:
        raise RuntimeError(f"{command} exited {as_json['code']}/{as_text['code']}")
    data = json.loads(as_json["stdout"])
    src = source_bytes(command, data)
    json_bytes, text_bytes = len(as_json["stdout"]), len(as_text["stdout"])
    row = {
        "command": " ".join([command, "<target>"] + extra),
        "json_bytes": json_bytes,
        "text_bytes": text_bytes,
        "text_as_share_of_json": round(text_bytes / json_bytes, 4),
        "source_bytes": src,
        "json_envelope_share": round(1 - src / json_bytes, 4),
        "text_non_source_share": round(1 - src / text_bytes, 4) if text_bytes else None,
        "rivet_estimate_json": estimate(json_bytes),
        "rivet_estimate_text": estimate(text_bytes),
        "rivet_estimate_source": estimate(src),
        "blake3_hashes_in_json": as_json["stdout"].count(b'"blake3:'),
    }
    if command == "context":
        row["estimated_tokens_reported"] = data["estimated_tokens"]
        row["segments"] = len(data["segments"])
    if command == "refs":
        row["page_items"] = len(data["references"])
        row["total"] = data["total"]
    if command == "symbol":
        row["calls_page_items"] = len(data["calls"]["items"]) if "calls" in data else None
        row["called_by_page_items"] = len(data["called_by"]["items"]) if "called_by" in data else None
    return row


def rg_reference(project: Path, name: str, runs: int) -> dict:
    rg = shutil.which("rg")
    if rg is None:
        return {"available": False}
    out = {"available": True, "version": subprocess.run([rg, "--version"], capture_output=True, text=True).stdout.splitlines()[0]}
    for label, extra in [("all_files", []), ("php_type_only", ["-t", "php"])]:
        walls, lines = [], None
        for _ in range(runs):
            start = time.perf_counter_ns()
            proc = subprocess.run(
                [rg, "-n", "-w", name, *extra, "."],
                cwd=project, stdin=subprocess.DEVNULL, capture_output=True,
            )
            walls.append((time.perf_counter_ns() - start) / 1e6)
            if proc.returncode not in (0, 1):
                raise RuntimeError(f"rg exited {proc.returncode}")
            count = proc.stdout.count(b"\n")
            if lines is not None and lines != count:
                raise RuntimeError("rg line count changed between runs")
            lines = count
        # Only the count is kept; matched lines are discarded here.
        out[label] = {"command": f"rg -n -w <name> {' '.join(extra)} .".replace("  ", " "), "matched_lines": lines, "wall_ms": summary(walls)}
    return out


def measure_project(rivet_bin: str, corpus: Path, pin: dict, scratch_root: Path, runs: int) -> tuple[dict, set[str]]:
    name = pin["name"]
    source = corpus / name
    if not (source / ".git").exists():
        raise RuntimeError(f"{name}: no copy at $RIVET_CORPUS_DIR/{name}")
    project = scratch_root / name
    log(f"{name}: copying to scratch (excluding .rivet/)")
    subprocess.run(["rsync", "-a", "--exclude", "/.rivet", f"{source}/", f"{project}/"], check=True)
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=project, capture_output=True, text=True, check=True).stdout.strip()
    if head != pin["commit"]:
        raise RuntimeError(f"{name}: HEAD {head} is not the pinned commit")

    rivet = Rivet(rivet_bin, project)
    seen_names: set[str] = set()
    seen_ids: set[str] = set()
    result: dict = {"name": name, "commit": head}
    content_bytes = php_bytes(project)

    # Warm the OS file cache and build the index once, untimed.
    log(f"{name}: warm-up index")
    rivet.run(["index", "--json"], timed=False)

    log(f"{name}: cold index x{runs}")
    rivet_dir = project / ".rivet"
    cold = repeat(rivet, ["index", "--json", "--timing"], runs,
                  before=lambda _i: shutil.rmtree(rivet_dir, ignore_errors=True))
    report = json.loads(cold["_last"]["stdout"])
    coverage = report["index"]["coverage"]
    result["corpus"] = {
        "files_seen": coverage["files_seen"],
        "files_indexed": coverage["files_indexed"],
        "skipped": coverage["skipped"],
        "symbols": report["symbols"],
        "uses": report["uses"],
        "bindings": report["bindings"],
        "php_files_hashed_estimate": content_bytes["files"],
        "php_bytes_hashed_estimate": content_bytes["bytes"],
    }
    index_db = rivet_dir / "index.db"
    result["index_size"] = {
        "rivet_dir_bytes": dir_bytes(rivet_dir),
        "index_db_bytes": index_db.stat().st_size,
        "ratio_to_php_bytes": round(index_db.stat().st_size / content_bytes["bytes"], 2),
    }
    result["cold_index"] = cold

    log(f"{name}: selecting targets (untimed, cached reads)")
    selection = select_targets(rivet, seen_names, seen_ids)
    target = selection["common"]["_id"]
    fanout_target = selection["fanout"]["_id"]
    result["selection"] = {
        "rule": selection["rule"],
        "per_name": selection["per_name"],
        "common_name": selection["common"]["name"],
        "ambiguous_symbol_name": selection["most_declared"]["name"],
        "fanout_name": selection["fanout"]["name"],
    }

    log(f"{name}: no-change refresh")
    refresh = {}
    for mode in ["content", "metadata"]:
        refresh[mode] = repeat(rivet, ["index", "--json", "--timing", "--freshness", mode], runs)
    refresh["force"] = repeat(rivet, ["index", "--json", "--timing", "--force"], runs)
    result["no_change_index"] = refresh

    log(f"{name}: no-change queries")
    queries = {}
    for command, extra in [("symbol", []), ("refs", []), ("context", [])]:
        queries[command] = {}
        for mode, flags in [("content", ["--freshness", "content"]),
                            ("metadata", ["--freshness", "metadata"]),
                            ("no_refresh", ["--no-refresh"])]:
            queries[command][mode] = repeat(rivet, [command, target, *extra, *flags, "--json"], runs)
    result["no_change_query"] = queries

    log(f"{name}: common-name refs")
    common = {"name": selection["common"]["name"], "declarations_with_name": selection["common"]["declarations"]}
    for mode in ["references", "candidates"]:
        timing = repeat(rivet, ["refs", target, "--mode", mode, "--json"], runs)
        data = json.loads(timing["_last"]["stdout"])
        common[mode] = {"total": data["total"], "by_resolution": data["by_resolution"],
                        "page_items": len(data["references"]), "timing_content_refresh": timing}
    common["rg_reference_point"] = rg_reference(project, selection["common"]["name"], runs)
    result["common_name_refs"] = common

    log(f"{name}: high-fanout queries")
    ambiguous_name = selection["most_declared"]["name"]
    amb = repeat(rivet, ["symbol", ambiguous_name, "--limit", "1000", "--json"], runs, expect=5)
    amb_data = json.loads(amb["_last"]["stderr"])
    fan_ctx = repeat(rivet, ["context", fanout_target, "--json"], runs)
    ctx_data = json.loads(fan_ctx["_last"]["stdout"])
    fan_sym = repeat(rivet, ["symbol", fanout_target, "--json"], runs)
    sym_data = json.loads(fan_sym["_last"]["stdout"])
    result["high_fanout"] = {
        "ambiguous_symbol": {"name": ambiguous_name, "candidates_total": amb_data["total"],
                             "candidates_returned": len(amb_data["candidates"]),
                             "exit_code": 5, "timing_content_refresh": amb},
        "context": {"name": selection["fanout"]["name"], "segments": len(ctx_data["segments"]),
                    "omitted": ctx_data["omitted"],
                    "explored_candidates": len(ctx_data["segments"]) + sum(ctx_data["omitted"].values()),
                    "candidate_limit_reached": ctx_data["candidate_limit_reached"],
                    "estimated_tokens": ctx_data["estimated_tokens"],
                    "timing_content_refresh": fan_ctx},
        "symbol": {"name": selection["fanout"]["name"],
                   "called_by_total": sym_data["called_by"]["total"],
                   "calls_total": sym_data["calls"]["total"],
                   "timing_content_refresh": fan_sym},
    }

    log(f"{name}: context sizes and output formats")
    targets = [("common", target)]
    if fanout_target != target:
        targets.append(("fanout", fanout_target))
    result["selection"]["fanout_target_is_common_target"] = fanout_target == target
    sizes = []
    for target_label, symbol_id in targets:
        for budget in TOKEN_BUDGETS:
            row = format_row(rivet, "context", symbol_id, ["--tokens", str(budget)])
            row["target"] = target_label
            row["budget_tokens"] = budget
            sizes.append(row)
    result["context_sizes"] = sizes
    formats = []
    for target_label, symbol_id in targets:
        for command, extra in [("symbol", []), ("symbol", ["--source"]), ("refs", []),
                               ("refs", ["--limit", "1000"]), ("context", ["--tokens", "4000"])]:
            row = format_row(rivet, command, symbol_id, extra)
            row["target"] = target_label
            formats.append(row)
    result["output_formats"] = formats

    log(f"{name}: one-file edit")
    # The edited file is the common-name target's file. An appended PHP line
    # comment changes content (and so the hash) without moving any symbol.
    target_file = project / json.loads(queries["symbol"]["no_refresh"]["_last"]["stdout"])["symbol"]["file"]
    original = target_file.read_bytes()

    def edit(index: int) -> None:
        target_file.write_bytes(original + f"\n// rivet-t37 edit {time.time_ns()} {index}\n".encode())

    edits = {
        "index_content": repeat(rivet, ["index", "--json", "--timing", "--freshness", "content"], runs, before=edit),
        "index_metadata": repeat(rivet, ["index", "--json", "--timing", "--freshness", "metadata"], runs, before=edit),
        "refs_query_content": repeat(rivet, ["refs", target, "--json"], runs, before=edit),
        "refs_query_metadata": repeat(rivet, ["refs", target, "--freshness", "metadata", "--json"], runs, before=edit),
    }
    updated = json.loads(edits["index_content"]["_last"]["stdout"])["updated"]
    edits["updated_files_per_edit"] = updated
    target_file.write_bytes(original)
    rivet.run(["index", "--json"], timed=False)
    result["one_file_edit"] = edits

    def med(block: dict) -> float:
        return block["wall_ms"]["median"]

    result["derived_split_ms"] = {
        "method": "differences of medians of whole-process wall times; no phase timing was added",
        "content_hashing": round(med(refresh["content"]) - med(refresh["metadata"]), 2),
        "reparse_one_file_plus_full_reresolution": round(med(edits["index_content"]) - med(refresh["content"]), 2),
        "reparse_one_file_plus_full_reresolution_metadata_mode": round(med(edits["index_metadata"]) - med(refresh["metadata"]), 2),
        "force_minus_cold": round(med(refresh["force"]) - med(cold), 2),
        "refs_refresh_overhead_content": round(med(queries["refs"]["content"]) - med(queries["refs"]["no_refresh"]), 2),
        "refs_refresh_overhead_metadata": round(med(queries["refs"]["metadata"]) - med(queries["refs"]["no_refresh"]), 2),
    }

    seen_names.update(seen_ids)
    return strip_private(result), seen_names


def scan_results_for_leaks(text: str, forbidden: set[str]) -> list[str]:
    """Returns every forbidden string (paths, IDs, qualified names, and their
    path/namespace components longer than the generic names) found in text."""
    pieces = set()
    for item in forbidden:
        pieces.add(item)
        for part in re.split(r"[\\/#.:$]+", item):
            if len(part) >= 4 and part not in GENERIC_NAMES and not part.isdigit():
                pieces.add(part)
    allowed = {"php", "fluent", "timesheet", "vendor", "json", "text", "true", "false", "null"}
    hits = []
    for piece in sorted(pieces):
        if piece.lower() in allowed:
            continue
        if re.search(r"(?<![A-Za-z0-9_])" + re.escape(piece) + r"(?![A-Za-z0-9_])", text):
            hits.append(piece)
    return hits


def environment(rivet_bin: str) -> dict:
    def sh(cmd: list[str]) -> str:
        try:
            return subprocess.run(cmd, capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout.strip()
        except OSError:
            return ""

    status = sh(["git", "-C", str(REPO), "status", "--porcelain", "--", "crates", "Cargo.toml", "Cargo.lock"])
    return {
        "machine_model": sh(["sysctl", "-n", "hw.model"]),
        "cpu": sh(["sysctl", "-n", "machdep.cpu.brand_string"]),
        "cpu_cores_physical": sh(["sysctl", "-n", "hw.physicalcpu"]),
        "memory_bytes": int(sh(["sysctl", "-n", "hw.memsize"]) or 0),
        "os": f"{sh(['sw_vers', '-productName'])} {sh(['sw_vers', '-productVersion'])} ({sh(['sw_vers', '-buildVersion'])})",
        "rustc": sh(["rustc", "--version"]),
        "rivet_version": sh([rivet_bin, "--version"]),
        "rivet_commit": sh(["git", "-C", str(REPO), "rev-parse", "HEAD"]),
        "rivet_sources_clean": status == "",
        "build": "cargo build --release",
        "load_average_at_start": sh(["sysctl", "-n", "vm.loadavg"]),
        "os_file_cache": "warm: each project copied and indexed once, untimed, before timed runs; cache never purged",
        "timing": "wall clock of one process via time.perf_counter_ns around /usr/bin/time -l; includes process start",
        "python": platform.python_version(),
    }


def main() -> int:
    rivet_bin = os.environ["RIVET_BIN"]
    corpus = Path(os.environ["RIVET_CORPUS_DIR"]).resolve()
    runs = int(os.environ.get("RIVET_RUNS", "7"))
    if runs < 5:
        raise SystemExit("RIVET_RUNS must be at least 5")
    out_dir = Path(os.environ.get("RIVET_RESULTS_DIR", REPO / "benchmark/results/T37-local"))
    projects = os.environ.get("RIVET_PROJECTS", "fluent,timesheet").split(",")
    manifest = tomllib.loads((REPO / "benchmark/corpus.toml").read_text())
    pins = {p["name"]: p for p in manifest["project"]}

    env = environment(rivet_bin)
    started = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    results = {"task": "T37", "schema": 1, "started_utc": started, "runs_per_measurement": runs,
               "environment": env,
               "corpus_pins": {n: pins[n]["commit"] for n in projects},
               "projects": {}}
    forbidden: set[str] = set()
    scratch_root = Path(tempfile.mkdtemp(prefix="rivet-t37-"))
    try:
        for name in projects:
            project_result, seen = measure_project(rivet_bin, corpus, pins[name], scratch_root, runs)
            results["projects"][name] = project_result
            forbidden |= seen
    finally:
        shutil.rmtree(scratch_root, ignore_errors=True)
    results["environment"]["load_average_at_end"] = subprocess.run(
        ["sysctl", "-n", "vm.loadavg"], capture_output=True, text=True).stdout.strip()

    text = json.dumps(results, indent=2, sort_keys=False) + "\n"
    hits = scan_results_for_leaks(text, forbidden)
    if hits:
        # Printed to the terminal only, so the operator can see what matched.
        log(f"refusing to write results: {len(hits)} project-derived strings found: {hits}")
        return 1
    results["privacy_scan"] = {"checked_strings": len(forbidden), "hits": 0}
    text = json.dumps(results, indent=2) + "\n"
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "results.json").write_text(text)
    log(f"wrote {out_dir / 'results.json'}; privacy scan checked {len(forbidden)} strings, 0 hits")
    return 0


if __name__ == "__main__":
    sys.exit(main())
