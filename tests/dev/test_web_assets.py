import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "infra/dev/copy-web-assets.sh"
REQUIRED_ASSETS = (
    "index.html",
    "worker.js",
    "core/peppy-browser-core.js",
    "core/peppy_browser_core.wasm",
)


class WebAssetCopyTests(unittest.TestCase):
    def prepare_script(self, root, source, output):
        script = root / "copy-web-assets.sh"
        script.write_text(
            SCRIPT.read_text()
            .replace("source=/web", f"source={source}")
            .replace("output=/output", f"output={output}")
            .replace("mountpoint=/output", f"mountpoint={output}")
        )
        script.chmod(0o755)
        return script

    def write_assets(self, source):
        for asset in REQUIRED_ASSETS:
            path = source / asset
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(asset)

    def run_copy(self, script, tools):
        awk = tools / "awk"
        awk.write_text("#!/bin/sh\nexit 0\n")
        awk.chmod(0o755)
        return subprocess.run(
            [str(script)], text=True, capture_output=True,
            env=os.environ | {"PATH": f"{tools}:{os.environ['PATH']}"},
        )

    def test_clean_copy_replaces_stale_assets_and_keeps_required_assets_readable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            output = root / "output"
            tools = root / "tools"
            source.mkdir()
            output.mkdir()
            tools.mkdir()
            self.write_assets(source)
            (source / "assets").mkdir()
            (source / "assets/current.js").write_text("current")
            (output / "assets").mkdir()
            (output / "assets/stale.js").write_text("stale")

            result = self.run_copy(self.prepare_script(root, source, output), tools)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse((output / "assets/stale.js").exists())
            self.assertEqual((output / "assets/current.js").read_text(), "current")
            for asset in REQUIRED_ASSETS:
                self.assertTrue(os.access(output / asset, os.R_OK), asset)

    def test_missing_required_asset_fails_without_cleaning_output(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            output = root / "output"
            tools = root / "tools"
            source.mkdir()
            output.mkdir()
            tools.mkdir()
            self.write_assets(source)
            (source / "worker.js").unlink()
            sentinel = output / "keep-me"
            sentinel.write_text("present")

            result = self.run_copy(self.prepare_script(root, source, output), tools)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Missing built asset: worker.js", result.stderr)
            self.assertEqual(sentinel.read_text(), "present")

    def test_production_script_requires_the_expected_mounted_output(self):
        script = SCRIPT.read_text()
        self.assertIn("output=/output", script)
        self.assertIn("mountpoint=/output", script)
        self.assertIn("Output directory is not a mounted volume", script)


if __name__ == "__main__":
    unittest.main()
