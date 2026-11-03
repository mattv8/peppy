#!/usr/bin/env python3
"""Fail closed when a cargo-deny advisory exception lacks a near-term review date."""

import datetime as dt
import os
import re
import sys
from pathlib import Path


EXPIRY = re.compile(r"\[expires: (\d{4}-\d{2}-\d{2})\]$")
EXCEPTION = re.compile(r'\{\s*id\s*=\s*"(?P<id>[^"]+)"\s*,\s*reason\s*=\s*"(?P<reason>[^"]*)"\s*\}')
MAX_REVIEW_WINDOW = dt.timedelta(days=180)


def today() -> dt.date:
    return dt.date.fromisoformat(os.environ.get("ADVISORY_EXCEPTION_TODAY", dt.date.today().isoformat()))


def advisory_ignores(path: Path) -> list[dict[str, str]]:
    text = path.read_text()
    section = re.search(r"^\[advisories\]$(.*?)(?=^\[|\Z)", text, re.MULTILINE | re.DOTALL)
    if section is None:
        return []
    matches = list(EXCEPTION.finditer(section.group(1)))
    if section.group(1).count("id =") != len(matches):
        raise ValueError("cannot parse every advisory exception")
    return [match.groupdict() for match in matches]


def validate(path: Path, reference_date: dt.date) -> list[str]:
    errors: list[str] = []
    for exception in advisory_ignores(path):
        identifier = exception["id"]
        reason = exception["reason"]
        expiry_match = EXPIRY.search(reason)
        if not expiry_match:
            errors.append(f"{identifier}: reason must end with [expires: YYYY-MM-DD]")
            continue
        expiry = dt.date.fromisoformat(expiry_match.group(1))
        if expiry <= reference_date:
            errors.append(f"{identifier}: exception expired on {expiry}")
        elif expiry > reference_date + MAX_REVIEW_WINDOW:
            errors.append(f"{identifier}: expiry {expiry} exceeds the 180-day review window")
    return errors


def main() -> int:
    path = Path(sys.argv[1]) if len(sys.argv) == 2 else Path("deny.toml")
    if len(sys.argv) > 2:
        print("usage: check-advisory-exceptions.py [deny.toml]", file=sys.stderr)
        return 2
    errors = validate(path, today())
    if errors:
        print("advisory exception policy failed:", *errors, sep="\n", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
