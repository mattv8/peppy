#!/usr/bin/env python3
"""Regression tests for the Rust Nextest shard runner."""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / ".github/scripts/run-rust-test-shard.sh"


class RustTestShardRunnerTests(unittest.TestCase):
    def run_runner(self, *args: str, cargo_exit: int = 0) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_path = pathlib.Path(temporary_directory)
            captured_arguments = temporary_path / "cargo-arguments"
            captured_cwd = temporary_path / "cargo-cwd"
            fake_cargo = temporary_path / "cargo"
            fake_cargo.write_text(
                "#!/usr/bin/env bash\n"
                "printf '%s\\n' \"$@\" > \"$CAPTURED_ARGUMENTS\"\n"
                "pwd > \"$CAPTURED_CWD\"\n"
                f"exit {cargo_exit}\n"
            )
            fake_cargo.chmod(0o755)
            environment = os.environ | {
                "CAPTURED_ARGUMENTS": str(captured_arguments),
                "CAPTURED_CWD": str(captured_cwd),
                "PATH": f"{temporary_path}:{os.environ['PATH']}",
            }
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


if __name__ == "__main__":
    unittest.main()
