#!/usr/bin/env python3
"""Fail closed when a cargo-deny advisory exception lacks a near-term review date."""

import datetime as dt
import os
import re
import sys
import tomllib
from collections.abc import Mapping
from pathlib import Path


EXPIRY = re.compile(r"\[expires: (\d{4}-\d{2}-\d{2})\]$")
MAX_REVIEW_WINDOW = dt.timedelta(days=180)


def today() -> dt.date:
    return dt.date.fromisoformat(os.environ.get("ADVISORY_EXCEPTION_TODAY", dt.date.today().isoformat()))


def advisory_ignores(path: Path) -> list[dict[str, str]]:
    try:
        with path.open("rb") as config_file:
            config = tomllib.load(config_file)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"cannot parse {path} as TOML: {error}") from error

    advisories = config.get("advisories")
    if advisories is None:
        return []
    if not isinstance(advisories, Mapping):
        raise ValueError("advisories must be a TOML table")
    ignores = advisories.get("ignore", [])
    if not isinstance(ignores, list):
        raise ValueError("advisories.ignore must be a list of exception tables")

    exceptions: list[dict[str, str]] = []
    for index, exception in enumerate(ignores, start=1):
        if not isinstance(exception, Mapping) or set(exception) != {"id", "reason"}:
            raise ValueError(f"advisories.ignore entry {index} must contain only id and reason")
        identifier = exception["id"]
        reason = exception["reason"]
        if not isinstance(identifier, str) or not identifier or not isinstance(reason, str):
            raise ValueError(f"advisories.ignore entry {index} must use nonempty string id and string reason")
        exceptions.append({"id": identifier, "reason": reason})
    return exceptions


def validate(path: Path, reference_date: dt.date) -> list[str]:
    errors: list[str] = []
    for exception in advisory_ignores(path):
        identifier = exception["id"]
        reason = exception["reason"]
        expiry_match = EXPIRY.search(reason)
        if not expiry_match:
            errors.append(f"{identifier}: reason must end with [expires: YYYY-MM-DD]")
            continue
        try:
            expiry = dt.date.fromisoformat(expiry_match.group(1))
        except ValueError:
            errors.append(f"{identifier}: expiry date is invalid")
            continue
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
    try:
        errors = validate(path, today())
    except ValueError as error:
        errors = [str(error)]
    if errors:
        print("advisory exception policy failed:", *errors, sep="\n", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
