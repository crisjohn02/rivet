#!/usr/bin/env python3
"""Tests for the pilot harness (T38a-T38c).

    python3 benchmark/runner/test_runner.py        # or add -v

Standard library only. No test calls a model, the network or the real
`claude`: every agent run uses testdata/fake_claude.py through
RIVET_PILOT_CLAUDE_BIN, and arm C uses testdata/fake_rivet.py.
"""

from __future__ import annotations

import csv
import io
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True  # keep __pycache__ out of the repository
sys.path.insert(0, HERE)

import checks  # noqa: E402
import extract  # noqa: E402
import report  # noqa: E402
import run_study  # noqa: E402
import sandbox  # noqa: E402
import study as studylib  # noqa: E402
from transcript import (  # noqa: E402
    Transcript,
    contamination_scan,
    fallbacks,
    provider_error,
    rivet_invocations,
    rivet_metrics,
)

TESTDATA = os.path.join(HERE, "testdata")
FIXTURES = os.path.join(TESTDATA, "transcripts")
FAKE_CLAUDE = os.path.join(TESTDATA, "fake_claude.py")
FAKE_RIVET = os.path.join(TESTDATA, "fake_rivet.py")
BASE_STUDY = os.path.join(TESTDATA, "studies", "demo-study", "study.toml")
CORPUS_MANIFEST = os.path.join(TESTDATA, "corpus.toml")
RUNNER = os.path.join(HERE, "run_study.py")
TOKEN = r"App\Billing\Invoice::total"


def slurp(path: str, mode: str = "r"):
    with open(path, mode) as handle:
        return handle.read()


def spit(path: str, text: str) -> None:
    with open(path, "w") as handle:
        handle.write(text)


def fixture(name: str) -> Transcript:
    return Transcript.from_file(os.path.join(FIXTURES, name))


def answer(obj) -> str:
    return "done\n```json\n" + json.dumps(obj) + "\n```\n"


# ---- checks ----------------------------------------------------------------


class CheckTests(unittest.TestCase):
    GOLD = {"file": "src/Billing/Invoice.php", "symbol": TOKEN}

    def test_exact_symbol_normalizes_only_separators_and_leading_dot_slash(self):
        for path in ("./src/Billing/Invoice.php", "src\\Billing\\Invoice.php", "././src/Billing/Invoice.php"):
            result = checks.evaluate("exact_symbol", self.GOLD, answer({"file": path, "symbol": f"  {TOKEN} "}))
            self.assertTrue(result["passed"], path)
        for path in ("/abs/src/Billing/Invoice.php", "src/billing/Invoice.php", "Billing/Invoice.php"):
            result = checks.evaluate("exact_symbol", self.GOLD, answer({"file": path, "symbol": TOKEN}))
            self.assertFalse(result["passed"], path)
        wrong = checks.evaluate("exact_symbol", self.GOLD, answer({"file": "src/Billing/Invoice.php", "symbol": "Invoice::total"}))
        self.assertEqual((wrong["passed"], wrong["answer_status"]), (False, "ok"))

    def test_last_json_block_wins_and_non_json_fences_are_ignored(self):
        text = answer({"file": "x.php", "symbol": "X"}) + "```php\n<?php\n```\n" + answer(self.GOLD)
        self.assertTrue(checks.evaluate("exact_symbol", self.GOLD, text)["passed"])

    def test_missing_unparseable_and_invalid_answers_fail(self):
        self.assertEqual(checks.evaluate("exact_symbol", self.GOLD, "no block")["answer_status"], "missing")
        self.assertEqual(checks.evaluate("exact_symbol", self.GOLD, None)["answer_status"], "missing")
        bad = checks.evaluate("exact_symbol", self.GOLD, "```json\n{\"file\": \n```")
        self.assertEqual((bad["answer_status"], bad["passed"]), ("unparseable", False))
        shape = checks.evaluate("exact_symbol", self.GOLD, answer({"file": "a.php"}))
        self.assertEqual((shape["answer_status"], shape["passed"]), ("invalid_shape", False))
        listed = checks.evaluate("exact_symbol", self.GOLD, answer([self.GOLD]))
        self.assertEqual(listed["answer_status"], "invalid_shape")

    def test_set_f1_reports_precision_recall_and_threshold(self):
        gold = {"items": [{"file": "a.php", "symbol": "A"}, {"file": "b.php", "symbol": "B"}, {"file": "c.php", "symbol": "C"}, {"file": "d.php", "symbol": "D"}]}
        # 3 of 4 found plus 1 wrong: P = 3/4, R = 3/4, F1 = 0.75.
        ans = {"items": [{"file": "./a.php", "symbol": "A"}, {"file": "b.php", "symbol": "B"}, {"file": "c.php", "symbol": "C"}, {"file": "z.php", "symbol": "Z"}]}
        result = checks.evaluate("set_f1", gold, answer(ans), 0.8)
        self.assertAlmostEqual(result["precision"], 0.75)
        self.assertAlmostEqual(result["recall"], 0.75)
        self.assertAlmostEqual(result["f1"], 0.75)
        self.assertFalse(result["passed"])
        self.assertTrue(checks.evaluate("set_f1", gold, answer(ans), 0.75)["passed"])
        # Duplicates collapse: the answer is a set.
        dup = {"items": [{"file": "a.php", "symbol": "A"}] * 3}
        result = checks.evaluate("set_f1", gold, answer(dup), 0.8)
        self.assertAlmostEqual(result["precision"], 1.0)
        self.assertAlmostEqual(result["recall"], 0.25)
        empty = checks.evaluate("set_f1", gold, answer({"items": []}), 0.8)
        self.assertEqual((empty["precision"], empty["recall"], empty["f1"], empty["passed"]), (0.0, 0.0, 0.0, False))
        missing = checks.evaluate("set_f1", gold, "nothing", 0.8)
        self.assertEqual((missing["f1"], missing["answer_status"]), (0.0, "missing"))

    def test_accepted_path_matches_any_accepted_path_exactly_in_order(self):
        a = {"file": "a.php", "symbol": "A"}
        b = {"file": "b.php", "symbol": "B"}
        gold = {"accepted_paths": [[a, b], [a]]}
        self.assertTrue(checks.evaluate("accepted_path", gold, answer({"path": [a, b]}))["passed"])
        self.assertTrue(checks.evaluate("accepted_path", gold, answer({"path": [a]}))["passed"])
        self.assertFalse(checks.evaluate("accepted_path", gold, answer({"path": [b, a]}))["passed"])
        self.assertFalse(checks.evaluate("accepted_path", gold, answer({"path": [a, b, b]}))["passed"])

    def test_gold_validation(self):
        with self.assertRaises(checks.TaskError):
            checks.parse_gold("set_f1", {"items": []})
        with self.assertRaises(checks.TaskError):
            checks.parse_gold("accepted_path", {"accepted_paths": [[]]})
        with self.assertRaises(checks.TaskError):
            checks.parse_gold("exact_symbol", {"file": "a.php", "symbol": ""})

    def test_fixture_tasks_load_and_gold_passes_its_own_check(self):
        tasks = os.path.join(TESTDATA, "tasks")
        for task_id in sorted(os.listdir(tasks)):
            task = checks.load_task(tasks, task_id)
            self.assertTrue(checks.gold_self_check(task)["passed"], task_id)

    def test_task_validation_rejects_bad_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            shutil.copytree(os.path.join(TESTDATA, "tasks", "demo-callers"), os.path.join(tmp, "demo-callers"))
            path = os.path.join(tmp, "demo-callers", "task.toml")
            original = slurp(path)
            for old, new in (
                ("f1_threshold = 0.8", ""),
                ('category = "callers"', 'category = "explain"'),
                ("max_turns = 10", "max_turns = 0"),
                ('id = "demo-callers"', 'id = "other"'),
            ):
                spit(path, original.replace(old, new))
                with self.assertRaises(checks.TaskError, msg=new):
                    checks.load_task(tmp, "demo-callers")
            with self.assertRaises(checks.TaskError):
                checks.load_task(tmp, "../demo-callers")


