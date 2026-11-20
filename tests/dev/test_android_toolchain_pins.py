import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
DOCKERFILE = ROOT / "infra/docker/android.Dockerfile"
CMDLINE_TOOLS_SHA256 = "2d2d50857e4eb553af5a6dc3ad507a17adf43d115264b1afc116f95c92e5e258"
RUSTUP_VERSION = "1.29.3"
RUSTUP_INIT_SHA256 = "b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55"
CMDLINE_TOOLS_VERIFY = '"${ANDROID_CMDLINE_TOOLS_SHA256}  /tmp/tools.zip" | sha256sum -c -'
RUSTUP_INIT_DOWNLOAD = '-o /tmp/rustup-init'
RUSTUP_INIT_VERIFY = '"${RUSTUP_INIT_SHA256}  /tmp/rustup-init" | sha256sum -c -'
RUSTUP_INIT_RUN = "/tmp/rustup-init -y"


class AndroidToolchainPinTests(unittest.TestCase):
    def setUp(self):
        self.dockerfile = DOCKERFILE.read_text()

    def position_of_single(self, token):
        self.assertEqual(self.dockerfile.count(token), 1, token)
        return self.dockerfile.index(token)

    def test_pins_are_declared_with_expected_values(self):
        self.assertIn(f"ARG ANDROID_CMDLINE_TOOLS_SHA256={CMDLINE_TOOLS_SHA256}\n", self.dockerfile)
        self.assertIn(f"ARG RUSTUP_VERSION={RUSTUP_VERSION}\n", self.dockerfile)
        self.assertIn(f"ARG RUSTUP_INIT_SHA256={RUSTUP_INIT_SHA256}\n", self.dockerfile)

    def test_cmdline_tools_zip_is_verified_before_unzip(self):
        download = self.position_of_single('-o /tmp/tools.zip')
        verify = self.position_of_single(CMDLINE_TOOLS_VERIFY)
        unzip = self.position_of_single("unzip -q /tmp/tools.zip")
        self.assertLess(download, verify)
        self.assertLess(verify, unzip)

    def test_rustup_init_is_pinned_version_and_verified_before_execution(self):
        self.assertIn(
            'https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init',
            self.dockerfile,
        )
        download = self.position_of_single(RUSTUP_INIT_DOWNLOAD)
        verify = self.position_of_single(RUSTUP_INIT_VERIFY)
        execute = self.position_of_single(RUSTUP_INIT_RUN)
        self.assertLess(download, verify)
        self.assertLess(verify, execute)

    def test_unverified_rustup_script_download_is_gone(self):
        self.assertNotIn("sh.rustup.rs", self.dockerfile)


if __name__ == "__main__":
    unittest.main()
