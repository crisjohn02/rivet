#!/usr/bin/env python3
"""T46: contract checks for rivet on a pinned real TypeScript project and on
the authored PHP and TypeScript fixtures, alone and in one repository.

Standard library only; Python 3.9 or newer. Drives a built `rivet` binary
(`--rivet`, default `target/release/rivet`) as a subprocess and reads nothing
from rivet's internals except, read-only, the SQLite store it wrote
(`.rivet/index.db`), to enumerate symbols and bindings for sampling and for
the coverage tables. Every answer that is compared is a CLI answer.

Every check runs on copies made in a fresh temporary directory, never on the
fetched checkout (`tests/real/fetch.sh`) or the fixtures themselves. Each check
prints `PASS <name>: <summary>` or `FAIL <name>: <reason>`; the script exits
1 if any check failed.

Checks (see tests/real/RESULTS.md for the last recorded run):

  determinism   two clean indexes of the same tree, in two directories:
                `index --json` byte-identical once timing fields are dropped,
                and `symbol`/`refs`/`context` byte-identical.
  incremental   from a clean index, a scripted, seeded sequence of edits
                (EDIT_SCRIPT), refreshing after each: the incremental store
                must answer `symbol`, `refs` (both modes) and `context`
                byte-identically to a `--force` rebuild of the same tree, and
                report the same snapshot, coverage and diagnostics.
  coverage      files seen/indexed/skipped by reason, symbols, uses, bindings
                by tier and language, parse failures by path; the JSON counts
                must agree with each other and with the store.
  two-language  the authored PHP and TypeScript fixtures in one tree: each
                language's answers equal its single-language tree's (apart
                from the `index` object), and no use crosses languages.
  budget        seeded `context` queries on the real project at two budgets
                respect `--tokens` as docs/OUTPUT-CONTRACT.md defines it.

`--timings` adds the indicative index timings of benchmark/corpus.toml's
header; `--audit` prints the seeded honesty-audit sample instead of checking.

Unit tests for the pure helpers: `python3 tests/real/test_check_real.py`.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import random
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Callable, Dict, Iterable, List, Optional, Sequence, Set, Tuple

REPO = Path(__file__).resolve().parents[2]
MANIFEST = REPO / "tests" / "real" / "manifest.toml"
PHP_FIXTURE = REPO / "tests" / "fixtures" / "php" / "authored"
TS_FIXTURE = REPO / "tests" / "fixtures" / "typescript" / "authored"

DEFAULT_SEED = 46
# `index` fields that describe elapsed time (OUTPUT-CONTRACT: `--timing` adds
# `elapsed_ms`). Nothing else in `index --json` is timing.
TIMING_KEYS = frozenset({"elapsed_ms"})
BUDGETS = (1000, 4000)
SAMPLE_OTHERS = 200
BUDGET_SAMPLE = 100
AUDIT_SAMPLE = 40

# The scripted edit sequence, in order. Each is planned against the store the
# previous step left, so byte offsets are always current.
EDIT_SCRIPT = (
    "modify_body",
    "rename_method",
    "remove_export",
    "add_file",
    "delete_file",
    "rename_file",
    "break_syntax",
    "fix_syntax",
)

LANG_EXT = {
    "typescript": (".ts", ".tsx"),
    "php": (".php",),
}


# ---------------------------------------------------------------------------
# Pure helpers (unit-tested in test_check_real.py)
# ---------------------------------------------------------------------------


def drop_timing(value):
    """Returns `value` with every TIMING_KEYS field removed, at any depth."""
    if isinstance(value, dict):
        return {k: drop_timing(v) for k, v in value.items() if k not in TIMING_KEYS}
    if isinstance(value, list):
        return [drop_timing(v) for v in value]
    return value


def canonical_json(value) -> bytes:
    """Compact JSON with keys in their existing order (as rivet writes it)."""
    return (json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")


def normalize_index_output(stdout: bytes) -> bytes:
    """`index --json` output with timing fields dropped, re-serialized."""
    return canonical_json(drop_timing(json.loads(stdout)))


def without_index_block(stream: bytes) -> bytes:
    """A JSON answer (success or error object) without its `index` object."""
    data = json.loads(stream)
    data.pop("index", None)
    return canonical_json(data)


def estimate_tokens(source: str) -> int:
    """rivet's utf8-bytes-v1 estimate: ceil(UTF-8 bytes / 3)."""
    return math.ceil(len(source.encode("utf-8")) / 3)


def seeded_rng(seed: int, *labels: object) -> random.Random:
    """A generator seeded from a string, so it does not depend on hash seeds."""
    return random.Random(":".join([str(seed)] + [str(label) for label in labels]))


def sample_ids(all_ids: Iterable[str], must: Iterable[str], rng: random.Random, others: int) -> List[str]:
    """`must` plus a seeded sample of `others` of the remaining ids, sorted.

    The population is sorted before sampling, so the result depends only on
    the id set, the generator state, and `others`.
    """
    must_set = set(must)
    rest = sorted(set(all_ids) - must_set)
    picked = rng.sample(rest, min(others, len(rest)))
    return sorted(must_set | set(picked))


def splice(content: bytes, edits: Sequence[Tuple[int, int, bytes]]) -> bytes:
    """Replaces each `[start, end)` byte range; ranges must not overlap."""
    ordered = sorted(edits, key=lambda e: (e[0], e[1]))
    for (s1, e1, _), (s2, _e2, _) in zip(ordered, ordered[1:]):
        if s2 < e1:
            raise ValueError(f"overlapping edits at {s1}..{e1} and {s2}")
    out = bytearray()
    cursor = 0
    for start, end, replacement in ordered:
        if start < cursor or end < start or end > len(content):
            raise ValueError(f"bad edit range {start}..{end} for {len(content)} bytes")
        out += content[cursor:start]
        out += replacement
        cursor = end
    out += content[cursor:]
    return bytes(out)


def declaration_name_offset(language: str, kind: str, source: bytes, name: str) -> Optional[int]:
    """Offset of a declaration's name inside its span source, or None.

    TypeScript members are written `[modifiers] name(`/`name<`; PHP functions
    and methods `function [&]name(`; PHP class-likes `class|interface|trait|
    enum name`. The first such occurrence in the span is the declaration's own
    name, since the span starts at the declaration (decorators on a method are
    outside it).
    """
    text = source.decode("utf-8", "surrogateescape")
    escaped = re.escape(name)
    if language == "php":
        if kind in ("function", "method"):
            pattern = r"\bfunction\s+&?(" + escaped + r")\s*\("
        else:
            pattern = r"\b(?:class|interface|trait|enum)\s+(" + escaped + r")\b"
        match = re.search(pattern, text, re.IGNORECASE)
    else:
        if kind in ("function", "method"):
            pattern = r"(?<![\w$#])(" + escaped + r")\s*[(<]"
        else:
            pattern = r"\b(?:class|interface|type|enum|function|const)\s+(" + escaped + r")\b"
        match = re.search(pattern, text)
    if match is None:
        return None
    return len(text[: match.start(1)].encode("utf-8", "surrogateescape"))


