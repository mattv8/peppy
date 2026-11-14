import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

import infra.dev.dev_tls as dev_tls


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "infra/dev/dev_tls.py"
CONFIGURATION_VARIABLES = ("API_HOST_PORT", "PUBLIC_API_URL", "PEPPY_DEV_CONFIG_ROOT", "PEPPY_DEV_TLS_DIR", "PEPPY_SOURCE_TREE", "DEBUG")


def run_dev_tls(script, *arguments, **environment):
    env = os.environ.copy()
    env.update(environment)
    return subprocess.run(["python3", str(script), *arguments], cwd=script.parents[2], text=True, capture_output=True, env=env)


def git(directory, *arguments):
    environment = {name: value for name, value in os.environ.items() if not name.startswith("GIT_")}
    subprocess.run(
        ["git", "-c", "user.name=Test", "-c", "user.email=test@example.test", "-c", "commit.gpgsign=false", *arguments],
        cwd=directory, env=environment, text=True, capture_output=True, check=True,
    )


def make_linked_checkout(root):
    """Return (primary, linked): a primary checkout holding DEBUG=true .env and a linked worktree without .env."""
    primary, linked = root / "primary", root / "linked"
    primary.mkdir()
    git(primary, "init", "-q")
    git(primary, "commit", "-q", "--allow-empty", "-m", "synthetic")
    (primary / ".env").write_text("DEBUG=true\n")
    git(primary, "worktree", "add", "-q", "--detach", str(linked))
    return primary, linked


def install_helpers(directory):
    scripts = directory / "infra/dev"
    scripts.mkdir(parents=True)
    for name in ("dev_tls.py", "dev_port.py"):
        shutil.copy(ROOT / "infra/dev" / name, scripts / name)
    return scripts / "dev_tls.py"


def prepare_ca(root, tls):
    """Prepare a development CA under tls for a checkout at root and return its certificate bytes."""
    dotenv = root / ".env"
    dotenv.write_text("DEBUG=true\n")
    result = run_dev_tls(HELPER, "prepare", str(dotenv), DEBUG="true", PEPPY_DEV_TLS_DIR=str(tls))
    assert result.returncode == 0, result.stderr
    return (tls / "ca.pem").read_bytes()


def write_apk(path, ca=None):
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("AndroidManifest.xml", "synthetic\n")
        if ca is not None:
            archive.writestr("res/raw/peppy_dev_tls_ca.pem", ca)
    return path


class IsolatedConfigurationTests(unittest.TestCase):
    """Route ordinary tests, subprocess and in-process, to a synthetic config root; no real checkout's .env or TLS is read."""

    def setUp(self):
        environment = mock.patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        for name in CONFIGURATION_VARIABLES:
            os.environ.pop(name, None)
        config = tempfile.TemporaryDirectory()
        self.addCleanup(config.cleanup)
        self.config_root = Path(config.name)
        (self.config_root / ".env").write_text("DEBUG=false\n")
        os.environ["PEPPY_DEV_CONFIG_ROOT"] = str(self.config_root)


