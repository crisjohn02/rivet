#!/usr/bin/env python3
"""Verify tests/gold/php-authored.toml against the authored PHP fixture.

Slices every recorded byte span out of the named fixture file and checks the
recorded text plus the line/column or line range derived from the offset.
Stdlib only. Exits non-zero and prints the failing entry on any mismatch.

TypeScript gold (T41). tests/gold/typescript-authored.toml tags every entry
with the task that makes it verifiable (T42-T45) and lists the finished ones
in `done_tasks`. Only entries of a done task are span-verified here (the
recorded text, and the line and column or line range, as for PHP entries);
every other entry is counted as pending, per task, and printed, never skipped
silently. A done task that still has an [[undecided]] entry fails.
Independently of task state, a span self-check requires every TypeScript
entry's recorded text to be the fixture's bytes at its span, so a pending
entry is never unchecked. An unknown table, an unknown or missing task tag,
or an unknown `done_tasks` value is an error.

This script never runs rivet, so it never claims that rivet produced a
TypeScript entry. Comparing a done task's entries with rivet's output is owned
by Rust tests that `cargo test` runs, named per task in TS_HARNESS (T42: the
extractor comparison in crates/rivet-languages/tests/typescript_gold.rs; T43:
that comparison plus the end-to-end check through the CLI in
crates/rivet-cli/tests/typescript_index.rs; T44: the extractor-and-resolver
comparison in crates/rivet-index/tests/typescript_gold.rs plus the same
end-to-end CLI check). The report names those harnesses and says they are not
run here; a done task with no harness listed fails.

Private corpus gold (T35). The benchmark corpus is private, so its gold
samples live outside this repository. When both environment variables are
set, every `<name>.toml` in RIVET_PRIVATE_GOLD_DIR is also verified against
the copy at RIVET_CORPUS_DIR/<name>/, whose detached HEAD must equal the
sample's `commit`. When either variable is unset, or a project's copy is
absent, that part is skipped with a message; the authored check still runs.
Private entries print only as failures, so paths stay out of routine logs.
"""
import os
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
GOLD = HERE / "php-authored.toml"
FIXTURES = HERE.parent / "fixtures" / "php" / "authored"
TS_GOLD = HERE / "typescript-authored.toml"
TS_FIXTURES = HERE.parent / "fixtures" / "typescript" / "authored"
TS_TASKS = ("T42", "T43", "T44", "T45")
# The Rust tests that compare each done task's entries with rivet, and the
# commands that run them. check_gold.py does not run them.
TS_HARNESS = {
    "T42": [("crates/rivet-languages/tests/typescript_gold.rs",
             "cargo test -p rivet-languages --test typescript_gold")],
    "T43": [("crates/rivet-languages/tests/typescript_gold.rs",
             "cargo test -p rivet-languages --test typescript_gold"),
            ("crates/rivet-cli/tests/typescript_index.rs",
             "cargo test -p rivet-cli --test typescript_index")],
    "T44": [("crates/rivet-index/tests/typescript_gold.rs",
             "cargo test -p rivet-index --test typescript_gold"),
            ("crates/rivet-cli/tests/typescript_index.rs",
             "cargo test -p rivet-cli --test typescript_index")],
}
# Each table and whether its spans are declaration-like (a line range plus
# `text`/`end_text`) or use-like (line and column plus the exact `text`).
TS_TABLES = (
    ("declaration", True),
    ("not_a_declaration", True),
    ("use", False),
    ("not_a_use", False),
    ("binding", False),
    ("undecided", None),
)
TS_KEYS = {"done_tasks", "focus_names"} | {name for name, _ in TS_TABLES}


def line_col(data, off):
    line = data[:off].count(b"\n") + 1
    column = off - (data.rfind(b"\n", 0, off) + 1) + 1
    return line, column


def check(label, entry, data):
    start, end = entry["start_byte"], entry["end_byte"]
    if not (0 <= start < end <= len(data)):
        return "span [%d, %d) is outside %d bytes" % (start, end, len(data))
    actual = data[start:end]
    text = entry["text"].encode("utf-8")
    if actual[:len(text)] != text:
        return "text mismatch: got %r, recorded %r" % (actual[:len(text)], text)
    if label == "declaration":
        if text != data[start:end][:40]:
            return "declaration text is not the first 40 bytes"
        start_line, _ = line_col(data, start)
        end_line, _ = line_col(data, end - 1)
        if (start_line, end_line) != (entry["start_line"], entry["end_line"]):
            return "lines %d-%d, recorded %d-%d" % (
                start_line, end_line, entry["start_line"], entry["end_line"])
    else:
        got = line_col(data, start)
        if got != (entry["line"], entry["column"]):
            return "line/column %d:%d, recorded %d:%d" % (
                got[0], got[1], entry["line"], entry["column"])
    return None


