import contextlib
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNING_X86 = "List of devices attached\nemulator-5554\tdevice\n"
NO_DEVICES = "List of devices attached\n"
ADB_SHIM = """echo "adb:$*" >> "$CALLS"
if [ "$1" = devices ]; then cat "$DEVICE_STATE/devices"; exit 0; fi
if [ "$3" = shell ] && [ "$4" = getprop ]; then cat "$DEVICE_STATE/abi"; fi
if [ "$3" = shell ] && [ "$4" = am ]; then echo "Status: ok"; fi
if [ "$3" = install ] && [ -f "$DEVICE_STATE/abi-after-install" ]; then mv "$DEVICE_STATE/abi-after-install" "$DEVICE_STATE/abi"; fi
"""
TLS_STUB = """#!/usr/bin/env python3
import os
import sys

with open(os.environ["CALLS"], "a") as calls:
    calls.write(f"tls:{sys.argv[1]}\\n")
if sys.argv[1] == "enabled":
    print("true")
elif sys.argv[1] == "ca-path":
    print(os.environ["STUB_CA"])
elif sys.argv[1] != "prepare":
    sys.exit(64)
"""


def install_android_scripts(repo):
    helper_directory = repo / "infra/dev"
    helper_directory.mkdir(parents=True)
    for name in ("android.sh", "dev_port.py", "dev_tls.py"):
        shutil.copy(ROOT / "infra/dev" / name, helper_directory / name)
    return helper_directory / "android.sh"


