import json
import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class CommunityWebComposeTests(unittest.TestCase):
    docker = shutil.which("docker")

    @staticmethod
    def environment(**overrides):
        required = {
            "POSTGRES_DB": "peppy",
            "POSTGRES_USER": "peppy",
            "POSTGRES_PASSWORD": "test-password",
            "S3_ACCESS_KEY": "test-access",
            "S3_SECRET_KEY": "test-secret",
            "PUBLIC_API_URL": "https://peppy.invalid",
            "PUBLIC_ATTACHMENT_URL": "https://peppy.invalid",
            "PEPPY_SERVER_IMAGE": f"example.invalid/peppy@sha256:{'a' * 64}",
        }
        return os.environ | required | overrides

    def render(self, *files, **environment):
        command = ["docker", "compose", "--env-file", "/dev/null"]
        for file in files:
            command.extend(("-f", str(ROOT / file)))
        command.extend(("config", "--format", "json"))
        return subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            env=self.environment(**environment),
        )

    @unittest.skipUnless(docker, "Docker Compose is unavailable")
    def test_base_root_mode_defaults_to_enabled_without_legacy_server_host(self):
        result = self.render("docker-compose.yml", WEB_CLIENT_HOST="app.peppy.invalid")
        self.assertEqual(result.returncode, 0, result.stderr)
        services = json.loads(result.stdout)["services"]
        self.assertEqual(services["api"]["environment"]["PEPPY_WEB_CLIENT_ROOT"], "true")
        self.assertNotIn("PEPPY_WEB_CLIENT_HOST", services["api"]["environment"])
        self.assertNotIn("PEPPY_WEB_CLIENT_ROOT", services["migrate"]["environment"])

    @unittest.skipUnless(docker, "Docker Compose is unavailable")
    def test_community_root_mode_ignores_legacy_server_host(self):
        result = self.render(
            "docker-compose.yml",
            "infra/compose/compose.community.yml",
            WEB_CLIENT_HOST="app.peppy.invalid",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        services = json.loads(result.stdout)["services"]
        self.assertEqual(services["api"]["environment"]["PEPPY_WEB_CLIENT_ROOT"], "true")
        self.assertNotIn("PEPPY_WEB_CLIENT_HOST", services["api"]["environment"])
        self.assertNotIn("PEPPY_WEB_CLIENT_ROOT", services["migrate"]["environment"])

    @unittest.skipUnless(docker, "Docker Compose is unavailable")
    def test_community_ui_can_be_disabled_without_affecting_migrations(self):
        result = self.render(
            "docker-compose.yml",
            "infra/compose/compose.community.yml",
            WEB_UI_ENABLED="false",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        services = json.loads(result.stdout)["services"]
        self.assertEqual(services["api"]["environment"]["PEPPY_WEB_CLIENT_ROOT"], "false")
        self.assertNotIn("PEPPY_WEB_CLIENT_ROOT", services["migrate"]["environment"])

    @unittest.skipUnless(docker, "Docker Compose is unavailable")
    def test_community_caddy_overlay_keeps_legacy_value_only_for_retirement_warning(self):
        result = self.render(
            "docker-compose.yml",
            "infra/compose/compose.community.yml",
            "infra/compose/compose.caddy.yml",
            PUBLIC_HOST="peppy.invalid",
            WEB_CLIENT_HOST="app.peppy.invalid",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        caddy = json.loads(result.stdout)["services"]["caddy"]
        self.assertEqual(caddy["environment"]["WEB_CLIENT_HOST"], "app.peppy.invalid")


class CaddyEntrypointTests(unittest.TestCase):
    def run_entrypoint(self, environment):
        sed_path = shutil.which("sed")
        self.assertIsNotNone(sed_path, "sed is required for this test")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "Caddyfile"
            tools = root / "tools"
            tools.mkdir()
            template = ROOT / "infra/proxy/Caddyfile"
            (tools / "mktemp").write_text(
                f"#!/bin/sh\nprintf '%s\\n' {shlex.quote(str(output))}\n"
            )
            (tools / "sed").write_text(
                "#!/bin/sh\n"
                "set -eu\n"
                "if [ \"$2\" = /etc/caddy/Caddyfile.template ]; then\n"
                f"    set -- \"$1\" {shlex.quote(str(template))}\n"
                "fi\n"
                f"exec {shlex.quote(sed_path)} \"$@\"\n"
            )
            (tools / "caddy").write_text("#!/bin/sh\ncat \"$3\"\n")
            for tool in tools.iterdir():
                tool.chmod(0o755)

            result = subprocess.run(
                [str(ROOT / "infra/proxy/caddy-entrypoint.sh"), "validate"],
                capture_output=True,
                text=True,
                env=os.environ | {
                    "PATH": f"{tools}:{os.environ['PATH']}",
                    "PUBLIC_HOST": "peppy.invalid",
                } | environment,
            )
        return result

    def test_legacy_host_warns_but_generated_site_uses_public_host_only(self):
        result = self.run_entrypoint({"WEB_CLIENT_HOST": "app.peppy.invalid"})

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("WEB_CLIENT_HOST is retired", result.stderr)
        self.assertIn("peppy.invalid {", result.stdout)
        self.assertNotIn("app.peppy.invalid", result.stdout)

    def test_absent_legacy_host_does_not_warn(self):
        result = self.run_entrypoint({})

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("WEB_CLIENT_HOST is retired", result.stderr)