def ts_self_check(entry, data, block):
    """The span self-check: the recorded text is the fixture's bytes there."""
    start, end = entry["start_byte"], entry["end_byte"]
    if not (0 <= start < end <= len(data)):
        return "span [%d, %d) is outside %d bytes" % (start, end, len(data))
    actual = data[start:end]
    text = entry["text"].encode("utf-8")
    if block:
        end_text = entry["end_text"].encode("utf-8")
        if not text or not actual.startswith(text):
            return "text mismatch: span starts %r, recorded %r" % (actual[:len(text)], text)
        if not end_text or not actual.endswith(end_text):
            return "end_text mismatch: span ends %r, recorded %r" % (
                actual[-len(end_text):], end_text)
    elif actual != text:
        return "text mismatch: got %r, recorded %r" % (actual, text)
    return None


def ts_verify(entry, data, block):
    """Full verification of a done task's entry: text, then lines/column."""
    error = ts_self_check(entry, data, block)
    if error:
        return error
    start, end = entry["start_byte"], entry["end_byte"]
    if block:
        lines = (line_col(data, start)[0], line_col(data, end - 1)[0])
        if lines != (entry["start_line"], entry["end_line"]):
            return "lines %d-%d, recorded %d-%d" % (
                lines[0], lines[1], entry["start_line"], entry["end_line"])
    elif line_col(data, start) != (entry["line"], entry["column"]):
        got = line_col(data, start)
        return "line/column %d:%d, recorded %d:%d" % (
            got[0], got[1], entry["line"], entry["column"])
    return None


def check_typescript():
    """Verify done-task TypeScript entries and report the rest as pending.

    Returns the number of failures.
    """
    with TS_GOLD.open("rb") as fh:
        gold = tomllib.load(fh)
    failures = 0
    unknown = sorted(set(gold) - TS_KEYS)
    if unknown:
        failures += 1
        print("FAIL typescript gold: unknown key(s) or table(s) %s" % ", ".join(unknown))
    done = gold.get("done_tasks")
    if not isinstance(done, list) or any(task not in TS_TASKS for task in done):
        failures += 1
        print("FAIL typescript gold: done_tasks %r must list tasks from %s" % (
            done, ", ".join(TS_TASKS)))
        done = []
    for task in done:
        if task not in TS_HARNESS:
            failures += 1
            print("FAIL typescript gold: done task %s names no harness in TS_HARNESS, "
                  "so nothing compares its entries with rivet" % task)
    cache = {}
    verified = 0
    per_task = {task: 0 for task in TS_TASKS}
    self_checked = 0
    self_failed = 0
    pending = {task: 0 for task in TS_TASKS}
    for table, block in TS_TABLES:
        for entry in gold.get(table, []):
            name = entry.get("file", "")
            task = entry.get("task")
            if task not in TS_TASKS:
                failures += 1
                print("FAIL typescript %s %s: task %r is not one of %s" % (
                    table, name, task, ", ".join(TS_TASKS)))
                continue
            path = TS_FIXTURES / name
            if not name or not path.is_file():
                failures += 1
                print("FAIL typescript %s %r: no such fixture file" % (table, name))
                continue
            if name not in cache:
                cache[name] = path.read_bytes()
            is_block = entry.get("form") == "declaration" if block is None else block
            error = ts_self_check(entry, cache[name], is_block)
            self_checked += 1
            if error:
                failures += 1
                self_failed += 1
                print("FAIL typescript self-check %s %s: %s" % (table, name, error))
                print("     entry: %r" % (entry,))
                continue
            if task not in done:
                pending[task] += 1
                continue
            if table == "undecided":
                failures += 1
                print("FAIL typescript undecided %s [%d, %d): %s is done but this "
                      "construct is still undecided" % (
                          name, entry["start_byte"], entry["end_byte"], task))
                continue
            error = ts_verify(entry, cache[name], is_block)
            verified += 1
            per_task[task] += 1
            if error:
                failures += 1
                print("FAIL typescript %s %s: %s" % (table, name, error))
                print("     entry: %r" % (entry,))
            else:
                print("ok   %-17s %-26s [%d, %d)" % (
                    table, name, entry["start_byte"], entry["end_byte"]))
    print("span self-check: %d of %d typescript entries match the fixture bytes" % (
        self_checked - self_failed, self_checked))
    print("span-verified %d typescript entries of done tasks (done: %s)" % (
        verified, ", ".join(done) if done else "none"))
    for task in done:
        if task in TS_HARNESS:
            owners = "; ".join("%s (`%s`)" % (harness, command)
                               for harness, command in TS_HARNESS[task])
            print("typescript %s: %d entries span-verified here; their comparison with "
                  "rivet is owned by %s, which this script does not run" % (
                      task, per_task[task], owners))
    total = sum(pending.values())
    if total:
        detail = ", ".join("%s: %d" % (task, count) for task, count in pending.items() if count)
        print("pending %d typescript entries (%s)" % (total, detail))
    else:
        print("pending 0 typescript entries")
    return failures


