#!/usr/bin/env python3
"""Offline unit tests for tests/real/check_real.py's pure helpers (T46).

Needs neither the fetched project nor a rivet binary:

    python3 tests/real/test_check_real.py
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_real as cr  # noqa: E402


class NormalizerTests(unittest.TestCase):
    def test_drops_elapsed_ms_only(self):
        raw = b'{"schema_version":1,"index":{"snapshot":"blake3:x"},"symbols":3,"updated":1,"elapsed_ms":789}\n'
        self.assertEqual(
            cr.normalize_index_output(raw),
            b'{"schema_version":1,"index":{"snapshot":"blake3:x"},"symbols":3,"updated":1}\n',
        )

    def test_nested_timing_fields_are_dropped(self):
        self.assertEqual(cr.drop_timing({"a": [{"elapsed_ms": 1, "b": 2}]}), {"a": [{"b": 2}]})

    def test_two_runs_differing_only_in_timing_normalize_equal(self):
        a = b'{"schema_version":1,"symbols":3,"elapsed_ms":10}\n'
        b = b'{"schema_version":1,"symbols":3,"elapsed_ms":99}\n'
        self.assertEqual(cr.normalize_index_output(a), cr.normalize_index_output(b))

    def test_a_real_difference_survives_normalization(self):
        a = b'{"schema_version":1,"symbols":3,"elapsed_ms":10}\n'
        b = b'{"schema_version":1,"symbols":4,"elapsed_ms":10}\n'
        self.assertNotEqual(cr.normalize_index_output(a), cr.normalize_index_output(b))

    def test_key_order_is_preserved(self):
        raw = b'{"z":1,"a":2,"elapsed_ms":3}\n'
        self.assertEqual(cr.normalize_index_output(raw), b'{"z":1,"a":2}\n')

    def test_unicode_is_kept_as_utf8(self):
        raw = '{"s":"café","elapsed_ms":1}\n'.encode()
        self.assertEqual(cr.normalize_index_output(raw), '{"s":"café"}\n'.encode())

    def test_without_index_block(self):
        a = b'{"schema_version":1,"index":{"snapshot":"blake3:a"},"total":1}\n'
        b = b'{"schema_version":1,"index":{"snapshot":"blake3:b","coverage":{}},"total":1}\n'
        self.assertEqual(cr.without_index_block(a), cr.without_index_block(b))
        c = b'{"schema_version":1,"index":{},"total":2}\n'
        self.assertNotEqual(cr.without_index_block(a), cr.without_index_block(c))


class EstimateTests(unittest.TestCase):
    def test_utf8_bytes_over_three(self):
        self.assertEqual(cr.estimate_tokens(""), 0)
        self.assertEqual(cr.estimate_tokens("abc"), 1)
        self.assertEqual(cr.estimate_tokens("abcd"), 2)
        self.assertEqual(cr.estimate_tokens("…"), 1)  # three UTF-8 bytes
        self.assertEqual(cr.estimate_tokens("café"), 2)  # five bytes


class SamplerTests(unittest.TestCase):
    ids = [f"src/f{i}.ts#s{i}" for i in range(50)]

    def test_same_seed_same_sample(self):
        a = cr.sample_ids(self.ids, [], cr.seeded_rng(46, "x"), 10)
        b = cr.sample_ids(list(reversed(self.ids)), [], cr.seeded_rng(46, "x"), 10)
        self.assertEqual(a, b)
        self.assertEqual(len(a), 10)
        self.assertEqual(a, sorted(a))

    def test_other_seed_or_label_changes_sample(self):
        a = cr.sample_ids(self.ids, [], cr.seeded_rng(46, "x"), 10)
        self.assertNotEqual(a, cr.sample_ids(self.ids, [], cr.seeded_rng(47, "x"), 10))
        self.assertNotEqual(a, cr.sample_ids(self.ids, [], cr.seeded_rng(46, "y"), 10))

    def test_must_ids_always_included_and_not_counted(self):
        must = ["src/f3.ts#s3", "src/zz.ts#gone"]
        got = cr.sample_ids(self.ids, must, cr.seeded_rng(1), 5)
        self.assertTrue(set(must) <= set(got))
        self.assertEqual(len(got), 7)

    def test_small_population_takes_everything(self):
        self.assertEqual(cr.sample_ids(self.ids[:3], [], cr.seeded_rng(1), 200), sorted(self.ids[:3]))

    def test_string_seed_is_stable_across_processes(self):
        # random.Random(str) hashes with SHA-512, not hash(), so this value is fixed.
        self.assertEqual(cr.seeded_rng(46, "a").randrange(10**6), cr.seeded_rng(46, "a").randrange(10**6))
        self.assertEqual(cr.seeded_rng(46, "a").random(), __import__("random").Random("46:a").random())


class SpliceTests(unittest.TestCase):
    def test_insert_replace_delete(self):
        self.assertEqual(cr.splice(b"abcdef", [(1, 1, b"X")]), b"aXbcdef")
        self.assertEqual(cr.splice(b"abcdef", [(1, 3, b"XY")]), b"aXYdef")
        self.assertEqual(cr.splice(b"abcdef", [(0, 2, b"")]), b"cdef")

    def test_several_edits_in_any_order(self):
        self.assertEqual(cr.splice(b"foo(foo)", [(4, 7, b"barr"), (0, 3, b"barr")]), b"barr(barr)")

    def test_overlap_and_bad_range_refused(self):
        with self.assertRaises(ValueError):
            cr.splice(b"abcdef", [(0, 3, b""), (2, 4, b"")])
        with self.assertRaises(ValueError):
            cr.splice(b"abc", [(2, 9, b"")])


class DeclarationNameTests(unittest.TestCase):
    def test_typescript_method(self):
        src = b"async launch<T>(x: T): void { this.launch() }"
        self.assertEqual(cr.declaration_name_offset("typescript", "method", src, "launch"), 6)

    def test_typescript_private_and_static(self):
        src = b"static #reset(): void {}"
        self.assertEqual(cr.declaration_name_offset("typescript", "method", src, "#reset"), 7)
        # a prefix of a longer name is not the declaration
        self.assertIsNone(cr.declaration_name_offset("typescript", "method", b"launchAll() {}", "launch"))

    def test_php_method_and_class(self):
        src = b"public function &Launch(): void {}"
        self.assertEqual(cr.declaration_name_offset("php", "method", src, "launch"), 17)
        src = b"final class SurveyService extends Base {}"
        self.assertEqual(cr.declaration_name_offset("php", "class", src, "SurveyService"), 12)

    def test_utf8_offsets_are_bytes(self):
        src = "/** café */ run() {}".encode()
        self.assertEqual(cr.declaration_name_offset("typescript", "method", src, "run"), src.index(b"run"))


class PathTests(unittest.TestCase):
    def test_renamed_path(self):
        self.assertEqual(cr.renamed_path("src/a.ts", "-x"), "src/a-x.ts")
        self.assertEqual(cr.renamed_path("src/a.d.ts", "-x"), "src/a-x.d.ts")
        self.assertEqual(cr.renamed_path("a/B.tsx", "-x"), "a/B-x.tsx")
        self.assertEqual(cr.renamed_path("Foo.php", "-x"), "Foo-x.php")
        with self.assertRaises(ValueError):
            cr.renamed_path("a.js", "-x")

    def test_module_specifier(self):
        self.assertEqual(cr.module_specifier("src/new.ts", "src/util.ts"), "./util")
        self.assertEqual(cr.module_specifier("src/a/new.ts", "src/b/x.tsx"), "../b/x")
        self.assertEqual(cr.module_specifier("new.ts", "lib/index.ts"), "./lib/index")


class FakeStore(cr.Store):
    """A Store built from literals instead of SQLite, for the planner."""

    def __init__(self, files, symbols, bindings):
        self.files = {p: (lang, "ok") for p, (lang, _) in files.items()}
        self._sources = {p: src for p, (_, src) in files.items()}
        self.symbols = {}
        for sid, (path, name, kind, text) in symbols.items():
            start = self._sources[path].index(text)
            self.symbols[sid] = {"id": sid, "file": path, "name": name, "qualified_name": sid.split("#")[1],
                                 "kind": kind, "start": start, "end": start + len(text), "line": 1,
                                 "signature": None}
        self.bindings = []
        for path, needle, occurrence, target, kind in bindings:
            src = self._sources[path]
            start = -1
            for _ in range(occurrence + 1):
                start = src.index(needle, start + 1)
            self.bindings.append({"file": path, "start": start, "end": start + len(needle), "line": 1,
                                  "col": 1, "ref_kind": kind, "spelling": needle.decode(), "receiver": None,
                                  "container": None, "target": target, "resolution": "exact"})
        self.use_count = len(self.bindings)
        self.uses_by_language = {}
        self.unresolved_member_calls = []


UTIL = b"export function format(n: number) {\n  return String(n)\n}\n\nexport class Svc {\n  run(): void {\n  }\n}\n"
MAIN = b"import { format, Svc } from './util'\n\nconst s: Svc = new Svc()\ns.run()\nformat(1)\n"


def fake_store():
    return FakeStore(
        {"src/util.ts": ("typescript", UTIL), "src/main.ts": ("typescript", MAIN)},
        {
            "src/util.ts#format": ("src/util.ts", "format", "function",
                                   b"export function format(n: number) {\n  return String(n)\n}"),
            "src/util.ts#Svc": ("src/util.ts", "Svc", "class", b"export class Svc {\n  run(): void {\n  }\n}"),
            "src/util.ts#Svc.run": ("src/util.ts", "run", "method", b"run(): void {\n  }"),
        },
        [
            ("src/main.ts", b"format", 0, "src/util.ts#format", "import"),
            ("src/main.ts", b"Svc", 0, "src/util.ts#Svc", "import"),
            ("src/main.ts", b"run", 0, "src/util.ts#Svc.run", "call"),
            ("src/main.ts", b"format", 1, "src/util.ts#format", "call"),
        ],
    )


class PlannerTests(unittest.TestCase):
    def plan(self, kind, state=None, seed=46):
        return cr.plan_step(kind, "typescript", fake_store(), cr.seeded_rng(seed, kind), {} if state is None else state)

    def test_script_order(self):
        self.assertEqual(cr.EDIT_SCRIPT, ("modify_body", "rename_method", "remove_export", "add_file",
                                          "delete_file", "rename_file", "break_syntax", "fix_syntax"))

    def test_modify_body_inserts_before_closing_brace(self):
        step = self.plan("modify_body")
        new = step.writes["src/util.ts"]
        self.assertIn(b"  return String(n)\n\n  rivetT46Probe()\n}", new)
        self.assertEqual(new.replace(b"\n  rivetT46Probe()\n", b"", 1), UTIL)

    def test_rename_method_renames_declaration_and_bound_uses(self):
        step = self.plan("rename_method")
        self.assertIn(b"  runRivetT46(): void {", step.writes["src/util.ts"])
        self.assertIn(b"s.runRivetT46()\n", step.writes["src/main.ts"])
        self.assertEqual(step.touched(), {"src/util.ts", "src/main.ts"})

    def test_remove_export_drops_the_keyword(self):
        step = self.plan("remove_export")
        new = step.writes["src/util.ts"]
        self.assertEqual(len(new), len(UTIL) - len(b"export "))
        self.assertTrue(new.startswith(b"function format") or b"\nclass Svc" in new)

    def test_add_file_imports_an_existing_function(self):
        step = self.plan("add_file")
        self.assertEqual(list(step.writes), ["src/rivet-t46-new.ts"])
        self.assertTrue(step.writes["src/rivet-t46-new.ts"].startswith(b"import { format } from './util'\n"))
        self.assertEqual(step.related, {"src/util.ts"})

    def test_delete_and_rename_prefer_a_depended_file(self):
        self.assertEqual(self.plan("delete_file").deletes, ["src/util.ts"])
        self.assertEqual(self.plan("rename_file").renames, [("src/util.ts", "src/util-rivet-t46.ts")])

    def test_break_then_fix_restores_bytes(self):
        state = {}
        broken = self.plan("break_syntax", state)
        self.assertEqual(broken.expect_parse_error, "src/util.ts")
        self.assertTrue(broken.writes["src/util.ts"].startswith(UTIL))
        fixed = self.plan("fix_syntax", state)
        self.assertEqual(fixed.writes, {"src/util.ts": UTIL})
        with self.assertRaises(cr.NoCandidate):
            self.plan("fix_syntax", state)

    def test_plans_are_deterministic(self):
        for kind in cr.EDIT_SCRIPT[:-2]:
            a, b = self.plan(kind), self.plan(kind)
            self.assertEqual((a.writes, a.deletes, a.renames), (b.writes, b.deletes, b.renames))

    def test_apply_writes_deletes_renames(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / "src/util.ts").write_bytes(UTIL)
            (root / "src/main.ts").write_bytes(MAIN)
            before = cr.tree_digest(root)
            self.plan("rename_file").apply(root)
            self.assertFalse((root / "src/util.ts").exists())
            self.assertEqual((root / "src/util-rivet-t46.ts").read_bytes(), UTIL)
            self.assertNotEqual(cr.tree_digest(root), before)
            (root / ".rivet").mkdir()
            (root / ".rivet/index.db").write_bytes(b"x")
            digest = cr.tree_digest(root)
            (root / ".rivet/index.db").write_bytes(b"y")
            self.assertEqual(cr.tree_digest(root), digest)


class ManifestTests(unittest.TestCase):
    def test_manifest_pin(self):
        project = cr.read_manifest()
        self.assertEqual(project["name"], "hono")
        self.assertRegex(project["commit"], r"^[0-9a-f]{40}$")
        self.assertEqual(project["url"], "https://github.com/honojs/hono")


class DifferenceTests(unittest.TestCase):
    def test_first_difference(self):
        self.assertIn("at byte 3", cr.first_difference(b"abcdef", b"abcXef"))
        self.assertIn("lengths 3/4", cr.first_difference(b"abc", b"abcd"))


if __name__ == "__main__":
    unittest.main(verbosity=1)
