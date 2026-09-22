#!/usr/bin/env python3
"""Verify tests/gold/php-authored.toml against the authored PHP fixture.

Slices every recorded byte span out of the named fixture file and checks the
recorded text plus the line/column or line range derived from the offset.
Stdlib only. Exits non-zero and prints the failing entry on any mismatch.

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
    private_checked, private_failures = check_private()
    if private_failures:
        print("%d private gold check(s) failed (of %d entries)" % (
            private_failures, private_checked))
    return 1 if failures or private_failures else 0


if __name__ == "__main__":
    sys.exit(main())
