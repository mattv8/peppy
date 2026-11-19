#!/usr/bin/env python3
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit


SCRIPT = Path(__file__).with_name("ci-reuse.py")
spec = importlib.util.spec_from_file_location("ci_reuse", SCRIPT)
reuse = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = reuse
spec.loader.exec_module(reuse)
SHA = "a" * 40
BEFORE = "b" * 40
HEAD = "c" * 40


class Fixture:
    def __init__(self):
        self.diff = {"status": "ahead", "behind_by": 0, "files": [{"filename": "x"}]}
        self.pr = {"number": 42, "merged_at": "now", "base": {"ref": "production"},
                   "merge_commit_sha": SHA, "head": {"sha": HEAD, "ref": "feature",
                                                      "repo": {"full_name": "o/r"}}}
        self.pulls = [self.pr]
        self.head_pulls = [self.pr]
        self.pull_responses = []
        self.tree = {SHA: "tree", HEAD: "tree"}
        self.pr_run = {"id": 5, "run_attempt": 2, "path": ".github/workflows/ci.yml",
                       "head_sha": HEAD, "event": "pull_request", "head_branch": "feature",
                       "head_repository": {"full_name": "o/r"}, "pull_requests": [],
                       "html_url": "https://example/pr", "status": "in_progress"}
        self.full = {"id": 8, "run_attempt": 1, "created_at": "2026-01-01T00:00:00Z",
                     "status": "completed", "conclusion": "success", "html_url": "https://example/full"}
        self.pr_runs = [self.pr_run]
        self.full_runs = [self.full]
        self.jobs = {5: [{"name": "gate", "conclusion": "success"}],
                     8: [{"name": "gate", "conclusion": "success"}]}
        self.sleeps = []

    def request(self, path, paginate=False):
        if "/compare/" in path:
            return self.diff
        if urlsplit(path).path.endswith("/pulls"):
            page = int(parse_qs(urlsplit(path).query)["page"][0])
            if f"/commits/{HEAD}/pulls" in path:
                pulls = self.head_pulls
            elif self.pull_responses:
                pulls = self.pull_responses.pop(0)
            else:
                pulls = self.pulls
            return pulls[(page - 1) * 100:page * 100]
        if "/git/commits/" in path:
            return {"tree": {"sha": self.tree[path.rsplit("/", 1)[1]]}}
        if "/attempts/" in path:
            run_id = int(path.split("/actions/runs/")[1].split("/")[0])
            attempt = int(path.split("/attempts/")[1].split("/")[0])
            if run_id == 5:
                assert attempt == 2
            assert paginate
            return [{"jobs": self.jobs[run_id]}]
        if "event=pull_request" in path:
            return {"workflow_runs": self.pr_runs}
        if "event=schedule" in path:
            page = int(parse_qs(urlsplit(path).query)["page"][0])
            runs = sorted(self.full_runs, key=lambda run: run["created_at"], reverse=True)
            return {"workflow_runs": runs[(page - 1) * 100:page * 100]}
        if "event=workflow_dispatch" in path:
            return {"workflow_runs": []}
        raise AssertionError(path)

    def verifier(self, before=BEFORE):
        return reuse.Verifier("o/r", SHA, before, "production", "ci.yml",
                              self.request, self.sleeps.append)