def renamed_path(path: str, suffix: str) -> str:
    """`dir/name.ext` -> `dir/name<suffix>.ext`, keeping `.d.ts` whole."""
    for ext in (".d.ts", ".tsx", ".ts", ".php"):
        if path.endswith(ext):
            return path[: -len(ext)] + suffix + ext
    raise ValueError(f"unsupported extension: {path}")


def module_specifier(from_path: str, to_path: str) -> str:
    """A relative TypeScript specifier from one file to another, no extension."""
    to_stem = re.sub(r"(\.d)?\.tsx?$", "", to_path)
    rel = os.path.relpath(to_stem, os.path.dirname(from_path) or ".").replace(os.sep, "/")
    return rel if rel.startswith("../") else "./" + rel


def receiver_shape(receiver: str, hint: dict) -> str:
    """A mechanical label for why an unresolved member call's receiver gave
    no binding, from its text and recorded hint alone. Coarser than the hand
    audit: a hinted receiver may fail for inheritance, a re-export, a
    non-class type, and so on, which only reading the source tells apart.
    """
    kind = hint.get("kind", "unresolved")
    if kind in ("typed", "new_expr", "this"):
        spelling = hint.get("type_spelling") or hint.get("class_spelling") or ""
        if "." in spelling:
            return f"hinted ({kind}), qualified type name"
        return f"hinted ({kind}), no own member bound"
    if re.fullmatch(r"[A-Za-z_$][\w$]*", receiver):
        if receiver[0].isupper():
            return "capitalized identifier, no hint (class-name or global object)"
        return "identifier, no hint (untyped)"
    if re.fullmatch(r"[A-Za-z_$#][\w$#]*(\??\.[A-Za-z_$#][\w$#]*)+", receiver):
        return "property chain (a.b, this.a)"
    return "call or other expression"


def first_difference(a: bytes, b: bytes, width: int = 60) -> str:
    """A short description of where two byte strings first differ."""
    limit = min(len(a), len(b))
    index = next((i for i in range(limit) if a[i] != b[i]), limit)
    lo = max(0, index - width)
    return (
        f"at byte {index} (lengths {len(a)}/{len(b)}): "
        f"{a[lo:index + width]!r} vs {b[lo:index + width]!r}"
    )


# ---------------------------------------------------------------------------
# Edit planning
# ---------------------------------------------------------------------------


def open_readonly(path: Path) -> sqlite3.Connection:
    """Opens a rivet store read-only.

    Some SQLite builds (for example macOS's system Python 3.9) refuse
    `mode=ro` on a WAL database whose `-shm` file does not exist. rivet
    checkpoints and removes its WAL when it exits, and this script never
    reads a store while rivet writes it, so with no `-wal` file present the
    store can be opened `immutable`, which writes nothing either.
    """
    try:
        con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        con.execute("SELECT 1 FROM meta LIMIT 1").fetchall()
        return con
    except sqlite3.OperationalError:
        wal = Path(str(path) + "-wal")
        if wal.exists() and wal.stat().st_size > 0:
            raise
        return sqlite3.connect(f"file:{path}?mode=ro&immutable=1", uri=True)


class Store:
    """A read-only view of one `.rivet/index.db`."""

    def __init__(self, root: Path):
        con = open_readonly(root / ".rivet" / "index.db")
        try:
            self.files: Dict[str, Tuple[Optional[str], str]] = {
                p: (lang, status)
                for p, lang, status in con.execute("SELECT path, language, parse_status FROM files")
            }
            self.symbols: Dict[str, dict] = {}
            for row in con.execute(
                "SELECT id, file, name, qualified_name, kind, start_byte, end_byte, start_line, signature"
                " FROM symbols"
            ):
                self.symbols[row[0]] = {
                    "id": row[0], "file": row[1], "name": row[2], "qualified_name": row[3],
                    "kind": row[4], "start": row[5], "end": row[6], "line": row[7], "signature": row[8],
                }
            self.bindings: List[dict] = []
            for row in con.execute(
                "SELECT u.file, u.start_byte, u.end_byte, u.line, u.col, u.ref_kind, u.spelling,"
                " u.receiver, u.containing_symbol, b.target_id, b.resolution"
                " FROM bindings b JOIN uses u ON u.use_id = b.use_id"
            ):
                self.bindings.append({
                    "file": row[0], "start": row[1], "end": row[2], "line": row[3], "col": row[4],
                    "ref_kind": row[5], "spelling": row[6], "receiver": row[7],
                    "container": row[8], "target": row[9], "resolution": row[10],
                })
            self.bindings.sort(key=lambda b: (b["file"], b["start"], b["end"], b["ref_kind"]))
            self.use_count = con.execute("SELECT COUNT(*) FROM uses").fetchone()[0]
            self.uses_by_language = dict(con.execute(
                "SELECT COALESCE(f.language, ''), COUNT(*) FROM uses u JOIN files f ON f.path = u.file"
                " GROUP BY 1"
            ).fetchall())
            self.unresolved_member_calls = [
                {"file": r[0], "start": r[1], "end": r[2], "line": r[3], "col": r[4],
                 "spelling": r[5], "receiver": r[6], "hint": r[7]}
                for r in con.execute(
                    "SELECT u.file, u.start_byte, u.end_byte, u.line, u.col, u.spelling, u.receiver,"
                    " u.hint_json FROM uses u JOIN files f ON f.path = u.file"
                    " LEFT JOIN bindings b ON b.use_id = u.use_id"
                    " WHERE b.use_id IS NULL AND u.ref_kind = 'call' AND u.receiver IS NOT NULL"
                    " AND f.language = 'typescript'"
                    " ORDER BY u.file, u.start_byte, u.end_byte"
                )
            ]
            self._sources: Dict[str, bytes] = {
                p: bytes(src) for p, src in con.execute(
                    "SELECT path, source FROM files WHERE source IS NOT NULL")
            }
        finally:
            con.close()

    def source(self, path: str) -> bytes:
        return self._sources[path]

    def slice(self, symbol: dict) -> bytes:
        return self._sources[symbol["file"]][symbol["start"]:symbol["end"]]

    def language_of(self, path: str) -> Optional[str]:
        entry = self.files.get(path)
        return entry[0] if entry else None

    def ok_files(self, language: str) -> List[str]:
        return sorted(
            p for p, (lang, status) in self.files.items()
            if lang == language and status == "ok" and not p.endswith(".d.ts")
        )

    def inbound(self) -> Dict[str, Set[str]]:
        """file -> the other files whose uses bind into it."""
        result: Dict[str, Set[str]] = {}
        for b in self.bindings:
            target = self.symbols.get(b["target"])
            if target and target["file"] != b["file"]:
                result.setdefault(target["file"], set()).add(b["file"])
        return result

    def dependents(self, files: Iterable[str]) -> Set[str]:
        """Files (outside `files`) with a use bound to a symbol in `files`."""
        files = set(files)
        out = set()
        for b in self.bindings:
            target = self.symbols.get(b["target"])
            if target and target["file"] in files and b["file"] not in files:
                out.add(b["file"])
        return out

    def ids_in(self, files: Iterable[str]) -> Set[str]:
        files = set(files)
        return {sid for sid, s in self.symbols.items() if s["file"] in files}


