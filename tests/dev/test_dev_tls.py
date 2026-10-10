import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import infra.dev.dev_tls as dev_tls


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "infra/dev/dev_tls.py"


class DevelopmentTlsTests(unittest.TestCase):
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

    def test_compose_normalizes_loopback_attachment_url_without_dropping_path(self):
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
            self.assertEqual(captured[0]["PUBLIC_ATTACHMENT_URL"], "https://127.0.0.1:7100/files?version=1")