class VerificationTests(unittest.TestCase):
    def setUp(self):
        self.f = Fixture()

    def assert_declined(self, text):
        result = self.f.verifier().verify()
        self.assertFalse(result.reused)
        self.assertIn(text, result.reason)

    def test_success_in_progress(self):
        result = self.f.verifier().verify()
        self.assertTrue(result.reused)
        self.assertEqual(result.evidence_run_url, "https://example/pr")

    def test_invalid_before(self):
        for before in ("0" * 40, "no", ""):
            self.assertFalse(self.f.verifier(before).verify().reused)

    def test_compare_failures(self):
        for change in ({"status": "identical"}, {"behind_by": 1},
                       {"files": [{"filename": "x"}] * 300},
                       {"files": [{"filename": ".github/workflows/ci.yml"}]},
                       {"files": [{"filename": "renamed", "previous_filename": ".github/scripts/ci-gate.sh"}]}):
            with self.subTest(change=change):
                self.f.diff = {"status": "ahead", "behind_by": 0, "files": []} | change
                self.assertFalse(self.f.verifier().verify().reused)

    def test_pr_failures(self):
        for mutation in ("none", "two", "base", "fork", "merge"):
            with self.subTest(mutation=mutation):
                self.f = Fixture()
                if mutation == "none":
                    self.f.pulls = []
                elif mutation == "two":
                    self.f.pulls.append(dict(self.f.pr))
                elif mutation == "base":
                    self.f.pr["base"]["ref"] = "other"
                elif mutation == "fork":
                    self.f.pr["head"]["repo"]["full_name"] = "fork/r"
                else:
                    self.f.pr["merge_commit_sha"] = BEFORE
                self.assert_declined("matching merged PR")

    def test_pr_retry(self):
        self.f.pull_responses = [[], [self.f.pr]]
        self.assertTrue(self.f.verifier().verify().reused)
        self.assertEqual(self.f.sleeps, [10])

    def test_tree_mismatch(self):
        self.f.tree[HEAD] = "other"
        self.assert_declined("trees differ")

    def test_pr_run_failures(self):
        for mutation in ("none", "branch", "fork", "event", "current_failure", "missing", "duplicate", "path", "sha"):
            with self.subTest(mutation=mutation):
                self.f = Fixture()
                if mutation == "none":
                    self.f.pr_runs = []
                elif mutation == "branch":
                    self.f.pr_run["head_branch"] = "other"
                elif mutation == "fork":
                    self.f.pr_run["head_repository"]["full_name"] = "fork/r"
                elif mutation == "event":
                    self.f.pr_run["event"] = "push"
                elif mutation == "current_failure":
                    self.f.jobs[5][0]["conclusion"] = "failure"
                elif mutation == "missing":
                    self.f.jobs[5] = []
                elif mutation == "duplicate":
                    self.f.jobs[5] *= 2
                elif mutation == "path":
                    self.f.pr_run["path"] = "wrong"
                else:
                    self.f.pr_run["head_sha"] = "wrong"
                self.assertFalse(self.f.verifier().verify().reused)

    def test_other_base_for_head_declines(self):
        self.f.head_pulls.append({"head": {"sha": HEAD}, "base": {"ref": "staging"}})
        self.assert_declined("head also proposed against staging")

    def test_pr_associations_paginated(self):
        self.f.head_pulls = [self.f.pr] * 100 + [
            {"head": {"sha": HEAD}, "base": {"ref": "staging"}}]
        self.assert_declined("head also proposed against staging")
        self.f = Fixture()
        unmatched = dict(self.f.pr, merged_at=None)
        self.f.pulls = [self.f.pr] + [unmatched] * 99 + [self.f.pr]
        self.assert_declined("expected exactly one matching merged PR")
        self.f = Fixture()
        self.f.pulls = [self.f.pr] * 500
        self.assert_declined("PR association incomplete")
        self.f = Fixture()
        self.f.head_pulls = [self.f.pr] * 500
        self.assert_declined("PR association incomplete")

    def test_unrelated_head_other_base_is_ignored(self):
        self.f.head_pulls.append({"head": {"sha": BEFORE}, "base": {"ref": "staging"}})
        self.assertTrue(self.f.verifier().verify().reused)

    def test_wrong_branch_and_fork_runs_ignored_when_newer(self):
        self.f.pr_runs.extend([
            dict(self.f.pr_run, id=6, head_branch="other"),
            dict(self.f.pr_run, id=7, head_repository={"full_name": "fork/r"}),
        ])
        self.assertTrue(self.f.verifier().verify().reused)

    def test_full_validation(self):
        self.f.full_runs = []
        self.assert_declined("no completed full validation run")
        self.f = Fixture()
        self.f.jobs[8][0]["conclusion"] = "failure"
        self.assert_declined("latest full validation run failed")
        self.f = Fixture()
        self.f.full_runs.append(dict(self.f.full, id=9, created_at="2027", conclusion="cancelled"))
        self.assertTrue(self.f.verifier().verify().reused)
        self.f.full_runs.append(dict(self.f.full, id=10, created_at="2028", conclusion="failure",
                                     html_url="https://example/failure"))
        self.f.jobs[10] = [{"name": "gate", "conclusion": "failure"}]
        self.assert_declined("https://example/failure")

    def test_full_history_pagination_and_cap(self):
        cancelled = [dict(self.f.full, id=100 + n, created_at=f"2029-{n:03}",
                          conclusion="cancelled") for n in range(100)]
        failure = dict(self.f.full, id=10, created_at="2028", conclusion="failure",
                       html_url="https://example/failure")
        self.f.full_runs = cancelled + [failure, self.f.full]
        self.f.jobs[10] = [{"name": "gate", "conclusion": "failure"}]
        self.assert_declined("https://example/failure")
        self.f.full_runs = cancelled * 3
        self.assert_declined("full validation history incomplete")

    def test_newest_matching_pr_run_wins(self):
        self.f.pr_runs.append(dict(self.f.pr_run, id=6, run_attempt=1))
        self.f.jobs[6] = [{"name": "gate", "conclusion": "failure"}]
        self.assert_declined("PR CI gate")

    def test_gate_on_later_page(self):
        request = self.f.request

        def paginated(path, paginate=False):
            if "/attempts/" in path:
                pages = request(path, paginate)
                return [{"jobs": [{"name": "other", "conclusion": "success"}]}] + pages
            return request(path, paginate)

        self.f.request = paginated
        self.assertTrue(self.f.verifier().verify().reused)

    def test_output_files(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            summary = Path(directory) / "summary"
            with patch.dict(os.environ, {"GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)}):
                reuse.write_result(reuse.Result(True, "one\ntwo", "url"))
            self.assertEqual(output.read_text(), "reused=true\nreason=one two\nevidence_run_url=url\n")
            self.assertIn("one two", summary.read_text())

    def test_output_errors_and_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.dict(os.environ, {"GITHUB_OUTPUT": str(output),
                                         "GITHUB_STEP_SUMMARY": directory}):
                self.assertEqual(reuse.write_result(reuse.Result(False, "no")), 0)
                self.assertTrue(output.exists())
            with patch.dict(os.environ, {"GITHUB_OUTPUT": directory,
                                         "GITHUB_STEP_SUMMARY": str(output)}):
                self.assertEqual(reuse.write_result(reuse.Result(False, "no")), 1)
        with patch.object(reuse.subprocess, "run", side_effect=subprocess.TimeoutExpired("gh", 60)) as run:
            with self.assertRaises(subprocess.TimeoutExpired):
                reuse.api("endpoint")
            self.assertEqual(run.call_args.kwargs["timeout"], 60)
            with patch.object(sys, "argv", ["ci-reuse.py", "verify", "--repo", "o/r",
                                          "--sha", SHA, "--before", BEFORE]), \
                    patch.dict(os.environ, {"GITHUB_OUTPUT": "", "GITHUB_STEP_SUMMARY": ""}), \
                    patch("sys.stdout", new_callable=io.StringIO) as stdout:
                self.assertEqual(reuse.main(), 0)
                self.assertIn("reused=false", stdout.getvalue())

    def test_cli_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            gh = Path(directory) / "gh"
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"])
            env.pop("GITHUB_OUTPUT", None)
            env.pop("GITHUB_STEP_SUMMARY", None)
            for body in ("printf 'not json'", "exit 1"):
                gh.write_text("#!/bin/sh\n" + body + "\n")
                gh.chmod(0o755)
                result = subprocess.run([sys.executable, str(SCRIPT), "verify", "--repo", "o/r",
                                         "--sha", SHA, "--before", BEFORE],
                                        env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0)
                self.assertIn("reused=false", result.stdout)


if __name__ == "__main__":
    unittest.main()
