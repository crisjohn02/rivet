#!/usr/bin/env python3
"""Verify tests/gold/php-authored.toml against the authored PHP fixture.

Slices every recorded byte span out of the named fixture file and checks the
recorded text plus the line/column or line range derived from the offset.
Stdlib only. Exits non-zero and prints the failing entry on any mismatch.
"""
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
        return 1
    print("verified %d gold entries" % checked)
    return 0


if __name__ == "__main__":
    sys.exit(main())
