import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]


class AndroidNativeVerifierTests(unittest.TestCase):
    def test_prefers_current_host_prebuilt_tools_over_other_ndk_hosts(self):
        cases = (
            ("Darwin", ("linux-x86_64", "darwin-x86_64"), "darwin-x86_64"),
            ("Linux", ("darwin-x86_64", "linux-x86_64"), "linux-x86_64"),
        )
        for host_name, prebuilt_hosts, expected_host in cases:
            with self.subTest(host=host_name), tempfile.TemporaryDirectory() as temporary_directory:
                temporary_directory = pathlib.Path(temporary_directory)
                repository = temporary_directory / "repository"
                script = repository / "infra/compose/verify-android-native.sh"
                script.parent.mkdir(parents=True)
                shutil.copy(ROOT / "infra/compose/verify-android-native.sh", script)

                target_directory = repository / "target"
                cargo_log = temporary_directory / "cargo-log.jsonl"
                tools_directory = temporary_directory / "tools"
                tools_directory.mkdir()
                self.write_fake_cargo(tools_directory / "cargo")
                self.write_fake_uname(tools_directory / "uname", host_name)
                ndk_home, _ = self.create_fake_ndk(temporary_directory, prebuilt_hosts)

                environment = os.environ.copy()
                environment.update({
                    "ANDROID_NDK_HOME": str(ndk_home),
                    "CARGO_TARGET_DIR": str(target_directory),
                    "CARGO_LOG": str(cargo_log),
                    "PATH": f"{tools_directory}:{environment['PATH']}",
                })
                result = subprocess.run(
                    ["bash", str(script)], cwd=repository, env=environment,
                    text=True, capture_output=True,
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                cargo_calls = [json.loads(line) for line in cargo_log.read_text().splitlines()]
                target_build = next(
                    call for call in cargo_calls
                    if call["arguments"][:2] == ["build", "--locked"] and "--target" in call["arguments"]
                )
                self.assertEqual(
                    target_build["environment"]["CC"],
                    str(
                        ndk_home
                        / f"toolchains/llvm/prebuilt/{expected_host}/bin"
                        / "aarch64-linux-android26-clang"
                    ),
                )

    def test_host_bindgen_uses_system_libsodium_without_affecting_ndk_builds(self):
        for profile in ("debug", "release"):
            with self.subTest(profile=profile), tempfile.TemporaryDirectory() as temporary_directory:
                temporary_directory = pathlib.Path(temporary_directory)
                repository = temporary_directory / "repository"
                script = repository / "infra/compose/verify-android-native.sh"
                script.parent.mkdir(parents=True)
                shutil.copy(ROOT / "infra/compose/verify-android-native.sh", script)

                target_directory = repository / "target"
                cargo_log = temporary_directory / "cargo-log.jsonl"
                tools_directory = temporary_directory / "tools"
                tools_directory.mkdir()
                self.write_fake_cargo(tools_directory / "cargo")
                ndk_home, ndk_bin = self.create_fake_ndk(temporary_directory)

                environment = os.environ.copy()
                environment.update({
                    "ANDROID_NDK_HOME": str(ndk_home),
                    "CARGO_TARGET_DIR": str(target_directory),
                    "CARGO_LOG": str(cargo_log),
                    "PATH": f"{tools_directory}:{environment['PATH']}",
                    "PEPPY_ANDROID_NATIVE_PROFILE": profile,
                    "SODIUM_USE_PKG_CONFIG": "inherited-host-setting",
                })
                result = subprocess.run(
                    ["bash", str(script)], cwd=repository, env=environment,
                    text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)

                cargo_calls = [json.loads(line) for line in cargo_log.read_text().splitlines()]
                host_calls = [call for call in cargo_calls if "--target" not in call["arguments"]]
                self.assertEqual([call["arguments"][0] for call in host_calls], ["build", "run"])
                self.assertEqual(
                    [call["sodium_use_pkg_config"] for call in host_calls], ["1", "1"],
                )

                target_builds = [
                    call for call in cargo_calls
                    if call["arguments"][0] == "build" and "--target" in call["arguments"]
                ]
                self.assertEqual(len(target_builds), 2)
                for call in target_builds:
                    target = call["arguments"][call["arguments"].index("--target") + 1]
                    compiler = ndk_bin / f"{target}26-clang"
                    self.assertIsNone(call["sodium_use_pkg_config"])
                    self.assertEqual(call["environment"]["CC"], str(compiler))
                    self.assertEqual(call["environment"][f"CC_{target}"], str(compiler))
                for abi in ("arm64-v8a", "x86_64"):
                    library = repository / f"apps/android/app/src/main/jniLibs/{abi}/libpeppy_mobile_bindings.so"
                    self.assertGreater(library.stat().st_size, 0)

    def test_resolves_relative_paths_before_changing_to_script_root(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = pathlib.Path(temporary_directory)
            repository = temporary_directory / "repository"
            script = repository / "infra/compose/verify-android-native.sh"
            script.parent.mkdir(parents=True)
            shutil.copy(ROOT / "infra/compose/verify-android-native.sh", script)

            caller_directory = temporary_directory / "caller directory"
            caller_directory.mkdir()
            cargo_log = temporary_directory / "cargo-log.jsonl"
            tools_directory = temporary_directory / "tools"
            tools_directory.mkdir()
            self.write_fake_cargo(tools_directory / "cargo")
            self.create_fake_ndk(caller_directory)

            environment = os.environ.copy()
            environment.update({
                "ANDROID_NDK_HOME": "ndk",
                "CARGO_TARGET_DIR": "target directory",
                "CARGO_LOG": str(cargo_log),
                "PATH": f"{tools_directory}:{environment['PATH']}",
            })
            result = subprocess.run(
                ["bash", str(script)], cwd=caller_directory, env=environment,
                text=True, capture_output=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(
                (
                    caller_directory
                    / "target directory/debug/libpeppy_mobile_bindings.dylib"
                ).is_file(),
            )
            self.assertTrue(
                (
                    repository
                    / "apps/android/app/src/main/jniLibs/arm64-v8a"
                    / "libpeppy_mobile_bindings.so"
                ).is_file(),
            )

    def create_fake_ndk(self, temporary_directory, prebuilt_hosts=("test-host",)):
        ndk_home = temporary_directory / "ndk"
        ndk_bin = None
        for host in prebuilt_hosts:
            host_bin = ndk_home / f"toolchains/llvm/prebuilt/{host}/bin"
            host_bin.mkdir(parents=True)
            for tool in (
                "llvm-ar", "llvm-ranlib", "llvm-readelf",
                "aarch64-linux-android26-clang", "x86_64-linux-android26-clang",
            ):
                path = host_bin / tool
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            if host.startswith("darwin") or ndk_bin is None:
                ndk_bin = host_bin
        return ndk_home, ndk_bin

    def write_fake_uname(self, path, host_name):
        path.write_text(f"#!/bin/sh\nprintf '%s\\n' '{host_name}'\n")
        path.chmod(0o755)

    def write_fake_cargo(self, path):
        path.write_text(
            f"#!{sys.executable}\n"
            "import json\n"
            "import os\n"
            "import pathlib\n"
            "import sys\n"
            "arguments = sys.argv[1:]\n"
            "target = arguments[arguments.index('--target') + 1] if '--target' in arguments else None\n"
            "environment = {'CC': os.getenv('CC')}\n"
            "if target:\n"
            "    environment[f'CC_{target}'] = os.getenv(f'CC_{target}')\n"
            "with open(os.environ['CARGO_LOG'], 'a') as log:\n"
            "    json.dump({'arguments': arguments, 'sodium_use_pkg_config': os.getenv('SODIUM_USE_PKG_CONFIG'), 'environment': environment}, log)\n"
            "    log.write('\\n')\n"
            "if arguments[0] == 'build':\n"
            "    target_directory = pathlib.Path(os.environ['CARGO_TARGET_DIR'])\n"
            "    profile = 'release' if '--release' in arguments else 'debug'\n"
            "    if '--target' in arguments:\n"
            "        target = arguments[arguments.index('--target') + 1]\n"
            "        library = target_directory / target / profile / 'libpeppy_mobile_bindings.so'\n"
            "    else:\n"
            "        library = target_directory / 'debug' / 'libpeppy_mobile_bindings.dylib'\n"
            "    library.parent.mkdir(parents=True, exist_ok=True)\n"
            "    library.write_bytes(b'library')\n"
        )
        path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