class Step:
    """One scripted edit: file writes, deletions, and renames."""

    def __init__(self, kind: str, language: str, description: str):
        self.kind = kind
        self.language = language
        self.description = description
        self.writes: Dict[str, bytes] = {}
        self.deletes: List[str] = []
        self.renames: List[Tuple[str, str]] = []
        self.related: Set[str] = set()  # files read but not written (e.g. an import target)
        self.expect_parse_error: Optional[str] = None

    def touched(self) -> Set[str]:
        out = set(self.writes) | set(self.deletes)
        for old, new in self.renames:
            out |= {old, new}
        return out

    def apply(self, root: Path) -> None:
        for path, content in sorted(self.writes.items()):
            target = root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        for path in sorted(self.deletes):
            (root / path).unlink()
        for old, new in self.renames:
            (root / new).parent.mkdir(parents=True, exist_ok=True)
            (root / old).rename(root / new)


class NoCandidate(Exception):
    pass


def _choose(rng: random.Random, tiers: Sequence[Sequence], what: str):
    """A seeded choice from the first non-empty tier (each tier sorted)."""
    for index, tier in enumerate(tiers):
        if tier:
            return rng.choice(sorted(tier)), index
    raise NoCandidate(f"no candidate for {what}")


def plan_step(kind: str, language: str, store: Store, rng: random.Random, state: dict) -> Step:
    """Plans one EDIT_SCRIPT step against `store`, the tree's current index.

    `state` carries what `fix_syntax` must restore. Candidate lists are
    tiered by preference (for example a file other files depend on before
    any file); the chosen tier is recorded in the description.
    """
    ok = [p for p in store.ok_files(language) if "rivet-t46" not in p and "RivetT46" not in p]
    ok_set = set(ok)
    inbound = store.inbound()
    probe_ts = b"\n  rivetT46Probe()\n"
    probe_php = b"\n        rivet_t46_probe();\n"

    if kind == "modify_body":
        def body_ok(s):
            if s["file"] not in ok_set or s["kind"] not in ("function", "method"):
                return False
            text = store.slice(s)
            if not text.endswith(b"}") or b"{" not in text:
                return False
            head = text.lstrip()
            if head.startswith((b"declare", b"export declare", b"abstract")):
                return False
            if language == "typescript" and s["kind"] == "method":
                return False  # interface method signatures can end in `}`
            return True
        cands = [sid for sid, s in store.symbols.items() if body_ok(s)]
        funcs = [sid for sid in cands if store.symbols[sid]["kind"] == "function"]
        sid, tier = _choose(rng, [funcs, cands], "modify_body")
        s = store.symbols[sid]
        probe = probe_php if language == "php" else probe_ts
        content = store.source(s["file"])
        step = Step(kind, language, f"insert a call before the closing brace of {sid} (tier {tier})")
        step.writes[s["file"]] = splice(content, [(s["end"] - 1, s["end"] - 1, probe)])
        return step

    if kind == "rename_method":
        by_target: Dict[str, List[dict]] = {}
        for b in store.bindings:
            by_target.setdefault(b["target"], []).append(b)
        cross, local = [], []
        for sid, s in store.symbols.items():
            if s["kind"] != "method" or s["file"] not in ok_set or sid not in by_target:
                continue
            if s["name"] == "constructor" or s["name"].startswith("__"):
                continue
            if declaration_name_offset(language, "method", store.slice(s), s["name"]) is None:
                continue
            uses = by_target[sid]
            if any(u["file"] not in ok_set for u in uses):
                continue
            (cross if any(u["file"] != s["file"] for u in uses) else local).append(sid)
        sid, tier = _choose(rng, [cross, local], "rename_method")
        s = store.symbols[sid]
        new_name = s["name"] + "RivetT46"
        edits: Dict[str, List[Tuple[int, int, bytes]]] = {}
        offset = s["start"] + declaration_name_offset(language, "method", store.slice(s), s["name"])
        edits.setdefault(s["file"], []).append((offset, offset + len(s["name"].encode()), new_name.encode()))
        for u in by_target[sid]:
            spelled = store.source(u["file"])[u["start"]:u["end"]].decode("utf-8")
            same = spelled.lower() == s["name"].lower() if language == "php" else spelled == s["name"]
            if not same:
                raise NoCandidate(f"use at {u['file']}:{u['start']} spells {spelled!r}, not {s['name']!r}")
            edits.setdefault(u["file"], []).append((u["start"], u["end"], new_name.encode()))
        step = Step(kind, language,
                    f"rename method {sid} to {new_name} and its {len(by_target[sid])} bound uses (tier {tier})")
        for path, file_edits in edits.items():
            step.writes[path] = splice(store.source(path), file_edits)
        return step

    if kind == "remove_export":
        imported: Dict[str, Set[str]] = {}
        for b in store.bindings:
            t = store.symbols.get(b["target"])
            if t and b["ref_kind"] == "import" and t["file"] != b["file"]:
                imported.setdefault(b["target"], set()).add(b["file"])
        cands = []
        for sid in imported:
            s = store.symbols[sid]
            if s["file"] not in ok_set:
                continue
            text = store.slice(s)
            if language == "typescript":
                if text.startswith(b"export ") and not text.startswith(b"export default"):
                    cands.append(sid)
            elif s["kind"] in ("class", "interface", "enum", "function"):
                if declaration_name_offset(language, s["kind"], text, s["name"]) is not None:
                    cands.append(sid)
        sid, tier = _choose(rng, [cands], "remove_export")
        s = store.symbols[sid]
        content = store.source(s["file"])
        if language == "typescript":
            edit = (s["start"], s["start"] + len(b"export "), b"")
            what = f"drop `export` from {sid}, imported by {len(imported[sid])} file(s)"
        else:
            # PHP has no exports; the equivalent is that the imported
            # declaration disappears under its name.
            offset = s["start"] + declaration_name_offset(language, s["kind"], store.slice(s), s["name"])
            edit = (offset, offset + len(s["name"].encode()), (s["name"] + "RivetT46Gone").encode())
            what = f"rename the declaration of {sid} (PHP has no export), imported by {len(imported[sid])} file(s)"
        step = Step(kind, language, what)
        step.writes[s["file"]] = splice(content, [edit])
        return step

    if kind == "add_file":
        if language == "typescript":
            cands = [
                sid for sid, s in store.symbols.items()
                if s["kind"] == "function" and s["file"] in ok_set
                and re.fullmatch(r"[A-Za-z_$][\w$]*", s["name"])
                and store.slice(s).startswith((b"export function", b"export async function", b"export const"))
                and "." not in s["qualified_name"]
            ]
            sid, tier = _choose(rng, [cands], "add_file")
            s = store.symbols[sid]
            directory = os.path.dirname(s["file"])
            new_path = (directory + "/" if directory else "") + "rivet-t46-new.ts"
            spec = module_specifier(new_path, s["file"])
            content = (
                f"import {{ {s['name']} }} from '{spec}'\n\n"
                f"export function rivetT46New() {{\n  return {s['name']}\n}}\n"
            ).encode()
        else:
            cands = [
                sid for sid, s in store.symbols.items()
                if s["kind"] == "class" and s["file"] in ok_set and "\\" in s["qualified_name"]
            ]
            sid, tier = _choose(rng, [cands], "add_file")
            s = store.symbols[sid]
            new_path = "RivetT46New.php"
            content = (
                "<?php\n\nnamespace RivetT46;\n\n"
                f"use {s['qualified_name']};\n\n"
                f"function rivet_t46_new(): {s['name']}\n{{\n    return new {s['name']}();\n}}\n"
            ).encode()
        if new_path in store.files:
            raise NoCandidate(f"{new_path} already exists")
        step = Step(kind, language, f"add {new_path} importing {sid}")
        step.writes[new_path] = content
        step.related.add(s["file"])
        return step

    if kind in ("delete_file", "rename_file", "break_syntax"):
        depended = [p for p in ok if inbound.get(p)]
        path, tier = _choose(rng, [depended, ok], kind)
        users = len(inbound.get(path, ()))
        if kind == "delete_file":
            step = Step(kind, language, f"delete {path} ({users} dependent file(s), tier {tier})")
            step.deletes.append(path)
        elif kind == "rename_file":
            new = renamed_path(path, "-rivet-t46")
            step = Step(kind, language, f"rename {path} to {new} ({users} dependent file(s), tier {tier})")
            step.renames.append((path, new))
        else:
            original = store.source(path)
            broken = b"\nfunction rivet_t46_broken( {\n" if language == "php" else b"\nexport function rivetT46Broken( {\n"
            step = Step(kind, language, f"append a syntax error to {path} ({users} dependent file(s), tier {tier})")
            step.writes[path] = original + broken
            step.expect_parse_error = path
            state["broken"] = (path, original)
        return step

    if kind == "fix_syntax":
        if "broken" not in state:
            raise NoCandidate("fix_syntax without a preceding break_syntax")
        path, original = state.pop("broken")
        step = Step(kind, language, f"restore {path}")
        step.writes[path] = original
        return step

    raise ValueError(f"unknown step kind {kind}")