class AndroidFixture:
    """Controlled repository, tool shims, and emulator state for one run-fast invocation."""

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
        (self.repo / ".env").write_text("DEBUG=false\n")
        self.set_devices(RUNNING_X86)
        self.set_abi("x86_64")
        self.shim("uname", "echo Darwin")
        self.shim("cargo", "exit 0")
        self.shim("rustup", '[ "$1" = target ] && [ "$2" = list ] && printf "aarch64-linux-android\\nx86_64-linux-android\\n"')
        self.shim("pkg-config", "exit 0")
        self.shim("curl", 'echo "curl:$*" >> "$CALLS"')
        self.write_script(self.sdk / "cmdline-tools/latest/bin/sdkmanager", 'echo "sdk:$*" >> "$CALLS"')
        self.write_script(self.sdk / "platform-tools/adb", ADB_SHIM)
        self.write_script(self.jdk / "bin/java", "echo 'openjdk version \"17.0.1\"' >&2")
        self.write_script(self.repo / "infra/compose/verify-android-native.sh", 'echo "verify:$PEPPY_ANDROID_ABI" >> "$CALLS"')
        self.write_gradle()

    def write_script(self, path, body):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"#!/bin/sh\n{body}\n")
        path.chmod(0o755)

    def shim(self, name, body):
        self.write_script(self.bin / name, body)

    def write_gradle(self, before=""):
        self.write_script(
            self.repo / "apps/android/gradlew",
            f'echo "gradle:$*" >> "$CALLS"\n{before}\n'
            "mkdir -p app/build/outputs/apk/debug\n"
            "touch app/build/outputs/apk/debug/app-debug.apk",
        )

    def set_devices(self, text):
        (self.state / "devices").write_text(text)

    def set_abi(self, abi):
        (self.state / "abi").write_text(f"{abi}\n")

    def environment(self, **overrides):
        environment = {
            "HOME": str(self.root),
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "CALLS": str(self.calls),
            "DEVICE_STATE": str(self.state),
            "ANDROID_SDK_ROOT": str(self.sdk),
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


class AndroidFastTests(unittest.TestCase):
    @contextlib.contextmanager
    def android_fixture(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            yield AndroidFixture(pathlib.Path(temporary_directory))

    def write_command(self, directory, name, body):
        command = directory / name
        command.write_text(f"#!/bin/sh\n{body}\n")
        command.chmod(0o755)

    def copy_android_script(self, repo):
        return install_android_scripts(repo)

    def assert_no_build_or_deploy(self, fixture):
        calls = fixture.call_lines()
        self.assertFalse(any(call.startswith(("verify:", "gradle:")) for call in calls))
        self.assertFalse(any(" install -r " in call or " am start " in call for call in calls))

    def test_run_fast_requires_license_before_environment_or_device_tools(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = pathlib.Path(temporary_directory)
            repo = temporary_directory / "repo"
            script = self.copy_android_script(repo)
            calls = temporary_directory / "calls"
            bin_directory = temporary_directory / "bin"
            bin_directory.mkdir()
            self.write_command(bin_directory, "adb", "echo adb >> \"$CALLS\"")

            result = subprocess.run(
                ["bash", str(script), "run-fast"],
                cwd=repo,
                text=True,
                capture_output=True,
                env={"PATH": f"{bin_directory}:{os.environ['PATH']}", "CALLS": str(calls)},
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("PEPPY_ACCEPT_ANDROID_LICENSES", result.stderr)
            self.assertFalse(calls.exists())

    def test_run_fast_requires_environment_before_device_tools(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = pathlib.Path(temporary_directory)
            repo = temporary_directory / "repo"
            script = self.copy_android_script(repo)
            calls = temporary_directory / "calls"
            bin_directory = temporary_directory / "bin"
            bin_directory.mkdir()
            self.write_command(bin_directory, "adb", "echo adb >> \"$CALLS\"")

            result = subprocess.run(
                ["bash", str(script), "run-fast"],
                cwd=repo,
                text=True,
                capture_output=True,
                env={
                    "PATH": f"{bin_directory}:{os.environ['PATH']}",
                    "CALLS": str(calls),
                    "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                },
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing .env", result.stderr)
            self.assertFalse(calls.exists())

    def test_invalid_fast_selectors_fail_before_device_tools(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = pathlib.Path(temporary_directory)
            repo = temporary_directory / "repo"
            script = self.copy_android_script(repo)
            (repo / ".env").touch()
            calls = temporary_directory / "calls"
            bin_directory = temporary_directory / "bin"
            bin_directory.mkdir()
            self.write_command(bin_directory, "adb", "echo adb >> \"$CALLS\"")
            base_environment = {
                "PATH": f"{bin_directory}:{os.environ['PATH']}",
                "CALLS": str(calls),
                "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
            }

            for selector, value in (
                ("PEPPY_ANDROID_ABI", "armeabi-v7a"),
                ("PEPPY_ANDROID_FORCE_NATIVE_CLEAN", "yes"),
                ("PEPPY_ANDROID_BUILD_BACKEND", "remote"),
            ):
                with self.subTest(selector=selector):
                    result = subprocess.run(
                        ["bash", str(script), "run-fast"], cwd=repo, text=True, capture_output=True,
                        env=base_environment | {selector: value},
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(calls.exists())

    def test_run_fast_uses_running_emulator_and_isolated_artifact(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = pathlib.Path(temporary_directory)
            repo = temporary_directory / "repo"
            script = self.copy_android_script(repo)
            (repo / ".env").write_text("DEBUG=false\n")
            calls = temporary_directory / "calls"
            bin_directory = temporary_directory / "bin"
            bin_directory.mkdir()
            self.write_command(bin_directory, "uname", "echo Darwin")
            self.write_command(
                bin_directory,
                "adb",
                "echo \"adb:$*\" >> \"$CALLS\"\n"
                "if [ \"$1\" = devices ]; then printf 'List of devices attached\\nemulator-5554\\tdevice\\n'; fi\n"
                "if [ \"$3\" = shell ] && [ \"$4\" = getprop ]; then echo x86_64; fi\n"
                "if [ \"$3\" = shell ] && [ \"$4\" = am ]; then echo 'Status: ok'; fi",
            )
            self.write_command(bin_directory, "curl", "echo \"curl:$*\" >> \"$CALLS\"")
            self.write_command(bin_directory, "cargo", "exit 0")
            self.write_command(
                bin_directory,
                "rustup",
                "[ \"$1\" = target ] && [ \"$2\" = list ] && echo x86_64-linux-android",
            )
            self.write_command(bin_directory, "pkg-config", "exit 0")
            sdk = temporary_directory / "sdk"
            sdkmanager = sdk / "cmdline-tools/latest/bin/sdkmanager"
            sdkmanager.parent.mkdir(parents=True)
            sdkmanager.write_text("#!/bin/sh\nexit 0\n")
            sdkmanager.chmod(0o755)
            adb = sdk / "platform-tools/adb"
            adb.parent.mkdir(parents=True)
            shutil.copy(bin_directory / "adb", adb)
            jdk = temporary_directory / "jdk"
            java = jdk / "bin/java"
            java.parent.mkdir(parents=True)
            java.write_text("#!/bin/sh\necho 'openjdk version \"17.0.1\"' >&2\n")
            java.chmod(0o755)
            verifier = repo / "infra/compose/verify-android-native.sh"
            verifier.parent.mkdir(parents=True)
            verifier.write_text(
                "#!/bin/sh\n"
                "echo \"verify:$PEPPY_ANDROID_ABI\" >> \"$CALLS\"\n"
                "[ \"${FAIL_VERIFIER:-}\" = 1 ] && exit 23\n"
                "exit 0\n"
            )
            verifier.chmod(0o755)
            gradlew = repo / "apps/android/gradlew"
            gradlew.parent.mkdir(parents=True)
            gradlew.write_text(
                "#!/bin/sh\n"
                "echo \"gradle:$*\" >> \"$CALLS\"\n"
                "[ \"${FAIL_GRADLE:-}\" = 1 ] && exit 24\n"
                "mkdir -p app/build/outputs/apk/debug\n"
                "touch app/build/outputs/apk/debug/app-debug.apk\n"
                "exit 0\n"
            )
            gradlew.chmod(0o755)
            artifacts = temporary_directory / "artifacts"
            fast_artifact = artifacts / "fast/x86_64/app-debug.apk"
            fast_artifact.parent.mkdir(parents=True)
            fast_artifact.write_text("stale\n")
            normal_artifact = artifacts / "app-debug.apk"
            normal_artifact.write_text("normal\n")
            environment = {
                "HOME": str(temporary_directory),
                "PATH": f"{bin_directory}:{os.environ['PATH']}",
                "CALLS": str(calls),
                "ANDROID_SDK_ROOT": str(sdk),
                "JAVA_HOME": str(jdk),
                "PEPPY_ACCEPT_ANDROID_LICENSES": "1",
                "PEPPY_ANDROID_ARTIFACTS": str(artifacts),
            }

            for failure_variable in ("FAIL_VERIFIER", "FAIL_GRADLE"):
                failed = subprocess.run(
                    ["bash", str(script), "run-fast"], cwd=repo, text=True, capture_output=True,
                    env=environment | {failure_variable: "1"},
                )
                self.assertNotEqual(failed.returncode, 0)
                self.assertFalse(fast_artifact.exists())
                self.assertEqual(normal_artifact.read_text(), "normal\n")
                self.assertNotIn(" install -r ", calls.read_text())
                self.assertNotIn(" am start ", calls.read_text())
                fast_artifact.write_text("stale\n")
                calls.unlink()

            result = subprocess.run(
                ["bash", str(script), "run-fast"],
                cwd=repo,
                text=True,
                capture_output=True,
                env=environment,
            )

            self.assertEqual(result.returncode, 0, f"{result.stderr}\n{calls.read_text() if calls.exists() else ''}")
            self.assertTrue((artifacts / "fast/x86_64/app-debug.apk").is_file())
            self.assertEqual(normal_artifact.read_text(), "normal\n")
            self.assertIn("verify:x86_64", calls.read_text())
            self.assertIn("gradle:-PpeppyAndroidAbi=x86_64 :app:assembleDebug", calls.read_text())
            self.assertNotIn("--no-daemon", calls.read_text())
            call_lines = calls.read_text().splitlines()
            self.assertLess(next(index for index, call in enumerate(call_lines) if call.startswith("curl:")), call_lines.index("verify:x86_64"))
            self.assertTrue(all("-s emulator-5554" in call for call in call_lines if call.startswith("adb:") and "devices" not in call))

    def test_run_fast_docker_false_success_never_deploys_stale_fast_apk(self):
        with self.android_fixture() as fixture:
            fixture.shim("uname", "echo Linux")
            fixture.shim("docker", 'echo "docker:$*" >> "$CALLS"\nexit 0')
            fast_artifact = fixture.artifacts / "fast/x86_64/app-debug.apk"
            fast_artifact.parent.mkdir(parents=True)
            fast_artifact.write_text("stale\n")
            normal_artifact = fixture.artifacts / "app-debug.apk"
            normal_artifact.write_text("normal\n")

            result = fixture.run("run-fast", PEPPY_ANDROID_BUILD_BACKEND="docker")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("APK missing", result.stderr)
            self.assertFalse(fast_artifact.exists())
            self.assertEqual(normal_artifact.read_text(), "normal\n")
            self.assert_no_build_or_deploy(fixture)
            self.assertIn(
                f"-v {fixture.artifacts}/fast/x86_64:/artifacts/android/fast/x86_64",
                fixture.calls.read_text(),
            )

    def test_run_fast_docker_success_deploys_only_its_isolated_artifact(self):
        with self.android_fixture() as fixture:
            fixture.shim("uname", "echo Linux")
            fixture.shim(
                "docker",
                'echo "docker:$*" >> "$CALLS"\n'
                'case "$*" in *"android run build"*) mkdir -p "$(dirname "$DOCKER_APK")"; printf fast > "$DOCKER_APK" ;; esac\n'
                "exit 0",
            )
            fast_artifact = fixture.artifacts / "fast/x86_64/app-debug.apk"
            normal_artifact = fixture.artifacts / "app-debug.apk"
            normal_artifact.parent.mkdir(parents=True)
            normal_artifact.write_text("normal\n")

            result = fixture.run("run-fast", PEPPY_ANDROID_BUILD_BACKEND="docker", DOCKER_APK=str(fast_artifact))

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(fast_artifact.read_text(), "fast")
            self.assertEqual(normal_artifact.read_text(), "normal\n")
            calls = fixture.call_lines()
            self.assertEqual(sum(f"install -r {fast_artifact}" in call for call in calls), 1)
            self.assertTrue(any(" am start " in call for call in calls))
            self.assertFalse(any(call.startswith(("verify:", "gradle:")) for call in calls))

    def test_backend_readiness_failure_blocks_verifier_gradle_install_and_launch(self):
        with self.android_fixture() as fixture:
            fixture.shim("curl", 'echo "curl:$*" >> "$CALLS"\nexit 7')

            result = fixture.run("run-fast")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("local server is not reachable: http://127.0.0.1:7000/healthz", result.stderr)
            calls = fixture.call_lines()
            self.assertTrue(any(call.startswith("curl:") and "--fail" in call for call in calls))
            self.assert_no_build_or_deploy(fixture)

    def test_run_fast_https_readiness_uses_configured_certificate_before_build(self):
        for label, dotenv, server, expected_url in (
            ("configured DEBUG port", "DEBUG=true\nAPI_HOST_PORT=8443\n", None, "https://127.0.0.1:8443/healthz"),
            ("explicit debug server", "DEBUG=true\n", "https://localhost:9443", "https://localhost:9443/healthz"),
        ):
            with self.subTest(configuration=label), self.android_fixture() as fixture:
                (fixture.repo / ".env").write_text(dotenv)
                (fixture.repo / "infra/dev/dev_tls.py").write_text(TLS_STUB)
                certificate = fixture.root / "scratch/dev-ca.pem"
                certificate.parent.mkdir()
                certificate.write_text("public ca\n")
                overrides = {"STUB_CA": str(certificate)}
                if server:
                    overrides["PEPPY_DEBUG_SERVER"] = server

                result = fixture.run("run-fast", **overrides)

                self.assertEqual(result.returncode, 0, result.stderr)
                calls = fixture.call_lines()
                curl_line = next(call for call in calls if call.startswith("curl:"))
                self.assertIn(f"--cacert {certificate}", curl_line)
                self.assertIn(expected_url, curl_line)
                self.assertLess(calls.index("tls:prepare"), calls.index("verify:x86_64"))
                self.assertLess(calls.index(curl_line), calls.index("verify:x86_64"))

    def test_run_fast_https_readiness_failure_blocks_build_and_launch(self):
        with self.android_fixture() as fixture:
            (fixture.repo / ".env").write_text("DEBUG=true\n")
            (fixture.repo / "infra/dev/dev_tls.py").write_text(TLS_STUB)
            certificate = fixture.root / "scratch/dev-ca.pem"
            certificate.parent.mkdir()
            certificate.write_text("public ca\n")
            fixture.shim("curl", 'echo "curl:$*" >> "$CALLS"\nexit 7')

            result = fixture.run("run-fast", STUB_CA=str(certificate))

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("local server is not reachable: https://127.0.0.1:7000/healthz", result.stderr)
            self.assert_no_build_or_deploy(fixture)

    def test_run_fast_rejects_abi_selector_that_mismatches_running_emulator(self):
        for selector, device_abi in (("arm64-v8a", "x86_64"), ("x86_64", "arm64-v8a")):
            with self.subTest(selector=selector, device_abi=device_abi), self.android_fixture() as fixture:
                fixture.set_abi(device_abi)

                result = fixture.run("run-fast", PEPPY_ANDROID_ABI=selector)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn(f"PEPPY_ANDROID_ABI={selector} does not match running emulator ABI {device_abi}", result.stderr)
                self.assertFalse(any(call.startswith("curl:") for call in fixture.call_lines()))
                self.assert_no_build_or_deploy(fixture)

    def test_run_fast_requires_one_selectable_emulator_before_build(self):
        two_emulators = "List of devices attached\nemulator-5554\tdevice\nemulator-5556\tdevice\n"
        physical = "List of devices attached\nR58M123\tdevice\n"
        for label, devices, overrides, message in (
            ("no device", NO_DEVICES, {}, "select exactly one running emulator"),
            ("offline emulator", "List of devices attached\nemulator-5554\toffline\n", {}, "select exactly one running emulator"),
            ("multiple emulators", two_emulators, {}, "select exactly one running emulator"),
            ("physical device without selector", physical, {}, "select exactly one running emulator"),
            ("explicit physical serial", physical, {"PEPPY_ANDROID_SERIAL": "R58M123"}, "refusing physical device 'R58M123'"),
            ("explicit missing emulator", RUNNING_X86, {"PEPPY_ANDROID_SERIAL": "emulator-5556"}, "selected emulator is unavailable: emulator-5556"),
        ):
            with self.subTest(case=label), self.android_fixture() as fixture:
                fixture.set_devices(devices)

                result = fixture.run("run-fast", **overrides)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)
                self.assertFalse(any(call.startswith("curl:") for call in fixture.call_lines()))
                self.assert_no_build_or_deploy(fixture)

    def test_device_loss_during_build_blocks_install_and_launch(self):
        with self.android_fixture() as fixture:
            fixture.write_gradle('printf "List of devices attached\\n" > "$DEVICE_STATE/devices"')
            normal_artifact = fixture.artifacts / "app-debug.apk"
            normal_artifact.parent.mkdir(parents=True)
            normal_artifact.write_text("normal\n")

            result = fixture.run("run-fast")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("selected emulator is unavailable: emulator-5554", result.stderr)
            self.assertTrue(any(call.startswith("gradle:") for call in fixture.call_lines()))
            self.assertFalse(any(" install -r " in call or " am start " in call for call in fixture.call_lines()))
            self.assertEqual(normal_artifact.read_text(), "normal\n")

    def test_pinned_serial_abi_change_before_deploy_blocks_install_and_launch(self):
        with self.android_fixture() as fixture:
            fixture.write_gradle('echo arm64-v8a > "$DEVICE_STATE/abi"')

            result = fixture.run("run-fast")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("selected emulator ABI changed from x86_64 to arm64-v8a", result.stderr)
            self.assertFalse(any(" install -r " in call or " am start " in call for call in fixture.call_lines()))

    def test_abi_change_after_install_installs_once_but_never_launches(self):
        with self.android_fixture() as fixture:
            (fixture.state / "abi-after-install").write_text("arm64-v8a\n")

            result = fixture.run("run-fast")

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("selected emulator ABI changed from x86_64 to arm64-v8a", result.stderr)
            calls = fixture.call_lines()
            self.assertEqual(sum(" install -r " in call for call in calls), 1)
            self.assertFalse(any(" am start " in call for call in calls))


if __name__ == "__main__":
    unittest.main()
