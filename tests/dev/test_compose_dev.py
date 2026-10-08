import json
import os
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class ComposeDevelopmentTests(unittest.TestCase):
    compose = shutil.which("docker")

    @staticmethod
    def synthetic_env(**overrides):
        values = {
            "POSTGRES_USER": "peppy",
            "POSTGRES_PASSWORD": "synthetic-password",
            "POSTGRES_DB": "peppy",
            "S3_ACCESS_KEY": "synthetic-access",
            "S3_SECRET_KEY": "synthetic-secret",
            "PUBLIC_API_URL": "http://127.0.0.1:7000",
            "PUBLIC_ATTACHMENT_URL": "http://127.0.0.1:7000",
        }
        inherited = {
            key: value for key, value in os.environ.items()
            if key not in {'BIND_ADDR', 'API_HOST_PORT', 'PUBLIC_API_URL', 'PUBLIC_ATTACHMENT_URL', 'WEB_UI_ENABLED', 'PEPPY_REPLAY_RETENTION_DAYS', 'VAULT_ATTACHMENT_QUOTA_BYTES'}
            and not key.startswith('COMPOSE_')
        }
        return inherited | values | overrides

    @staticmethod
    def fixture_env(**overrides):
        values = os.environ.copy()
        for name in ("API_HOST_PORT", "PUBLIC_API_URL", "PUBLIC_ATTACHMENT_URL", "WEB_UI_ENABLED"):
            values.pop(name, None)
        return values | overrides

    def render(self, *files, profile=None, **env):
        command = ["docker", "compose", "--env-file", "/dev/null"]
        for file in files:
            command.extend(["-f", str(ROOT / file)])
        if profile:
            command.extend(["--profile", profile])
        command.extend(["config", "--format", "json"])
        return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, env=self.synthetic_env(**env))

    def assert_root_source_mount(self, service):
        source_mount = next(volume for volume in service["volumes"] if volume["target"] == "/source")
        self.assertEqual(source_mount["type"], "bind")
        self.assertEqual(source_mount["source"], str(ROOT))
        self.assertTrue(source_mount["read_only"])

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_dev_overlay_renders_consolidated_runtime_with_defaults(self):
        with mock.patch.dict(os.environ, {
            'BIND_ADDR': 'hostile-bind', 'API_HOST_PORT': '29999',
            'PEPPY_REPLAY_RETENTION_DAYS': '91', 'VAULT_ATTACHMENT_QUOTA_BYTES': '44',
            'COMPOSE_PROJECT_NAME': 'hostile-project',
        }):
            result = self.render("docker-compose.yml", "infra/compose/compose.dev.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        self.assertEqual(set(config["services"]), {"postgres", "seaweedfs", "web", "dev"})
        web = config["services"]["web"]
        self.assertEqual(web["build"]["dockerfile"], "infra/docker/server.Dockerfile")
        self.assertEqual(web["build"]["target"], "web-assets")
        self.assertEqual(web["volumes"], [{"type": "volume", "source": "dev-web", "target": "/output", "volume": {}}])
        dev = config["services"]["dev"]
        self.assertEqual(dev["build"]["context"], str(ROOT))
        self.assertEqual(dev["build"]["dockerfile"], "infra/docker/development.Dockerfile")
        self.assert_root_source_mount(dev)
        self.assertEqual(dev["command"], ["run", "serve"])
        self.assertEqual(dev["environment"]["BIND_ADDR"], "0.0.0.0:8080")
        self.assertEqual(dev["environment"]["PEPPY_WEB_CLIENT_ROOT"], "true")
        self.assertEqual(dev["environment"]["PEPPY_WEB_CLIENT_DIR"], "/web")
        self.assertEqual(dev["ports"], [{"mode": "ingress", "host_ip": "127.0.0.1", "target": 8080, "published": "7000", "protocol": "tcp"}])
        self.assertEqual(dev["depends_on"], {
            "postgres": {"condition": "service_healthy", "required": True},
            "seaweedfs": {"condition": "service_healthy", "required": True},
            "web": {"condition": "service_completed_successfully", "required": True},
        })
        web_mount = next(volume for volume in dev["volumes"] if volume["target"] == "/web")
        self.assertEqual(web_mount["type"], "volume")
        self.assertEqual(web_mount["source"], "dev-web")
        self.assertTrue(web_mount["read_only"])
        self.assertEqual(dev["healthcheck"]["test"], ["CMD", "/home/developer/.local/lib/peppy/peppy-server", "healthcheck"])
        self.assertEqual(dev["healthcheck"]["start_period"], "30m0s")
        self.assertEqual(dev["environment"]["PEPPY_REPLAY_RETENTION_DAYS"], "30")
        self.assertEqual(dev["environment"]["VAULT_ATTACHMENT_QUOTA_BYTES"], "536870912")
        android = self.render("docker-compose.yml", "infra/compose/compose.dev.yml", profile="android")
        self.assertEqual(android.returncode, 0, android.stderr)
        android_service = json.loads(android.stdout)["services"]["android"]
        self.assertEqual(android_service["build"]["context"], str(ROOT))
        self.assertEqual(android_service["build"]["dockerfile"], "infra/docker/android.Dockerfile")
        self.assert_root_source_mount(android_service)
        self.assertEqual(android_service["profiles"], ["android"])
        self.assertEqual(android_service["platform"], "linux/amd64")
        self.assertNotIn("ports", android_service)

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_dev_overlay_honors_bind_and_server_environment_overrides(self):
        result = self.render(
            "docker-compose.yml", "infra/compose/compose.dev.yml",
            BIND_ADDR="0.0.0.0:49152", API_HOST_PORT="17000",
            PEPPY_REPLAY_RETENTION_DAYS="90", VAULT_ATTACHMENT_QUOTA_BYTES="1234",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        dev = json.loads(result.stdout)["services"]["dev"]
        self.assertEqual(dev["environment"]["BIND_ADDR"], "0.0.0.0:49152")
        self.assertEqual(dev["environment"]["PEPPY_REPLAY_RETENTION_DAYS"], "90")
        self.assertEqual(dev["environment"]["VAULT_ATTACHMENT_QUOTA_BYTES"], "1234")
        self.assertEqual(dev["ports"][0]["published"], "17000")

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_dev_overlay_can_disable_root_web_serving(self):
        result = self.render("docker-compose.yml", "infra/compose/compose.dev.yml", WEB_UI_ENABLED="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["services"]["dev"]["environment"]["PEPPY_WEB_CLIENT_ROOT"], "false")

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_base_production_service_set_is_unchanged(self):
        result = self.render("docker-compose.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(json.loads(result.stdout)["services"]), {"postgres", "seaweedfs", "migrate", "api"})

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_community_overlay_rendered_with_server_image(self):
        server_image = f"example.invalid/peppy@sha256:{'a' * 64}"
        result = self.render(
            "docker-compose.yml", "infra/compose/compose.community.yml",
            PEPPY_SERVER_IMAGE=server_image
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        self.assertEqual(config["name"], "peppy")
        api = config["services"]["api"]
        self.assertEqual(api["image"], server_image)
        self.assertNotIn("build", api)
        migrate = config["services"]["migrate"]
        self.assertEqual(migrate["image"], server_image)
        self.assertNotIn("build", migrate)
        self.assertEqual(migrate["restart"], "no")
        for name in ("postgres", "seaweedfs", "api"):
            self.assertEqual(config["services"][name].get("restart"), "unless-stopped", name)

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_caddy_overlay_mount_and_public_host(self):
        result = self.render(
            "docker-compose.yml", "infra/compose/compose.caddy.yml",
            PUBLIC_HOST="peppy.invalid"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        self.assertEqual(config["name"], "peppy")
        caddy = config["services"]["caddy"]
        caddyfile_mount = next(
            (v for v in caddy["volumes"] if v["target"] == "/etc/caddy/Caddyfile.template"),
            None
        )
        self.assertIsNotNone(caddyfile_mount, "Caddyfile mount not found")
        self.assertEqual(caddyfile_mount["type"], "bind")
        self.assertEqual(caddyfile_mount["source"], str(ROOT / "infra/proxy/Caddyfile"))
        self.assertTrue(caddyfile_mount["read_only"])
        entrypoint_mount = next(
            (v for v in caddy["volumes"] if v["target"] == "/usr/local/bin/peppy-caddy"),
            None
        )
        self.assertIsNotNone(entrypoint_mount, "Caddy entrypoint mount not found")
        self.assertEqual(entrypoint_mount["source"], str(ROOT / "infra/proxy/caddy-entrypoint.sh"))
        self.assertTrue(entrypoint_mount["read_only"])
        self.assertEqual(caddy["entrypoint"], ["/usr/local/bin/peppy-caddy"])
        self.assertEqual(caddy["environment"]["WEB_CLIENT_HOST"], "")

    def test_dev_recipes_clean_legacy_services_and_use_one_off_helpers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy(ROOT / "justfile", root / "justfile")
            (root / "infra/dev").mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/dev_port.py", root / "infra/dev/dev_port.py")
            (root / ".env").write_text("synthetic=1\n")
            tools = root / "tools"
            tools.mkdir()
            log = root / "docker.log"
            docker = tools / "docker"
            docker.write_text(
                "#!/bin/sh\n"
                "printf '%s\\n' \"$*\" >> \"$PEPPY_DOCKER_LOG\"\n"
                "case \"$*\" in *' build web dev'*) exit \"${BUILD_EXIT:-0}\";; esac\n"
                "case \"$*\" in *' rm --stop --force api migrate'*) exit \"${RM_EXIT:-0}\";; esac\n"
                "case \"$*\" in *' run --rm --no-deps dev run build server'*) exit \"${RUN_EXIT:-0}\";; esac\n"
            )
            docker.chmod(0o755)
            env = self.fixture_env(PATH=f"{tools}:{os.environ['PATH']}", PEPPY_DOCKER_LOG=str(log))

            for recipe in ("dev-up", "dev-down", "dev-build", "dev-test"):
                result = subprocess.run(["just", recipe], cwd=root, text=True, capture_output=True, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)

            calls = log.read_text().splitlines()
            rm = "compose --env-file .env -f docker-compose.yml rm --stop --force api migrate"
            dev_rm = "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml rm --stop --force web dev"
            build = "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml build web dev"
            dev_up = "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml up --detach --wait --wait-timeout 1800 --force-recreate dev"
            self.assertLess(calls.index(build), calls.index(rm))
            self.assertLess(calls.index(rm), calls.index(dev_rm))
            self.assertLess(calls.index(dev_rm), calls.index(dev_up))
            self.assertFalse(any(call.endswith(" web") and " up " in call for call in calls))
            self.assertEqual(calls.count(rm), 2)
            down = "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml down"
            self.assertLess(calls.index(rm, calls.index(rm) + 1), calls.index(down))
            self.assertIn("compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml run --rm --no-deps dev run build server", calls)
            self.assertIn("compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml run --rm --no-deps dev run test rust", calls)

            log.unlink()
            failure = subprocess.run(["just", "dev-up"], cwd=root, text=True, capture_output=True, env=env | {"RM_EXIT": "1"})
            self.assertNotEqual(failure.returncode, 0)
            self.assertEqual(log.read_text().splitlines()[-1], rm)

            log.unlink()
            failure = subprocess.run(["just", "dev-up"], cwd=root, text=True, capture_output=True, env=env | {"BUILD_EXIT": "1"})
            self.assertNotEqual(failure.returncode, 0)
            self.assertEqual(log.read_text().splitlines()[-1], build)
            self.assertFalse(any(" rm --stop --force" in call for call in log.read_text().splitlines()))

            log.unlink()
            failure = subprocess.run(["just", "dev-build"], cwd=root, text=True, capture_output=True, env=env | {"RUN_EXIT": "1"})
            self.assertNotEqual(failure.returncode, 0)
            self.assertEqual(log.read_text().splitlines()[-1], "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml run --rm --no-deps dev run build server")

    def test_dev_up_rejects_malformed_web_ui_before_service_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy(ROOT / "justfile", root / "justfile")
            helper_directory = root / "infra/dev"
            helper_directory.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/dev_port.py", helper_directory / "dev_port.py")
            (root / ".env").write_text("WEB_UI_ENABLED=maybe\n")
            tools = root / "tools"
            tools.mkdir()
            log = root / "docker.log"
            docker = tools / "docker"
            docker.write_text(
                "#!/bin/sh\n"
                "case \"$*\" in *'compose version'*) exit 0;; esac\n"
                "printf '%s\\n' \"$*\" >> \"$PEPPY_DOCKER_LOG\"\n"
            )
            docker.chmod(0o755)
            result = subprocess.run(
                ["just", "dev-up"],
                cwd=root,
                text=True,
                capture_output=True,
                env=self.fixture_env(PATH=f"{tools}:{os.environ['PATH']}", PEPPY_DOCKER_LOG=str(log)),
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("WEB_UI_ENABLED must be true, false, 1, or 0", result.stderr)
            self.assertFalse(log.exists())

    def test_smoke_infra_uses_the_resolved_port(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy(ROOT / "justfile", root / "justfile")
            helper_directory = root / "infra/dev"
            helper_directory.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/dev_port.py", helper_directory / "dev_port.py")
            (root / ".env").write_text(
                "API_HOST_PORT=7100\nPUBLIC_API_URL=http://127.0.0.1:7100\nPUBLIC_ATTACHMENT_URL=http://127.0.0.1:7100\n"
            )
            tools = root / "tools"
            tools.mkdir()
            docker_log = root / "docker.log"
            curl_log = root / "curl.log"
            docker = tools / "docker"
            docker.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PEPPY_DOCKER_LOG\"\n")
            curl = tools / "curl"
            curl.write_text("#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$PEPPY_CURL_LOG\"\n")
            docker.chmod(0o755)
            curl.chmod(0o755)
            result = subprocess.run(
                ["just", "smoke-infra"],
                cwd=root,
                text=True,
                capture_output=True,
                env=self.fixture_env(
                    PATH=f"{tools}:{os.environ['PATH']}",
                    PEPPY_DOCKER_LOG=str(docker_log),
                    PEPPY_CURL_LOG=str(curl_log),
                ),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("http://127.0.0.1:7100/healthz", curl_log.read_text())
            self.assertIn("http://127.0.0.1:7100/readyz", curl_log.read_text())
