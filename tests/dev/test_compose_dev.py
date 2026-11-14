import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path
from urllib.parse import urlsplit

from infra.dev import dev_tls


ROOT = Path(__file__).resolve().parents[2]

# Synthetic fixture credentials only. They mirror the documented .env.example role
# names but are distinct from the owner role so DSN tests prove role selection.
RUNTIME_ROLE = "peppy_runtime_fixture"
RUNTIME_PASSWORD = "synthetic-runtime-password"
MAINTENANCE_ROLE = "peppy_maintenance_fixture"
MAINTENANCE_PASSWORD = "synthetic-maintenance-password"
PASSWORD_PREFLIGHT_TARGET = "/usr/local/bin/validate-db-passwords" + ".sh"
BOOTSTRAP_ROLES_TARGET = "/usr/local/bin/peppy-bootstrap-roles" + ".sh"


class ComposeDevelopmentTests(unittest.TestCase):
    compose = shutil.which("docker")

    @staticmethod
    def synthetic_env(**overrides):
        values = {
            "PEPPY_SERVER_IMAGE": "peppy-server:local",
            "POSTGRES_USER": "peppy",
            "POSTGRES_PASSWORD": "synthetic-password",
            "POSTGRES_DB": "peppy",
            "PEPPY_RUNTIME_DB_USER": RUNTIME_ROLE,
            "PEPPY_RUNTIME_DB_PASSWORD": RUNTIME_PASSWORD,
            "PEPPY_MAINTENANCE_DB_USER": MAINTENANCE_ROLE,
            "PEPPY_MAINTENANCE_DB_PASSWORD": MAINTENANCE_PASSWORD,
            "S3_ACCESS_KEY": "synthetic-access",
            "S3_SECRET_KEY": "synthetic-secret",
            "PUBLIC_API_URL": "http://127.0.0.1:7000",
        }
        inherited = {
            key: value for key, value in os.environ.items()
            if key not in {
                'BIND_ADDR', 'API_HOST_PORT', 'PUBLIC_API_URL', 'WEB_UI_ENABLED', 'DEBUG',
                'PEPPY_REPLAY_RETENTION_DAYS', 'VAULT_ATTACHMENT_QUOTA_BYTES',
                'TRUSTED_PROXY_CIDRS',
            }
            and not key.startswith('COMPOSE_')
        }
        return inherited | values | overrides

    @staticmethod
    def fixture_env(**overrides):
        values = os.environ.copy()
        for name in ("API_HOST_PORT", "PUBLIC_API_URL", "WEB_UI_ENABLED", "DEBUG"):
            values.pop(name, None)
        return values | overrides

    @staticmethod
    def install_migration_controller(root):
        controller = root / "infra/dev/migration_repair.py"
        controller.write_text(
            "import subprocess, sys\n"
            "recipes = ('_dev-up-build', '_dev-up-stop-writers', '_dev-up-start') if sys.argv[1] == 'dev-up' else ('_dev-down-raw',)\n"
            "for recipe in recipes: subprocess.run(['just', recipe], check=True)\n"
        )

    def render(self, *files, profile=None, **env):
        command = ["docker", "compose", "--env-file", "/dev/null"]
        for file in files:
            command.extend(["-f", str(ROOT / file)])
        if profile:
            command.extend(["--profile", profile])
        command.extend(["config", "--format", "json"])
        return subprocess.run(command, cwd=ROOT, text=True, capture_output=True, env=self.synthetic_env(**env))

    def render_with_required_environment(self, **overrides):
        required = {
            "PUBLIC_HOST": "peppy.invalid",
            "POSTGRES_PASSWORD": "synthetic-password",
            "PEPPY_RUNTIME_DB_PASSWORD": RUNTIME_PASSWORD,
            "PEPPY_MAINTENANCE_DB_PASSWORD": MAINTENANCE_PASSWORD,
            "S3_SECRET_KEY": "synthetic-secret",
        }
        environment = os.environ.copy()
        for name in {
            "PEPPY_SERVER_IMAGE", "POSTGRES_DB", "POSTGRES_USER", "POSTGRES_PASSWORD",
            "PEPPY_RUNTIME_DB_USER", "PEPPY_RUNTIME_DB_PASSWORD",
            "PEPPY_MAINTENANCE_DB_USER", "PEPPY_MAINTENANCE_DB_PASSWORD",
            "S3_ACCESS_KEY", "S3_SECRET_KEY", "PUBLIC_HOST", "PUBLIC_API_URL",
        }:
            environment.pop(name, None)
        return subprocess.run(
            ["docker", "compose", "--env-file", "/dev/null", "-f", str(ROOT / "docker-compose.yml"), "config", "--format", "json"],
            cwd=ROOT,
            text=True,
            capture_output=True,
            env=environment | required | overrides,
        )

    def assert_source_mount(self, service, source=ROOT):
        source_mount = next(volume for volume in service["volumes"] if volume["target"] == "/source")
        self.assertEqual(source_mount["type"], "bind")
        self.assertEqual(source_mount["source"], str(source))
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
        self.assertEqual(
            set(config["services"]),
            {"database-password-preflight", "postgres", "seaweedfs", "web", "dev"},
        )
        web = config["services"]["web"]
        self.assertEqual(web["build"]["dockerfile"], "infra/docker/server.Dockerfile")
        self.assertEqual(web["build"]["target"], "web-assets")
        self.assertEqual(web["volumes"], [{"type": "volume", "source": "dev-web", "target": "/output", "volume": {}}])
        dev = config["services"]["dev"]
        self.assertEqual(dev["build"]["context"], str(ROOT))
        self.assertEqual(dev["build"]["dockerfile"], "infra/docker/development.Dockerfile")
        self.assert_source_mount(dev)
        self.assertEqual(dev["command"], ["run", "serve"])
        self.assertEqual(dev["environment"]["BIND_ADDR"], "0.0.0.0:8080")
        self.assertEqual(dev["environment"]["PEPPY_WEB_CLIENT_ROOT"], "true")
        self.assertEqual(dev["environment"]["PEPPY_WEB_CLIENT_DIR"], "/web")
        self.assertEqual(dev["ports"], [{"mode": "ingress", "host_ip": "127.0.0.1", "target": 8080, "published": "7000", "protocol": "tcp"}])
        self.assertEqual(dev["depends_on"], {
            "database-password-preflight": {
                "condition": "service_completed_successfully",
                "required": True,
            },
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
        self.assert_source_mount(android_service)
        self.assertEqual(android_service["profiles"], ["android"])
        self.assertEqual(android_service["platform"], "linux/amd64")
        self.assertNotIn("ports", android_service)

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_dev_overlay_selects_an_absolute_source_tree_without_container_environment_leakage(self):
        with tempfile.TemporaryDirectory(prefix="selected source ") as directory:
            source = Path(directory).resolve()
            result = self.render(
                "docker-compose.yml", "infra/compose/compose.dev.yml", PEPPY_SOURCE_TREE=str(source)
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        for name in ("web", "dev"):
            self.assertEqual(config["services"][name]["build"]["context"], str(source))
        self.assert_source_mount(config["services"]["dev"], source)
        self.assertNotIn("PEPPY_SOURCE_TREE", config["services"]["dev"].get("environment", {}))

        android = self.render(
            "docker-compose.yml", "infra/compose/compose.dev.yml", profile="android", PEPPY_SOURCE_TREE=str(source)
        )
        self.assertEqual(android.returncode, 0, android.stderr)
        android_service = json.loads(android.stdout)["services"]["android"]
        self.assertEqual(android_service["build"]["context"], str(source))
        self.assert_source_mount(android_service, source)
        self.assertNotIn("PEPPY_SOURCE_TREE", android_service.get("environment", {}))

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_fresh_dev_setup_environment_renders_all_database_roles(self):
        generated_names = {
            "PEPPY_ENV",
            "POSTGRES_DB",
            "POSTGRES_USER",
            "POSTGRES_PASSWORD",
            "PEPPY_RUNTIME_DB_USER",
            "PEPPY_RUNTIME_DB_PASSWORD",
            "PEPPY_MAINTENANCE_DB_USER",
            "PEPPY_MAINTENANCE_DB_PASSWORD",
            "S3_ACCESS_KEY",
            "S3_SECRET_KEY",
            "S3_BUCKET",
            "PEPPY_REPLAY_RETENTION_DAYS",
            "VAULT_ATTACHMENT_QUOTA_BYTES",
            "API_HOST_PORT",
            "PUBLIC_API_URL",
        }
        environment = {
            key: value for key, value in os.environ.items()
            if key not in generated_names and not key.startswith("COMPOSE_")
        }
        environment["PEPPY_SERVER_IMAGE"] = "peppy-server:local"
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            setup = subprocess.run(
                [sys.executable, str(ROOT / "infra/dev/dev_port.py"), "write-setup", str(dotenv)],
                text=True,
                capture_output=True,
                env=environment,
            )
            self.assertEqual(setup.returncode, 0, setup.stderr)
            rendered = subprocess.run(
                [
                    "docker", "compose", "--env-file", str(dotenv),
                    "-f", str(ROOT / "docker-compose.yml"),
                    "-f", str(ROOT / "infra/compose/compose.dev.yml"),
                    "config", "--format", "json",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                env=environment,
            )
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        values = json.loads(rendered.stdout)["services"]["dev"]["environment"]
        runtime = self.database_credentials(values["DATABASE_URL"])
        maintenance = self.database_credentials(values["MAINTENANCE_DATABASE_URL"])
        owner = self.database_credentials(values["DATABASE_MIGRATION_URL"])
        self.assertNotEqual(owner[:2], runtime[:2])
        self.assertNotEqual(owner[:2], maintenance[:2])
        self.assertNotEqual(runtime[:2], maintenance[:2])

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
        result = self.render(
            "docker-compose.yml", "infra/compose/compose.dev.yml",
            WEB_UI_ENABLED="false"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["services"]["dev"]["environment"]["PEPPY_WEB_CLIENT_ROOT"], "false")

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_setup_token_is_optional_and_forwarded_to_server_services(self):
        for value in ("", "synthetic-setup-token-at-least-32-bytes"):
            base = self.render("docker-compose.yml", PEPPY_SETUP_TOKEN=value)
            self.assertEqual(base.returncode, 0, base.stderr)
            api = json.loads(base.stdout)["services"]["api"]
            self.assertEqual(api["environment"]["PEPPY_SETUP_TOKEN"], value)

            dev = self.render(
                "docker-compose.yml", "infra/compose/compose.dev.yml",
                PEPPY_SETUP_TOKEN=value
            )
            self.assertEqual(dev.returncode, 0, dev.stderr)
            dev_service = json.loads(dev.stdout)["services"]["dev"]
            self.assertEqual(dev_service["environment"]["PEPPY_SETUP_TOKEN"], value)

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_trusted_proxy_cidrs_are_optional_and_forwarded_to_api(self):
        for value in ("", "192.0.2.10/32,2001:db8::10/128"):
            base = self.render("docker-compose.yml", TRUSTED_PROXY_CIDRS=value)
            self.assertEqual(base.returncode, 0, base.stderr)
            self.assertEqual(
                json.loads(base.stdout)["services"]["api"]["environment"]["TRUSTED_PROXY_CIDRS"],
                value,
            )

            caddy = self.render(
                "docker-compose.yml",
                "infra/compose/compose.caddy.yml",
                PUBLIC_HOST="peppy.invalid",
                TRUSTED_PROXY_CIDRS=value,
            )
            self.assertEqual(caddy.returncode, 0, caddy.stderr)
            self.assertEqual(
                json.loads(caddy.stdout)["services"]["api"]["environment"]["TRUSTED_PROXY_CIDRS"],
                value,
            )

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_base_production_service_set(self):
        result = self.render("docker-compose.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            set(json.loads(result.stdout)["services"]),
            {"database-password-preflight", "postgres", "seaweedfs", "migrate", "api"},
        )

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_base_renders_with_only_required_values_and_defaults_shared_storage_credentials(self):
        result = self.render_with_required_environment()
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        environment = config["services"]["api"]["environment"]
        self.assertEqual(
            config["services"]["api"]["image"],
            "hub.docker.visnovsky.us/library/peppy-server:latest",
        )
        self.assertEqual(environment["PUBLIC_API_URL"], "https://peppy.invalid")
        self.assertEqual(environment["S3_ACCESS_KEY"], "peppy")
        self.assertEqual(self.database_credentials(environment["DATABASE_URL"])[0], "peppy_runtime")
        self.assertEqual(self.database_credentials(environment["MAINTENANCE_DATABASE_URL"])[0], "peppy_maintenance")
        self.assertEqual(
            config["services"]["postgres"]["environment"]["POSTGRES_USER"],
            "peppy",
        )
        self.assertIn('"accessKey":"peppy"', config["configs"]["seaweed-s3-config"]["content"])

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_explicit_public_api_url_overrides_required_host_derivation(self):
        result = self.render_with_required_environment(PUBLIC_API_URL="https://api.peppy.invalid")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout)["services"]["api"]["environment"]["PUBLIC_API_URL"],
            "https://api.peppy.invalid",
        )

    @staticmethod
    def database_credentials(url):
        parts = urlsplit(url)
        return parts.username, parts.password, parts.hostname, parts.path

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_base_services_select_owner_runtime_and_maintenance_database_roles(self):
        result = self.render("docker-compose.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        services = json.loads(result.stdout)["services"]
        owner = ("peppy", "synthetic-password", "postgres", "/peppy")
        runtime = (RUNTIME_ROLE, RUNTIME_PASSWORD, "postgres", "/peppy")
        maintenance = (MAINTENANCE_ROLE, MAINTENANCE_PASSWORD, "postgres", "/peppy")

        migrate = services["migrate"]["environment"]
        self.assertEqual(self.database_credentials(migrate["DATABASE_URL"]), runtime)
        self.assertEqual(self.database_credentials(migrate["DATABASE_MIGRATION_URL"]), owner)
        self.assertEqual(self.database_credentials(migrate["MAINTENANCE_DATABASE_URL"]), maintenance)

        api = services["api"]["environment"]
        self.assertEqual(self.database_credentials(api["DATABASE_URL"]), runtime)
        self.assertEqual(self.database_credentials(api["MAINTENANCE_DATABASE_URL"]), maintenance)
        self.assertNotIn("DATABASE_MIGRATION_URL", api)
        self.assertNotIn("synthetic-password", json.dumps(api))

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_dev_overlay_separates_runtime_and_owner_database_urls(self):
        result = self.render("docker-compose.yml", "infra/compose/compose.dev.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        dev = json.loads(result.stdout)["services"]["dev"]["environment"]
        self.assertEqual(
            self.database_credentials(dev["DATABASE_URL"]),
            (RUNTIME_ROLE, RUNTIME_PASSWORD, "postgres", "/peppy"),
        )
        self.assertEqual(
            self.database_credentials(dev["DATABASE_MIGRATION_URL"]),
            ("peppy", "synthetic-password", "postgres", "/peppy"),
        )

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_base_production_layout_is_self_contained_and_uses_server_image(self):
        server_image = f"example.invalid/peppy@sha256:{'a' * 64}"
        result = self.render(
            "docker-compose.yml",
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
        self.assertEqual(api["ports"], [{"mode": "ingress", "host_ip": "127.0.0.1", "target": 8080, "published": "7000", "protocol": "tcp"}])
        self.assertFalse(any(volume["type"] == "bind" for service in config["services"].values() for volume in service.get("volumes", [])))
        postgres_configs = {entry["source"]: entry["target"] for entry in config["services"]["postgres"]["configs"]}
        self.assertEqual(postgres_configs["database-password-preflight"], PASSWORD_PREFLIGHT_TARGET)
        self.assertEqual(postgres_configs["bootstrap-roles"], BOOTSTRAP_ROLES_TARGET)
        self.assertEqual(postgres_configs["bootstrap-roles-init"], "/docker-entrypoint-initdb.d/10-peppy-roles.sh")
        self.assertEqual(config["configs"]["bootstrap-roles-init"]["content"], f"sh {BOOTSTRAP_ROLES_TARGET}\n")

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_source_build_overlay_adds_server_builds(self):
        result = self.render("docker-compose.yml", "infra/compose/compose.build.yml", PEPPY_SERVER_IMAGE="peppy-server:local")
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in ("api", "migrate"):
            build = json.loads(result.stdout)["services"][name]["build"]
            self.assertEqual(build["context"], str(ROOT))
            self.assertEqual(build["dockerfile"], "infra/docker/server.Dockerfile")

    def test_dev_wrapper_uses_dev_overlay_and_defaults_only_its_required_image(self):
        with tempfile.TemporaryDirectory() as directory:
            dotenv = Path(directory) / ".env"
            dotenv.write_text("DEBUG=false\n")
            for provided, expected in ((None, "peppy-server:local"), ("", "peppy-server:local"), ("example.invalid/server@sha256:abc", "example.invalid/server@sha256:abc")):
                with self.subTest(provided=provided):
                    calls = []

                    def capture(_program, arguments, environment):
                        calls.append((arguments, environment))
                        raise RuntimeError("captured")

                    environment = {} if provided is None else {"PEPPY_SERVER_IMAGE": provided}
                    with mock.patch.dict(os.environ, environment, clear=True), mock.patch.object(dev_tls.os, "execvpe", capture):
                        with self.assertRaisesRegex(RuntimeError, "captured"):
                            dev_tls.compose_arguments(dotenv, ["config"], False)

                    arguments, values = calls[0]
                    files = [arguments[index + 1] for index, value in enumerate(arguments) if value == "-f"]
                    self.assertEqual(files, [str(ROOT / "docker-compose.yml"), str(ROOT / "infra/compose/compose.dev.yml")])
                    self.assertNotIn(str(ROOT / "infra/compose/compose.build.yml"), files)
                    self.assertEqual(values["PEPPY_SERVER_IMAGE"], expected)

    def test_just_source_build_commands_default_only_their_server_image(self):
        justfile = (ROOT / "justfile").read_text()
        self.assertIn(
            'compose := "PEPPY_SERVER_IMAGE=${PEPPY_SERVER_IMAGE:-peppy-server:local} docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.build.yml"',
            justfile,
        )
        self.assertNotIn("export PEPPY_SERVER_IMAGE", justfile)
        self.assertIn("backup destination:\n    ./infra/compose/backup.sh {{ quote(destination) }}", justfile)
        self.assertIn("restore archive project:\n    ./infra/compose/restore.sh {{ quote(archive) }} {{ quote(project) }}", justfile)

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
        configs = {entry["source"]: entry["target"] for entry in caddy["configs"]}
        self.assertEqual(configs["caddyfile-template"], "/etc/caddy/Caddyfile.template")
        self.assertEqual(configs["caddy-entrypoint"], "/usr/local/bin/peppy-caddy")
        self.assertEqual(caddy["entrypoint"], ["/bin/sh", "/usr/local/bin/peppy-caddy"])
        self.assertNotIn("WEB_CLIENT_HOST", caddy["environment"])

    @unittest.skipUnless(compose, "Docker Compose is unavailable")
    def test_mcp_overlay_isolates_worker_from_storage_network(self):
        result = self.render("docker-compose.yml", "infra/compose/compose.mcp.yml")
        self.assertEqual(result.returncode, 0, result.stderr)
        config = json.loads(result.stdout)
        self.assertEqual(config["services"]["mcp"]["image"], "hub.docker.visnovsky.us/library/peppy-mcp:latest")
        self.assertEqual(set(config["services"]["mcp"]["networks"]), {"mcp-api", "mcp-egress"})
        self.assertNotIn("private", config["services"]["mcp"]["networks"])
        self.assertTrue(config["networks"]["mcp-api"]["internal"])
        self.assertIn("mcp-api", config["services"]["api"]["networks"])
        self.assertEqual(config["services"]["mcp-state-init"]["network_mode"], "none")
        self.assertNotIn("networks", config["services"]["mcp-state-init"])
        self.assertNotIn("build", config["services"]["mcp"])

        overridden = self.render("docker-compose.yml", "infra/compose/compose.mcp.yml", PEPPY_MCP_IMAGE="peppy-mcp:local")
        self.assertEqual(overridden.returncode, 0, overridden.stderr)
        self.assertEqual(json.loads(overridden.stdout)["services"]["mcp"]["image"], "peppy-mcp:local")

        built = self.render(
            "docker-compose.yml", "infra/compose/compose.mcp.yml", "infra/compose/compose.mcp-build.yml",
            PEPPY_MCP_IMAGE="peppy-mcp:local"
        )
        self.assertEqual(built.returncode, 0, built.stderr)
        self.assertEqual(json.loads(built.stdout)["services"]["mcp"]["build"]["dockerfile"], "infra/docker/mcp.Dockerfile")

    def test_shell_entrypoints_receive_their_script_as_one_argument(self):
        # Compose splits a string command into argv words, so `sh -c` would run only the first word.
        result = self.render("docker-compose.yml", "infra/compose/compose.mcp.yml", PEPPY_MCP_IMAGE="peppy-mcp:local")
        self.assertEqual(result.returncode, 0, result.stderr)
        for name, service in json.loads(result.stdout)["services"].items():
            entrypoint = service.get("entrypoint") or []
            if entrypoint and entrypoint[-1].startswith("-") and entrypoint[-1].endswith("c"):
                with self.subTest(service=name):
                    self.assertEqual(len(service.get("command") or []), 1)

    def test_dev_recipes_clean_legacy_services_and_use_one_off_helpers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy(ROOT / "justfile", root / "justfile")
            (root / "infra/dev").mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/dev_port.py", root / "infra/dev/dev_port.py")
            shutil.copy(ROOT / "infra/dev/dev_tls.py", root / "infra/dev/dev_tls.py")
            self.install_migration_controller(root)
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
            rm = "compose --env-file .env -f docker-compose.yml -f infra/compose/compose.build.yml rm --stop --force api migrate"
            resolved_root = root.resolve()
            dev_prefix = (
                f"compose --env-file {resolved_root / '.env'} -f {resolved_root / 'docker-compose.yml'} "
                f"-f {resolved_root / 'infra/compose/compose.dev.yml'}"
            )
            teardown_prefix = f"{dev_prefix} -f {resolved_root / 'infra/compose/compose.dev-tls.yml'}"
            dev_rm = f"{teardown_prefix} rm --stop --force web dev caddy"
            build = f"{dev_prefix} build web dev"
            dev_up = f"{dev_prefix} up --detach --wait --wait-timeout 1800 --force-recreate dev"
            self.assertLess(calls.index(build), calls.index(rm))
            self.assertLess(calls.index(rm), calls.index(dev_rm))
            self.assertLess(calls.index(dev_rm), calls.index(dev_up))
            self.assertFalse(any(call.endswith(" web") and " up " in call for call in calls))
            self.assertEqual(calls.count(rm), 2)
            down = f"{teardown_prefix} down"
            self.assertLess(calls.index(rm, calls.index(rm) + 1), calls.index(down))
            self.assertIn(f"{dev_prefix} run --rm --no-deps dev run build server", calls)
            self.assertIn(f"{dev_prefix} run --rm --no-deps dev run test rust", calls)

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
            self.assertEqual(log.read_text().splitlines()[-1], f"{dev_prefix} run --rm --no-deps dev run build server")

    def test_dev_up_rejects_malformed_web_ui_before_service_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            shutil.copy(ROOT / "justfile", root / "justfile")
            helper_directory = root / "infra/dev"
            helper_directory.mkdir(parents=True)
            shutil.copy(ROOT / "infra/dev/dev_port.py", helper_directory / "dev_port.py")
            self.install_migration_controller(root)
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
                "API_HOST_PORT=7100\nPUBLIC_API_URL=http://127.0.0.1:7100\n"
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