# ---------------------------------------------------------------------------
# Running rivet
# ---------------------------------------------------------------------------


class Rivet:
    def __init__(self, binary: Path, jobs: int):
        self.binary = str(binary)
        self.jobs = jobs

    def run(self, cwd: Path, args: Sequence[str]) -> Tuple[int, bytes, bytes]:
        proc = subprocess.run(
            [self.binary, *args], cwd=cwd, stdin=subprocess.DEVNULL, capture_output=True
        )
        return proc.returncode, proc.stdout, proc.stderr

    def index(self, cwd: Path, *flags: str) -> Tuple[dict, bytes]:
        code, out, err = self.run(cwd, ["index", "--json", *flags])
        if code != 0:
            raise RuntimeError(f"rivet index {' '.join(flags)} in {cwd} exited {code}: {err[:400]!r}")
        return json.loads(out), out

    def many(self, jobs: Sequence[Tuple[Path, Sequence[str]]]) -> List[Tuple[int, bytes, bytes]]:
        with concurrent.futures.ThreadPoolExecutor(max_workers=self.jobs) as pool:
            return list(pool.map(lambda job: self.run(job[0], job[1]), jobs))


def query_argvs(symbol_id: str) -> List[Tuple[str, List[str]]]:
    """The queries compared for one symbol, labelled."""
    base = ["--json", "--no-refresh"]
    return [
        ("symbol", ["symbol", symbol_id, *base]),
        ("refs", ["refs", symbol_id, "--limit", "1000", *base]),
        ("refs-candidates", ["refs", symbol_id, "--mode", "candidates", "--limit", "1000", *base]),
        ("context", ["context", symbol_id, *base]),
    ]


def compare_queries(rivet: Rivet, left: Path, right: Path, ids: Sequence[str],
                    normalize: Optional[Callable[[bytes], bytes]] = None) -> Tuple[int, List[str]]:
    """Runs every query for every id in both trees; returns (count, mismatches)."""
    labelled = [(sid, label, argv) for sid in ids for label, argv in query_argvs(sid)]
    jobs = [(left, argv) for _, _, argv in labelled] + [(right, argv) for _, _, argv in labelled]
    results = rivet.many(jobs)
    n = len(labelled)
    mismatches = []
    for i, (sid, label, _) in enumerate(labelled):
        a, b = results[i], results[n + i]
        if normalize is not None:
            a = (a[0], normalize(a[1]) if a[1] else b"", normalize(a[2]) if a[2] else b"")
            b = (b[0], normalize(b[1]) if b[1] else b"", normalize(b[2]) if b[2] else b"")
        if a == b:
            continue
        if a[0] != b[0]:
            why = f"exit {a[0]} vs {b[0]}"
        elif a[1] != b[1]:
            why = "stdout " + first_difference(a[1], b[1])
        else:
            why = "stderr " + first_difference(a[2], b[2])
        mismatches.append(f"{label} {sid}: {why}")
    return n, mismatches


def snapshot_of(result: Tuple[int, bytes, bytes]) -> Optional[str]:
    stream = result[1] if result[0] == 0 else result[2]
    try:
        return json.loads(stream).get("index", {}).get("snapshot")
    except ValueError:
        return None


def tree_digest(root: Path) -> str:
    """BLAKE2 over sorted relative paths and bytes, ignoring .rivet/ and .git/."""
    h = hashlib.blake2b(digest_size=16)
    for path in sorted(p for p in root.rglob("*") if p.is_file() and not p.is_symlink()):
        rel = path.relative_to(root).as_posix()
        if rel.split("/")[0] in (".rivet", ".git"):
            continue
        h.update(rel.encode() + b"\0" + hashlib.sha256(path.read_bytes()).digest())
    return h.hexdigest()


# ---------------------------------------------------------------------------
# Trees
# ---------------------------------------------------------------------------


def read_manifest() -> dict:
    """The `[project]` string fields of manifest.toml (a tiny TOML subset)."""
    project: Dict[str, str] = {}
    table = ""
    for raw in MANIFEST.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            table = line.split("#", 1)[0].strip()
            continue
        match = re.fullmatch(r'([A-Za-z_][\w-]*)\s*=\s*"([^"]*)"\s*(?:#.*)?', line)
        if table == "[project]" and match:
            project[match.group(1)] = match.group(2)
    return project


