#!/usr/bin/env python3
"""Tests for CI timing reports."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("ci-timing-report.py")


class TimingReportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def run_report(self, *args, summary=None):
        env = os.environ.copy()
        env.pop("GITHUB_STEP_SUMMARY", None)
        if summary:
            env["GITHUB_STEP_SUMMARY"] = str(summary)
        return subprocess.run([sys.executable, str(SCRIPT), *map(str, args)],
                              capture_output=True, text=True, env=env, check=True)

    def test_junit_directory_and_formats(self):
        (self.root / "nested").mkdir()
        (self.root / "nested" / "nextest.xml").write_text(
            '<testsuites><testsuite><testcase classname="crate" name="slow" time="8.25"/></testsuite></testsuites>')
        (self.root / "gradle.xml").write_text(
            '<testsuite><testcase classname="Android" name="fast" time="1.5"/></testsuite>')
        (self.root / "invalid.xml").write_text("<bad")
        result = self.run_report("junit", "--limit", "1", self.root)
        self.assertIn("Tests: 2; summed test time: 9.750", result.stdout)
        self.assertIn("crate.slow", result.stdout)
        self.assertNotIn("Android.fast", result.stdout)

    def test_missing_and_malformed_results(self):
        (self.root / "bad.xml").write_text("<bad")
        result = self.run_report("junit", self.root / "missing", self.root / "bad.xml")
        self.assertIn("No readable JUnit", result.stdout)

    def test_jobs_order_missing_times_and_summary_append(self):
        jobs = {"jobs": [
            {"name": "short", "conclusion": "success", "created_at": "2026-01-01T00:00:00Z",
             "started_at": "2026-01-01T00:00:05Z", "completed_at": "2026-01-01T00:00:07Z"},
            {"name": "long", "conclusion": "failure", "created_at": "2026-01-01T00:00:00Z",
             "started_at": "2026-01-01T00:00:10Z", "completed_at": "2026-01-01T00:00:30Z"},
            {"name": "queued", "created_at": "2026-01-01T00:00:00Z", "started_at": None, "completed_at": None},
        ]}
        fixture = self.root / "jobs.json"
        fixture.write_text(json.dumps(jobs))
        summary = self.root / "summary"
        summary.write_text("previous\n")
        result = self.run_report("run", "--repo", "a/b", "--run-id", "1",
                                 "--jobs-json", fixture, summary=summary)
        text = summary.read_text()
        self.assertEqual(result.stdout, "")
        self.assertTrue(text.startswith("previous\n"))
        self.assertIn("Wall time: 30 seconds; total queued: 15 seconds", text)
        self.assertLess(text.index("| long |"), text.index("| short |"))
        self.assertIn("| queued | — | — | — |", text)

    def test_bad_json_warns_without_failure(self):
        fixture = self.root / "bad.json"
        fixture.write_text("{")
        result = self.run_report("run", "--repo", "a/b", "--run-id", "1", "--jobs-json", fixture)
        self.assertIn("::warning::", result.stderr)

    def test_unexpected_jobs_shape_warns_without_failure(self):
        fixture = self.root / "bad-shape.json"
        for data in ('[[]]', '[{"jobs": [[]]}]'):
            with self.subTest(data=data):
                fixture.write_text(data)
                result = self.run_report("run", "--repo", "a/b", "--run-id", "1", "--jobs-json", fixture)
                self.assertIn("::warning::", result.stderr)


if __name__ == "__main__":
    unittest.main()
