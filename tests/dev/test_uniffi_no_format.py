import re
import subprocess
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
# Repository instruction files are out of scope for CI generation; they are never scanned.
EXCLUDED_PREFIXES = (".agents/", ".opencode/")
EXCLUDED_FILES = ("tests/dev/test_uniffi_no_format.py", "tests/dev/test_ci.py")
# Production generation sites today: ci.yml 2, build-ios-artifacts.sh 1, verify-android-native.sh 1,
# ci-test.sh 2, ios.sh 1, CONTRIBUTING.md 1, justfile 2. The floor prevents an empty or
# incomplete scan from passing; assertion strings in test_ci.py are not production sites.
MINIMUM_PRODUCTION_INVOCATIONS = 10
CONTINUATION = re.compile(r"\\\r?\n\s*")
GENERATE_TOKEN = re.compile(r"\bgenerate\b")


def logical_lines(text):
    return CONTINUATION.sub(" ", text).splitlines()


def generation_commands(text):
    return [
        line
        for line in logical_lines(text)
        if "uniffi-bindgen" in line and GENERATE_TOKEN.search(line)
    ]


def tracked_scannable_files():
    listed = subprocess.run(
        ["git", "-C", str(REPOSITORY_ROOT), "ls-files", "-z"],
        check=True, capture_output=True,
    ).stdout.decode().split("\0")
    for relative_path in filter(None, listed):
        if relative_path.startswith(EXCLUDED_PREFIXES) or relative_path in EXCLUDED_FILES:
            continue
        path = REPOSITORY_ROOT / relative_path
        if path.is_file():
            yield relative_path, path


class UniffiNoFormatTests(unittest.TestCase):
    def test_parser_recognises_single_and_continued_generation_commands(self):
        single = "cargo run --bin uniffi-bindgen -- generate --no-format --library x --language swift"
        continued = "cargo run --bin uniffi-bindgen -- \\\n  generate \\\n  --library x --language swift"
        self.assertEqual(generation_commands(single), [single])
        self.assertEqual(len(generation_commands(continued)), 1)
        self.assertNotIn("--no-format", generation_commands(continued)[0])

    def test_every_tracked_generate_invocation_opts_out_of_optional_formatting(self):
        checked = []
        for relative_path, path in tracked_scannable_files():
            text = path.read_text(errors="ignore")
            for command in generation_commands(text):
                checked.append((relative_path, command))
                self.assertIn("--no-format", command, f"{relative_path}: {command}")
        self.assertGreaterEqual(
            len(checked), MINIMUM_PRODUCTION_INVOCATIONS,
            "fewer UniFFI generate invocations found than expected; a checked site may have been removed",
        )


if __name__ == "__main__":
    unittest.main()
