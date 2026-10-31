import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
# Hang guard only: `just doctor` finishes quickly, but a loaded machine running modules in parallel can exceed a few seconds.
DOCTOR_TIMEOUT = 30


@unittest.skipUnless(shutil.which("just"), "just is required")
class DoctorNodeVersionTests(unittest.TestCase):
    def test_doctor_honors_stable_node_version_selectors(self):
        cases = (
            ("26", "26.11.1", True),
            ("26", "26.12.0", True),
            ("26", "24.22.0", False),
            ("26", "27.0.0", False),
            ("26", "260.0.0", False),
            ("26", "26.12.0-rc.1", False),
            ("26.12", "26.12.1", True),
            ("26.12", "26.13.0", False),
            ("26.12.0", "26.12.0", True),
            ("26.12.0", "26.12.1", False),
            ("24.21.0", "24.21.0", True),
            ("", "26.12.0", False),
            ("26.", "26.12.0", False),
            ("abc", "26.12.0", False),
        )
        for selector, actual, accepted in cases:
            with self.subTest(selector=selector, actual=actual):
                result = self.run_doctor(selector, actual)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
                if not accepted:
                    self.assertIn("Node", result.stderr)
                    self.assertIn(actual, result.stderr)

    def run_doctor(self, selector, actual):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            tools = root / "bin"
            tools.mkdir()
            (root / ".node-version").write_text(selector + "\n")
            (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.98.1"\n')
            (root / ".env").write_text("PEPPY_ENV=test\n")
            outputs = {
                "cargo": "echo 'cargo 1.98.1'",
                "rustc": "echo 'rustc 1.98.1 (test)'",
                "node": f"echo '{actual}'",
                "pnpm": "echo 12.8.1",
                "docker": "exit 0",
                "xcode-select": "exit 1",
            }
            for name, body in outputs.items():
                tool = tools / name
                tool.write_text("#!/usr/bin/env bash\n" + body + "\n")
                tool.chmod(0o755)
            return subprocess.run(
                ["just", "--justfile", str(ROOT / "justfile"), "--working-directory", str(root), "doctor"],
                env=os.environ | {"PATH": f"{tools}:{os.environ['PATH']}"},
                text=True,
                capture_output=True,
                timeout=DOCTOR_TIMEOUT,
            )


if __name__ == "__main__":
    unittest.main()
