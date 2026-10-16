import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
ISOLATED_ENVIRONMENT_KEYS = {
    "DEBUG",
    "PEPPY_ACCEPT_ANDROID_LICENSES",
    "PEPPY_DEV_TLS_CERT",
    "PEPPY_REPOSITORY_ROOT",
    "PEPPY_SOURCE_TREE",
    "PEPPY_TEST_CALLS",
}


class FastRecipeTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        (self.root / "infra/dev").mkdir(parents=True)
        shutil.copy(REPOSITORY_ROOT / "infra/dev/dev.sh", self.root / "infra/dev/dev.sh")
        shutil.copy(REPOSITORY_ROOT / "justfile", self.root / "justfile")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.calls = self.root / "calls"

    def tearDown(self):
        self.temp_dir.cleanup()

    def write_tool(self, name, body):
        tool = self.tools / name
        tool.write_text(f"#!/bin/bash\nset -euo pipefail\n{body}\n")
        tool.chmod(0o755)

    def write_android_helper(self):
        helper = self.root / "infra/dev/android.sh"
        helper.write_text('#!/bin/bash\nprintf "%s\\n" "$*" >> "$PEPPY_TEST_CALLS"\n')
        helper.chmod(0o755)

    def command_env(self, **overrides):
        environment = {
            key: value
            for key, value in os.environ.items()
            if key not in ISOLATED_ENVIRONMENT_KEYS and not key.startswith("PEPPY_ANDROID_")
        }
        environment.update({
            "PATH": f"{self.tools}:{environment['PATH']}",
            "PEPPY_REPOSITORY_ROOT": str(self.root),
            "PEPPY_TEST_CALLS": str(self.calls),
        })
        environment.update(overrides)
        return environment

    def test_fast_recipe_requires_license_before_android_helper(self):
        self.write_android_helper()

        result = subprocess.run(
            ["just", "android-run-fast"], cwd=self.root, text=True, capture_output=True,
            env=self.command_env(), check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PEPPY_ACCEPT_ANDROID_LICENSES=1 is required", result.stderr)
        self.assertFalse(self.calls.exists())

    def test_fast_recipe_invokes_only_android_run_fast(self):
        self.write_android_helper()

        result = subprocess.run(
            ["just", "android-run-fast"], cwd=self.root, text=True, capture_output=True,
            env=self.command_env(PEPPY_ACCEPT_ANDROID_LICENSES="1"), check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls.read_text(), "run-fast\n")

    def test_dev_script_routes_fast_recipe_through_source_selection(self):
        self.write_tool("git", 'exit 1')
        self.write_tool("just", 'printf "%s|%s\\n" "$PEPPY_SOURCE_TREE" "$*" > "$PEPPY_TEST_CALLS"')

        result = subprocess.run(
            ["bash", "infra/dev/dev.sh", "android-run-fast"], cwd=self.root, text=True,
            capture_output=True, env=self.command_env(), check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls.read_text(), f"{self.root}|android-run-fast\n")


if __name__ == "__main__":
    unittest.main()
