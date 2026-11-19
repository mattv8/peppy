#!/usr/bin/env python3
"""Regression tests for the Rust Nextest shard runner."""

from __future__ import annotations

import os
import pathlib
import shlex
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / ".github/scripts/run-rust-test-shard.sh"


class RustTestShardRunnerTests(unittest.TestCase):
    def run_runner(self, *args: str, cargo_exit: int = 0, summary: bool = False, target: str = "", write_junit: bool = False, broken_python3: bool = False, archive: str = "") -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_path = pathlib.Path(temporary_directory)
            captured_arguments = temporary_path / "cargo-arguments"
            captured_cwd = temporary_path / "cargo-cwd"
            fake_cargo = temporary_path / "cargo"
            fake_cargo.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' \"$@\" > \"$CAPTURED_ARGUMENTS\"\n"
                "pwd > \"$CAPTURED_CWD\"\n"
                + ("printf '%s' '<testsuite><testcase name=\"slow\" time=\"9\"/></testsuite>' > \"$CARGO_TARGET_DIR/nextest/ci/junit.xml\"\n" if write_junit else "")
                +
                f"exit {cargo_exit}\n"
            )
            fake_cargo.chmod(0o755)
            if broken_python3:
                python3 = temporary_path / "python3"
                python3.write_text("#!/bin/sh\nexit 1\n")
                python3.chmod(0o755)
                python = temporary_path / "python"
                python.write_text(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} \"$@\"\n")
                python.chmod(0o755)
            environment = os.environ | {
                "CAPTURED_ARGUMENTS": str(captured_arguments),
                "CAPTURED_CWD": str(captured_cwd),
                "PATH": f"{temporary_path}:{os.environ['PATH']}",
            }
            environment.pop("GITHUB_STEP_SUMMARY", None)
            environment.pop("CARGO_TARGET_DIR", None)
            environment["PEPPY_NEXTEST_ARCHIVE"] = archive
            if summary:
                environment["GITHUB_STEP_SUMMARY"] = str(temporary_path / "summary")
            if target:
                environment["CARGO_TARGET_DIR"] = target
            result = subprocess.run(
                [str(RUNNER), *args],
                cwd=temporary_path,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )
            result.cargo_arguments = (
                captured_arguments.read_text().splitlines()
                if captured_arguments.exists()
                else []
            )
            result.cargo_cwd = captured_cwd.read_text().strip() if captured_cwd.exists() else None
            summary_path = temporary_path / "summary"
            result.summary = summary_path.read_text() if summary_path.exists() else ""
            return result

    def test_workspace_command_uses_root_config_and_partition(self) -> None:
        result = self.run_runner("workspace", "2", "4")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.cargo_cwd, str(ROOT))
        self.assertEqual(
            result.cargo_arguments,
            [
                "nextest", "run", "--workspace", "--locked", "--profile", "ci",
                "--config-file", str(ROOT / ".config/nextest.toml"),
                "--partition", "hash:2/4",
            ],
        )

    def test_desktop_command_uses_separate_manifest_and_partition(self) -> None:
        result = self.run_runner("desktop", "4", "4")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.cargo_cwd, str(ROOT))
        self.assertEqual(
            result.cargo_arguments,
            [
                "nextest", "run", "--manifest-path",
                str(ROOT / "apps/desktop/src-tauri/Cargo.toml"), "--locked", "--profile", "ci",
                "--config-file", str(ROOT / ".config/nextest.toml"),
                "--partition", "hash:4/4",
            ],
        )

    def test_archive_commands_extract_to_target_parent(self) -> None:
        for lane, parent in [("workspace", ROOT), ("desktop", ROOT / "apps/desktop/src-tauri")]:
            with self.subTest(lane=lane):
                result = self.run_runner(lane, "2", "4", archive="/tmp/tests.tar.zst")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.cargo_arguments, [
                    "nextest", "run", "--archive-file", "/tmp/tests.tar.zst",
                    "--extract-to", str(parent), "--extract-overwrite",
                    "--profile", "ci", "--config-file", str(ROOT / ".config/nextest.toml"),
                    "--partition", "hash:2/4",
                ])

    def test_archive_reports_junit_and_propagates_failure(self) -> None:
        with tempfile.TemporaryDirectory() as parent:
            target = pathlib.Path(parent) / "target"
            (target / "nextest/ci").mkdir(parents=True)
            result = self.run_runner("desktop", "2", "4", cargo_exit=37, summary=True,
                                     target=str(target), write_junit=True, archive="/tmp/tests.tar.zst")
        self.assertEqual(result.returncode, 37)
        self.assertIn("slow", result.summary)
        self.assertEqual(result.cargo_arguments[5], parent)

    def test_empty_archive_uses_normal_mode(self) -> None:
        result = self.run_runner("workspace", "1", "1", archive="")
        self.assertIn("--workspace", result.cargo_arguments)
        self.assertNotIn("--archive-file", result.cargo_arguments)

    def test_archive_rejects_target_dir_with_nonstandard_basename(self) -> None:
        with tempfile.TemporaryDirectory() as parent:
            result = self.run_runner("workspace", "1", "1",
                                     target=str(pathlib.Path(parent) / "custom-output"),
                                     archive="/tmp/tests.tar.zst")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.cargo_arguments, [])

    def test_forwards_each_canonical_shard_argument(self) -> None:
        partitions = set()
        for index in range(1, 5):
            result = self.run_runner("workspace", str(index), "4")
            self.assertEqual(result.returncode, 0, result.stderr)
            partitions.add(result.cargo_arguments[-1])

        self.assertEqual(partitions, {"hash:1/4", "hash:2/4", "hash:3/4", "hash:4/4"})

    def test_rejects_invalid_or_noncanonical_arguments_without_running_cargo(self) -> None:
        invalid_arguments = [
            (), ("unknown", "1", "1"), ("workspace",), ("workspace", "1"),
            ("workspace", "0", "1"), ("workspace", "2", "1"),
            ("workspace", "01", "1"), ("workspace", "+1", "1"),
            ("workspace", "one", "1"), ("workspace", "1", "01"),
            ("workspace", "18446744073709551617", "4"),
            ("workspace", "9223372036854775808", "4"),
            ("workspace", "1", "1", "extra"),
        ]
        for arguments in invalid_arguments:
            with self.subTest(arguments=arguments):
                result = self.run_runner(*arguments)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.cargo_arguments, [])

    def test_propagates_nextest_failure(self) -> None:
        result = self.run_runner("workspace", "1", "1", cargo_exit=37)

        self.assertEqual(result.returncode, 37)

    def test_reports_after_failure_and_respects_target_dir(self) -> None:
        with tempfile.TemporaryDirectory() as target:
            junit = pathlib.Path(target) / "nextest/ci/junit.xml"
            junit.parent.mkdir(parents=True)
            result = self.run_runner("desktop", "2", "4", cargo_exit=37, summary=True, target=target, write_junit=True)
        self.assertEqual(result.returncode, 37)
        self.assertIn("desktop shard 2/4", result.summary)
        self.assertIn("slow", result.summary)

    def test_stale_junit_is_not_reported(self) -> None:
        with tempfile.TemporaryDirectory() as target:
            junit = pathlib.Path(target) / "nextest/ci/junit.xml"
            junit.parent.mkdir(parents=True)
            junit.write_text('<testsuite><testcase name="stale" time="9"/></testsuite>')
            result = self.run_runner("workspace", "1", "1", summary=True, target=target)
        self.assertEqual(result.returncode, 0)
        self.assertIn("No readable JUnit", result.summary)
        self.assertNotIn("stale", result.summary)

    def test_falls_back_when_python3_cannot_run(self) -> None:
        result = self.run_runner("workspace", "1", "1", summary=True, broken_python3=True)
        self.assertEqual(result.returncode, 0)
        self.assertIn("No readable JUnit", result.summary)


if __name__ == "__main__":
    unittest.main()
