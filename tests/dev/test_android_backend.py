import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


class AndroidBackendTests(unittest.TestCase):
    def write_command(self, directory, name, body):
        command = directory / name
        command.write_text(f"#!/bin/sh\n{body}\n")
        command.chmod(0o755)

    def test_auto_on_darwin_builds_natively_without_docker_or_env_file(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "native repo"
            script = repo / "infra/dev/android.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/android.sh", script)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            sdk = temp / "sdk"
            sdkmanager = sdk / "cmdline-tools/latest/bin/sdkmanager"
            sdkmanager.parent.mkdir(parents=True)
            sdkmanager.write_text("#!/bin/sh\nprintf 'sdk:%s\\n' \"$*\" >> \"$CALLS\"\n")
            sdkmanager.chmod(0o755)
            java_home = temp / "jdk"
            (java_home / "bin").mkdir(parents=True)
            (java_home / "bin/java").write_text("#!/bin/sh\necho 'openjdk version \"17.0.1\"' >&2\n")
            (java_home / "bin/java").chmod(0o755)
            for name, body in {
                "uname": "echo Darwin",
                "cargo": "exit 0",
                "rustup": "echo \"rustup:$PWD\" >> \"$CALLS\"; [ \"$PWD\" = \"$EXPECTED_RUSTUP_CWD\" ] && [ \"$1\" = target ] && [ \"$2\" = list ] && echo 'aarch64-linux-android\\nx86_64-linux-android'",
                "pkg-config": "exit 0",
                "sdkmanager": "echo path-sdkmanager >> \"$CALLS\"; exit 99",
                "docker": "echo docker >> \"$CALLS\"; exit 99",
            }.items():
                self.write_command(bin_dir, name, body)
            gradlew = repo / "apps/android/gradlew"
            gradlew.parent.mkdir(parents=True)
            gradlew.write_text(
                "#!/bin/sh\n"
                "mkdir -p app/build/outputs/apk/debug\n"
                "touch app/build/outputs/apk/debug/app-debug.apk\n"
            )
            gradlew.chmod(0o755)
            verifier = repo / "infra/compose/verify-android-native.sh"
            verifier.parent.mkdir(parents=True)
            verifier.write_text(
                "#!/bin/sh\n"
                "echo verifier >> \"$CALLS\"\n"
                "printf '%s\\n' \"$ANDROID_SDK_ROOT|$ANDROID_HOME|$ANDROID_NDK_HOME|$JAVA_HOME|$CARGO_TARGET_DIR\" >> \"$CALLS\"\n"
            )
            verifier.chmod(0o755)
            calls = temp / "calls"
            artifacts = temp / "artifacts"

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=temp, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:{os.environ['PATH']}",
                    "CALLS": str(calls), "ANDROID_SDK_ROOT": "sdk",
                    "JAVA_HOME": "jdk", "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                    "PEPPY_ANDROID_ARTIFACTS": "artifacts", "CARGO_TARGET_DIR": "target cache",
                    "EXPECTED_RUSTUP_CWD": str(repo),
                },
            )

            self.assertEqual(result.returncode, 0, f"{result.stderr}\n{calls.read_text() if calls.exists() else ''}")
            self.assertIn("native", result.stderr)
            self.assertTrue((artifacts / "app-debug.apk").is_file())
            self.assertIn("verifier", calls.read_text())
            self.assertNotIn("docker", calls.read_text())
            self.assertIn("sdk:--licenses", calls.read_text())
            self.assertIn("sdk:platforms;android-36 build-tools;35.0.0 platform-tools ndk;27.2.12479018", calls.read_text())
            self.assertNotIn("path-sdkmanager", calls.read_text())
            self.assertIn(
                f"{sdk.resolve()}|{sdk.resolve()}|{(sdk / 'ndk/27.2.12479018').resolve()}|{java_home.resolve()}|{(temp / 'target cache').resolve()}",
                calls.read_text(),
            )

    def test_docker_is_available_as_an_explicit_darwin_override(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            script = repo / "infra/dev/android.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/android.sh", script)
            (repo / ".env").touch()
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            calls = temp / "calls"
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "docker", "echo \"$@\" >> \"$CALLS\"")
            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls),
                    "PEPPY_ANDROID_BUILD_BACKEND": "docker",
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("Docker", result.stderr)
            self.assertIn("compose", calls.read_text())

    def test_auto_uses_docker_on_linux_and_wsl(self):
        for wsl in (False, True):
            with self.subTest(wsl=wsl), tempfile.TemporaryDirectory() as temp:
                temp = pathlib.Path(temp)
                repo = temp / "repo"
                script = repo / "infra/dev/android.sh"
                script.parent.mkdir(parents=True)
                shutil.copy(ROOT / "infra/dev/android.sh", script)
                (repo / ".env").touch()
                bin_dir = temp / "bin"
                bin_dir.mkdir()
                calls = temp / "calls"
                self.write_command(bin_dir, "uname", "echo Linux")
                self.write_command(bin_dir, "docker", "echo docker >> \"$CALLS\"")
                env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls)}
                if wsl:
                    env["WSL_INTEROP"] = "1"
                result = subprocess.run(["bash", str(script), "build"], cwd=repo, text=True, capture_output=True, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("docker", calls.read_text())

    def test_invalid_and_unsupported_backends_fail_before_build_effects(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            script = repo / "infra/dev/android.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/android.sh", script)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            calls = temp / "calls"
            self.write_command(bin_dir, "uname", "echo Linux")
            self.write_command(bin_dir, "docker", "echo docker >> \"$CALLS\"")
            for backend, expected in (("invalid", "must be auto"), ("native", "only on macOS")):
                with self.subTest(backend=backend):
                    result = subprocess.run(
                        ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                        env={"PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls), "PEPPY_ANDROID_BUILD_BACKEND": backend},
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(expected, result.stderr)
            self.assertFalse(calls.exists())

    def test_native_requires_license_before_invoking_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            script = repo / "infra/dev/android.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/android.sh", script)
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            calls = temp / "calls"
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "sdkmanager", "echo sdkmanager >> \"$CALLS\"")
            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={"HOME": str(temp), "PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls)},
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("PEPPY_ACCEPT_ANDROID_LICENSES", result.stderr)
            self.assertFalse(calls.exists())
