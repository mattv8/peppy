import os
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "infra/dev/dev_port.py"


class DevelopmentPortTests(unittest.TestCase):
    def run_helper(self, command, dotenv, **environment):
        env = os.environ.copy()
        for name in ("API_HOST_PORT", "PUBLIC_API_URL", "PUBLIC_ATTACHMENT_URL", "WEB_UI_ENABLED"):
            env.pop(name, None)
        env.update(environment)
        return subprocess.run(
            ["python3", str(HELPER), command, str(dotenv)],
            text=True,
            capture_output=True,
            env=env,
        )

    def test_environment_port_overrides_dotenv_and_sets_smoke_url(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("API_HOST_PORT=7000\nPUBLIC_API_URL=https://example.test\n")
            result = self.run_helper("smoke-url", dotenv, API_HOST_PORT="7100")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "http://127.0.0.1:7100\n")

    def test_external_public_urls_do_not_block_validation(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("API_HOST_PORT=7100\nPUBLIC_API_URL=https://api.example.test\nPUBLIC_ATTACHMENT_URL=https://files.example.test\n")
            result = self.run_helper("validate", dotenv)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_mismatched_loopback_public_url(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("API_HOST_PORT=7100\nPUBLIC_API_URL=http://127.0.0.1:7000\n")
            result = self.run_helper("validate", dotenv)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("PUBLIC_API_URL uses loopback port 7000", result.stderr)

    def test_write_setup_uses_selected_port_and_preserves_external_urls(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            result = self.run_helper(
                "write-setup",
                dotenv,
                API_HOST_PORT="7100",
                PUBLIC_API_URL="https://api.example.test",
                PUBLIC_ATTACHMENT_URL="https://files.example.test",
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            values = dotenv.read_text()
            self.assertIn("API_HOST_PORT=7100\n", values)
            self.assertIn("PUBLIC_API_URL=https://api.example.test\n", values)
            self.assertIn("PUBLIC_ATTACHMENT_URL=https://files.example.test\n", values)
            self.assertEqual(dotenv.stat().st_mode & 0o777, 0o600)

    def test_dotenv_values_are_never_evaluated_as_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            sentinel = Path(directory) / "sentinel"
            dotenv.write_text(f"API_HOST_PORT=7100\nUNTRUSTED=$(touch {sentinel})\n")
            result = self.run_helper("validate", dotenv)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(sentinel.exists())

    def test_dotenv_matches_compose_for_comments_quotes_and_defaults(self):
        if not shutil.which("docker"):
            self.skipTest("Docker Compose is unavailable")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dotenv = root / ".env"
            dotenv.write_text(
                "PORT=7100 # local development port\n"
                "API_HOST_PORT=${PORT:-7000}\n"
                "PUBLIC_API_URL=\"https://${API_HOST:-api.example.test}:7443\" # browser origin\n"
                "PUBLIC_ATTACHMENT_URL=${PUBLIC_API_URL:-https://files.example.test}\n"
            )
            compose_file = root / "compose.yml"
            compose_file.write_text(
                "services:\n"
                "  test:\n"
                "    image: busybox\n"
                "    environment:\n"
                "      API_HOST_PORT: ${API_HOST_PORT}\n"
                "      PUBLIC_API_URL: ${PUBLIC_API_URL}\n"
                "      PUBLIC_ATTACHMENT_URL: ${PUBLIC_ATTACHMENT_URL}\n"
            )
            environment = os.environ.copy()
            for name in ("API_HOST_PORT", "PUBLIC_API_URL", "PUBLIC_ATTACHMENT_URL", "WEB_UI_ENABLED", "API_HOST"):
                environment.pop(name, None)
            result = subprocess.run(
                ["docker", "compose", "--env-file", str(dotenv), "-f", str(compose_file), "config", "--format", "json"],
                cwd=root,
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            rendered = json.loads(result.stdout)["services"]["test"]["environment"]
            self.assertEqual(rendered["API_HOST_PORT"], "7100")
            self.assertEqual(rendered["PUBLIC_API_URL"], "https://api.example.test:7443")
            self.assertEqual(rendered["PUBLIC_ATTACHMENT_URL"], "https://api.example.test:7443")
            self.assertEqual(self.run_helper("port", dotenv).stdout, "7100\n")
            self.assertEqual(self.run_helper("public-api-origin", dotenv).stdout, "https://api.example.test:7443\n")

    def test_rejects_unsupported_dotenv_interpolation(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("API_HOST_PORT=${PORT:?set PORT}\n")
            result = self.run_helper("validate", dotenv)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unsupported dotenv interpolation", result.stderr)

    def test_loopback_url_without_an_explicit_port_uses_scheme_default(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("API_HOST_PORT=7000\nPUBLIC_API_URL=http://127.0.0.1\n")
            result = self.run_helper("validate", dotenv)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("PUBLIC_API_URL uses loopback port 80", result.stderr)

    def test_web_ui_enabled_reads_dotenv_as_data(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("WEB_UI_ENABLED=false\n")
            result = self.run_helper("web-enabled", dotenv)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, "false\n")

    def test_web_ui_enabled_matches_runtime_boolean_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            for contents, expected in (("", "true"), ("WEB_UI_ENABLED=true\n", "true"), ("WEB_UI_ENABLED=1\n", "true"), ("WEB_UI_ENABLED=false\n", "false"), ("WEB_UI_ENABLED=0\n", "false")):
                with self.subTest(contents=contents):
                    dotenv.write_text(contents)
                    result = self.run_helper("web-enabled", dotenv)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, f"{expected}\n")

    def test_rejects_malformed_web_ui_boolean_before_startup(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("WEB_UI_ENABLED=maybe\n")
            result = self.run_helper("validate", dotenv)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("WEB_UI_ENABLED must be true, false, 1, or 0", result.stderr)
