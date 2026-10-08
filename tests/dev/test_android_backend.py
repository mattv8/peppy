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

    def copy_script_with_fake_java_home(self, repo, fake_java_home):
        script = repo / "infra/dev/android.sh"
        script.parent.mkdir(parents=True)
        source = (ROOT / "infra/dev/android.sh").read_text()
        script.write_text(source.replace("/usr/libexec/java_home", str(fake_java_home)))
        script.chmod(0o755)
        return script

    def create_native_build_files(self, repo, sdk):
        sdkmanager = sdk / "cmdline-tools/latest/bin/sdkmanager"
        sdkmanager.parent.mkdir(parents=True)
        sdkmanager.write_text("#!/bin/sh\nexit 0\n")
        sdkmanager.chmod(0o755)
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
        verifier.write_text("#!/bin/sh\nexit 0\n")
        verifier.chmod(0o755)

    def write_jdk(self, java_home):
        java = java_home / "bin/java"
        java.parent.mkdir(parents=True)
        java.write_text("#!/bin/sh\necho 'openjdk version \"17.0.1\"' >&2\n")
        java.chmod(0o755)

    def write_rust_tools(self, bin_dir):
        bin_dir.mkdir(parents=True)
        self.write_command(bin_dir, "cargo", "exit 0")
        self.write_command(
            bin_dir,
            "rustup",
            "[ \"$1\" = target ] && [ \"$2\" = list ] && echo 'aarch64-linux-android\\nx86_64-linux-android'",
        )

    def test_native_discovers_unregistered_homebrew_jdk_and_rustup(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "native repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            calls = temp / "calls"
            fake_java_home = bin_dir / "java_home"
            self.write_command(bin_dir, "java_home", "exit 1")
            script = self.copy_script_with_fake_java_home(repo, fake_java_home)
            sdk = temp / "sdk"
            self.create_native_build_files(repo, sdk)
            jdk = temp / "brew cellar with spaces/openjdk@17/libexec/openjdk.jdk/Contents/Home"
            self.write_jdk(jdk)
            rustup_prefix = temp / "brew cellar with spaces/rustup"
            self.write_rust_tools(rustup_prefix / "bin")
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(
                bin_dir,
                "brew",
                "echo \"brew:$*\" >> \"$CALLS\"\n"
                "[ \"$1\" = --prefix ] && [ \"$2\" = --installed ] && [ \"$3\" = openjdk@17 ] && echo \"$BREW_JDK_PREFIX\"\n"
                "[ \"$1\" = --prefix ] && [ \"$2\" = --installed ] && [ \"$3\" = rustup ] && echo \"$BREW_RUSTUP_PREFIX\"",
            )
            self.write_command(bin_dir, "pkg-config", "[ \"$1\" = --exists ] && [ \"$2\" = libsodium ]")

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "ANDROID_SDK_ROOT": str(sdk), "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                    "BREW_JDK_PREFIX": str(jdk.parents[3]), "BREW_RUSTUP_PREFIX": str(rustup_prefix),
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                calls.read_text().splitlines(),
                ["brew:--prefix --installed openjdk@17", "brew:--prefix --installed rustup"],
            )

    def test_native_uses_default_cargo_home_before_homebrew_rustup(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            script = self.copy_script_with_fake_java_home(repo, bin_dir / "java_home")
            sdk = temp / "sdk"
            self.create_native_build_files(repo, sdk)
            jdk = temp / "jdk"
            self.write_jdk(jdk)
            self.write_rust_tools(temp / ".cargo/bin")
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"; exit 99")
            self.write_command(bin_dir, "pkg-config", "exit 0")
            calls = temp / "calls"

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "ANDROID_SDK_ROOT": str(sdk), "JAVA_HOME": str(jdk), "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(calls.exists())

    def test_native_uses_explicit_cargo_home_before_homebrew_rustup(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            script = self.copy_script_with_fake_java_home(repo, bin_dir / "java_home")
            sdk = temp / "sdk"
            self.create_native_build_files(repo, sdk)
            jdk = temp / "jdk"
            self.write_jdk(jdk)
            cargo_home = temp / "cargo home"
            self.write_rust_tools(cargo_home / "bin")
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"; exit 99")
            self.write_command(bin_dir, "pkg-config", "exit 0")
            calls = temp / "calls"

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "ANDROID_SDK_ROOT": str(sdk), "JAVA_HOME": str(jdk), "CARGO_HOME": str(cargo_home),
                    "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(calls.exists())

    def test_native_preserves_relative_explicit_cargo_home_after_changing_to_repo_root(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            caller = temp / "caller"
            caller.mkdir()
            repo = temp / "repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            script = self.copy_script_with_fake_java_home(repo, bin_dir / "java_home")
            sdk = temp / "sdk"
            self.create_native_build_files(repo, sdk)
            jdk = temp / "jdk"
            self.write_jdk(jdk)
            cargo_home = caller / "cargo home"
            self.write_rust_tools(cargo_home / "bin")
            cargo_home_record = temp / "cargo-home-record"
            verifier = repo / "infra/compose/verify-android-native.sh"
            verifier.write_text("#!/bin/sh\nprintf '%s\\n' \"$CARGO_HOME\" > \"$CARGO_HOME_RECORD\"\n")
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"; exit 99")
            self.write_command(bin_dir, "pkg-config", "exit 0")
            calls = temp / "calls"

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=caller, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "ANDROID_SDK_ROOT": str(sdk), "JAVA_HOME": str(jdk), "CARGO_HOME": "cargo home",
                    "CARGO_HOME_RECORD": str(cargo_home_record), "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(cargo_home_record.read_text().strip(), str(cargo_home.resolve()))
            self.assertFalse(calls.exists())

    def test_native_rejects_invalid_explicit_java_home_without_fallback(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            script = self.copy_script_with_fake_java_home(repo, bin_dir / "java_home")
            self.create_native_build_files(repo, temp / "sdk")
            calls = temp / "calls"
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"")

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "ANDROID_SDK_ROOT": str(temp / "sdk"), "JAVA_HOME": str(temp / "missing jdk"),
                    "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("JDK 17 java executable is missing", result.stderr)
            self.assertFalse(calls.exists())

    def test_native_reports_missing_homebrew_jdk(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = pathlib.Path(temp)
            repo = temp / "repo"
            bin_dir = temp / "bin"
            bin_dir.mkdir()
            fake_java_home = bin_dir / "java_home"
            self.write_command(bin_dir, "java_home", "exit 1")
            script = self.copy_script_with_fake_java_home(repo, fake_java_home)
            self.create_native_build_files(repo, temp / "sdk")
            self.write_command(bin_dir, "uname", "echo Darwin")
            self.write_command(bin_dir, "brew", "exit 1")

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "ANDROID_SDK_ROOT": str(temp / "sdk"),
                    "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("JDK 17 is required", result.stderr)

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
                "brew": "echo brew >> \"$CALLS\"; exit 99",
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
            self.assertNotIn("brew", calls.read_text())
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
                self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"")
                env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls)}
                if wsl:
                    env["WSL_INTEROP"] = "1"
                result = subprocess.run(["bash", str(script), "build"], cwd=repo, text=True, capture_output=True, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("docker", calls.read_text())
                self.assertNotIn("brew", calls.read_text())

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

    def test_native_rejects_false_license_before_invoking_tools(self):
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
            self.write_command(bin_dir, "brew", "echo brew >> \"$CALLS\"")

            result = subprocess.run(
                ["bash", str(script), "build"], cwd=repo, text=True, capture_output=True,
                env={
                    "HOME": str(temp), "PATH": f"{bin_dir}:/usr/bin:/bin", "CALLS": str(calls),
                    "PEPPY_ACCEPT_ANDROID_LICENSES": "false",
                },
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("PEPPY_ACCEPT_ANDROID_LICENSES must equal 1", result.stderr)
            self.assertFalse(calls.exists())