def copy_tree(src: Path, dst: Path, git_marker: bool = True) -> Path:
    """Copies `src` to `dst` without any `.rivet/`, keeping `.git` if present.

    A tree without `.git` gets an empty `.git/` directory so repository
    discovery stops at its root (spec §25).
    """
    shutil.copytree(src, dst, symlinks=True, ignore=shutil.ignore_patterns(".rivet"))
    if git_marker and not (dst / ".git").exists():
        (dst / ".git").mkdir()
    return dst


def make_mixed(dst: Path) -> Path:
    """The authored TypeScript fixture, then the PHP one, in one tree.

    Both fixtures keep their own relative paths. Their only shared path is
    README.md, which is unsupported and never indexed; the TypeScript one is
    kept.
    """
    copy_tree(TS_FIXTURE, dst)
    for path in sorted(PHP_FIXTURE.rglob("*")):
        rel = path.relative_to(PHP_FIXTURE)
        target = dst / rel
        if path.is_dir():
            target.mkdir(exist_ok=True)
        elif not target.exists():
            shutil.copy2(path, target)
    return dst


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


class Report:
    def __init__(self):
        self.failures = 0
        self.data: Dict[str, object] = {}

    def result(self, name: str, ok: bool, message: str, details: Sequence[str] = ()) -> None:
        print(f"{'PASS' if ok else 'FAIL'} {name}: {message}", flush=True)
        for line in list(details)[:10]:
            print(f"    {line}", flush=True)
        if len(details) > 10:
            print(f"    ... {len(details) - 10} more", flush=True)
        if not ok:
            self.failures += 1

    def info(self, message: str) -> None:
        print(f"  {message}", flush=True)


# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------


def check_determinism(rivet: Rivet, report: Report, name: str, source: Callable[[Path], Path],
                      scratch: Path, seed: int) -> None:
    a = source(scratch / f"det-{name}-a")
    b = source(scratch / f"det-{name}-b")
    _, out_a = rivet.index(a, "--timing")
    _, out_b = rivet.index(b, "--timing")
    problems = []
    if normalize_index_output(out_a) != normalize_index_output(out_b):
        problems.append("index --json: " + first_difference(normalize_index_output(out_a), normalize_index_output(out_b)))
    store = Store(a)
    ids = sample_ids(store.symbols, [], seeded_rng(seed, "determinism", name), SAMPLE_OTHERS)
    count, mismatches = compare_queries(rivet, a, b, ids)
    problems += mismatches
    report.result(
        f"determinism[{name}]", not problems,
        f"index output identical apart from timing; {count} queries over {len(ids)} of "
        f"{len(store.symbols)} symbols byte-identical" if not problems else f"{len(problems)} difference(s)",
        problems,
    )


def coverage_summary(index_json: dict, store: Store) -> dict:
    coverage = index_json["index"]["coverage"]
    tiers: Dict[str, Dict[str, int]] = {}
    for b in store.bindings:
        lang = store.language_of(b["file"]) or "none"
        tiers.setdefault(lang, {"exact": 0, "scoped": 0})
        tiers[lang][b["resolution"]] += 1
    symbols_by_lang: Dict[str, int] = {}
    for s in store.symbols.values():
        lang = store.language_of(s["file"]) or "none"
        symbols_by_lang[lang] = symbols_by_lang.get(lang, 0) + 1
    files_by: Dict[str, int] = {}
    for _, (lang, status) in store.files.items():
        key = f"{lang or 'none'}:{status}"
        files_by[key] = files_by.get(key, 0) + 1
    return {
        "files_seen": coverage["files_seen"],
        "files_indexed": coverage["files_indexed"],
        "skipped": coverage["skipped"],
        "complete": coverage["complete"],
        "files_by_language_status": dict(sorted(files_by.items())),
        "symbols": index_json["symbols"],
        "symbols_by_language": dict(sorted(symbols_by_lang.items())),
        "uses": index_json["uses"],
        "uses_by_language": dict(sorted(store.uses_by_language.items())),
        "bindings": index_json["bindings"],
        "bindings_by_language_tier": dict(sorted(tiers.items())),
        "parse_failures": sorted(p for p, (_, st) in store.files.items() if st == "parse_error"),
        "resource_limits": sorted(p for p, (_, st) in store.files.items() if st == "resource_limit"),
        "diagnostics": [(d["file"], d["code"], d["detail"]) for d in index_json["index"]["diagnostics"]["items"]],
    }


def check_coverage(rivet: Rivet, report: Report, name: str, source: Callable[[Path], Path], scratch: Path) -> None:
    root = source(scratch / f"cov-{name}")
    data, _ = rivet.index(root)
    store = Store(root)
    summary = coverage_summary(data, store)
    report.data.setdefault("coverage", {})[name] = summary
    problems = []
    cov = data["index"]["coverage"]
    if cov["files_seen"] != cov["files_indexed"] + sum(cov["skipped"].values()):
        problems.append("files_seen != files_indexed + sum(skipped)")
    if cov["complete"] != (sum(cov["skipped"].values()) == 0):
        problems.append("complete disagrees with the skip counts")
    if len(store.files) != cov["files_seen"]:
        problems.append(f"store has {len(store.files)} file rows, coverage says {cov['files_seen']} seen")
    by_status: Dict[str, int] = {}
    for _, (_, status) in store.files.items():
        by_status[status] = by_status.get(status, 0) + 1
    if by_status.get("ok", 0) != cov["files_indexed"]:
        problems.append(f"store has {by_status.get('ok', 0)} ok files, coverage says {cov['files_indexed']} indexed")
    for key, count in cov["skipped"].items():
        if by_status.get(key, 0) != count:
            problems.append(f"skipped.{key} {count} but {by_status.get(key, 0)} store rows")
    if data["symbols"] != len(store.symbols):
        problems.append(f"symbols {data['symbols']} vs {len(store.symbols)} in store")
    if data["uses"] != store.use_count:
        problems.append(f"uses {data['uses']} vs {store.use_count} in store")
    if data["bindings"] != len(store.bindings):
        problems.append(f"bindings {data['bindings']} vs {len(store.bindings)} in store")
    diag_parse = sorted(d["file"] for d in data["index"]["diagnostics"]["items"] if d["code"] == "parse_error")
    if not data["index"]["diagnostics"]["truncated"] and diag_parse != summary["parse_failures"]:
        problems.append(f"parse_error diagnostics {diag_parse} vs store {summary['parse_failures']}")
    tiers = summary["bindings_by_language_tier"]
    report.result(
        f"coverage[{name}]", not problems,
        (f"{cov['files_seen']} seen, {cov['files_indexed']} indexed, skipped "
         + ", ".join(f"{k} {v}" for k, v in cov["skipped"].items() if v)
         + f"; {data['symbols']} symbols, {data['uses']} uses, {data['bindings']} bindings ("
         + "; ".join(f"{lang}: exact {t['exact']}, scoped {t['scoped']}" for lang, t in tiers.items())
         + ")"
         + (f"; parse failures: {', '.join(summary['parse_failures'])}" if summary["parse_failures"] else "")),
        problems,
    )


