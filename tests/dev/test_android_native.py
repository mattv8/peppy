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
    def test_keeps_healthy_archives_for_successive_full_builds(self):
        with self.native_verifier_fixture() as fixture:
            for target in ("aarch64-linux-android", "x86_64-linux-android"):
                self.write_archive(fixture["target_directory"], target, "debug", b"healthy")

            first_result = self.run_verifier(fixture)
            second_result = self.run_verifier(fixture)

            self.assertEqual(first_result.returncode, 0, first_result.stderr)
            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertFalse(any(call["arguments"][0] == "clean" for call in cargo_calls))

    def test_recovers_incomplete_release_cache_for_selected_abi(self):
        with self.native_verifier_fixture(
            PEPPY_ANDROID_ABI="arm64-v8a",
            PEPPY_ANDROID_NATIVE_PROFILE="release",
        ) as fixture:
            self.write_incomplete_package_output(
                fixture["target_directory"], "aarch64-linux-android", "release"
            )

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            clean_calls = [call for call in cargo_calls if call["arguments"][0] == "clean"]
            self.assertEqual(len(clean_calls), 1)
            self.assertEqual(
                clean_calls[0]["arguments"],
                ["clean", "--target", "aarch64-linux-android", "-p", "libsodium-sys-stable", "--release"],
            )
            self.assertIn(
                "Android native cache recovery: target=aarch64-linux-android profile=release reason=missing-archive",
                result.stderr,
            )
            target_builds = self.target_builds(cargo_calls)
            self.assertEqual(len(target_builds), 1)
            self.assertIn("aarch64-linux-android", target_builds[0]["arguments"])

    def test_treats_zero_member_archive_as_invalid(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="arm64-v8a") as fixture:
            self.write_archive(
                fixture["target_directory"], "aarch64-linux-android", "debug", b"!<arch>\n"
            )

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertEqual(
                [call["arguments"][0] for call in cargo_calls].count("clean"), 1
            )

    def test_recovers_when_archive_cannot_be_listed(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="arm64-v8a") as fixture:
            self.write_archive(
                fixture["target_directory"], "aarch64-linux-android", "debug", b"invalid"
            )

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertEqual(
                [call["arguments"][0] for call in cargo_calls].count("clean"), 1
            )

    def test_recovers_when_archive_is_unreadable(self):
        if os.geteuid() == 0:
            self.skipTest("root can read files without read permission")
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="arm64-v8a") as fixture:
            archive = self.write_archive(
                fixture["target_directory"], "aarch64-linux-android", "debug", b"healthy"
            )
            archive.chmod(0)

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertEqual(
                [call["arguments"][0] for call in cargo_calls].count("clean"), 1
            )

    def test_cold_cache_does_not_clean(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="arm64-v8a") as fixture:
            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertFalse(any(call["arguments"][0] == "clean" for call in cargo_calls))

    def test_existing_build_directory_without_package_output_does_not_clean(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="arm64-v8a") as fixture:
            (fixture["target_directory"] / "aarch64-linux-android/debug/build").mkdir(parents=True)

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(
                any(call["arguments"][0] == "clean" for call in self.read_cargo_calls(fixture))
            )

    def test_force_clean_overrides_healthy_x86_cache(self):
        with self.native_verifier_fixture(
            PEPPY_ANDROID_ABI="x86_64", PEPPY_ANDROID_FORCE_NATIVE_CLEAN="1"
        ) as fixture:
            self.write_archive(fixture["target_directory"], "x86_64-linux-android", "debug", b"healthy")
            result = self.run_verifier(fixture)
            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertEqual(
                [call["arguments"] for call in cargo_calls if call["arguments"][0] == "clean"],
                [["clean", "--target", "x86_64-linux-android", "-p", "libsodium-sys-stable"]],
            )
            self.assertIn(
                "Android native cache recovery: target=x86_64-linux-android profile=debug reason=forced",
                result.stderr,
            )

    def test_empty_force_selector_uses_normal_cache_validation(self):
        with self.native_verifier_fixture(
            PEPPY_ANDROID_ABI="x86_64", PEPPY_ANDROID_FORCE_NATIVE_CLEAN=""
        ) as fixture:
            self.write_archive(fixture["target_directory"], "x86_64-linux-android", "debug", b"healthy")

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(
                any(call["arguments"][0] == "clean" for call in self.read_cargo_calls(fixture))
            )

    def test_invalid_options_fail_before_cargo(self):
        for name, value, expected_error in (
            ("PEPPY_ANDROID_ABI", "bad-abi", "PEPPY_ANDROID_ABI"),
            ("PEPPY_ANDROID_FORCE_NATIVE_CLEAN", "yes", "PEPPY_ANDROID_FORCE_NATIVE_CLEAN"),
        ):
            with self.subTest(option=name), self.native_verifier_fixture(**{name: value}) as fixture:
                result = self.run_verifier(fixture)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertFalse(fixture["cargo_log"].exists())

    def test_selected_profile_ignores_invalid_other_profile_cache(self):
        for profile, other_profile in (("debug", "release"), ("release", "debug")):
            with self.subTest(profile=profile), self.native_verifier_fixture(
                PEPPY_ANDROID_ABI="arm64-v8a", PEPPY_ANDROID_NATIVE_PROFILE=profile
            ) as fixture:
                self.write_archive(
                    fixture["target_directory"], "aarch64-linux-android", profile, b"healthy"
                )
                self.write_archive(
                    fixture["target_directory"], "aarch64-linux-android", other_profile, b"invalid"
                )

                result = self.run_verifier(fixture)

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(
                    any(call["arguments"][0] == "clean" for call in self.read_cargo_calls(fixture))
                )

    def test_default_both_abis_recovers_only_corrupt_x86_cache_in_order(self):
        with self.native_verifier_fixture() as fixture:
            self.write_archive(
                fixture["target_directory"], "aarch64-linux-android", "debug", b"healthy"
            )
            self.write_archive(fixture["target_directory"], "x86_64-linux-android", "debug", b"invalid")

            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            cargo_calls = self.read_cargo_calls(fixture)
            self.assertEqual(
                [call["arguments"] for call in cargo_calls if call["arguments"][0] == "clean"],
                [["clean", "--target", "x86_64-linux-android", "-p", "libsodium-sys-stable"]],
            )
            self.assertEqual(
                [call["arguments"][call["arguments"].index("--target") + 1] for call in self.target_builds(cargo_calls)],
                ["aarch64-linux-android", "x86_64-linux-android"],
            )
            self.assertIn(
                "Android native cache recovery: target=x86_64-linux-android profile=debug reason=invalid-archive",
                result.stderr,
            )

    def test_empty_abi_selector_defaults_to_both_targets(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="") as fixture:
            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                [call["arguments"][call["arguments"].index("--target") + 1] for call in self.target_builds(self.read_cargo_calls(fixture))],
                ["aarch64-linux-android", "x86_64-linux-android"],
            )

    def test_x86_abi_selector_builds_only_x86_target(self):
        with self.native_verifier_fixture(PEPPY_ANDROID_ABI="x86_64") as fixture:
            result = self.run_verifier(fixture)

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                [call["arguments"][call["arguments"].index("--target") + 1] for call in self.target_builds(self.read_cargo_calls(fixture))],
                ["x86_64-linux-android"],
            )

    def test_rejects_unresolved_crypto_symbols_for_selected_abi(self):
        with self.native_verifier_fixture(
            PEPPY_ANDROID_ABI="x86_64", FAKE_UNRESOLVED_SYMBOL="1"
        ) as fixture:
            result = self.run_verifier(fixture)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unresolved crypto symbols", result.stderr)

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

                environment = self.native_verifier_environment()
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

                environment = self.native_verifier_environment()
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

            environment = self.native_verifier_environment()
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
                "llvm-ranlib",
                "aarch64-linux-android26-clang", "x86_64-linux-android26-clang",
            ):
                path = host_bin / tool
                path.write_text("#!/bin/sh\nexit 0\n")
                path.chmod(0o755)
            llvm_readelf = host_bin / "llvm-readelf"
            llvm_readelf.write_text(
                "#!/bin/sh\n"
                "if [ -n \"$FAKE_UNRESOLVED_SYMBOL\" ]; then\n"
                "  printf '0 0 0 0 0 0 UND sodium_test\\n'\n"
                "fi\n"
            )
            llvm_readelf.chmod(0o755)
            llvm_ar = host_bin / "llvm-ar"
            llvm_ar.write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = t ]; then\n"
                "  case \"$(cat \"$2\")\" in\n"
                "    healthy) printf 'member.o\\n' ;;\n"
                "    invalid) exit 1 ;;\n"
                "  esac\n"
                "fi\n"
            )
            llvm_ar.chmod(0o755)
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

    def native_verifier_fixture(self, **settings):
        return NativeVerifierFixture(self, settings)

    def native_verifier_environment(self):
        environment = os.environ.copy()
        for name in (
            "PEPPY_ANDROID_ABI",
            "PEPPY_ANDROID_FORCE_NATIVE_CLEAN",
            "PEPPY_ANDROID_NATIVE_PROFILE",
        ):
            environment.pop(name, None)
        return environment

    def run_verifier(self, fixture):
        return subprocess.run(
            ["bash", str(fixture["script"])], cwd=fixture["repository"],
            env=fixture["environment"], text=True, capture_output=True,
        )

    def read_cargo_calls(self, fixture):
        if not fixture["cargo_log"].exists():
            return []
        return [json.loads(line) for line in fixture["cargo_log"].read_text().splitlines()]

    def target_builds(self, cargo_calls):
        return [
            call for call in cargo_calls
            if call["arguments"][0] == "build" and "--target" in call["arguments"]
        ]

    def write_archive(self, target_directory, target, profile, contents):
        archive = (
            target_directory / target / profile / "build"
            / "libsodium-sys-stable-test" / "out" / "libsodium.a"
        )
        archive.parent.mkdir(parents=True)
        archive.write_bytes(contents)
        return archive

    def write_incomplete_package_output(self, target_directory, target, profile):
        (target_directory / target / profile / "build" / "libsodium-sys-stable-test" / "out").mkdir(
            parents=True
        )