# ---- transcripts -----------------------------------------------------------


class TranscriptTests(unittest.TestCase):
    def test_token_totals_come_from_model_usage_across_models(self):
        t = fixture("ok_exact.jsonl")
        usage = t.usage()
        self.assertEqual(usage["usage_source"], "result.modelUsage")
        # opus 100 + 1000 + 200, haiku 10: total 1310, cached 1000.
        self.assertEqual(usage["input_tokens"], 110)
        self.assertEqual(usage["cache_read_input_tokens"], 1000)
        self.assertEqual(usage["cache_creation_input_tokens"], 200)
        self.assertEqual(usage["output_tokens"], 55)
        self.assertEqual(t.assistant_turns(), 3)
        self.assertEqual(t.tool_calls_by_name(), {"Grep": 1, "Read": 1})

    def test_repeated_block_usage_is_not_double_counted_without_a_result(self):
        t = fixture("timeout_partial.jsonl")
        usage = t.usage()
        self.assertEqual(usage["usage_source"], "assistant_messages_partial")
        self.assertEqual(
            (usage["input_tokens"], usage["cache_read_input_tokens"], usage["cache_creation_input_tokens"], usage["output_tokens"]),
            (100, 900, 900, 20),
        )
        # ok_exact repeats msg_1's usage on two events; the fallback path
        # must count it once.
        t2 = fixture("ok_exact.jsonl")
        t2.result = None
        self.assertEqual(t2.usage()["input_tokens"], 100)

    def test_missing_usage_is_unavailable_not_zero(self):
        usage = fixture("missing_usage.jsonl").usage()
        self.assertEqual(usage["usage_source"], "none")
        self.assertIsNone(usage["input_tokens"])
        self.assertIsNone(usage["output_tokens"])
        partial = Transcript([{"type": "result", "subtype": "success", "usage": {"input_tokens": 5, "output_tokens": 1}}])
        usage = partial.usage()
        self.assertEqual(usage["input_tokens"], 5)
        self.assertIsNone(usage["cache_read_input_tokens"])

    def test_rivet_invocation_parsing(self):
        cases = {
            "rivet symbol Foo --json": [("symbol", "Foo")],
            "rivet context Foo.bar --tokens 3000 --json": [("context", "Foo.bar")],
            "RIVET_X=1 /opt/bin/rivet refs total | head -5": [("refs", "total")],
            "cd src && rivet refs --mode candidates run": [("refs", "run")],
            "find . -name '*.php' -exec rivet symbol {} \\;": [("symbol", "{}")],
            "rg --pre=rivet foo": [("foo", None)],
            "rivet --version": [("--version", None)],
            "rg -n rivet src": [],
            "ls .rivet": [],
            "cat /x/rivet-notes.txt": [],
        }
        for command, expected in cases.items():
            got = [(i["subcommand"], i["query"]) for i in rivet_invocations(command)]
            self.assertEqual(got, expected, command)

    def test_rivet_metrics_and_fallbacks(self):
        t = fixture("c_rivet.jsonl")
        metrics = rivet_metrics(t)
        self.assertEqual(metrics["rivet_invocations_by_command"], {"context": 1, "refs": 1, "symbol": 1})
        # symbol failed with exit 4 as a sole command; refs failed inside a pipeline.
        self.assertEqual(metrics["rivet_errors_by_exit"], {"4": 1, "unattributed": 1})
        self.assertEqual(metrics["refresh_mode"], "content")
        # context App\Billing\Invoice::total is followed by Grep "function total".
        self.assertEqual(metrics["rivet_to_text_fallbacks"], 1)
        self.assertEqual(fallbacks(fixture("ok_exact.jsonl")), 0)

    def test_contamination_scan(self):
        evidence = contamination_scan(fixture("b_contaminated.jsonl"))
        kinds = [e["kind"] for e in evidence]
        self.assertEqual(kinds, ["transcript_invocation", "transcript_invocation"])
        # A workspace path containing a `rivet` component is scrubbed first.
        clean = Transcript.from_file(os.path.join(FIXTURES, "b_clean.jsonl"))
        for use in clean.tool_uses:
            use["input"]["command"] = use["input"]["command"].replace("{{WORKSPACE}}", "/tmp/rivet/runs/ws")
        self.assertEqual(contamination_scan(clean, "/tmp/rivet/runs/ws"), [])
        self.assertEqual([e["kind"] for e in contamination_scan(clean)], ["transcript_mention"])
        output = Transcript(
            [
                {"type": "assistant", "message": {"id": "m", "content": [{"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}}]}},
                {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": '{"snapshot":"blake3:0123456789abcdef01"}'}]}},
            ]
        )
        self.assertEqual([e["kind"] for e in contamination_scan(output)], ["transcript_rivet_output"])

    def test_provider_errors_come_only_from_harness_errors(self):
        self.assertIsNotNone(provider_error(fixture("provider_error.jsonl")))
        # b_clean greps for `rate_limit_error`; that is agent work, not an outage.
        self.assertIsNone(provider_error(fixture("b_clean.jsonl")))
        self.assertIsNone(provider_error(fixture("ok_exact.jsonl")))


# ---- study, schedule, command, environment, ledger -------------------------


def study_text(**overrides) -> str:
    text = slurp(BASE_STUDY)
    for old, new in overrides.items():
        if old not in text:
            raise AssertionError(f"base study lacks {old!r}")
        text = text.replace(old, new)
    return text


def write_study(root: str, study_id: str, text: str) -> str:
    text = text.replace('study_id = "demo-study"', f'study_id = "{study_id}"')
    directory = os.path.join(root, "studies", study_id)
    os.makedirs(directory, exist_ok=True)
    path = os.path.join(directory, "study.toml")
    with open(path, "w") as handle:
        handle.write(text)
    return path


class StudyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def load(self, text):
        return studylib.load_study(write_study(self.tmp, "demo-study", text))

    def test_committed_pilot_manifest_is_valid(self):
        pilot = os.path.join(studylib.REPO_ROOT, "benchmark", "studies", "pilot-01", "study.toml")
        loaded = studylib.load_study(pilot)
        self.assertEqual(loaded["tasks"], ["p1", "p2", "p3", "p4", "p5"])
        self.assertEqual((loaded["budget_cap_usd"], loaded["model"], loaded["arms"]), (25.0, "claude-opus-5-5", ["B", "C"]))
        self.assertEqual(
            loaded["sandbox"]["deny_read_paths"],
            ["~/rivet-corpus/gold", "~/ssr", "~/rivet", "~/rivet-wt", "~/.claude/projects"],
        )
        self.assertEqual(loaded["sandbox"]["deny_read_regexes"], ["^/private/tmp/claude-"])
        with self.assertRaises(studylib.StudyError):
            self.load(study_text(**{"deny_rivet_exec_in_b = true": "deny_rivet_exec_in_b = false"}))
        with self.assertRaises(studylib.StudyError):
            self.load(study_text(**{"deny_read_paths = []": 'deny_read_paths = ["relative/path"]'}))

    def test_manifest_guards(self):
        bad = {
            'model = "claude-opus-5-5"': 'model = "opus"',
            '"--strict-mcp-config",\n': "",
            '"--no-session-persistence",\n': '"--no-session-persistence",\n  "--dangerously-skip-permissions",\n',
            '"Agent", "Task", "Bash(rivet:*)"]': '"Agent", "Task"]',
            '"Bash(wc:*)"]\ndisallowed_tools = ["Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "Agent", "Task", "Bash(rivet:*)"]': '"Bash(wc:*)", "Bash(rivet:*)"]\ndisallowed_tools = ["Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch", "Agent", "Task", "Bash(rivet:*)"]',
            'rivet = true\n': "rivet = false\n",
            "Read specific line ranges": "Read whole files",
            'retry_rule = "rerun_block_once"': 'retry_rule = "none"',
            'kind = "pilot"': 'kind = "confirmatory"',
            '"WebSearch", "Agent", "Task"]\n': '"WebSearch", "Agent"]\n',
        }
        for old, new in bad.items():
            with self.assertRaises(studylib.StudyError, msg=f"{old!r} -> {new!r}"):
                self.load(study_text(**{old: new}))
        self.load(study_text())

    def test_arm_order_is_seeded_per_block_and_varies(self):
        orders = {tuple(studylib.arm_order(7, f"t{i}", 1, ["B", "C"])) for i in range(20)}
        self.assertEqual(orders, {("B", "C"), ("C", "B")})
        self.assertEqual(studylib.arm_order(7, "t1", 1, ["C", "B"]), studylib.arm_order(7, "t1", 1, ["B", "C"]))
        self.assertEqual(
            [studylib.arm_order(7, f"t{i}", 1, ["B", "C"]) for i in range(20)],
            [studylib.arm_order(7, f"t{i}", 1, ["B", "C"]) for i in range(20)],
        )
        different = [studylib.arm_order(8, f"t{i}", 1, ["B", "C"]) for i in range(20)]
        self.assertNotEqual(different, [studylib.arm_order(7, f"t{i}", 1, ["B", "C"]) for i in range(20)])

    def test_harness_argv_per_arm(self):
        s = self.load(study_text())
        common = [
            "claude", "-p", "--setting-sources", "project", "--strict-mcp-config", "--disable-slash-commands",
            "--no-session-persistence", "--output-format", "stream-json", "--verbose", "--permission-prompts", "none",
            "--model", "claude-opus-5-5", "--settings", '{"effortLevel":"high"}',
            "--append-system-prompt", studylib.config_b_prompt_from_doc(), "--tools", "Read,Grep,Glob,Bash",
        ]
        self.assertEqual(
            studylib.harness_argv(s, "B", "claude", 1.5),
            common
            + [
                "--allowedTools", "Read,Grep,Glob,Bash(rg:*),Bash(ls:*),Bash(find:*),Bash(wc:*)",
                "--disallowedTools", "Edit,Write,NotebookEdit,WebFetch,WebSearch,Agent,Task,Bash(rivet:*)",
                "--max-budget-usd", "1.5",
            ],
        )
        self.assertEqual(
            studylib.harness_argv(s, "C", "claude", 2.0),
            common
            + [
                "--allowedTools", "Read,Grep,Glob,Bash(rg:*),Bash(ls:*),Bash(find:*),Bash(wc:*),Bash(rivet:*)",
                "--disallowedTools", "Edit,Write,NotebookEdit,WebFetch,WebSearch,Agent,Task",
                "--max-budget-usd", "2",
            ],
        )

    def test_child_env_keeps_rivet_out_of_arm_b(self):
        decoy = os.path.join(self.tmp, "decoy")
        os.makedirs(decoy)
        fake = os.path.join(decoy, "rivet")
        with open(fake, "w") as handle:
            handle.write("#!/bin/sh\n")
        os.chmod(fake, 0o755)
        base = {"PATH": os.pathsep.join([decoy, "/usr/bin", "/bin"]), "RIVET_PILOT_TASKS_DIR": "/secret", "CLAUDECODE": "1", "HOME": "/h"}
        env = studylib.child_env(base, "B", None, None)
        self.assertEqual(env["PATH"], os.pathsep.join(["/usr/bin", "/bin"]))
        self.assertNotIn("RIVET_PILOT_TASKS_DIR", env)
        self.assertNotIn("CLAUDECODE", env)
        self.assertEqual(env["HOME"], "/h")
        bin_dir = os.path.join(self.tmp, "bin")
        os.makedirs(bin_dir)
        os.symlink(FAKE_RIVET, os.path.join(bin_dir, "rivet"))
        env = studylib.child_env(base, "C", FAKE_RIVET, bin_dir)
        self.assertEqual(env["PATH"].split(os.pathsep), [bin_dir, "/usr/bin", "/bin"])

    def test_ledger_guard(self):
        path = os.path.join(self.tmp, "ledger.jsonl")
        ledger = studylib.Ledger(path, cap=2.0, reserve=0.25)
        self.assertTrue(ledger.allows(1.75))
        self.assertFalse(ledger.allows(1.76))
        ledger.start("a", 1.0)
        # Started but not ended: charged at the worst case.
        self.assertAlmostEqual(ledger.charged_usd(), 1.25)
        ledger.end("a", 0.3)
        self.assertAlmostEqual(ledger.charged_usd(), 0.3)
        ledger.start("b", 1.0)
        ledger.end("b", None)
        self.assertAlmostEqual(ledger.charged_usd(), 1.55)
        ledger.start("c", 0.2)
        ledger.end("c", None, launched=False)
        self.assertAlmostEqual(ledger.charged_usd(), 1.55)
        with self.assertRaises(studylib.StudyError):
            ledger.start("d", 1.0)
        reloaded = studylib.Ledger(path, cap=2.0, reserve=0.25)
        self.assertAlmostEqual(reloaded.charged_usd(), 1.55)
        self.assertAlmostEqual(reloaded.reported_usd(), 0.3)


# ---- sandbox ---------------------------------------------------------------


@unittest.skipUnless(sandbox.available(), "macOS sandbox-exec is required")
class SandboxTests(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def test_profile_resolves_paths_orders_rules_and_blocks_rivet_only_in_b(self):
        real = os.path.join(self.tmp, "secret")
        os.makedirs(real)
        link = os.path.join(self.tmp, "link")
        os.symlink(real, link)
        runs = os.path.join(self.tmp, "runs")
        ws = os.path.join(runs, "study", "workspaces", "a1")
        self.assertEqual(sandbox.resolve(link), real)
        b = sandbox.build_profile([sandbox.resolve(link)], ["^/private/tmp/claude-"], runs, [ws], deny_rivet_exec=True)
        c = sandbox.build_profile([sandbox.resolve(link)], ["^/private/tmp/claude-"], runs, [ws], deny_rivet_exec=False)
        self.assertIn(f'(deny file-read* (subpath "{real}"))', b)
        self.assertNotIn(link + '"', b)
        lines = b.splitlines()
        self.assertEqual(lines[:2], ["(version 1)", "(allow default)"])
        # Denies first, then metadata-only ancestors, then the workspace: SBPL
        # is last-match-wins.
        self.assertLess(lines.index(f'(deny file-read* (subpath "{runs}"))'), lines.index(f'(allow file-read* (subpath "{ws}"))'))
        for ancestor in (runs, os.path.join(runs, "study"), os.path.join(runs, "study", "workspaces")):
            self.assertIn(f'(allow file-read-metadata (literal "{ancestor}"))', b)
        self.assertEqual(lines[-1], '(deny process-exec (regex #"/rivet$"))')
        self.assertEqual(b.replace('(deny process-exec (regex #"/rivet$"))\n', ""), c)
        with self.assertRaises(sandbox.SandboxError):
            sandbox.build_profile([], [], runs, [os.path.join(self.tmp, "elsewhere")], False)
        with self.assertRaises(sandbox.SandboxError):
            sandbox.build_profile(['/x"y'], [], runs, [ws], False)

    def test_profile_denies_reads_through_symlinks_and_children_and_allows_the_workspace(self):
        secret = os.path.join(self.tmp, "gold")
        os.makedirs(secret)
        spit(os.path.join(secret, "gold.json"), "{}")
        runs = os.path.join(self.tmp, "runs")
        ws = os.path.join(runs, "s", "workspaces", "a1")
        other = os.path.join(runs, "s", "workspaces", "a0")
        for d in (ws, other, os.path.join(runs, "s", "runs", "a0")):
            os.makedirs(d)
        spit(os.path.join(ws, "f.txt"), "mine")
        spit(os.path.join(other, "f.txt"), "theirs")
        spit(os.path.join(runs, "s", "runs", "a0", "transcript.jsonl"), "answer")
        os.symlink(secret, os.path.join(self.tmp, "gold-link"))
        profile = os.path.join(self.tmp, "p.sb")
        spit(profile, sandbox.build_profile([secret], [], runs, [ws], deny_rivet_exec=True))

        def cat(path):
            return subprocess.run(sandbox.wrap(profile, ["/bin/sh", "-c", f"cd {ws} && /bin/cat {path}"]), capture_output=True, text=True)

        self.assertEqual(cat("f.txt").stdout, "mine")
        for path in (
            os.path.join(secret, "gold.json"),
            os.path.join(self.tmp, "gold-link", "gold.json"),
            os.path.join(other, "f.txt"),
            os.path.join(runs, "s", "runs", "a0", "transcript.jsonl"),
            "../a0/f.txt",
        ):
            done = cat(path)
            self.assertNotEqual(done.returncode, 0, path)
            self.assertIn("Operation not permitted", done.stderr, path)
        rivet = os.path.join(ws, "rivet")
        shutil.copyfile("/bin/echo", rivet)
        os.chmod(rivet, 0o755)
        done = subprocess.run(sandbox.wrap(profile, ["/bin/sh", "-c", f"{rivet} hi"]), capture_output=True, text=True)
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("Operation not permitted", done.stderr)

    def test_probe_fails_when_a_protected_path_is_readable(self):
        spit(os.path.join(self.tmp, "gold.json"), "{}")
        permissive = os.path.join(self.tmp, "open.sb")
        spit(permissive, "(version 1)(allow default)\n")
        result = sandbox.probe(permissive, self.tmp, dict(os.environ), [("ls", ["/bin/ls", self.tmp])], [("gold", ["/bin/cat", os.path.join(self.tmp, "gold.json")])])
        self.assertFalse(result["passed"])
        self.assertEqual([c["passed"] for c in result["checks"]], [True, False])


# ---- end to end with the fake agent ----------------------------------------


class Harness:
    """A temporary study environment. The runs directory has a `rivet` path
    component on purpose, to prove the contamination scan scrubs it."""

    def __init__(self, tasks=("demo-locate",), plan=None, study_overrides=None, study_id="demo-study", cap=None, reserve=None):
        self.root = tempfile.mkdtemp()
        self.tasks_dir = os.path.join(self.root, "tasks")
        shutil.copytree(os.path.join(TESTDATA, "tasks"), self.tasks_dir)
        self.corpus_dir = os.path.join(self.root, "corpus")
        shutil.copytree(os.path.join(TESTDATA, "corpus"), self.corpus_dir)
        demo = os.path.join(self.corpus_dir, "demo")
        # Repository-provided instructions and a stale cache, created here so
        # the public repository carries no instruction files.
        for rel, content in (
            ("CLAUDE.md", "# repo instructions\n"),
            ("AGENTS.md", "# agents\n"),
            ("src/Billing/CLAUDE.md", "# nested\n"),
            (".claude/settings.json", '{"hooks": {}}\n'),
            (".rivet/stale.txt", "stale\n"),
        ):
            full = os.path.join(demo, rel)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as handle:
                handle.write(content)
        self.runs_dir = os.path.join(self.root, "rivet", "runs")
        os.makedirs(self.runs_dir)
        self.state = os.path.join(self.root, "fake-state")
        self.log = os.path.join(self.root, "fake-log.jsonl")
        self.plan_path = os.path.join(self.root, "plan.json")
        with open(self.plan_path, "w") as handle:
            json.dump(plan or {}, handle)
        self.decoy = os.path.join(self.root, "decoy-bin")
        os.makedirs(self.decoy)
        with open(os.path.join(self.decoy, "rivet"), "w") as handle:
            handle.write("#!/bin/sh\necho decoy\n")
        os.chmod(os.path.join(self.decoy, "rivet"), 0o755)
        overrides = {'tasks = ["demo-locate"]': "tasks = [" + ", ".join(f'"{t}"' for t in tasks) + "]"}
        if cap is not None:
            overrides["budget_cap_usd = 25.0"] = f"budget_cap_usd = {cap}"
        if reserve is not None:
            overrides["per_run_reserve_usd = 0.25"] = f"per_run_reserve_usd = {reserve}"
        overrides.update(study_overrides or {})
        self.study_id = study_id
        self.study_path = write_study(self.root, study_id, study_text(**overrides))
        self.study_dir = os.path.join(self.runs_dir, study_id)
        self.corpus_hash_before = self.corpus_hash()

    def corpus_hash(self):
        sys.path.insert(0, HERE)
        import run_study

        entries = []
        for dirpath, _dirs, files in os.walk(self.corpus_dir):
            for name in files:
                full = os.path.join(dirpath, name)
                entries.append((os.path.relpath(full, self.corpus_dir), studylib.sha256_file(full)))
        return run_study.studylib.canonical_hash(sorted(entries))

    def env(self, claude=FAKE_CLAUDE, rivet=FAKE_RIVET):
        env = {k: v for k, v in os.environ.items() if not k.startswith("RIVET_")}
        env.update(
            {
                "RIVET_PILOT_TASKS_DIR": self.tasks_dir,
                "RIVET_PILOT_RUNS_DIR": self.runs_dir,
                "RIVET_CORPUS_DIR": self.corpus_dir,
                "RIVET_PILOT_RIVET_BIN": rivet,
                "RIVET_PILOT_CLAUDE_BIN": claude,
                "FAKE_CLAUDE_PLAN": self.plan_path,
                "FAKE_CLAUDE_STATE": self.state,
                "FAKE_CLAUDE_LOG": self.log,
                "CLAUDECODE": "1",
                "PATH": os.pathsep.join([self.decoy, os.environ.get("PATH", "")]),
            }
        )
        return env

    def run(self, command="run", **env_kw):
        return subprocess.run(
            [sys.executable, RUNNER, command, self.study_path, "--corpus-manifest", CORPUS_MANIFEST],
            env=self.env(**env_kw),
            capture_output=True,
            text=True,
            timeout=120,
        )

    def records(self):
        runs = os.path.join(self.study_dir, "runs")
        out = {}
        for name in sorted(os.listdir(runs)):
            with open(os.path.join(runs, name, "record.json")) as handle:
                out[name] = json.load(handle)
        return out

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as handle:
            return [json.loads(line) for line in handle]

    def ledger(self):
        with open(os.path.join(self.study_dir, "ledger.jsonl")) as handle:
            return [json.loads(line) for line in handle]

    def csv_rows(self):
        text = extract.extract(self.study_dir)
        return {row["attempt_id"]: row for row in csv.DictReader(io.StringIO(text))}

    def close(self):
        shutil.rmtree(self.root, ignore_errors=True)


class EndToEndTests(unittest.TestCase):
    def harness(self, **kw):
        h = Harness(**kw)
        self.addCleanup(h.close)
        return h

    def test_synthetic_run_round_trips(self):
        h = self.harness()
        done = h.run()
        self.assertEqual(done.returncode, 0, done.stderr + done.stdout)
        records = h.records()
        self.assertEqual(sorted(records), ["demo-locate.t1.B.a1", "demo-locate.t1.C.a1"])
        order = studylib.arm_order(7, "demo-locate", 1, ["B", "C"])
        study = studylib.load_study(h.study_path)
        for arm, r in ((a, records[f"demo-locate.t1.{a}.a1"]) for a in ("B", "C")):
            self.assertEqual(r["execution_order"], order.index(arm) + 1)
            self.assertEqual((r["study_id"], r["task_id"], r["trial"], r["attempt"], r["block_id"]), ("demo-study", "demo-locate", 1, 1, "demo-locate.t1"))
            self.assertEqual(r["limits"], {"max_budget_usd": 1.0, "max_turns": 10, "wall_seconds": 20.0})
            self.assertEqual(r["harness_version"], "0.0.0 (Fake Claude)")
            self.assertEqual(r["model_id"], "claude-opus-5-5")
            self.assertEqual(r["repository_commit"], "0" * 40)
            self.assertEqual((r["termination_reason"], r["infrastructure_failure"], r["pass"]), ("completed", False, True))
            self.assertEqual(r["cost_usd"], 0.12)
            self.assertEqual(r["argv"], studylib.harness_argv(study, arm, FAKE_CLAUDE, 1.0))
            self.assertEqual([x["path"] for x in r["removed_instructions"]], [".claude/", "AGENTS.md", "CLAUDE.md", "src/Billing/CLAUDE.md"])
            self.assertFalse(r["workspace_modified"])
            self.assertFalse(any(n.startswith("RIVET_") for n in r["child_env_names"]))
        b, c = records["demo-locate.t1.B.a1"], records["demo-locate.t1.C.a1"]
        self.assertEqual(b["removed_instructions"], c["removed_instructions"])
        self.assertEqual(b["settings_hash"], c["settings_hash"])
        self.assertNotEqual(b["tool_policy_hash"], c["tool_policy_hash"])
        self.assertEqual((b["tool_version"], b["snippet_hash"], b["contaminated"]), (None, None, False))
        self.assertEqual(c["tool_version"], "rivet 0.0.0-fake")
        self.assertEqual(c["snippet_hash"], studylib.sha256_bytes(subprocess.run([FAKE_RIVET, "snippet"], capture_output=True).stdout))
        # Workspaces: instructions normalized, stale cache not copied, arm applied.
        ws_b, ws_c = b["workspace"], c["workspace"]
        for ws in (ws_b, ws_c):
            self.assertFalse(os.path.exists(os.path.join(ws, "AGENTS.md")))
            self.assertFalse(os.path.exists(os.path.join(ws, ".claude")))
            self.assertFalse(os.path.exists(os.path.join(ws, "src", "Billing", "CLAUDE.md")))
            self.assertFalse(os.path.exists(os.path.join(ws, ".rivet", "stale.txt")))
        self.assertFalse(os.path.exists(os.path.join(ws_b, "CLAUDE.md")))
        self.assertFalse(os.path.exists(os.path.join(ws_b, ".rivet")))
        self.assertEqual(sorted(os.listdir(os.path.join(ws_c, ".rivet"))), ["config.toml"])
        # What the fake agent saw.
        calls = {c["key"]: c for c in h.calls()}
        prompt = slurp(os.path.join(h.tasks_dir, "demo-locate", "prompt.md"))
        self.assertEqual(calls["demo-locate:B"]["stdin"], prompt)
        self.assertEqual(calls["demo-locate:C"]["stdin"], prompt)
        self.assertIsNone(calls["demo-locate:B"]["rivet_on_path"])
        self.assertNotIn(h.decoy, calls["demo-locate:B"]["path"])
        self.assertEqual(calls["demo-locate:C"]["rivet_on_path"], os.path.join(os.path.realpath(h.study_dir), "tools", "rivet"))
        # Every attempt passed its sandbox pre-flight probe inside its profile.
        for r in records.values():
            self.assertTrue(r["sandbox_probe"]["passed"], r["sandbox_probe"])
            profile = slurp(os.path.join(h.study_dir, "runs", r["attempt_id"], "sandbox.sb"))
            self.assertEqual(studylib.sha256_bytes(profile.encode()), r["sandbox_profile_sha256"])
        self.assertIn("exec_rivet_stand_in", [c["name"] for c in b["sandbox_probe"]["checks"]])
        self.assertIn("process-exec", slurp(os.path.join(h.study_dir, "runs", "demo-locate.t1.B.a1", "sandbox.sb")))
        self.assertNotIn("process-exec", slurp(os.path.join(h.study_dir, "runs", "demo-locate.t1.C.a1", "sandbox.sb")))
        for call in calls.values():
            self.assertFalse(any(n.startswith("RIVET_") or n == "CLAUDECODE" for n in call["env_names"]))
            self.assertEqual(call["argv"], records[f"demo-locate.t1.{call['key'][-1]}.a1"]["argv"][1:])
        # The corpus copy was never written.
        self.assertEqual(h.corpus_hash(), h.corpus_hash_before)
        self.assertTrue(os.path.exists(os.path.join(h.corpus_dir, "demo", ".rivet", "stale.txt")))
        events = [(e["event"], e.get("attempt_id")) for e in h.ledger()]
        self.assertEqual(sorted(events), sorted([("start", r) for r in records] + [("end", r) for r in records]))
        manifest = json.loads(slurp(os.path.join(h.study_dir, "manifest.json")))
        self.assertEqual(manifest["schedule"][0]["arm_order"], order)
        # Extraction.
        rows = h.csv_rows()
        row = rows["demo-locate.t1.C.a1"]
        self.assertEqual(
            {k: row[k] for k in ("input_tokens_total", "input_tokens_cached", "input_tokens_uncached", "output_tokens", "pass", "usage_source")},
            {"input_tokens_total": "1310", "input_tokens_cached": "1000", "input_tokens_uncached": "310", "output_tokens": "55", "pass": "true", "usage_source": "result.modelUsage"},
        )
        self.assertEqual(row["tool_calls_by_name"], '{"Grep":1,"Read":1}')
        self.assertEqual(row["cost_usd_claude_code_estimate"], "0.120000")
        self.assertEqual((row["indexing_seconds"], row["snapshot_mismatches"]), ("unavailable", "unavailable"))
        self.assertEqual(rows["demo-locate.t1.B.a1"]["rivet_invocations_by_command"], "not_applicable")
        self.assertEqual(rows["demo-locate.t1.B.a1"]["contaminated"], "false")
        self.assertEqual(row["analysis_inclusion"], "included")
        # A second `run` resumes: nothing new starts.
        again = h.run()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(len(h.calls()), 2)

    def test_agent_cannot_read_protected_paths_but_can_read_its_workspace(self):
        h = self.harness()
        secret = os.path.join(h.root, "secret")
        os.makedirs(secret)
        spit(os.path.join(secret, "notes.md"), "held-out")
        os.symlink(h.tasks_dir, os.path.join(h.root, "tasks-link"))
        text = slurp(h.study_path).replace("deny_read_paths = []", f'deny_read_paths = ["{secret}"]')
        spit(h.study_path, text)
        order = studylib.arm_order(7, "demo-locate", 1, ["B", "C"])
        first, second = order
        ws = lambda arm: os.path.join(h.study_dir, "workspaces", f"demo-locate.t1.{arm}.a1")
        gold = os.path.join(h.tasks_dir, "demo-locate", "gold.json")
        reads = [
            "{{WORKSPACE}}/README.txt",
            "{{WORKSPACE}}/src/Billing/Invoice.php",
            gold,
            os.path.join(h.root, "tasks-link", "demo-locate", "gold.json"),
            os.path.join(h.corpus_dir, "demo", "README.txt"),
            os.path.join(secret, "notes.md"),
            os.path.join(h.study_dir, "ledger.jsonl"),
            os.path.join(ws(first), "README.txt"),
            os.path.join(h.study_dir, "runs", f"demo-locate.t1.{first}.a1", "transcript.jsonl"),
        ]
        pinned = os.path.join(h.study_dir, "tools", "rivet")
        step = {"transcript": "ok_exact.jsonl", "reads": reads, "execs": [[pinned, "--version"], ["/bin/echo", "x"]]}
        spit(h.plan_path, json.dumps({"demo-locate:B": [step], "demo-locate:C": [step]}))
        done = h.run()
        self.assertEqual(done.returncode, 0, done.stderr)
        probes = {c["key"]: c["probes"] for c in h.calls() if "probes" in c}
        for arm in ("B", "C"):
            got = probes[f"demo-locate:{arm}"]
            self.assertEqual(got[os.path.join(os.path.realpath(ws(arm)), "README.txt")], "ok", arm)
            self.assertEqual(got[os.path.join(os.path.realpath(ws(arm)), "src/Billing/Invoice.php")], "ok", arm)
            for path in reads[2:7]:
                self.assertEqual(got[path], "denied", f"{arm} {path}")
            self.assertEqual(got[f"/bin/echo x"], "exit:0", arm)
        # The arm that runs second cannot read the first arm's workspace or transcript.
        for path in reads[7:]:
            self.assertEqual(probes[f"demo-locate:{second}"][path], "denied", path)
        self.assertEqual(probes["demo-locate:B"][f"{pinned} --version"], "denied")
        self.assertEqual(probes["demo-locate:C"][f"{pinned} --version"], "exit:0")

    def test_failed_preflight_probe_stops_the_study_before_the_agent_starts(self):
        h = self.harness()
        env = h.env()
        argv = ["run", h.study_path, "--corpus-manifest", CORPUS_MANIFEST]
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(
            run_study.sandbox, "build_profile", lambda *a, **k: "(version 1)(allow default)\n"
        ), mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            code = run_study.main(argv)
        self.assertEqual(code, 1)
        self.assertIn("sandbox pre-flight probe failed", err.getvalue())
        self.assertEqual(h.calls(), [])
        record = next(iter(h.records().values()))
        self.assertFalse(record["sandbox_probe"]["passed"])
        failed = sorted(c["name"] for c in record["sandbox_probe"]["checks"] if not c["passed"])
        self.assertIn("read_gold", failed)

    def test_runs_directory_under_a_denied_rule_is_refused(self):
        h = self.harness()
        text = slurp(h.study_path).replace("deny_read_regexes = []", f"deny_read_regexes = ['^{os.path.realpath(h.root)}/rivet']")
        spit(h.study_path, text)
        done = h.run("plan")
        self.assertEqual(done.returncode, 1)
        self.assertIn("falls under the denied read rule", done.stderr)

    def test_workspace_git_is_one_deterministic_clean_commit_in_both_arms(self):
        h = self.harness()
        self.assertEqual(h.run().returncode, 0)
        heads = {}
        for arm in ("B", "C"):
            record = h.records()[f"demo-locate.t1.{arm}.a1"]
            ws = record["workspace"]

            def git(*args):
                return subprocess.run(["git", "-C", ws, *args], capture_output=True, text=True, env={"PATH": os.environ["PATH"], "GIT_CONFIG_GLOBAL": os.devnull}).stdout

            self.assertEqual(git("status", "--porcelain"), "")
            self.assertEqual(git("log", "--format=%an <%ae> %ad %cn %cd %s", "--date=iso-strict").strip(),
                             "rivet pilot <pilot@rivet.invalid> 2000-01-01T00:00:00Z rivet pilot 2000-01-01T00:00:00Z rivet pilot workspace")
            self.assertEqual(git("rev-parse", "HEAD").strip(), record["workspace_git_reset_head"])
            tracked = git("ls-files").split()
            self.assertEqual("CLAUDE.md" in tracked, arm == "C")
            self.assertNotIn("AGENTS.md", tracked)
            heads[arm] = record["workspace_git_reset_head"]
        ignored = subprocess.run(["git", "-C", h.records()["demo-locate.t1.C.a1"]["workspace"], "status", "--porcelain", "--ignored"], capture_output=True, text=True).stdout
        self.assertIn("!! .rivet/", ignored)
        # Same prepared tree, same commit: a second study reproduces both heads.
        h2 = self.harness(study_id="demo-study")
        self.assertEqual(h2.run().returncode, 0)
        for arm in ("B", "C"):
            self.assertEqual(h2.records()[f"demo-locate.t1.{arm}.a1"]["workspace_git_reset_head"], heads[arm])

    def test_an_unignored_rivet_cache_is_never_committed(self):
        h = self.harness()
        env = h.env()
        env["FAKE_RIVET_SKIP_GITIGNORE"] = "1"
        done = subprocess.run([sys.executable, RUNNER, "run", h.study_path, "--corpus-manifest", CORPUS_MANIFEST], env=env, capture_output=True, text=True, timeout=120)
        self.assertEqual(done.returncode, 1)
        self.assertIn(".rivet/ is not ignored", done.stderr)

    def test_budget_guard_stops_before_a_block_that_could_breach_the_cap(self):
        # Worst case per run 1.0 + 0.25. Block 1 needs 2.5 <= 2.6; it reports
        # 0.24, so block 2 would need 0.24 + 2.5 = 2.74 > 2.6.
        h = self.harness(tasks=("demo-locate", "demo-trace"), cap=2.6, plan={"demo-trace:B": [{"transcript": "path_answer.jsonl"}], "demo-trace:C": [{"transcript": "path_answer.jsonl"}]})
        done = h.run()
        self.assertEqual(done.returncode, 3, done.stderr)
        self.assertIn("budget guard", done.stderr)
        self.assertEqual(len(h.calls()), 2)
        self.assertEqual(h.ledger()[-1]["event"], "stopped")

    def test_unknown_costs_are_charged_at_worst_case(self):
        plan = {k: [{"transcript": "missing_usage.jsonl"}] for k in ("demo-locate:B", "demo-locate:C")}
        h = self.harness(tasks=("demo-locate", "demo-trace"), cap=4.0, plan=plan)
        done = h.run()
        # Block 1 charged 2 x 1.25 = 2.5 (no cost reported); block 2 would reach 5.0.
        self.assertEqual(done.returncode, 3, done.stderr)
        self.assertEqual(len(h.calls()), 2)
        self.assertIn("charged so far $2.5000", done.stderr)

    def test_budget_guard_stops_mid_block_after_an_overshoot(self):
        # Cap 2.5 admits the block (2 x 1.25). The first run then reports
        # $1.40, above its own $1.00 budget, so the second run would need
        # 1.40 + 1.25 = 2.65 > 2.5 and is refused.
        over = [{"transcript": "overshoot.jsonl"}]
        h = self.harness(cap=2.5, plan={"demo-locate:B": over, "demo-locate:C": over})
        done = h.run()
        self.assertEqual(done.returncode, 3, done.stderr)
        self.assertEqual(len(h.calls()), 1)
        self.assertEqual(len(h.records()), 1)
        self.assertIn("not started", done.stderr)

    def test_symmetric_block_retry_preserves_both_transcripts(self):
        plan = {"demo-locate:B": [{"transcript": "provider_error.jsonl"}, {"transcript": "ok_exact.jsonl"}]}
        h = self.harness(plan=plan)
        done = h.run()
        self.assertEqual(done.returncode, 0, done.stderr)
        records = h.records()
        self.assertEqual(sorted(records), ["demo-locate.t1.B.a1", "demo-locate.t1.B.a2", "demo-locate.t1.C.a1", "demo-locate.t1.C.a2"])
        self.assertEqual(records["demo-locate.t1.B.a1"]["termination_reason"], "provider_error")
        self.assertTrue(records["demo-locate.t1.B.a1"]["infrastructure_failure"])
        self.assertIsNone(records["demo-locate.t1.B.a1"]["pass"])
        self.assertIn("API Error", slurp(os.path.join(h.study_dir, "runs", "demo-locate.t1.B.a1", "transcript.jsonl")))
        rows = h.csv_rows()
        self.assertEqual(rows["demo-locate.t1.B.a1"]["inclusion_reason"], "superseded_by_block_retry")
        self.assertEqual(rows["demo-locate.t1.C.a1"]["inclusion_reason"], "superseded_by_block_retry")
        self.assertEqual(rows["demo-locate.t1.C.a1"]["analysis_inclusion"], "excluded")
        self.assertEqual(rows["demo-locate.t1.B.a2"]["inclusion_reason"], "block_retry")
        self.assertEqual(rows["demo-locate.t1.B.a2"]["analysis_inclusion"], "included")
        # Tokens of the failed attempt are still in its row.
        self.assertEqual(rows["demo-locate.t1.B.a1"]["input_tokens_total"], "10")

    def test_retry_happens_once_only(self):
        plan = {"demo-locate:C": [{"transcript": "provider_error.jsonl"}]}
        h = self.harness(plan=plan)
        self.assertEqual(h.run().returncode, 0)
        self.assertEqual(len(h.records()), 4)
        rows = h.csv_rows()
        self.assertEqual(rows["demo-locate.t1.C.a2"]["inclusion_reason"], "infrastructure_failure_after_retry")
        self.assertEqual(rows["demo-locate.t1.B.a2"]["analysis_inclusion"], "included")

    def test_launch_failure_is_infrastructure_and_costs_nothing(self):
        h = self.harness()
        done = h.run(claude=os.path.join(h.root, "no-such-claude"))
        self.assertEqual(done.returncode, 0, done.stderr)
        records = h.records()
        self.assertEqual(len(records), 4)
        self.assertTrue(all(r["termination_reason"] == "launch_failed" for r in records.values()))
        ledger = studylib.Ledger(os.path.join(h.study_dir, "ledger.jsonl"), 25.0, 0.25)
        self.assertEqual(ledger.charged_usd(), 0.0)

    def test_wall_timeout_and_turn_limit_kill_the_agent_and_count_tokens(self):
        plan = {
            "demo-slow:B": [{"transcript": "timeout_partial.jsonl", "sleep": 30}],
            "demo-slow:C": [{"turns": 10, "sleep": 30}],
        }
        h = self.harness(tasks=("demo-slow",), plan=plan)
        started = time.monotonic()
        done = h.run()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertLess(time.monotonic() - started, 25)
        records = h.records()
        b, c = records["demo-slow.t1.B.a1"], records["demo-slow.t1.C.a1"]
        self.assertEqual((b["termination_reason"], b["pass"], b["infrastructure_failure"]), ("wall_timeout", False, False))
        self.assertEqual((c["termination_reason"], c["pass"]), ("max_turns", False))
        self.assertIsNone(b["cost_usd"])
        rows = h.csv_rows()
        self.assertEqual(rows["demo-slow.t1.B.a1"]["input_tokens_total"], "1900")
        self.assertEqual(rows["demo-slow.t1.B.a1"]["usage_source"], "assistant_messages_partial")
        self.assertEqual(rows["demo-slow.t1.B.a1"]["cost_usd_claude_code_estimate"], "unavailable")
        self.assertEqual(rows["demo-slow.t1.B.a1"]["analysis_inclusion"], "included")
        self.assertEqual(rows["demo-slow.t1.C.a1"]["num_turns"], "4")
        # Both unknown costs are charged at 0.5 + 0.25.
        self.assertAlmostEqual(studylib.Ledger(os.path.join(h.study_dir, "ledger.jsonl"), 25.0, 0.25).charged_usd(), 1.5)

    def test_unparseable_and_wrong_answers_fail_with_tokens(self):
        plan = {"demo-locate:B": [{"transcript": "unparseable.jsonl"}], "demo-locate:C": [{"transcript": "wrong_answer.jsonl"}]}
        h = self.harness(plan=plan)
        self.assertEqual(h.run().returncode, 0)
        rows = h.csv_rows()
        b, c = rows["demo-locate.t1.B.a1"], rows["demo-locate.t1.C.a1"]
        self.assertEqual((b["pass"], b["answer_status"], b["input_tokens_total"]), ("false", "unparseable", "100"))
        self.assertEqual((c["pass"], c["answer_status"], c["input_tokens_total"]), ("false", "ok", "200"))

    def test_contaminated_b_runs_are_flagged_and_excluded(self):
        plan = {"demo-locate:B": [{"transcript": "b_contaminated.jsonl"}]}
        h = self.harness(plan=plan)
        self.assertEqual(h.run().returncode, 0)
        record = h.records()["demo-locate.t1.B.a1"]
        self.assertTrue(record["contaminated"])
        row = h.csv_rows()["demo-locate.t1.B.a1"]
        self.assertEqual((row["contaminated"], row["contamination_evidence"]), ("true", "transcript_invocation"))
        self.assertEqual((row["analysis_inclusion"], row["inclusion_reason"]), ("excluded", "contaminated"))

    def test_rivet_dir_in_b_workspace_is_contamination(self):
        plan = {"demo-locate:B": [{"transcript": "b_clean.jsonl", "mkdir_rivet": True}]}
        h = self.harness(plan=plan)
        self.assertEqual(h.run().returncode, 0)
        record = h.records()["demo-locate.t1.B.a1"]
        self.assertEqual([e["kind"] for e in record["contamination_evidence"]], ["workspace_rivet_dir"])
        clean = self.harness(plan={"demo-locate:B": [{"transcript": "b_clean.jsonl"}]})
        self.assertEqual(clean.run().returncode, 0)
        self.assertFalse(clean.records()["demo-locate.t1.B.a1"]["contaminated"])

    def test_extract_refuses_an_altered_transcript(self):
        h = self.harness()
        self.assertEqual(h.run().returncode, 0)
        path = os.path.join(h.study_dir, "runs", "demo-locate.t1.B.a1", "transcript.jsonl")
        with open(path, "a") as handle:
            handle.write("\n")
        with self.assertRaises(extract.ExtractError):
            extract.extract(h.study_dir)

    def test_setup_failure_stops_the_study_without_charging(self):
        h = self.harness()
        shutil.rmtree(os.path.join(h.corpus_dir, "demo"))
        done = h.run()
        self.assertEqual(done.returncode, 1)
        self.assertIn("setup failed", done.stderr)
        self.assertEqual(h.calls(), [])
        self.assertEqual(studylib.Ledger(os.path.join(h.study_dir, "ledger.jsonl"), 25.0, 0.25).charged_usd(), 0.0)

    def test_private_directories_inside_the_repository_are_refused(self):
        h = self.harness()
        env = h.env()
        env["RIVET_PILOT_RUNS_DIR"] = os.path.join(studylib.REPO_ROOT, "benchmark", "raw")
        done = subprocess.run([sys.executable, RUNNER, "plan", h.study_path, "--corpus-manifest", CORPUS_MANIFEST], env=env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 1)
        self.assertIn("inside the public repository", done.stderr)

    def test_validate_and_plan(self):
        h = self.harness(tasks=("demo-locate", "demo-callers", "demo-trace"))
        done = h.run("validate")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        planned = h.run("plan")
        self.assertEqual(planned.returncode, 0, planned.stderr)
        self.assertIn("worst case without retries $7.50", planned.stdout)
        self.assertEqual(h.calls(), [])
        with open(os.path.join(h.tasks_dir, "demo-trace", "gold.json"), "w") as handle:
            handle.write('{"accepted_paths": []}')
        self.assertEqual(h.run("validate").returncode, 2)


# ---- report ----------------------------------------------------------------


def synthetic_rows():
    rows = []

    def add(task, trial, arm, tokens, passed, attempt=1, inclusion="included", reason="first_attempt", wall="10.0", category="locate", infra="false"):
        row = {c: "unavailable" for c in extract.COLUMNS}
        row.update(
            {
                "run_id": f"{task}.t{trial}.{arm}",
                "attempt_id": f"{task}.t{trial}.{arm}.a{attempt}",
                "block_id": f"{task}.t{trial}",
                "task_id": task,
                "project": "demo",
                "category": category,
                "config": arm,
                "trial": str(trial),
                "attempt": str(attempt),
                "analysis_inclusion": inclusion,
                "inclusion_reason": reason,
                "pass": passed,
                "answer_status": "ok",
                "termination_reason": "completed",
                "infrastructure_failure": infra,
                "contaminated": "false" if arm == "B" else "not_applicable",
                "isolation_violations": "none",
                "input_tokens_total": tokens,
                "wall_clock_seconds": wall,
                "tool_calls_total": "4",
                "output_tokens": "100",
                "cost_usd_claude_code_estimate": "0.100000",
                "model_id": "claude-opus-5-5",
                "started_at": "2026-09-23T10:00:00Z",
                "rivet_invocations_by_command": '{"context":1,"symbol":2}' if arm == "C" else "not_applicable",
                "rivet_errors_by_exit": '{"4":1}' if arm == "C" else "not_applicable",
                "rivet_to_text_fallbacks": "1" if arm == "C" else "not_applicable",
            }
        )
        rows.append(row)

    # t1: B 1000 and 3000 over two trials (mean 2000), C 1000 in one trial.
    add("t1", 1, "B", "1000", "true")
    add("t1", 2, "B", "3000", "false", wall="30.0")
    add("t1", 1, "C", "1000", "true", wall="5.0")
    # t2: B 4000; C 1000 plus a run with unavailable usage (missing, not zero).
    add("t2", 1, "B", "4000", "false", category="trace")
    add("t2", 1, "C", "1000", "true", category="trace", wall="5.0")
    add("t2", 2, "C", "unavailable", "true", category="trace", wall="5.0")
    # A superseded attempt with enormous usage must not count.
    add("t2", 2, "B", "999999999", "false", inclusion="excluded", reason="superseded_by_block_retry", category="trace", infra="true")
    return rows


def write_rows(path, rows):
    with open(path, "w", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=extract.COLUMNS, lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        text = study_text(**{'tasks = ["demo-locate"]': 'tasks = ["t1", "t2"]', "trials = 1": "trials = 2"})
        self.study = write_study(self.tmp, "synthetic", text)
        self.csv = os.path.join(self.tmp, "runs.csv")
        write_rows(self.csv, synthetic_rows())

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def test_known_ratios_and_counts(self):
        out = os.path.join(self.tmp, "out")
        summary = report.generate(self.csv, self.study, out)
        tokens = summary["arm_means"]["input_tokens_total"]
        # Task-weighted: B (2000 + 4000) / 2 = 3000, C (1000 + 1000) / 2 = 1000.
        # A run-weighted B mean would be 2666.67.
        self.assertAlmostEqual(tokens["paired"]["B"], 3000.0)
        self.assertAlmostEqual(tokens["paired"]["C"], 1000.0)
        self.assertEqual(tokens["missing_runs"], {"B": 0, "C": 1})
        comp = summary["comparisons"]
        self.assertAlmostEqual(comp["input_tokens_total"]["ratio_c_over_b"], 1 / 3)
        self.assertAlmostEqual(comp["input_tokens_total"]["reduction_pct"], 200 / 3)
        # Success: B t1 0.5, t2 0.0 -> 0.25; C 1.0 -> difference +75 pp.
        self.assertAlmostEqual(comp["success"]["difference_pp"], 75.0)
        # Wall: B (20 + 10) / 2 = 15, C 5 -> change -66.7%.
        self.assertAlmostEqual(comp["wall_clock_seconds"]["change_pct"], (5 / 15 - 1) * 100)
        acc = summary["accounting"]
        self.assertEqual(acc["B"]["scheduled"], 4)
        self.assertEqual(acc["B"]["accounted"], 3)
        self.assertEqual(acc["B"]["missing"], 1)
        self.assertEqual(acc["B"]["infrastructure_failures"], 1)
        self.assertEqual(acc["C"]["accounted"], 3)
        self.assertEqual(acc["B"]["successful"], 1)
        self.assertEqual(acc["B"]["evaluator_failures"], 2)
        adoption = summary["adoption_c"]
        self.assertEqual((adoption["runs_invoking_rivet"], adoption["eligible_runs"]), (3, 3))
        self.assertEqual(adoption["invocations_by_command"], {"context": 3, "symbol": 6})
        self.assertEqual(adoption["text_fallbacks"], 3)
        self.assertEqual(set(summary["gates"]) - {"reason"}, {"efficiency_point_estimate", "evidence_of_reduction", "success_non_inferiority", "study_integrity"})
        self.assertTrue(all(v == "not_evaluated" for k, v in summary["gates"].items() if k != "reason"))

        text = slurp(os.path.join(out, "report.md"))
        self.assertEqual(text.count("NOT EVALUATED (pilot)"), 4)
        self.assertNotIn("STUDY_ID", text)
        self.assertNotIn("Status: NOT RUN", text)
        self.assertIn("| Mean total input tokens / run | — | 3,000 | 1,000 | Ratio: 0.333; reduction: 66.7% | not computed (pilot) |", text)
        self.assertIn("| t1 | demo / locate | 50% | 100% | 2,000 | 1,000 | 50.0% | -75.0% |", text)
        self.assertIn("| Missing / contaminated runs | — | 1 / 0 | 1 / 0 | — |", text)
        self.assertIn("[per-task.csv](per-task.csv)", text)
        # Template links are rewritten relative to the output directory.
        self.assertIn(f"]({os.path.relpath(os.path.join(studylib.REPO_ROOT, 'docs', 'BENCHMARK.md'), out)})", text)

    def test_outputs_are_byte_identical_for_identical_inputs(self):
        a, b = os.path.join(self.tmp, "a"), os.path.join(self.tmp, "b")
        report.generate(self.csv, self.study, a)
        report.generate(self.csv, self.study, b)
        for name in ("report.md", "summary.json", "per-task.csv"):
            self.assertEqual(slurp(os.path.join(a, name), "rb"), slurp(os.path.join(b, name), "rb"), name)

    def test_zero_denominator_and_no_pairs_are_undefined_not_numbers(self):
        rows = [r for r in synthetic_rows() if r["task_id"] == "t1"]
        for r in rows:
            if r["config"] == "B":
                r["input_tokens_total"] = "0"
        write_rows(self.csv, rows)
        summary = report.generate(self.csv, self.study, os.path.join(self.tmp, "z"))
        comp = summary["comparisons"]["input_tokens_total"]
        self.assertIsNone(comp["ratio_c_over_b"])
        self.assertEqual(comp["undefined_reason"], "zero denominator")
        rows = [r for r in synthetic_rows() if r["config"] == "B"]
        write_rows(self.csv, rows)
        summary = report.generate(self.csv, self.study, os.path.join(self.tmp, "n"))
        self.assertEqual(summary["comparisons"]["input_tokens_total"]["undefined_reason"], "no paired tasks")
        self.assertIn("unavailable (no paired tasks)", slurp(os.path.join(self.tmp, "n", "report.md")))

    def test_non_opaque_values_are_refused(self):
        rows = synthetic_rows()
        rows[0]["category"] = "locate the Billing bug"
        write_rows(self.csv, rows)
        with self.assertRaises(report.ReportError):
            report.generate(self.csv, self.study, os.path.join(self.tmp, "x"))

    def test_end_to_end_csv_feeds_the_report(self):
        h = Harness()
        self.addCleanup(h.close)
        self.assertEqual(h.run().returncode, 0)
        csv_path = os.path.join(h.root, "runs.csv")
        with open(csv_path, "w") as handle:
            handle.write(extract.extract(h.study_dir))
        summary = report.generate(csv_path, h.study_path, os.path.join(h.root, "report"))
        self.assertAlmostEqual(summary["comparisons"]["input_tokens_total"]["ratio_c_over_b"], 1.0)
        self.assertEqual(summary["accounting"]["C"]["accounted"], 1)


if __name__ == "__main__":
    unittest.main()