def run_incremental(rivet: Rivet, report: Report, name: str, source: Callable[[Path], Path],
                    languages: Sequence[str], scratch: Path, seed: int) -> None:
    inc = source(scratch / f"inc-{name}-incremental")
    full = source(scratch / f"inc-{name}-full")
    rivet.index(inc)
    rivet.index(full)
    steps_run = 0
    failures: List[str] = []
    total_queries = 0
    state: Dict[str, tuple] = {}
    for language in languages:
        for step_index, kind in enumerate(EDIT_SCRIPT):
            label = f"{language}/{step_index + 1}-{kind}"
            before = Store(inc)
            try:
                step = plan_step(kind, language, before, seeded_rng(seed, name, label), state)
            except NoCandidate as error:
                failures.append(f"{label}: could not plan: {error}")
                report.result(f"incremental[{name}] {label}", False, f"could not plan: {error}")
                continue
            step.apply(inc)
            step.apply(full)
            if tree_digest(inc) != tree_digest(full):
                raise RuntimeError(f"{label}: the two trees differ after applying the same edit")
            inc_json, _ = rivet.index(inc)
            full_json, _ = rivet.index(full, "--force")
            after_inc = Store(inc)
            after_full = Store(full)
            problems = []
            if inc_json["index"]["snapshot"] != full_json["index"]["snapshot"]:
                problems.append(
                    f"snapshot digest differs: incremental {inc_json['index']['snapshot']} vs "
                    f"force {full_json['index']['snapshot']}")
            for key in ("coverage", "diagnostics"):
                if inc_json["index"][key] != full_json["index"][key]:
                    problems.append(f"index.{key} differs: {inc_json['index'][key]} vs {full_json['index'][key]}")
            for key in ("symbols", "uses", "bindings"):
                if inc_json[key] != full_json[key]:
                    problems.append(f"{key} {inc_json[key]} vs {full_json[key]}")
            if step.expect_parse_error:
                status = after_full.files.get(step.expect_parse_error, (None, "missing"))[1]
                if status != "parse_error":
                    problems.append(f"{step.expect_parse_error} is {status}, expected parse_error")
            else:
                for path in step.touched():
                    entry = after_full.files.get(path)
                    if entry and entry[0] in LANG_EXT and entry[1] != "ok":
                        problems.append(f"planner error: edited {path} is {entry[1]}")
            edited = step.touched() | step.related
            dependents = before.dependents(edited) | after_inc.dependents(edited) | after_full.dependents(edited)
            must = (before.ids_in(edited | dependents) | after_inc.ids_in(edited | dependents)
                    | after_full.ids_in(edited | dependents))
            universe = set(after_inc.symbols) | set(after_full.symbols)
            ids = sample_ids(universe | must, must, seeded_rng(seed, name, label, "sample"), SAMPLE_OTHERS)
            count, mismatches = compare_queries(rivet, inc, full, ids)
            total_queries += count
            problems += mismatches
            steps_run += 1
            failures += [f"{label}: {p}" for p in problems]
            report.result(
                f"incremental[{name}] {label}", not problems,
                f"{step.description}; {len(edited)} edited, {len(dependents)} dependent file(s); "
                f"{count} queries over {len(ids)} symbols ({len(must)} from edited/dependent files) identical to --force"
                if not problems else f"{step.description}: {len(problems)} difference(s)",
                problems,
            )
    report.result(
        f"incremental[{name}]", not failures,
        f"{steps_run} steps, {total_queries} queries per store, all identical" if not failures
        else f"{len(failures)} problem(s) over {steps_run} steps",
    )


def check_two_languages(rivet: Rivet, report: Report, scratch: Path) -> None:
    php = copy_tree(PHP_FIXTURE, scratch / "lang-php")
    ts = copy_tree(TS_FIXTURE, scratch / "lang-ts")
    mixed = make_mixed(scratch / "lang-mixed")
    for root in (php, ts, mixed):
        rivet.index(root)
    s_php, s_ts, s_mixed = Store(php), Store(ts), Store(mixed)
    problems = []
    counts = {}
    for label, single, s_single, language in (("php", php, s_php, "php"), ("typescript", ts, s_ts, "typescript")):
        ids = sorted(s_single.symbols)
        mixed_ids = sorted(sid for sid, s in s_mixed.symbols.items() if s_mixed.language_of(s["file"]) == language)
        if ids != mixed_ids:
            problems.append(f"{label}: symbol ids differ between the single-language and mixed trees")
        count, mismatches = compare_queries(rivet, single, mixed, ids, normalize=without_index_block)
        counts[label] = (len(ids), count)
        problems += [f"{label}: {m}" for m in mismatches]
    # No binding crosses languages.
    crossing = [
        f"{b['file']}:{b['line']}:{b['col']} {b['spelling']} -> {b['target']}"
        for b in s_mixed.bindings
        if s_mixed.language_of(b["file"]) != s_mixed.language_of(s_mixed.symbols[b["target"]]["file"])
    ]
    problems += [f"cross-language binding: {c}" for c in crossing]
    # No reference-mode (or candidate-mode) row lists a use in another language.
    listed = 0
    jobs = []
    for sid in sorted(s_mixed.symbols):
        for mode in ("references", "candidates"):
            jobs.append((sid, mode, (mixed, ["refs", sid, "--mode", mode, "--limit", "1000", "--json", "--no-refresh"])))
    results = rivet.many([job for _, _, job in jobs])
    for (sid, mode, _), (code, out, err) in zip(jobs, results):
        if code != 0:
            problems.append(f"refs {mode} {sid} exited {code}: {err[:200]!r}")
            continue
        data = json.loads(out)
        if data["total"] > len(data["references"]):
            problems.append(f"refs {mode} {sid}: more than one page; raise --limit")
        target_lang = s_mixed.language_of(s_mixed.symbols[sid]["file"])
        for ref in data["references"]:
            listed += 1
            if s_mixed.language_of(ref["file"]) != target_lang:
                problems.append(f"refs {mode} {sid} lists {ref['file']}:{ref['line']} ({ref['ref_kind']})")
    names_php = {s["name"].lower() for s in s_php.symbols.values()}
    shared = sorted({s["name"] for s in s_ts.symbols.values() if s["name"].lower() in names_php})
    report.result(
        "two-language", not problems,
        (f"PHP {counts['php'][0]} symbols / {counts['php'][1]} queries and TypeScript "
         f"{counts['typescript'][0]} symbols / {counts['typescript'][1]} queries identical to their "
         f"single-language trees apart from `index`; 0 of {len(s_mixed.bindings)} bindings and 0 of "
         f"{listed} refs rows (both modes) cross languages; names shared across languages: "
         f"{', '.join(shared)}") if not problems else f"{len(problems)} problem(s)",
        problems,
    )


