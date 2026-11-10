"""Reporting and answer-source regressions with small synthetic task trees."""

import contextlib
import io
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import report


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tasks = self.root / "tasks"
        for task in ["a", "b"]:
            path = self.tasks / task
            path.mkdir(parents=True)
            (path / "task.toml").write_text(f'[task]\nname = "bench/{task}"\n')
        rules = json.loads(Path(__file__).with_name("answer-source-rules.json").read_text())
        self.rules = [(r["id"], re.compile(r["pattern"], re.I)) for r in rules["rules"]]

    def trial(self, job="job", task="a", trial="one", checksum="same"):
        path = self.root / job / trial
        path.mkdir(parents=True)
        (path / "result.json").write_text(json.dumps({
            "task_name": f"bench/{task}", "trial_name": trial, "task_checksum": checksum,
            "agent_info": {"name": "fixture"}, "config": {"agent": {"kwargs": {}}},
            "started_at": None, "finished_at": None,
            "verifier_result": {"rewards": {"reward": 1}},
        }))

    def run_report(self, oracle=False):
        args = ["report.py", "--tasks-dir", str(self.tasks), "--dataset", "fixture",
                str(self.root / "job")]
        if oracle:
            args += ["--oracle-job", str(self.root / "oracle")]
        with patch.object(sys, "argv", args), contextlib.redirect_stdout(io.StringIO()) as out:
            report.main()
        return json.loads(out.getvalue())

    def test_missing_task_counts_as_zero(self):
        self.trial()
        data = self.run_report()
        self.assertEqual(data["score"], 0.5)
        self.assertEqual(data["n_expected_tasks"], 2)
        self.assertEqual(data["missing_tasks"], ["bench/b"])

    def test_duplicate_task_is_error(self):
        self.trial()
        self.trial(trial="retry")
        with self.assertRaisesRegex(ValueError, "Duplicate"):
            report.trials([self.root / "job"])

    def test_all_missing_has_zero_score(self):
        (self.root / "job").mkdir()
        data = self.run_report()
        self.assertEqual(data["score"], 0)
        self.assertEqual(data["missing_tasks"], ["bench/a", "bench/b"])
        self.assertIsNone(data["totals"]["cost_usd"])

    def test_oracle_checksum_compared(self):
        self.trial()
        self.trial(job="oracle", checksum="different")
        data = self.run_report(oracle=True)
        self.assertEqual(data["trials"][0]["oracle_checksum_check"]["status"], "mismatch")

    def test_rules_cover_review_examples_without_bare_word(self):
        for text in ["pip install harbor", "uvx harbor", "pip download terminal-bench",
                     "harbor datasets download x", "terminal-bench/terminal-bench-2-1",
                     "https://gitee.com/foo/terminal-bench", "https://gitcode.com/harbor-framework/harbor",
                     "gh repo clone laude-institute/terminal-bench", "https://pypi.org/project/harbor/",
                     "https://files.pythonhosted.org/packages/x/harbor-0.25.0.tar.gz"]:
            with self.subTest(text=text):
                self.assertTrue(any(p.search(text) for _, p in self.rules))
        self.assertFalse(any(p.search("Update site: add Terminal-Bench") for _, p in self.rules))

    def test_extensionless_and_cast_scanned_binary_listed(self):
        (self.root / "rewind-1").write_text("pip install harbor")
        (self.root / "recording.cast").write_text("harbor datasets download x")
        (self.root / "binary").write_bytes(b"\xff\x00")
        data = report.audit(self.root, self.rules)
        self.assertEqual(data["status"], "incomplete")
        self.assertIn("rewind-1", data["files_scanned"])
        self.assertIn("recording.cast", data["files_scanned"])
        self.assertIn("binary", [item["file"] for item in data["files_not_scanned"]])
        self.assertTrue(data["matched"])


if __name__ == "__main__":
    unittest.main()