def detached_head(checkout):
    """Return the SHA a checkout's HEAD is detached at, or None."""
    git = checkout / ".git"
    if not git.is_dir():
        return None
    head = (git / "HEAD").read_text(encoding="ascii").strip()
    return head if len(head) == 40 and all(c in "0123456789abcdef" for c in head) else None


def check_private():
    """Verify private gold samples. Returns (checked, failures)."""
    gold_dir = os.environ.get("RIVET_PRIVATE_GOLD_DIR", "")
    corpus_dir = os.environ.get("RIVET_CORPUS_DIR", "")
    if not gold_dir or not corpus_dir:
        print("skip private corpus gold: set RIVET_PRIVATE_GOLD_DIR and "
              "RIVET_CORPUS_DIR to verify it (see benchmark/CORPUS.md)")
        return 0, 0
    gold_dir, corpus_dir = Path(gold_dir), Path(corpus_dir)
    samples = sorted(gold_dir.glob("*.toml")) if gold_dir.is_dir() else []
    if not samples:
        print("skip private corpus gold: no *.toml samples in RIVET_PRIVATE_GOLD_DIR")
        return 0, 0
    checked = failures = 0
    for sample in samples:
        with sample.open("rb") as fh:
            gold = tomllib.load(fh)
        project, commit = gold["project"], gold["commit"]
        checkout = corpus_dir / project
        if not checkout.is_dir():
            print("skip private gold %s: no copy at RIVET_CORPUS_DIR/%s "
                  "(run benchmark/fetch-corpus.sh)" % (project, project))
            continue
        head = detached_head(checkout)
        if head != commit:
            failures += 1
            print("FAIL private gold %s: copy HEAD is %s, sample pins %s" % (
                project, head or "not detached", commit))
            continue
        cache = {}
        count = 0
        before = failures
        for label in ("declaration", "use", "not_a_use"):
            for entry in gold.get(label, []):
                name = entry["file"]
                if name not in cache:
                    cache[name] = (checkout / name).read_bytes()
                error = check(label, entry, cache[name])
                count += 1
                if error:
                    failures += 1
                    print("FAIL private %s %s %s: %s" % (project, label, name, error))
        checked += count
        if failures == before:
            print("verified %d private gold entries for %s" % (count, project))
        else:
            print("%d of %d private gold entries failed for %s" % (
                failures - before, count, project))
    return checked, failures


def main():
    with GOLD.open("rb") as fh:
        gold = tomllib.load(fh)
    cache = {}
    checked = 0
    failures = 0
    for label in ("declaration", "use", "not_a_use"):
        for entry in gold.get(label, []):
            name = entry["file"]
            if name not in cache:
                cache[name] = (FIXTURES / name).read_bytes()
            error = check(label, entry, cache[name])
            checked += 1
            if error:
                failures += 1
                print("FAIL %s %s: %s" % (label, name, error))
                print("     entry: %r" % (entry,))
            else:
                print("ok   %-11s %-18s [%d, %d)" % (
                    label, name, entry["start_byte"], entry["end_byte"]))
    if failures:
        print("%d of %d entries failed" % (failures, checked))
    else:
        print("verified %d gold entries" % checked)
    ts_failures = check_typescript()
    if ts_failures:
        print("%d typescript gold check(s) failed" % ts_failures)
    private_checked, private_failures = check_private()
    if private_failures:
        print("%d private gold check(s) failed (of %d entries)" % (
            private_failures, private_checked))
    return 1 if failures or ts_failures or private_failures else 0


if __name__ == "__main__":
    sys.exit(main())
