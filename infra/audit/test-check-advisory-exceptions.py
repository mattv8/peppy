#!/usr/bin/env python3
import datetime as dt
import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE = Path(__file__).with_name("check-advisory-exceptions.py")
SPEC = importlib.util.spec_from_file_location("check_advisory_exceptions", MODULE)
assert SPEC and SPEC.loader
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class AdvisoryExceptionTests(unittest.TestCase):
    def check(self, reason: str) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "deny.toml"
            path.write_text(f'[advisories]\nignore = [{{ id = "RUSTSEC-0000-0000", reason = "{reason}" }}]\n')
            return CHECK.validate(path, dt.date(2026, 11, 3))

    def test_accepts_reasoned_near_term_exception(self) -> None:
        self.assertEqual(self.check("Blocked upstream. [expires: 2027-02-01]"), [])

    def test_rejects_missing_expiry(self) -> None:
        self.assertIn("must end", self.check("Blocked upstream.")[0])

    def test_rejects_expired_or_distant_exception(self) -> None:
        self.assertIn("expired", self.check("Blocked upstream. [expires: 2026-11-03]")[0])
        self.assertIn("180-day", self.check("Blocked upstream. [expires: 2027-05-03]")[0])


if __name__ == "__main__":
    unittest.main()