def check_budget(rivet: Rivet, report: Report, name: str, root: Path, seed: int) -> None:
    store = Store(root)
    ids = sample_ids(store.symbols, [], seeded_rng(seed, "budget", name), BUDGET_SAMPLE)
    jobs = [(sid, budget, (root, ["context", sid, "--tokens", str(budget), "--json", "--no-refresh"]))
            for sid in ids for budget in BUDGETS]
    results = rivet.many([job for _, _, job in jobs])
    problems = []
    fitted = too_small = segments = binding = 0
    fill = []
    for (sid, budget, _), (code, out, err) in zip(jobs, results):
        where = f"context {sid} --tokens {budget}"
        if code == 8:
            data = json.loads(err)
            too_small += 1
            if data.get("error") != "budget_too_small" or data.get("budget_tokens") != budget:
                problems.append(f"{where}: bad error object")
            elif not data["required_tokens"] > budget:
                problems.append(f"{where}: required_tokens {data['required_tokens']} fits the budget")
            full = estimate_tokens(store.slice(store.symbols[sid]).decode("utf-8"))
            if data.get("required_tokens", 0) > full:
                problems.append(f"{where}: required_tokens {data['required_tokens']} exceeds the full form {full}")
            continue
        if code != 0:
            problems.append(f"{where}: exit {code}: {err[:200]!r}")
            continue
        data = json.loads(out)
        fitted += 1
        if data["budget_tokens"] != budget:
            problems.append(f"{where}: budget_tokens {data['budget_tokens']}")
        if data["tokenizer"] != "utf8-bytes-v1" or data["budget_scope"] != "source":
            problems.append(f"{where}: tokenizer/budget_scope {data['tokenizer']}/{data['budget_scope']}")
        total = 0
        for seg in data["segments"]:
            segments += 1
            est = estimate_tokens(seg["source"])
            if seg["estimated_tokens"] != est:
                problems.append(f"{where}: segment {seg['symbol']['id']} reports {seg['estimated_tokens']}, source is {est}")
            total += seg["estimated_tokens"]
            if seg["form"] == "full":
                sym = store.symbols.get(seg["symbol"]["id"])
                if sym is None or store.slice(sym).decode("utf-8") != seg["source"]:
                    problems.append(f"{where}: full segment {seg['symbol']['id']} is not its stored span")
        if data["estimated_tokens"] != total:
            problems.append(f"{where}: estimated_tokens {data['estimated_tokens']} != segment sum {total}")
        if data["estimated_tokens"] > budget:
            problems.append(f"{where}: estimated_tokens {data['estimated_tokens']} > budget {budget}")
        if not data["segments"] or data["segments"][0]["reason"] != "target" or data["segments"][0]["symbol"]["id"] != sid:
            problems.append(f"{where}: first segment is not the target")
        fill.append(data["estimated_tokens"] / budget)
        if data["omitted"]["budget"] > 0:
            binding += 1
    report.data["budget"] = {"queries": len(jobs), "fitted": fitted, "budget_too_small": too_small,
                             "budget_omitted_candidates": binding,
                             "segments": segments, "median_fill": round(statistics.median(fill), 3) if fill else None}
    report.result(
        f"budget[{name}]", not problems,
        f"{len(jobs)} context queries ({len(ids)} symbols x budgets {', '.join(map(str, BUDGETS))}): "
        f"{fitted} fitted with {segments} segments, every estimate <= budget, every segment estimate = "
        f"ceil(bytes/3), every full segment its stored span; {too_small} budget_too_small (exit 8) with "
        f"required_tokens > budget; {binding} fitted answers left candidates out for budget; median fill "
        f"{report.data['budget']['median_fill']}"
        if not problems else f"{len(problems)} problem(s)",
        problems,
    )


def timings(rivet: Rivet, report: Report, root: Path) -> None:
    """benchmark/corpus.toml's indicative protocol, with `elapsed_ms`."""
    cold = []
    for _ in range(5):
        shutil.rmtree(root / ".rivet", ignore_errors=True)
        cold.append(rivet.index(root, "--timing")[0]["elapsed_ms"])
    noop = [rivet.index(root, "--timing")[0]["elapsed_ms"] for _ in range(3)]
    force = rivet.index(root, "--timing", "--force")[0]["elapsed_ms"]
    report.data["timings"] = {"cold_ms": statistics.median(cold), "cold_runs": cold,
                              "noop_refresh_ms": statistics.median(noop), "noop_runs": noop,
                              "force_ms": force}
    report.info(f"timings: cold_ms {statistics.median(cold)} (runs {cold}), noop_refresh_ms "
                f"{statistics.median(noop)} (runs {noop}), force_ms {force}")


MODULE_CANDIDATES = ("", ".ts", ".tsx", ".d.ts", "/index.ts", "/index.tsx", "/index.d.ts")


def import_outcomes(db: Path, store: Store) -> Dict[str, int]:
    """Why each named or default TypeScript import binding is, or is not,
    bound, by re-deriving docs/ADDING-A-LANGUAGE.md's relative-module
    candidate rule from the stored facts. Mechanical and audit-only."""
    con = open_readonly(db)
    try:
        facts = {(f, k): json.loads(j) for f, k, j in con.execute("SELECT file, scope_key, facts_json FROM scopes")}
        bound = {(f, s) for f, s in con.execute(
            "SELECT u.file, u.start_byte FROM uses u JOIN bindings b ON b.use_id = u.use_id"
            " WHERE u.ref_kind = 'import'")}
    finally:
        con.close()
    reexports: Dict[str, List[dict]] = {}
    import_locals: Dict[str, Set[str]] = {}
    exports: Dict[str, List[dict]] = {}
    for (path, _), fact in facts.items():
        for imp in fact.get("module_imports", []):
            if imp["kind"] in ("re_export", "re_export_all"):
                reexports.setdefault(path, []).append(imp)
            elif imp.get("local"):
                import_locals.setdefault(path, set()).add(imp["local"])
        exports.setdefault(path, []).extend(fact.get("module_exports", []))
    out: Dict[str, int] = {}
    for (path, _), fact in sorted(facts.items()):
        if store.language_of(path) != "typescript":
            continue
        for imp in fact.get("module_imports", []):
            if imp["kind"] not in ("named", "default"):
                continue
            spec = imp["specifier"]
            if (path, imp["span"]["start_byte"]) in bound:
                key = "bound (exact)"
            elif not spec.startswith(("./", "../")):
                key = ("bare '.' or '..' specifier" if spec in (".", "..")
                       else "path alias" if spec.startswith(("@/", "~/", "#"))
                       else "package or builtin")
            else:
                joined = os.path.normpath(os.path.join(os.path.dirname(path), spec)).replace(os.sep, "/")
                found = [joined + c for c in MODULE_CANDIDATES if (joined + c) in store.files]
                found = [p for p in found if not p.endswith(("/", "."))]
                if not found:
                    key = "relative, no candidate module (for example a .js specifier)"
                elif len(found) > 1:
                    key = "relative, ambiguous module"
                elif store.files[found[0]][1] != "ok":
                    key = f"relative, module not indexed ({store.files[found[0]][1]})"
                elif any(r["kind"] == "re_export_all" or r.get("exported") == imp["imported"]
                         for r in reexports.get(found[0], [])):
                    key = "relative, re-export (export ... from)"
                elif any(e.get("exported") == imp["imported"] and e.get("local") in import_locals.get(found[0], ())
                         for e in exports.get(found[0], [])):
                    key = "relative, exported import binding (import { a }; export { a })"
                else:
                    key = "relative, other (not exported, anonymous default, ...)"
            out[key] = out.get(key, 0) + 1
    return out


