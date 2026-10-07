import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "infra/build/build-browser-core.sh"


class BrowserCoreBuildTests(unittest.TestCase):
    def test_persistent_target_cache_refreshes_workspace_inputs_before_cargo(self):
        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary_directory = Path(temporary_directory)
            repository = temporary_directory / "repository"
            tools = temporary_directory / "tools"
            target = temporary_directory / "target"
            cache_sentinel = target / "cache-sentinel"
            cargo_log = temporary_directory / "cargo-log"
            self.prepare_repository(repository)
            self.write_tools(tools)
            cache_sentinel.parent.mkdir()
            cache_sentinel.write_text("preserve cache")
            os.utime(cache_sentinel, (1_500_000_000, 1_500_000_000))

            environment = os.environ | {
                "EMSDK": "test",
                "CARGO_TARGET_DIR": str(target),
                "CACHE_SENTINEL": str(cache_sentinel),
                "CARGO_LOG": str(cargo_log),
                "PATH": f"{tools}:{os.environ['PATH']}",
            }
            result = subprocess.run(
                ["bash", str(repository / "infra/build/build-browser-core.sh"), "output"],
                cwd=repository,
                text=True,
                capture_output=True,
                env=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(cargo_log.read_text(), "inputs fresh\n")
            self.assertEqual(cache_sentinel.read_text(), "preserve cache")
            self.assertEqual(cache_sentinel.stat().st_mtime, 1_500_000_000)

    def prepare_repository(self, repository):
        script = repository / "infra/build/build-browser-core.sh"
        script.parent.mkdir(parents=True)
        shutil.copy(SCRIPT, script)
        (repository / "Cargo.toml").write_text("[workspace]\n")
        (repository / "Cargo.lock").write_text("version = 4\n")
        (repository / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.98.1"\n')
        for manifest in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml"):
            os.utime(repository / manifest, (1_000_000_000, 1_000_000_000))
        for relative_path in (
            "crates/browser-bindings/src/lib.rs",
            "crates/crypto/src/lib.rs",
            "vendor/local-dependency/build.rs",
            "services/local-service/Cargo.toml",
        ):
            source = repository / relative_path
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text(relative_path)
            os.utime(source, (1_000_000_000, 1_000_000_000))

    def write_tools(self, tools):
        tools.mkdir()
        self.write_tool(tools / "emcc", '#!/bin/sh\nprintf "emcc (Emscripten gcc/clang-like replacement) 6.0.11 \\n"\n')
        self.write_tool(tools / "emar", "#!/bin/sh\n")
        self.write_tool(tools / "emranlib", "#!/bin/sh\n")
        self.write_tool(tools / "rustc", '#!/bin/sh\nprintf "rustc 1.98.1 (test)\\n"\n')
        self.write_tool(
            tools / "cargo",
            "#!/bin/sh\n"
            "for input in Cargo.toml Cargo.lock rust-toolchain.toml crates/browser-bindings/src/lib.rs crates/crypto/src/lib.rs vendor/local-dependency/build.rs services/local-service/Cargo.toml; do\n"
            "  [ \"$input\" -nt \"$CACHE_SENTINEL\" ] || exit 1\n"
            "done\n"
            "mkdir -p \"$CARGO_TARGET_DIR/wasm32-unknown-emscripten/release\"\n"
            "printf js > \"$CARGO_TARGET_DIR/wasm32-unknown-emscripten/release/peppy-browser-core.js\"\n"
            "printf wasm > \"$CARGO_TARGET_DIR/wasm32-unknown-emscripten/release/peppy_browser_core.wasm\"\n"
            "printf 'inputs fresh\\n' > \"$CARGO_LOG\"\n",
        )

    def write_tool(self, path, contents):
        path.write_text(contents)
        path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