class NativeVerifierFixture:
    def __init__(self, test_case, settings):
        self.test_case = test_case
        self.settings = settings
        self.temporary_directory = None

    def __enter__(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        temporary_directory = pathlib.Path(self.temporary_directory.name)
        repository = temporary_directory / "repository"
        script = repository / "infra/compose/verify-android-native.sh"
        script.parent.mkdir(parents=True)
        shutil.copy(ROOT / "infra/compose/verify-android-native.sh", script)
        tools_directory = temporary_directory / "tools"
        tools_directory.mkdir()
        self.test_case.write_fake_cargo(tools_directory / "cargo")
        ndk_home, _ = self.test_case.create_fake_ndk(temporary_directory)
        cargo_log = temporary_directory / "cargo-log.jsonl"
        environment = self.test_case.native_verifier_environment()
        environment.update({
            "ANDROID_NDK_HOME": str(ndk_home),
            "CARGO_TARGET_DIR": str(repository / "target"),
            "CARGO_LOG": str(cargo_log),
            "PATH": f"{tools_directory}:{environment['PATH']}",
        })
        environment.update(self.settings)
        return {
            "repository": repository,
            "script": script,
            "target_directory": repository / "target",
            "cargo_log": cargo_log,
            "environment": environment,
        }

    def __exit__(self, exception_type, exception, traceback):
        self.temporary_directory.cleanup()


if __name__ == "__main__":
    unittest.main()