def audit(rivet: Rivet, root: Path, seed: int) -> None:
    """Prints the seeded honesty-audit sample for reading against the source."""
    rivet.index(root)
    store = Store(root)

    def line_of(path: str, line: int) -> str:
        text = store.source(path).decode("utf-8", "replace").split("\n")
        return text[line - 1].strip() if 0 < line <= len(text) else ""

    for tier in ("exact", "scoped"):
        pool = [b for b in store.bindings if b["resolution"] == tier]
        rng = seeded_rng(seed, "audit", tier)
        picked = sorted(rng.sample(range(len(pool)), min(AUDIT_SAMPLE, len(pool))))
        print(f"== {tier}: {len(picked)} of {len(pool)}")
        for n, i in enumerate(picked, 1):
            b = pool[i]
            t = store.symbols[b["target"]]
            print(f"[{tier[0].upper()}{n:02d}] {b['file']}:{b['line']}:{b['col']} {b['ref_kind']} "
                  f"{b['spelling']!r} recv={b['receiver']!r}")
            print(f"      use:    {line_of(b['file'], b['line'])[:160]}")
            print(f"      target: {t['id']} ({t['kind']}) at {t['file']}:{t['line']}: "
                  f"{line_of(t['file'], t['line'])[:120]}")
    pool = store.unresolved_member_calls
    tally: Dict[str, int] = {}
    for u in pool:
        key = receiver_shape(u["receiver"], json.loads(u["hint"]))
        tally[key] = tally.get(key, 0) + 1
    print(f"== unresolved member calls by receiver shape (mechanical, all {len(pool)}):")
    for key, count in sorted(tally.items(), key=lambda kv: (-kv[1], kv[0])):
        print(f"   {count:6d}  {100 * count / len(pool):5.1f}%  {key}")
    imports = import_outcomes(root / ".rivet" / "index.db", store)
    print(f"== named/default import bindings by outcome (mechanical, all {sum(imports.values())}):")
    for key, count in sorted(imports.items(), key=lambda kv: (-kv[1], kv[0])):
        print(f"   {count:6d}  {key}")
    rng = seeded_rng(seed, "audit", "unresolved")
    picked = sorted(rng.sample(range(len(pool)), min(AUDIT_SAMPLE, len(pool))))
    print(f"== unresolved member calls: {len(picked)} of {len(pool)}")
    for n, i in enumerate(picked, 1):
        u = pool[i]
        print(f"[U{n:02d}] {u['file']}:{u['line']}:{u['col']} {u['spelling']!r} recv={u['receiver']!r} hint={u['hint']}")
        print(f"      use:    {line_of(u['file'], u['line'])[:160]}")


# ---------------------------------------------------------------------------


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--rivet", default=str(REPO / "target" / "release" / "rivet"),
                        help="rivet binary (default: target/release/rivet)")
    parser.add_argument("--real-dir", default=os.environ.get("RIVET_REAL_DIR", str(REPO / "target" / "real")),
                        help="where tests/real/fetch.sh put the project (default: $RIVET_REAL_DIR or target/real)")
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--jobs", type=int, default=min(12, os.cpu_count() or 4))
    parser.add_argument("--only", default="determinism,coverage,incremental,two-language,budget,timings",
                        help="comma-separated checks to run")
    parser.add_argument("--targets", default="hono,php,typescript,mixed",
                        help="comma-separated trees for coverage, determinism, and incremental "
                             "(the real project's name, php, typescript, mixed)")
    parser.add_argument("--audit", action="store_true", help="print the honesty-audit sample and exit")
    parser.add_argument("--json-out", help="write the recorded counts to this file")
    parser.add_argument("--keep", action="store_true", help="keep the temporary directory")
    args = parser.parse_args(argv)

    binary = Path(args.rivet)
    if not binary.is_absolute():
        binary = (REPO / binary) if (REPO / binary).exists() else binary.resolve()
    if not os.access(binary, os.X_OK):
        print(f"check_real: no executable rivet at {binary}; run `cargo build --release`", file=sys.stderr)
        return 2
    project = read_manifest()
    checkout = Path(args.real_dir) / project["name"]
    head = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"],
                          capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if head.returncode != 0 or head.stdout.strip() != project["commit"]:
        print(f"check_real: {checkout} is not at {project['commit']}; run tests/real/fetch.sh", file=sys.stderr)
        return 2

    rivet = Rivet(binary, args.jobs)
    real = project["name"]
    only = set(args.only.split(","))
    scratch = Path(tempfile.mkdtemp(prefix="rivet-t46-"))
    report = Report()
    print(f"check_real: rivet {binary} ({subprocess.run([str(binary), '--version'], capture_output=True, text=True).stdout.strip()}); "
          f"{real} {project['tag']} at {project['commit']}; seed {args.seed}; scratch {scratch}", flush=True)
    try:
        sources = {
            real: lambda dst: copy_tree(checkout, dst),
            "php": lambda dst: copy_tree(PHP_FIXTURE, dst),
            "typescript": lambda dst: copy_tree(TS_FIXTURE, dst),
            "mixed": make_mixed,
        }
        if args.audit:
            audit(rivet, sources[real](scratch / "audit"), args.seed)
            return 0
        targets = set(args.targets.split(","))
        if "coverage" in only:
            for name, source in sources.items():
                if name in targets:
                    check_coverage(rivet, report, name, source, scratch)
        if "determinism" in only:
            for name, source in sources.items():
                if name in targets:
                    check_determinism(rivet, report, name, source, scratch, args.seed)
        if "two-language" in only:
            check_two_languages(rivet, report, scratch)
        if "budget" in only:
            root = sources[real](scratch / "budget")
            rivet.index(root)
            check_budget(rivet, report, real, root, args.seed)
        if "incremental" in only:
            plans = [(real, ["typescript"]), ("php", ["php"]), ("typescript", ["typescript"]),
                     ("mixed", ["php", "typescript"])]
            for name, languages in plans:
                if name in targets:
                    run_incremental(rivet, report, name, sources[name], languages, scratch, args.seed)
        if "timings" in only:
            timings(rivet, report, sources[real](scratch / "timings"))
    finally:
        if args.keep:
            print(f"check_real: kept {scratch}")
        else:
            shutil.rmtree(scratch, ignore_errors=True)
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(report.data, indent=2) + "\n")
    print(f"check_real: {'FAIL' if report.failures else 'PASS'} ({report.failures} failed check(s))")
    return 1 if report.failures else 0


if __name__ == "__main__":
    sys.exit(main())
