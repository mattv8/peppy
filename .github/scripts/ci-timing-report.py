#!/usr/bin/env python3
"""Render best-effort CI timing summaries."""

import argparse
import datetime
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import xml.etree.ElementTree as ET


def cell(value):
    return str(value).replace("|", "\\|").replace("\n", " ")


def junit_report(paths, title, limit):
    tests = []
    for path in map(Path, paths):
        files = sorted(path.rglob("*.xml")) if path.is_dir() else [path]
        for file in files:
            try:
                root = ET.parse(file).getroot()
            except (OSError, ET.ParseError):
                continue
            for test in root.iter("testcase"):
                try:
                    duration = float(test.get("time", "0"))
                    if not math.isfinite(duration):
                        duration = 0
                except ValueError:
                    duration = 0
                name = ".".join(filter(None, (test.get("classname"), test.get("name"))))
                tests.append((duration, name))
    heading = f"## {title}\n\n"
    if not tests:
        return heading + "No readable JUnit test results found.\n"
    tests.sort(reverse=True)
    rows = "".join(f"| {cell(name)} | {duration:.3f} |\n" for duration, name in tests[:max(0, limit)])
    return (heading + f"Tests: {len(tests)}; summed test time: {sum(t[0] for t in tests):.3f} seconds.\n\n"
            + "| test | seconds |\n| --- | ---: |\n" + rows)


def timestamp(value):
    try:
        parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed if parsed.tzinfo else parsed.replace(tzinfo=datetime.timezone.utc)
    except (AttributeError, ValueError):
        return None


def seconds(start, end):
    return (end - start).total_seconds() if start and end else None


def display(value):
    return "—" if value is None else f"{value:.0f}"


def run_report(args):
    if args.jobs_json:
        data = json.loads(Path(args.jobs_json).read_text(encoding="utf-8"))
        pages = [data]
    else:
        endpoint = f"repos/{args.repo}/actions/runs/{args.run_id}"
        if args.attempt:
            endpoint += f"/attempts/{args.attempt}"
        output = subprocess.run(
            ["gh", "api", "--paginate", "--slurp", endpoint + "/jobs"],
            check=True, capture_output=True, text=True,
        ).stdout
        pages = json.loads(output)
    if isinstance(pages, dict):
        pages = [pages]
    jobs = [job for page in pages for job in page["jobs"]]
    rows = []
    created_times, completed_times = [], []
    for job in jobs:
        created = timestamp(job.get("created_at"))
        started = timestamp(job.get("started_at"))
        completed = timestamp(job.get("completed_at"))
        if created:
            created_times.append(created)
        if completed:
            completed_times.append(completed)
        rows.append((job, seconds(created, started), seconds(started, completed)))
    rows.sort(key=lambda row: row[2] if row[2] is not None else float("-inf"), reverse=True)
    wall = seconds(min(created_times), max(completed_times)) if created_times and completed_times else None
    total_queue = sum(queue for _, queue, _ in rows if queue is not None)
    report = (f"## CI job timing\n\nWall time: {display(wall)} seconds; total queued: {display(total_queue)} seconds.\n\n"
              "| job | conclusion | queued seconds | run seconds |\n| --- | --- | ---: | ---: |\n")
    for job, queue, duration in rows:
        report += f"| {cell(job.get('name', '—'))} | {cell(job.get('conclusion') or '—')} | {display(queue)} | {display(duration)} |\n"
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    junit = commands.add_parser("junit")
    junit.add_argument("--title", default="Slowest tests")
    junit.add_argument("--limit", type=int, default=15)
    junit.add_argument("paths", nargs="+")
    run = commands.add_parser("run")
    run.add_argument("--repo", required=True)
    run.add_argument("--run-id", required=True)
    run.add_argument("--attempt")
    run.add_argument("--jobs-json")
    args = parser.parse_args()
    try:
        report = junit_report(args.paths, args.title, args.limit) if args.command == "junit" else run_report(args)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a", encoding="utf-8") as output:
                output.write(report + "\n")
        else:
            print(report, end="")
    except Exception as error:
        print(f"::warning::CI timing report unavailable: {error}", file=sys.stderr)


if __name__ == "__main__":
    main()
