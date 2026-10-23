import importlib.util
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import URLError
from urllib.request import urlopen
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("demo", ROOT / "infra/dev/demo.py")
demo = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = demo
SPEC.loader.exec_module(demo)


class DemoTests(unittest.TestCase):
    def test_direct_script_invocation_loads_help(self):
        result = subprocess.run(
            [sys.executable, str(ROOT / "infra/dev/demo.py"), "--help"],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--inside-container", result.stdout)

    def test_private_run_files_and_synthetic_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            run = demo.make_run_directory(Path(directory))
            self.assertEqual((run.stat().st_mode & 0o777), 0o700)
            env_file = demo.write_environment(run)
            self.assertEqual((env_file.stat().st_mode & 0o777), 0o600)
            values = env_file.read_text()
            self.assertIn("POSTGRES_PASSWORD=synthetic-", values)
            self.assertNotIn("DATABASE_URL", values)

    def test_synthetic_environment_advertises_canonical_https_origin(self):
        values = demo.synthetic_values()
        self.assertEqual(values["PUBLIC_API_URL"], "https://127.0.0.1")
        self.assertEqual(values["PUBLIC_ATTACHMENT_URL"], "https://127.0.0.1")
        self.assertEqual(values["BIND_ADDR"], "127.0.0.1:8080")

    def test_simulator_passphrase_has_distinct_words_and_is_not_in_dotenv(self):
        with mock.patch.object(demo.secrets, "token_urlsafe", return_value="random-token") as random_token:
            passphrase = demo.synthetic_simulator_passphrase()
        self.assertEqual(random_token.call_count, 6)
        words = passphrase.split()
        self.assertGreaterEqual(len(words), 4)
        self.assertEqual(len(words), len(set(words)))
        self.assertGreaterEqual(len(passphrase), 24)
        with tempfile.TemporaryDirectory() as directory:
            env_file = demo.write_environment(demo.make_run_directory(Path(directory)))
            self.assertNotIn(passphrase, env_file.read_text())

    def test_tls_preparation_overrides_ambient_debug_without_mutating_it(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / "demo-tls.env"
            dotenv.write_text("DEBUG=true\n")
            calls = []
            with mock.patch.object(demo.subprocess, "run", side_effect=lambda command, **kwargs: calls.append((command, kwargs))):
                with mock.patch.dict(os.environ, {"DEBUG": "false", "PEPPY_DEV_TLS_DIR": "/ambient/tls"}, clear=False):
                    demo.prepare_demo_tls(dotenv, Path("/selected/tls"))
                    self.assertEqual(os.environ["DEBUG"], "false")
            command, kwargs = calls[0]
            self.assertEqual(command[-2:], ["prepare", str(dotenv)])
            self.assertEqual(kwargs["env"]["DEBUG"], "true")
            self.assertEqual(kwargs["env"]["PEPPY_DEV_TLS_DIR"], "/selected/tls")

    def test_pipe_reader_handles_eof_without_a_newline(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import sys; sys.stdout.write('ready')"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        with tempfile.TemporaryDirectory() as directory:
            reader = demo.ProcessOutput(process, Path(directory) / "log")
            self.assertTrue(reader.wait_for("ready", "stdout", 2))
            process.wait(timeout=2)
            reader.close()

    def test_pipe_reader_requires_marker_after_checkpoint(self):
        process = subprocess.Popen(
            [sys.executable, "-c", "import sys; print('phase=sync'); sys.stdout.flush(); sys.stdin.readline(); print('phase=sync'); sys.stdout.flush()"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        with tempfile.TemporaryDirectory() as directory:
            reader = demo.ProcessOutput(process, Path(directory) / "log")
            self.assertTrue(reader.wait_for("phase=sync", "stdout", 2))
            checkpoint = reader.checkpoint("stdout")
            process.stdin.write(b"continue\n")
            process.stdin.flush()
            self.assertTrue(reader.wait_for("phase=sync", "stdout", 2, after=checkpoint))
            self.assertEqual(process.wait(timeout=2), 0)
            process.stdin.close()
            reader.close()

    def test_sanitized_environment_removes_synthetic_and_compose_overrides(self):
        values = demo.synthetic_values()
        cleaned = demo.sanitized_environment({"PATH": "/bin", "BIND_ADDR": "bad", "COMPOSE_PROJECT_NAME": "bad", "OTHER": "ok"}, values)
        self.assertEqual(cleaned, {"PATH": "/bin", "OTHER": "ok"})

    def test_host_forwards_cargo_timeout_to_isolated_runner(self):
        calls = []
        original_checked, original_cleanup, original_root, original_prepare = demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare
        with tempfile.TemporaryDirectory() as directory:
            try:
                demo.ROOT = Path(directory)
                demo.checked = lambda command, **kwargs: calls.append((command, kwargs))
                demo.subprocess.run = lambda *args, **kwargs: None
                demo.dev_tls.prepare = lambda _dotenv: None
                previous = os.environ.get("PEPPY_DEMO_CARGO_TIMEOUT")
                os.environ["PEPPY_DEMO_CARGO_TIMEOUT"] = "17"
                demo.run_host_demo()
            finally:
                if previous is None:
                    os.environ.pop("PEPPY_DEMO_CARGO_TIMEOUT", None)
                else:
                    os.environ["PEPPY_DEMO_CARGO_TIMEOUT"] = previous
                demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare = original_checked, original_cleanup, original_root, original_prepare
        up, kwargs = next(call for call in calls if "--exit-code-from" in call[0])
        self.assertEqual(kwargs["env"]["PEPPY_DEMO_CARGO_TIMEOUT"], "17")
        self.assertIn("PEPPY_DEMO_RUN", kwargs["env"])
        self.assertGreaterEqual(len(kwargs["env"]["PEPPY_SIMULATOR_PASSPHRASE"].split()), 4)

    def test_host_demo_uses_isolated_tls_compose_services(self):
        calls = []
        original_checked, original_cleanup, original_root, original_prepare = demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare
        with tempfile.TemporaryDirectory() as directory:
            try:
                demo.ROOT = Path(directory)
                demo.checked = lambda command, **kwargs: calls.append(command)
                demo.subprocess.run = lambda *args, **kwargs: None
                demo.dev_tls.prepare = lambda _dotenv: None
                demo.run_host_demo()
            finally:
                demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare = original_checked, original_cleanup, original_root, original_prepare
        up = next(command for command in calls if "--exit-code-from" in command)
        self.assertEqual(up[-2:], ["dev", "caddy"])
        self.assertIn("infra/compose/compose.demo-tls.yml", " ".join(str(value) for value in up))

    def test_demo_uses_current_enrollment_commands_not_removed_owner_cli(self):
        source = (ROOT / "infra/dev/demo.py").read_text()
        self.assertIn('[simulator_binary, "claim-owner"', source)
        self.assertIn('[simulator_binary, "admit-gateway"', source)
        self.assertNotIn('"create-owner"', source)

    def test_migration_uses_built_server_binary_not_cargo_run(self):
        source = (ROOT / "infra/dev/demo.py").read_text()
        self.assertIn('checked([server_binary, "migrate"], env=server_env)', source)
        self.assertNotIn('"-p", "peppy-server", "--", "migrate"', source)

    def test_credential_metadata_is_secret_free_for_invalid_state(self):
        with tempfile.TemporaryDirectory() as directory:
            credentials = Path(directory) / "credentials.json"
            canary = "NEVER-LOG-DEVICE-TOKEN"
            credentials.write_text(json.dumps({"version": "wrong", "origin": "http://bad", "vaultId": "vault", "deviceId": "device", "deviceToken": canary}))
            credentials.chmod(0o600)
            metadata, valid = demo.credential_metadata(credentials, "https://127.0.0.1")
            rendered = json.dumps(metadata)
            self.assertFalse(valid)
            self.assertNotIn(canary, rendered)
            self.assertNotIn("http://bad", rendered)
            self.assertEqual(metadata["type"], "regular")
            self.assertFalse(metadata["version_valid"])

    def test_host_demo_down_is_scoped_on_success_and_failure(self):
        for failing in (False, True):
            calls = []
            original_checked, original_cleanup, original_root, original_prepare = demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare
            with tempfile.TemporaryDirectory() as directory:
                try:
                    demo.ROOT = Path(directory)
                    demo.dev_tls.prepare = lambda _dotenv: None
                    def checked(command, **kwargs):
                        calls.append(command)
                        if failing and command[-2:] == ["build", "dev"]:
                            raise RuntimeError("build failed")
                    demo.checked = checked
                    demo.subprocess.run = lambda command, **kwargs: calls.append(command)
                    if failing:
                        with self.assertRaises(RuntimeError):
                            demo.run_host_demo()
                    else:
                        demo.run_host_demo()
                finally:
                    demo.checked, demo.subprocess.run, demo.ROOT, demo.dev_tls.prepare = original_checked, original_cleanup, original_root, original_prepare
            down = next(command for command in calls if command[-3:] == ["down", "--volumes", "--remove-orphans"])
            self.assertIn("--project-name", down)
            self.assertTrue(down[down.index("--project-name") + 1].startswith("peppy-demo-"))
            self.assertNotIn("peppy", down)

    def test_timeout_rejects_nonpositive_and_non_numeric_values(self):
        previous = os.environ.get("DEMO_TEST_TIMEOUT")
        try:
            for value in ("0", "-1", "abc"):
                os.environ["DEMO_TEST_TIMEOUT"] = value
                with self.assertRaises(SystemExit):
                    demo.timeout_from_environment("DEMO_TEST_TIMEOUT", 1)
        finally:
            if previous is None:
                os.environ.pop("DEMO_TEST_TIMEOUT", None)
            else:
                os.environ["DEMO_TEST_TIMEOUT"] = previous

    def test_host_demo_installs_interrupt_handlers_for_cleanup(self):
        handlers, calls = {}, []
        original_checked, original_cleanup, original_root, original_signal, original_prepare = demo.checked, demo.subprocess.run, demo.ROOT, demo.signal.signal, demo.dev_tls.prepare
        with tempfile.TemporaryDirectory() as directory:
            try:
                demo.ROOT = Path(directory)
                demo.checked = lambda command, **kwargs: calls.append(command)
                demo.subprocess.run = lambda *args, **kwargs: None
                demo.dev_tls.prepare = lambda _dotenv: None
                def signal_stub(signum, handler):
                    handlers.setdefault(signum, handler)
                    return demo.signal.SIG_DFL
                demo.signal.signal = signal_stub
                demo.run_host_demo()
            finally:
                demo.checked, demo.subprocess.run, demo.ROOT, demo.signal.signal, demo.dev_tls.prepare = original_checked, original_cleanup, original_root, original_signal, original_prepare
        self.assertIn(demo.signal.SIGHUP, handlers)
        with self.assertRaises(KeyboardInterrupt):
            handlers[demo.signal.SIGHUP](demo.signal.SIGHUP, None)

    def test_child_cleanup_terminates_owned_child(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"])
        controller = demo.ChildController()
        controller.add(child)
        controller.cleanup()
        self.assertIsNotNone(child.wait(timeout=2))

    def test_credential_metadata_requires_exact_https_origin(self):
        with tempfile.TemporaryDirectory() as directory:
            credentials = Path(directory) / "credentials.json"
            payload = {"version": 1, "vaultId": "vault", "deviceId": "device", "deviceToken": "token"}
            for origin, expected in (("https://127.0.0.1", True), ("http://127.0.0.1", False), ("https://localhost", False), ("https://127.0.0.1/path", False)):
                credentials.write_text(json.dumps(payload | {"origin": origin}))
                credentials.chmod(0o600)
                _, valid = demo.credential_metadata(credentials, "https://127.0.0.1")
                self.assertEqual(valid, expected, origin)

    def test_tls_context_trusts_only_the_configured_ca_and_hostname(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tls = root / "tls"
            dotenv = root / ".env"
            dotenv.write_text("DEBUG=true\n")
            demo.prepare_demo_tls(dotenv, tls)

            class ReadyHandler(BaseHTTPRequestHandler):
                def do_GET(self):
                    self.send_response(200)
                    self.end_headers()

                def log_message(self, *_args):
                    pass

            server = ThreadingHTTPServer(("127.0.0.1", 0), ReadyHandler)
            server.socket = demo.ssl.SSLContext(demo.ssl.PROTOCOL_TLS_SERVER).wrap_socket(server.socket, server_side=True)
            server.socket.context.load_cert_chain(tls / "server.pem", tls / "server-key.pem")
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                url = f"https://127.0.0.1:{server.server_port}"
                with urlopen(url, context=demo.tls_context(tls / "ca.pem"), timeout=2) as response:
                    self.assertEqual(response.status, 200)
                wrong_tls = root / "wrong-tls"
                demo.prepare_demo_tls(dotenv, wrong_tls)
                with self.assertRaises(URLError) as failure:
                    urlopen(url, context=demo.tls_context(wrong_tls / "ca.pem"), timeout=2)
                self.assertIsInstance(failure.exception.reason, demo.ssl.SSLCertVerificationError)
                with socket.create_connection(("127.0.0.1", server.server_port)) as connection:
                    with self.assertRaises(demo.ssl.SSLCertVerificationError):
                        demo.tls_context(tls / "ca.pem").wrap_socket(connection, server_hostname="wrong.test")
            finally:
                server.shutdown()
                server.server_close()
                thread.join()

    def test_demo_tls_overlay_keeps_private_keys_out_of_client_and_no_host_ports(self):
        compose = (ROOT / "infra/compose/compose.demo-tls.yml").read_text()
        caddyfile = (ROOT / "infra/proxy/demo.Caddyfile").read_text()
        self.assertIn("network_mode: service:dev", compose)
        self.assertIn("condition: service_started", compose)
        self.assertIn("/tls/ca.pem:ro", compose)
        self.assertNotIn("ca-key.pem", compose)
        dev = compose.split("  caddy:", 1)[0]
        self.assertNotIn("server-key.pem", dev)
        self.assertIn("ports: !reset []", dev)
        self.assertIn("PEPPY_DEMO_RUN: ${PEPPY_DEMO_RUN:?demo run directory is required}", dev)
        self.assertIn("admin off", caddyfile)

    def test_readiness_fails_immediately_for_certificate_verification_errors(self):
        error = URLError(demo.ssl.SSLCertVerificationError("wrong CA"))
        with mock.patch.object(demo, "urlopen", side_effect=error), self.assertRaisesRegex(RuntimeError, "TLS certificate verification failed") as failure:
            demo.wait_for_ready("https://127.0.0.1/readyz", demo.ssl.create_default_context())
        self.assertIs(failure.exception.__cause__, error)

    def test_readiness_retries_connection_failures_until_timeout(self):
        error = URLError(ConnectionRefusedError("not running"))
        with mock.patch.object(demo, "urlopen", side_effect=error), mock.patch.object(demo.time, "monotonic", side_effect=(0, 0, 1)), mock.patch.object(demo.time, "sleep") as sleep:
            with self.assertRaisesRegex(RuntimeError, "did not become ready"):
                demo.wait_for_ready("https://127.0.0.1/readyz", demo.ssl.create_default_context(), timeout=1)
        sleep.assert_called_once()

    def test_replay_rejects_unexpected_credential_origin_before_request(self):
        credential = {"origin": "https://wrong.test", "deviceToken": "token"}
        with mock.patch.object(demo, "urlopen") as request:
            with self.assertRaisesRegex(RuntimeError, "does not match expected public API URL"):
                demo.replay_events(credential, "https://127.0.0.1", demo.ssl.create_default_context())
        request.assert_not_called()
