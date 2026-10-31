import contextlib
import os
import pathlib
import subprocess
import tempfile
import unittest

try:
    from tests.dev.stub_commands import install_android_scripts, write_stub
except ModuleNotFoundError:
    from stub_commands import install_android_scripts, write_stub


BOTH_RUST_TARGETS = "aarch64-linux-android\nx86_64-linux-android\n"


class AndroidFixture:
    """Controlled native-build repository, SDK, JDK, Rust, and Gradle shims for one build invocation."""

    def __init__(self, root):
        self.root = root
        self.repo = root / "repo"
        self.bin = root / "bin"
        self.state = root / "state"
        self.calls = root / "calls"
        self.sdk = root / "sdk"
        self.jdk = root / "jdk"
        self.artifacts = root / "artifacts"
        self.bin.mkdir()
        self.state.mkdir()
        self.script = install_android_scripts(self.repo)
        self.set_rust_targets(BOTH_RUST_TARGETS)
        self.write_script(self.sdk / "cmdline-tools/latest/bin/sdkmanager", 'echo "sdk:$*" >> "$CALLS"')
        self.write_script(self.jdk / "bin/java", "echo 'openjdk version \"17.0.1\"' >&2")
        self.write_script(self.repo / "infra/compose/verify-android-native.sh", 'echo "verify:$PEPPY_ANDROID_ABI" >> "$CALLS"')
        self.write_gradle()
        self.shim("sdkmanager", 'echo "sdk:$*" >> "$CALLS"')
        self.shim("uname", "echo Darwin")
        self.shim("cargo", 'echo "cargo" >> "$CALLS"')
        self.shim(
            "rustup",
            'echo "rustup:$*" >> "$CALLS"\n'
            '[ "$1" = target ] && [ "$2" = list ] && [ "$3" = --installed ] && cat "$FIXTURE_STATE/rust-targets"',
        )
        self.shim("pkg-config", 'echo "pkg-config:$*" >> "$CALLS"')
        self.shim("brew", 'echo "brew:$*" >> "$CALLS"\nexit 99')
        self.shim("docker", 'echo "docker:$*" >> "$CALLS"\nexit 99')

    def write_script(self, path, body):
        write_stub(path, body)

    def shim(self, name, body):
        self.write_script(self.bin / name, body)

    def write_gradle(self):
        self.write_script(
            self.repo / "apps/android/gradlew",
            'echo "gradle:$*" >> "$CALLS"\n'
            "mkdir -p app/build/outputs/apk/debug app/build/outputs/apk/androidTest/debug\n"
            "touch app/build/outputs/apk/debug/app-debug.apk app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk",
        )

    def set_rust_targets(self, text):
        (self.state / "rust-targets").write_text(text)

    def environment(self, **overrides):
        environment = {
            "HOME": str(self.root),
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "CALLS": str(self.calls),
            "FIXTURE_STATE": str(self.state),
            "ANDROID_SDK_ROOT": str(self.sdk),
            "ANDROID_NDK_HOME": str(self.sdk / "ndk/27.2.12479018"),
            "JAVA_HOME": str(self.jdk),
            "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
            "PEPPY_ANDROID_ARTIFACTS": str(self.artifacts),
        }
        return environment | overrides

    def run(self, *arguments, **overrides):
        return subprocess.run(
            ["bash", str(self.script), *arguments],
            cwd=self.repo,
            text=True,
            capture_output=True,
            env=self.environment(**overrides),
        )

    def call_lines(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []


class AndroidBackendTests(unittest.TestCase):
    @contextlib.contextmanager
    def android_fixture(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            yield AndroidFixture(pathlib.Path(temporary_directory))

    def write_command(self, directory, name, body):
        write_stub(directory / name, body)

    def copy_android_script(self, repo):
        return install_android_scripts(repo)

    def copy_script_with_fake_java_home(self, repo, fake_java_home):
        script = self.copy_android_script(repo)
        source = script.read_text()
        script.write_text(source.replace("/usr/libexec/java_home", str(fake_java_home)))
        script.chmod(0o755)
        return script

    def create_native_build_files(self, repo, sdk):
        sdkmanager = sdk / "cmdline-tools/latest/bin/sdkmanager"
        write_stub(sdkmanager, "exit 0\n")
        gradlew = repo / "apps/android/gradlew"
        write_stub(
            gradlew,
            "mkdir -p app/build/outputs/apk/debug\n"
            "touch app/build/outputs/apk/debug/app-debug.apk\n",
        )
        verifier = repo / "infra/compose/verify-android-native.sh"
        write_stub(verifier, "exit 0\n")

    def write_jdk(self, java_home):
        java = java_home / "bin/java"
        write_stub(java, "echo 'openjdk version \"17.0.1\"' >&2\n")

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
            write_stub(verifier, "printf '%s\\n' \"$CARGO_HOME\" > \"$CARGO_HOME_RECORD\"\n")
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
            script = self.copy_android_script(repo)
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
            script = self.copy_android_script(repo)
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
                script = self.copy_android_script(repo)
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
            script = self.copy_android_script(repo)
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
            script = self.copy_android_script(repo)
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
            script = self.copy_android_script(repo)
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

    def test_invalid_build_selectors_fail_before_sdk_cargo_or_docker_effects(self):
        for host in ("Darwin", "Linux"):
            for selector, value, message in (
                ("PEPPY_ANDROID_ABI", "armeabi-v7a", "PEPPY_ANDROID_ABI must be all, arm64-v8a, or x86_64"),
                ("PEPPY_ANDROID_ABI", "x86", "PEPPY_ANDROID_ABI must be all, arm64-v8a, or x86_64"),
                ("PEPPY_ANDROID_FORCE_NATIVE_CLEAN", "yes", "PEPPY_ANDROID_FORCE_NATIVE_CLEAN must be 0 or 1"),
                ("PEPPY_ANDROID_FORCE_NATIVE_CLEAN", "2", "PEPPY_ANDROID_FORCE_NATIVE_CLEAN must be 0 or 1"),
                ("PEPPY_ANDROID_BUILD_BACKEND", "remote", "PEPPY_ANDROID_BUILD_BACKEND must be auto, native, or docker"),
            ):
                with self.subTest(host=host, selector=selector, value=value), self.android_fixture() as fixture:
                    fixture.shim("uname", f"echo {host}")

                    result = fixture.run("build", **{selector: value})

                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertEqual(fixture.call_lines(), [])

    def test_gradle_daemon_policy_follows_test_ci_and_container_context(self):
        for command, overrides, expects_no_daemon in (
            ("build", {}, False),
            ("build", {"CI": "false"}, False),
            ("test", {}, True),
            ("build", {"CI": "1"}, True),
            ("build", {"CI": "true"}, True),
            ("build", {"PEPPY_ANDROID_CONTAINER": "1"}, True),
        ):
            with self.subTest(command=command, overrides=overrides), self.android_fixture() as fixture:
                result = fixture.run(command, **overrides)

                self.assertEqual(result.returncode, 0, result.stderr)
                gradle = [call for call in fixture.call_lines() if call.startswith("gradle:")]
                self.assertEqual(len(gradle), 1)
                self.assertEqual("--no-daemon" in gradle[0], expects_no_daemon, gradle[0])

    def test_native_rust_targets_and_gradle_abi_follow_selected_abi(self):
        arm_only = "aarch64-linux-android\n"
        x86_only = "x86_64-linux-android\n"
        for selector, installed, missing, expected_gradle in (
            ("all", BOTH_RUST_TARGETS, None, "gradle::app:assembleDebug"),
            ("all", arm_only, "Rust target x86_64-linux-android is missing", None),
            ("all", x86_only, "Rust target aarch64-linux-android is missing", None),
            ("arm64-v8a", arm_only, None, "gradle:-PpeppyAndroidAbi=arm64-v8a :app:assembleDebug"),
            ("arm64-v8a", x86_only, "Rust target aarch64-linux-android is missing", None),
            ("x86_64", arm_only, "Rust target x86_64-linux-android is missing", None),
            ("x86_64", x86_only, None, "gradle:-PpeppyAndroidAbi=x86_64 :app:assembleDebug"),
        ):
            with self.subTest(selector=selector, installed=installed), self.android_fixture() as fixture:
                fixture.set_rust_targets(installed)

                result = fixture.run("build", PEPPY_ANDROID_ABI=selector)

                calls = fixture.call_lines()
                if missing:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(missing, result.stderr)
                    self.assertFalse(any(call.startswith(("verify:", "gradle:")) for call in calls))
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(f"verify:{selector}", calls)
                    self.assertIn(expected_gradle, calls)
                    if selector == "all":
                        self.assertFalse(any("peppyAndroidAbi" in call for call in calls))
