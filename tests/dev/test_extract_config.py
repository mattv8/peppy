import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
EXTRACTOR = ROOT / "infra/compose/extract-config.py"


class ExtractConfigTests(unittest.TestCase):
    def run_extractor(self, *arguments: str):
        return subprocess.run(
            [sys.executable, str(EXTRACTOR), *arguments],
            text=True,
            capture_output=True,
        )

    def test_extracts_literal_content_and_unescapes_compose_dollars(self):
        with tempfile.TemporaryDirectory() as directory:
            compose = Path(directory) / "compose.yml"
            compose.write_text(
                "services: {}\nconfigs:\n  fixture:\n    content: |\n"
                "      first $$ value\n      $${VAR}\n\n      last\n"
            )
            result = self.run_extractor(str(compose), "fixture")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "first $ value\n${VAR}\n\nlast\n")

    def test_clips_trailing_blank_lines_before_the_next_config(self):
        with tempfile.TemporaryDirectory() as directory:
            compose = Path(directory) / "compose.yml"
            compose.write_text(
                "configs:\n  fixture:\n    content: |\n      first\n\n      last\n\n\n"
                "  next:\n    content: |\n      next\n"
            )
            result = self.run_extractor(str(compose), "fixture")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "first\n\nlast\n")

    def test_keeps_an_eof_literal_block_without_a_final_newline(self):
        with tempfile.TemporaryDirectory() as directory:
            compose = Path(directory) / "compose.yml"
            compose.write_text("configs:\n  fixture:\n    content: |\n      final")
            result = self.run_extractor(str(compose), "fixture")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "final")

    def test_reports_invalid_inputs_with_exit_code_two(self):
        cases = (
            ("missing file", ("missing.yml", "fixture"), "cannot read Compose file"),
            ("missing config", ("configs:\n", "fixture"), "was not found"),
            ("missing content", ("configs:\n  fixture:\n    external: true\n", "fixture"), "has no content: | block"),
        )
        with tempfile.TemporaryDirectory() as directory:
            for name, source, expected in cases:
                with self.subTest(name=name):
                    if name == "missing file":
                        result = self.run_extractor(str(Path(directory) / source[0]), source[1])
                    else:
                        compose = Path(directory) / f"{name}.yml"
                        compose.write_text(source[0])
                        result = self.run_extractor(str(compose), source[1])
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(expected, result.stderr)