class DevelopmentTlsTests(IsolatedConfigurationTests):
    def run_helper(self, *arguments, **environment):
        env = os.environ.copy()
        env.pop("DEBUG", None)
        env.update(environment)
        return subprocess.run(["python3", str(HELPER), *arguments], cwd=ROOT, text=True, capture_output=True, env=env)

    def test_enabled_reads_environment_before_dotenv(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=false\n")
            result = self.run_helper("enabled", str(dotenv), DEBUG="true")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "true\n")

    def test_prepare_creates_a_private_key_and_loopback_leaf(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            result = self.run_helper("prepare", str(dotenv), PEPPY_DEV_TLS_DIR=str(tls))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((tls.stat().st_mode & 0o777), 0o700)
            self.assertEqual((tls / "ca-key.pem").stat().st_mode & 0o777, 0o600)
            text = subprocess.run(["openssl", "x509", "-in", str(tls / "server.pem"), "-noout", "-text"], text=True, capture_output=True, check=True).stdout
            self.assertIn("CA:FALSE", text)
            self.assertIn("Subject Key Identifier", text)
            self.assertIn("Authority Key Identifier", text)
            ca_text = subprocess.run(["openssl", "x509", "-in", str(tls / "ca.pem"), "-noout", "-text"], text=True, capture_output=True, check=True).stdout
            self.assertIn("Subject Key Identifier", ca_text)
            for name in ("localhost", "127.0.0.1", "10.0.2.2"):
                self.assertIn(name, text)

    def test_prepare_rejects_an_invalid_existing_ca(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            tls.mkdir()
            (tls / "ca.pem").write_text("not a certificate\n")
            result = self.run_helper("prepare", str(dotenv), PEPPY_DEV_TLS_DIR=str(tls))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("existing CA", result.stderr)

    def test_prepare_keeps_ca_when_renewing_leaf(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            environment = {"PEPPY_DEV_TLS_DIR": str(tls)}
            first = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(first.returncode, 0, first.stderr)
            ca = (tls / "ca.pem").read_bytes()
            (tls / "server.pem").unlink()
            (tls / "server-key.pem").unlink()
            renewed = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(renewed.returncode, 0, renewed.stderr)
            self.assertEqual((tls / "ca.pem").read_bytes(), ca)

    def test_prepare_renews_missing_aki_leaf_without_replacing_legacy_ca(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            tls.mkdir()
            subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(tls / "ca-key.pem")], check=True, capture_output=True)
            subprocess.run(["openssl", "req", "-x509", "-new", "-key", str(tls / "ca-key.pem"), "-sha256", "-days", "3650", "-subj", "/CN=Peppy Local Development CA", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-out", str(tls / "ca.pem")], check=True, capture_output=True)
            ca, key = (tls / "ca.pem").read_bytes(), (tls / "ca-key.pem").read_bytes()
            result = self.run_helper("prepare", str(dotenv), PEPPY_DEV_TLS_DIR=str(tls))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((tls / "ca.pem").read_bytes(), ca)
            self.assertEqual((tls / "ca-key.pem").read_bytes(), key)
            details = subprocess.run(["openssl", "x509", "-in", str(tls / "server.pem"), "-noout", "-text"], text=True, capture_output=True, check=True).stdout
            self.assertIn("Authority Key Identifier", details)
            self.assertEqual((tls / "ca.pem").stat().st_mode & 0o777, 0o644)
            self.assertEqual((tls / "ca-key.pem").stat().st_mode & 0o777, 0o600)
            self.assertEqual((tls / "server.pem").stat().st_mode & 0o777, 0o644)
            self.assertEqual((tls / "server-key.pem").stat().st_mode & 0o777, 0o600)

    def test_prepare_preserves_current_leaf(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            environment = {"PEPPY_DEV_TLS_DIR": str(tls)}
            first = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(first.returncode, 0, first.stderr)
            certificate = (tls / "server.pem").read_bytes()
            key = (tls / "server-key.pem").read_bytes()
            second = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(second.returncode, 0, second.stderr)
            self.assertEqual((tls / "server.pem").read_bytes(), certificate)
            self.assertEqual((tls / "server-key.pem").read_bytes(), key)

    def test_leaf_without_dns_localhost_san_is_regenerated(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            environment = {"PEPPY_DEV_TLS_DIR": str(tls)}
            first = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(first.returncode, 0, first.stderr)
            config = root / "no-dns-san.cnf"
            config.write_text("""[extensions]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = IP:127.0.0.1,IP:::1,IP:10.0.2.2
""")
            subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(tls / "server-key.pem")], check=True, capture_output=True)
            subprocess.run(["openssl", "req", "-new", "-key", str(tls / "server-key.pem"), "-subj", "/CN=localhost", "-out", str(root / "no-dns.csr")], check=True, capture_output=True)
            subprocess.run(["openssl", "x509", "-req", "-in", str(root / "no-dns.csr"), "-CA", str(tls / "ca.pem"), "-CAkey", str(tls / "ca-key.pem"), "-CAcreateserial", "-CAserial", str(root / "ca.srl"), "-days", "365", "-sha256", "-extfile", str(config), "-extensions", "extensions", "-out", str(tls / "server.pem")], check=True, capture_output=True)
            certificate = (tls / "server.pem").read_bytes()
            verified = subprocess.run(["openssl", "verify", "-CAfile", str(tls / "ca.pem"), str(tls / "server.pem")], capture_output=True)
            self.assertEqual(verified.returncode, 0, verified.stderr)
            self.assertFalse(dev_tls.leaf_is_current(tls))
            renewed = self.run_helper("prepare", str(dotenv), **environment)
            self.assertEqual(renewed.returncode, 0, renewed.stderr)
            self.assertNotEqual((tls / "server.pem").read_bytes(), certificate)
            self.assertTrue(dev_tls.leaf_is_current(tls))

    def test_prepare_rejects_incomplete_ca_pair(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            tls = root / "tls"
            tls.mkdir()
            (tls / "ca-key.pem").write_text("incomplete\n")
            result = self.run_helper("prepare", str(dotenv), PEPPY_DEV_TLS_DIR=str(tls))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("incomplete", result.stderr)

    def test_prepare_serializes_concurrent_first_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            environment = os.environ | {"PEPPY_DEV_TLS_DIR": str(root / "tls")}
            commands = [["python3", str(HELPER), "prepare", str(dotenv)]] * 2
            processes = [subprocess.Popen(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment) for command in commands]
            for process in processes:
                _, stderr = process.communicate(timeout=30)
                self.assertEqual(process.returncode, 0, stderr)

    def test_enabled_rejects_malformed_debug(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=yes\n")
            result = self.run_helper("enabled", str(dotenv))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("DEBUG must be true, false, 1, or 0", result.stderr)

    def test_compose_adds_caddy_only_when_starting_dev(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=true\nAPI_HOST_PORT=7100\n")
            calls = []

            def capture(_program, arguments, environment):
                calls.append((arguments, environment))
                raise RuntimeError("captured")

            with mock.patch.object(dev_tls.os, "execvpe", capture):
                for arguments in (("up", "--detach", "postgres"), ("up", "dev"), ("up", "dev", "caddy")):
                    with self.assertRaisesRegex(RuntimeError, "captured"):
                        dev_tls.compose_arguments(dotenv, list(arguments), False)

            self.assertNotIn("caddy", calls[0][0])
            self.assertEqual(calls[1][0].count("caddy"), 1)
            self.assertEqual(calls[2][0].count("caddy"), 1)
            self.assertEqual(calls[1][1]["DEBUG"], "true")

    def test_ca_path_uses_the_configured_tls_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            tls = Path(directory) / "custom tls"
            result = self.run_helper("ca-path", PEPPY_DEV_TLS_DIR=str(tls))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, f"{tls.resolve() / 'ca.pem'}\n")

    def test_compose_normalizes_disabled_debug_for_selected_source(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=false\n")
            captured = []

            def capture(_program, _arguments, environment):
                captured.append(environment)
                raise RuntimeError("captured")

            with mock.patch.object(dev_tls.os, "execvpe", capture), mock.patch.dict(os.environ, {"DEBUG": "false"}, clear=False):
                with self.assertRaisesRegex(RuntimeError, "captured"):
                    dev_tls.compose_arguments(dotenv, ["config"], False)
            self.assertEqual(captured[0]["DEBUG"], "false")

    def test_compose_uses_caller_debug_over_selected_source_dotenv(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=false\n")
            selected = root / "selected"
            selected.mkdir()
            (selected / ".env").write_text("DEBUG=false\n")
            captured = []

            def capture(_program, _arguments, environment):
                captured.append(environment)
                raise RuntimeError("captured")

            with mock.patch.object(dev_tls.os, "execvpe", capture), mock.patch.dict(os.environ, {"DEBUG": "true", "PEPPY_SOURCE_TREE": str(selected)}, clear=False):
                with self.assertRaisesRegex(RuntimeError, "captured"):
                    dev_tls.compose_arguments(dotenv, ["config"], False)
            self.assertEqual(captured[0]["DEBUG"], "true")
            self.assertEqual(captured[0]["PEPPY_SOURCE_TREE"], str(selected.resolve()))

    def test_compose_preserves_external_public_urls(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=true\nPUBLIC_API_URL=https://api.example.test/base?version=1\n")
            captured = []

            def capture(_program, _arguments, environment):
                captured.append(environment)
                raise RuntimeError("captured")

            with mock.patch.object(dev_tls.os, "execvpe", capture):
                with self.assertRaisesRegex(RuntimeError, "captured"):
                    dev_tls.compose_arguments(dotenv, ["config"], False)
            self.assertEqual(captured[0]["PUBLIC_API_URL"], "https://api.example.test/base?version=1")

    def test_compose_ignores_stale_attachment_url(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=true\nAPI_HOST_PORT=7100\nPUBLIC_ATTACHMENT_URL=http://127.0.0.1:7000/files?version=1\n")
            captured = []

            def capture(_program, _arguments, environment):
                captured.append(environment)
                raise RuntimeError("captured")

            with mock.patch.object(dev_tls.os, "execvpe", capture):
                with self.assertRaisesRegex(RuntimeError, "captured"):
                    dev_tls.compose_arguments(dotenv, ["config"], False)
            self.assertNotIn("PUBLIC_ATTACHMENT_URL", captured[0])


class ConfigurationRootTests(unittest.TestCase):
    def setUp(self):
        environment = mock.patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        for name in CONFIGURATION_VARIABLES:
            os.environ.pop(name, None)

    def test_linked_worktree_without_env_uses_primary_dotenv_and_tls_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            primary, linked = make_linked_checkout(Path(directory))
            script = install_helpers(linked)
            dotenv = run_dev_tls(script, "dotenv-path")
            self.assertEqual(dotenv.returncode, 0, dotenv.stderr)
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")
            enabled = run_dev_tls(script, "enabled")
            self.assertEqual(enabled.returncode, 0, enabled.stderr)
            self.assertEqual(enabled.stdout, "true\n")
            ca = run_dev_tls(script, "ca-path")
            self.assertEqual(ca.returncode, 0, ca.stderr)
            self.assertEqual(ca.stdout, f"{(primary / '.opencode/dev/tls/ca.pem').resolve()}\n")

    def test_prepare_from_linked_worktree_writes_primary_tls_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            primary, linked = make_linked_checkout(Path(directory))
            script = install_helpers(linked)
            result = run_dev_tls(script, "prepare")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((primary / ".opencode/dev/tls/ca.pem").is_file())
            self.assertTrue((primary / ".opencode/dev/tls/server.pem").is_file())
            self.assertFalse((linked / ".opencode").exists())

    def test_checkout_dotenv_takes_precedence_over_primary(self):
        with tempfile.TemporaryDirectory() as directory:
            _, linked = make_linked_checkout(Path(directory))
            (linked / ".env").write_text("DEBUG=false\n")
            script = install_helpers(linked)
            dotenv = run_dev_tls(script, "dotenv-path")
            self.assertEqual(dotenv.stdout, f"{(linked / '.env').resolve()}\n")
            enabled = run_dev_tls(script, "enabled")
            self.assertEqual(enabled.stdout, "false\n")

    def test_config_root_override_selects_its_dotenv_and_tls_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, linked = make_linked_checkout(root)
            override = root / "override"
            override.mkdir()
            (override / ".env").write_text("DEBUG=false\n")
            script = install_helpers(linked)
            override_root = str(override)
            dotenv = run_dev_tls(script, "dotenv-path", PEPPY_DEV_CONFIG_ROOT=override_root)
            self.assertEqual(dotenv.stdout, f"{(override / '.env').resolve()}\n")
            enabled = run_dev_tls(script, "enabled", PEPPY_DEV_CONFIG_ROOT=override_root)
            self.assertEqual(enabled.stdout, "false\n")
            ca = run_dev_tls(script, "ca-path", PEPPY_DEV_CONFIG_ROOT=override_root)
            self.assertEqual(ca.stdout, f"{(override / '.opencode/dev/tls/ca.pem').resolve()}\n")

    def test_tls_directory_override_still_wins_over_config_root(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, linked = make_linked_checkout(root)
            custom = root / "custom tls"
            script = install_helpers(linked)
            ca = run_dev_tls(script, "ca-path", PEPPY_DEV_TLS_DIR=str(custom))
            self.assertEqual(ca.stdout, f"{(custom / 'ca.pem').resolve()}\n")

    def test_config_root_override_without_env_fails_instead_of_disabling_tls(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            _, linked = make_linked_checkout(root)
            script = install_helpers(linked)
            for command in ("dotenv-path", "enabled"):
                result = run_dev_tls(script, command, PEPPY_DEV_CONFIG_ROOT=str(root / "missing"))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("PEPPY_DEV_CONFIG_ROOT has no .env", result.stderr)

    def test_gitdir_pointing_outside_a_git_directory_is_not_a_primary_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unrelated = root / "unrelated"
            (unrelated / "metadata").mkdir(parents=True)
            (unrelated / ".env").write_text("DEBUG=true\n")
            (unrelated / "metadata/commondir").write_text("..\n")
            checkout = root / "checkout"
            script = install_helpers(checkout)
            (checkout / ".git").write_text(f"gitdir: {unrelated / 'metadata'}\n")
            dotenv = run_dev_tls(script, "dotenv-path")
            self.assertNotEqual(dotenv.returncode, 0)
            self.assertEqual(dotenv.stdout, "")
            self.assertIn("PEPPY_DEV_CONFIG_ROOT", dotenv.stderr)

    def test_plain_checkout_without_env_falls_back_to_its_own_root(self):
        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory) / "plain"
            script = install_helpers(checkout)
            dotenv = run_dev_tls(script, "dotenv-path")
            self.assertEqual(dotenv.stdout, f"{(checkout / '.env').resolve()}\n")
            enabled = run_dev_tls(script, "enabled")
            self.assertEqual(enabled.stdout, "false\n")

    def test_linked_worktree_of_separate_git_dir_primary_fails_closed_until_overridden(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary, linked = root / "primary checkout", root / "linked checkout"
            primary.mkdir()
            git(primary, "init", "-q", f"--separate-git-dir={root / 'separate git dir'}")
            (primary / ".env").write_text("DEBUG=true\n")
            git(primary, "commit", "-q", "--allow-empty", "-m", "synthetic")
            git(primary, "worktree", "add", "-q", "--detach", str(linked))
            script = install_helpers(linked)
            unresolved = run_dev_tls(script, "dotenv-path")
            self.assertNotEqual(unresolved.returncode, 0)
            self.assertEqual(unresolved.stdout, "")
            self.assertIn("PEPPY_DEV_CONFIG_ROOT", unresolved.stderr)
            overridden = {"PEPPY_DEV_CONFIG_ROOT": str(primary)}
            dotenv = run_dev_tls(script, "dotenv-path", **overridden)
            self.assertEqual(dotenv.returncode, 0, dotenv.stderr)
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")
            enabled = run_dev_tls(script, "enabled", **overridden)
            self.assertEqual(enabled.stdout, "true\n")
            ca = run_dev_tls(script, "ca-path", **overridden)
            self.assertEqual(ca.stdout, f"{(primary / '.opencode/dev/tls/ca.pem').resolve()}\n")

    def test_separate_git_dir_primary_without_env_stays_disabled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary = root / "primary checkout"
            primary.mkdir()
            git(primary, "init", "-q", f"--separate-git-dir={root / 'separate git dir'}")
            git(primary, "commit", "-q", "--allow-empty", "-m", "synthetic")
            script = install_helpers(primary)
            enabled = run_dev_tls(script, "enabled")
            self.assertEqual(enabled.returncode, 0, enabled.stderr)
            self.assertEqual(enabled.stdout, "false\n")

    def test_linked_worktree_of_bare_repository_fails_closed_until_overridden(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary, _ = make_linked_checkout(root)
            bare = root / "bare.git"
            git(root, "clone", "-q", "--bare", str(primary), str(bare))
            bare_linked = root / "bare linked"
            git(bare, "worktree", "add", "-q", "--detach", str(bare_linked))
            script = install_helpers(bare_linked)
            unresolved = run_dev_tls(script, "enabled")
            self.assertNotEqual(unresolved.returncode, 0)
            self.assertIn("PEPPY_DEV_CONFIG_ROOT", unresolved.stderr)
            dotenv = run_dev_tls(script, "dotenv-path", PEPPY_DEV_CONFIG_ROOT=str(primary))
            self.assertEqual(dotenv.returncode, 0, dotenv.stderr)
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")

    def test_linked_worktree_resolves_from_gitfile_metadata_when_git_is_unavailable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            primary, linked = make_linked_checkout(root)
            bin_directory = root / "bin"
            bin_directory.mkdir()
            (bin_directory / "python3").symlink_to(sys.executable)
            script = install_helpers(linked)
            dotenv = run_dev_tls(script, "dotenv-path", PATH=str(bin_directory))
            self.assertEqual(dotenv.returncode, 0, dotenv.stderr)
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")

    def test_linked_worktree_with_partial_metadata_fails_closed_until_overridden(self):
        with tempfile.TemporaryDirectory() as directory:
            primary, linked = make_linked_checkout(Path(directory))
            admin = primary / ".git/worktrees/linked"
            (admin / "commondir").unlink()
            self.assertTrue((admin / "gitdir").is_file())
            script = install_helpers(linked)
            unresolved = run_dev_tls(script, "enabled")
            self.assertNotEqual(unresolved.returncode, 0)
            self.assertEqual(unresolved.stdout, "")
            self.assertIn("PEPPY_DEV_CONFIG_ROOT", unresolved.stderr)
            dotenv = run_dev_tls(script, "dotenv-path", PEPPY_DEV_CONFIG_ROOT=str(primary))
            self.assertEqual(dotenv.returncode, 0, dotenv.stderr)
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")

    def test_linked_worktree_with_missing_registration_fails_closed_until_overridden(self):
        with tempfile.TemporaryDirectory() as directory:
            primary, linked = make_linked_checkout(Path(directory))
            shutil.rmtree(primary / ".git/worktrees/linked")
            script = install_helpers(linked)
            unresolved = run_dev_tls(script, "prepare")
            self.assertNotEqual(unresolved.returncode, 0)
            self.assertIn("PEPPY_DEV_CONFIG_ROOT", unresolved.stderr)
            self.assertFalse((linked / ".opencode").exists())
            dotenv = run_dev_tls(script, "dotenv-path", PEPPY_DEV_CONFIG_ROOT=str(primary))
            self.assertEqual(dotenv.stdout, f"{(primary / '.env').resolve()}\n")


class CertificateTests(unittest.TestCase):
    def test_certificate_der_accepts_exactly_one_pem_certificate(self):
        with tempfile.TemporaryDirectory() as directory:
            ca = prepare_ca(Path(directory), Path(directory) / "tls")
            self.assertTrue(dev_tls.certificate_der(ca, "ca"))

    def test_certificate_der_rejects_multiple_pem_certificates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ca = prepare_ca(root, root / "tls")
            other = root / "other"
            other.mkdir()
            other_ca = prepare_ca(other, other / "tls")
            for bundle in (ca + ca, ca + other_ca):
                with self.subTest(bundle=bundle.count(b"BEGIN CERTIFICATE")):
                    with self.assertRaisesRegex(dev_tls.TlsError, "exactly one PEM certificate"):
                        dev_tls.certificate_der(bundle, "bundle")


class ApkTrustTests(IsolatedConfigurationTests):
    def test_verify_apk_accepts_embedded_current_ca(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tls = root / "tls"
            ca = prepare_ca(root, tls)
            apk = write_apk(root / "app-debug.apk", ca)
            result = run_dev_tls(HELPER, "verify-apk", str(apk), DEBUG="true", PEPPY_DEV_TLS_DIR=str(tls))
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_verify_apk_rejects_apk_without_ca_resource(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tls = root / "tls"
            prepare_ca(root, tls)
            apk = write_apk(root / "app-debug.apk")
            result = run_dev_tls(HELPER, "verify-apk", str(apk), DEBUG="true", PEPPY_DEV_TLS_DIR=str(tls))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("peppy_dev_tls_ca.pem", result.stderr)
            self.assertIn("Android: Rebuild and open", result.stderr)

    def test_verify_apk_rejects_apk_embedding_a_different_ca(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tls = root / "tls"
            prepare_ca(root, tls)
            other = root / "other"
            other.mkdir()
            other_ca = prepare_ca(other, other / "tls")
            apk = write_apk(root / "app-debug.apk", other_ca)
            result = run_dev_tls(HELPER, "verify-apk", str(apk), DEBUG="true", PEPPY_DEV_TLS_DIR=str(tls))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not trust the current development CA", result.stderr)
            self.assertIn("Android: Rebuild and open", result.stderr)

    def test_verify_apk_requires_an_existing_apk_when_debug_is_enabled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tls = root / "tls"
            prepare_ca(root, tls)
            result = run_dev_tls(HELPER, "verify-apk", str(root / "absent.apk"), DEBUG="true", PEPPY_DEV_TLS_DIR=str(tls))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing", result.stderr.lower())

    def test_verify_apk_rejects_a_file_that_is_not_an_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            apk = root / "app-debug.apk"
            apk.write_text("not a zip\n")
            result = run_dev_tls(HELPER, "verify-apk", str(apk), DEBUG="true", PEPPY_DEV_TLS_DIR=str(root / "tls"))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("not a valid APK", result.stderr)

    def test_verify_apk_skips_checks_when_debug_is_disabled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            apk = write_apk(root / "app-release.apk")
            result = run_dev_tls(HELPER, "verify-apk", str(apk), DEBUG="false", PEPPY_DEV_TLS_DIR=str(root / "absent"))
            self.assertEqual(result.returncode, 0, result.stderr)
            absent = run_dev_tls(HELPER, "verify-apk", str(root / "absent.apk"), DEBUG="false")
            self.assertEqual(absent.returncode, 0, absent.stderr)
