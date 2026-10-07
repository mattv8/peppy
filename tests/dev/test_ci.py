import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
RUNNER_PATH = REPOSITORY_ROOT / "infra/dev/ci-test.sh"


class CiTestRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp_dir = tempfile.TemporaryDirectory()
        self.root = Path(self.temp_dir.name)
        self.bin_dir = self.root / "bin"
        self.bin_dir.mkdir()
        self.repository_root = self.root / "repository"
        self.repository_root.mkdir()
        self.repository_root = self.repository_root.resolve()
        for source_directory in (".github", "apps", "infra", "packages"):
            (self.repository_root / source_directory).symlink_to(REPOSITORY_ROOT / source_directory)
        (self.repository_root / ".env").write_text("PEPPY_ENV=test\n")
        self.log_path = self.root / "commands.log"
        self.log_path.touch()
        self._write_stub("uname", "#!/bin/bash\necho Darwin\n")
        self._write_stub("xcode-select", "#!/bin/bash\necho /Applications/Xcode.app\n")
        self._write_stub("xcrun", "#!/bin/bash\necho /sdk\n")
        self._write_stub(
            "bash",
            "#!/bin/bash\nprintf 'bash %s\\n' \"$*\" >> \"$COMMAND_LOG\"\n"
            "if [[ $* == 'infra/dev/android.sh test' ]]; then printf 'bash-env PEPPY_ANDROID_BUILD_BACKEND=%s\\n' \"${PEPPY_ANDROID_BUILD_BACKEND:-}\" >> \"$COMMAND_LOG\"; fi\n",
        )
        self._write_stub(
            "tool",
            "#!/bin/bash\nprintf '%s %s\\n' \"$(basename \"$0\")\" \"$*\" >> \"$COMMAND_LOG\"\n"
            "if [[ \"$(basename \"$0\")\" == cargo && \"$*\" == *'--out-dir'* ]]; then\n"
            "  out=${*: -1}; language=\"$*\";\n"
            "  if [[ $language == *'--language kotlin'* ]]; then cp -R apps/android/app/src/main/java/uniffi \"$out\"; else cp apps/ios/Generated/peppy_mobile_bindingsFFI.h apps/ios/Generated/peppy_mobile_bindings.swift apps/ios/Generated/peppy_mobile_bindingsFFI.modulemap \"$out\"; fi\n"
            "fi\n"
            "[[ ${FAIL_COMMAND:-} != \"$(basename \"$0\") $*\" ]]\n",
        )
        for name in "cargo git-cliff pnpm node python3 shellcheck gitleaks docker just".split():
            (self.bin_dir / name).symlink_to(self.bin_dir / "tool")
        self._write_stub(
            "swift",
            f"#!{sys.executable}\n"
            "import os\n"
            "import sys\n"
            "arguments = ' '.join(sys.argv[1:])\n"
            "database_url = os.environ.get('DATABASE_URL', '')\n"
            "dyld_library_path = os.environ.get('DYLD_LIBRARY_PATH', '')\n"
            "with open(os.environ['COMMAND_LOG'], 'a') as command_log:\n"
            "    print(f'swift {arguments}', file=command_log)\n"
            "    print(f'swift-env DATABASE_URL={database_url} DYLD_LIBRARY_PATH={dyld_library_path}', file=command_log)\n"
            "raise SystemExit(os.environ.get('FAIL_COMMAND') == f'swift {arguments}')\n",
        )
        self._write_stub("curl", "#!/bin/bash\nprintf 'curl %s\\n' \"$*\" >> \"$COMMAND_LOG\"\nif [[ $* == *'--aws-sigv4'* ]]; then echo 200; else echo 403; fi\n")

    def tearDown(self):
        self.temp_dir.cleanup()

    def _write_stub(self, name, contents):
        path = self.bin_dir / name
        path.write_text(contents)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)

    def run_runner(self, **environment):
        env = os.environ | {
            "COMMAND_LOG": str(self.log_path),
            "PATH": f"{self.bin_dir}:{os.environ['PATH']}",
            "PEPPY_CI_SCRATCH": str(self.root / "scratch"),
            "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
            "PEPPY_TEST_COMPOSE_PROJECT": "peppy-ci-fixture",
            "PEPPY_REPOSITORY_ROOT": ".",
        } | environment
        return subprocess.run(["/bin/bash", str(RUNNER_PATH)], cwd=self.repository_root, env=env, capture_output=True, text=True)

    def test_runs_every_local_ci_family_and_cleans_disposable_infrastructure(self):
        target_dir = "ci-target"
        result = self.run_runner(CARGO_TARGET_DIR=target_dir, DATABASE_URL="postgres://original", PEPPY_ANDROID_BUILD_BACKEND="native")

        self.assertEqual(result.returncode, 0, result.stderr)
        commands = self.log_path.read_text()
        for command in (
            "docker compose version",
            "python3 -c import sys; raise SystemExit(sys.version_info < (3, 11))",
            "git-cliff --version",
            "cargo deny --version",
            "cargo audit --version",
            "shellcheck infra/release/",
            "bash infra/release/test-compute-version.sh",
            "bash infra/release/test-release-scripts.sh",
            "just audit-secrets",
            "node packages/mobile-design/scripts/generate.mjs --check",
            "node --test packages/mobile-design/test/generate.test.mjs",
            "python3 -m unittest discover -s tests/dev -p test_*.py",
            "bash -n infra/dev/dev.sh",
            "shellcheck infra/dev/dev.sh",
            "bash .github/scripts/test-ci-scripts.sh",
            "pnpm install --frozen-lockfile",
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets --locked -- -D warnings",
            "cargo test --workspace --lib --exclude peppy-push-relay --locked",
            "cargo run --locked -p peppy-protocol --bin generate-contracts -- --check",
            "cargo deny check licenses bans sources",
            "cargo deny check advisories",
            "python3 infra/audit/check-vendored.py",
            "pnpm --filter @peppy/browser-runtime typecheck",
            "pnpm --filter @peppy/browser-runtime test",
            "pnpm --filter @peppy/desktop-ui test",
            "pnpm --filter @peppy/desktop test",
            "pnpm --filter @peppy/desktop build",
            "pnpm --filter @peppy/web typecheck",
            "pnpm --filter @peppy/web test",
            "cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml -- --ignored",
            "pnpm --filter @peppy/web build",
            "cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml",
            "cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml",
            "cargo deny --manifest-path apps/desktop/src-tauri/Cargo.toml --config deny.toml check licenses bans sources advisories",
            "bash infra/dev/android.sh test",
            "bash-env PEPPY_ANDROID_BUILD_BACKEND=docker",
            f"cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library {self.repository_root / target_dir}/debug/libpeppy_mobile_bindings.dylib --language swift",
            f"swift test -Xlinker -L -Xlinker {self.repository_root / target_dir}/debug --no-parallel",
            f"swift run -Xlinker -L -Xlinker {self.repository_root / target_dir}/debug PeppyMobileSmoke",
            "swift-env DATABASE_URL=postgres://original",
            "docker compose --project-name",
            "docker compose --project-name peppy-ci-fixture -f - down --volumes --remove-orphans",
        ):
            self.assertIn(command, commands)
        self.assertEqual(
            commands.count(
                f"swift-env DATABASE_URL=postgres://original DYLD_LIBRARY_PATH={self.repository_root / target_dir}/debug"
            ),
            2,
        )

    def test_stops_after_a_failed_suite_and_still_cleans_disposable_infrastructure(self):
        result = self.run_runner(FAIL_COMMAND="cargo test --workspace --locked")

        self.assertNotEqual(result.returncode, 0)
        commands = self.log_path.read_text()
        self.assertIn("docker compose --project-name peppy-ci-fixture -f - down --volumes --remove-orphans", commands)
        self.assertNotIn("bash infra/dev/android.sh test", commands)

    def test_rejects_missing_android_license_before_running_a_suite(self):
        result = self.run_runner(PEPPY_ACCEPT_ANDROID_LICENSES="")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PEPPY_ACCEPT_ANDROID_LICENSES=1 is required", result.stderr)
        self.assertEqual(self.log_path.read_text(), "")

    def test_rejects_missing_fixture_env_before_running_a_suite(self):
        (self.repository_root / ".env").unlink()
        result = self.run_runner()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(".env is required", result.stderr)
        self.assertEqual(self.log_path.read_text(), "")

    def test_rejects_non_macos_host_before_running_a_suite(self):
        self._write_stub("uname", "#!/bin/bash\necho Linux\n")
        result = self.run_runner()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires macOS and Xcode", result.stderr)
        self.assertEqual(self.log_path.read_text(), "")


if __name__ == "__main__":
    unittest.main()
