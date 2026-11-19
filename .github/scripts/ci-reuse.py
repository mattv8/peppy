#!/usr/bin/env python3
"""Fail-closed verification of reusable pull-request CI evidence."""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from urllib.parse import quote, urlencode


PROTECTED = {
    ".github/workflows/ci.yml",
    ".github/scripts/ci-scope.sh",
    ".github/scripts/ci-gate.sh",
    ".github/scripts/ci-reuse.py",
    ".github/ci-paths-filter.yml",
}
PAGE_SIZE = 100
PR_PAGE_LIMIT = 5
FULL_RUN_PAGE_LIMIT = 3
GH_TIMEOUT_SECONDS = 60


@dataclass
class Result:
    reused: bool
    reason: str
    evidence_run_url: str = ""


def api(endpoint, paginate=False):
    command = ["gh", "api"]
    if paginate:
        command += ["--paginate", "--slurp"]
    command.append(endpoint)
    result = subprocess.run(command, capture_output=True, text=True, check=True,
                            timeout=GH_TIMEOUT_SECONDS)
    return json.loads(result.stdout)


class Verifier:
    def __init__(self, repo, sha, before, branch, workflow, request=api, sleep=time.sleep):
        self.repo = repo
        self.sha = sha
        self.before = before
        self.branch = branch
        self.workflow = workflow
        self.request = request
        self.sleep = sleep
        self.root = f"repos/{repo}"

    def runs(self, **params):
        path = f"{self.root}/actions/workflows/{quote(self.workflow, safe='')}/runs"
        return self.request(f"{path}?{urlencode(params)}")["workflow_runs"]

    def gate_passed(self, run):
        path = (f"{self.root}/actions/runs/{run['id']}/attempts/"
                f"{run['run_attempt']}/jobs?per_page=100")
        pages = self.request(path, paginate=True)
        jobs = [job for page in pages for job in page["jobs"] if job["name"] == "gate"]
        return len(jobs) == 1 and jobs[0]["conclusion"] == "success"

    def associated_pulls(self, sha):
        pulls = []
        for page in range(1, PR_PAGE_LIMIT + 1):
            batch = self.request(f"{self.root}/commits/{sha}/pulls?per_page={PAGE_SIZE}&page={page}")
            if not isinstance(batch, list):
                raise ValueError("invalid pull request list")
            pulls.extend(batch)
            if len(batch) < PAGE_SIZE:
                return pulls
        return None

    def newest_full_run(self, event):
        for page in range(1, FULL_RUN_PAGE_LIMIT + 1):
            runs = self.runs(branch=self.branch, event=event, per_page=PAGE_SIZE, page=page)
            for run in runs:
                if run["status"] == "completed" and run["conclusion"] != "cancelled":
                    return run, True
            if len(runs) < PAGE_SIZE:
                return None, True
        return None, False

    def verify_pr(self):
        if not re.fullmatch(r"[0-9a-fA-F]{40}", self.before) or int(self.before, 16) == 0:
            return Result(False, "invalid before SHA")
        diff = self.request(f"{self.root}/compare/{self.before}...{self.sha}")
        if diff["status"] != "ahead" or diff["behind_by"] != 0:
            return Result(False, "comparison is not strictly ahead")
        files = diff["files"]
        if not isinstance(files, list):
            raise ValueError("invalid file list")
        if len(files) >= 300:
            return Result(False, "diff too large to inspect")
        for file in files:
            if file["filename"] in PROTECTED or file.get("previous_filename") in PROTECTED:
                return Result(False, "gate-defining file changed")

        for attempt in range(3):
            pulls = self.associated_pulls(self.sha)
            if pulls is None:
                return Result(False, "PR association incomplete")
            matches = [
                pr for pr in pulls
                if pr["merged_at"] and pr["base"]["ref"] == self.branch
                and pr["merge_commit_sha"] == self.sha
                and pr["head"]["repo"] and pr["head"]["repo"]["full_name"] == self.repo
            ]
            if matches:
                break
            if attempt < 2:
                self.sleep(10)
        if len(matches) != 1:
            return Result(False, "expected exactly one matching merged PR")
        pr = matches[0]
        head = pr["head"]["sha"]
        tree = self.request(f"{self.root}/git/commits/{self.sha}")["tree"]["sha"]
        head_tree = self.request(f"{self.root}/git/commits/{head}")["tree"]["sha"]
        if not tree or tree != head_tree:
            return Result(False, "commit trees differ")

        head_pulls = self.associated_pulls(head)
        if head_pulls is None:
            return Result(False, "PR association incomplete")
        for candidate in head_pulls:
            if candidate["head"]["sha"] == head and candidate["base"]["ref"] != self.branch:
                return Result(False, f"head also proposed against {candidate['base']['ref']}")

        runs = self.runs(event="pull_request", head_sha=head, per_page=100)
        matching = [
            run for run in runs
            if run["event"] == "pull_request" and run["head_sha"] == head
            and run["path"] == f".github/workflows/{self.workflow}"
            and run["head_repository"]["full_name"] == self.repo
            and run["head_branch"] == pr["head"]["ref"]
        ]
        if not matching:
            return Result(False, "no matching PR CI run")
        run = max(matching, key=lambda item: item["id"])
        if not self.gate_passed(run):
            return Result(False, "PR CI gate did not succeed")
        if not isinstance(run["html_url"], str) or not run["html_url"]:
            raise ValueError("invalid run URL")
        return Result(True, f"reused PR #{pr['number']} run {run['id']} (tree {tree[:7]})",
                      run["html_url"])

    def verify(self):
        result = self.verify_pr()
        if not result.reused:
            return result
        runs = []
        for event in ("schedule", "workflow_dispatch"):
            run, complete = self.newest_full_run(event)
            if not complete:
                return Result(False, "full validation history incomplete")
            if run is not None:
                runs.append(run)
        if not runs:
            return Result(False, f"no completed full validation run on {self.branch} yet")
        latest = max(runs, key=lambda run: run["created_at"])
        if not self.gate_passed(latest):
            return Result(False, f"latest full validation run failed: {latest['html_url']}")
        return result


def write_result(result):
    reason = " ".join(result.reason.splitlines())
    evidence = " ".join(result.evidence_run_url.splitlines())
    output = f"reused={str(result.reused).lower()}\nreason={reason}\nevidence_run_url={evidence}\n"
    try:
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as file:
                file.write(output)
        else:
            print(output, end="")
    except OSError:
        print("warning: could not write CI reuse outputs", file=sys.stderr)
        return 1
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        try:
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as file:
                file.write(f"- CI validation reuse: {'yes' if result.reused else 'no'} — {reason}\n")
        except OSError:
            print("warning: could not write CI reuse summary", file=sys.stderr)
    return 0


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    verify = subparsers.add_parser("verify")
    verify.add_argument("--repo", required=True)
    verify.add_argument("--sha", required=True)
    verify.add_argument("--before", required=True)
    verify.add_argument("--branch", default="production")
    verify.add_argument("--workflow", default="ci.yml")
    args = parser.parse_args()
    try:
        result = Verifier(args.repo, args.sha, args.before, args.branch, args.workflow).verify()
    except Exception:
        result = Result(False, "verification unavailable or malformed API response")
    return write_result(result)


if __name__ == "__main__":
    sys.exit(main())
