import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tests.dev.stub_commands import REPOSITORY_ROOT, write_stub


class TestEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.checkout = self.root / "checkout with spaces"
        (self.checkout / "infra/compose").mkdir(parents=True)
        for name in ("test-env.sh", "extract-config.py"):
            shutil.copy(REPOSITORY_ROOT / "infra/compose" / name, self.checkout / "infra/compose")
        shutil.copy(REPOSITORY_ROOT / "docker-compose.yml", self.checkout)
        self.bin = self.root / "bin"
        self.log = self.root / "commands"
        self.yaml = self.root / "compose.yaml"
        write_stub(self.bin / "docker", """
printf '%s\\n' "$*" >> "$COMMAND_LOG"
cat >> "$YAML_LOG"
case "$*" in
  *' ps '*) printf '%s' "${RUNNING:-}" ;;
  *' up '*) exit "${UP_STATUS:-0}" ;;
  *' down '*) exit "${DOWN_STATUS:-0}" ;;
  *' port postgres '*) if [ "${PORT_STATUS:-0}" != 0 ]; then exit "$PORT_STATUS"; fi; echo "${POSTGRES_OUTPUT:-127.0.0.1:${PEPPY_TEST_POSTGRES_PORT:-15432}}" ;;
  *' port seaweedfs '*) echo "127.0.0.1:${PEPPY_TEST_S3_PORT:-18333}" ;;
esac
""")
        write_stub(self.bin / "curl", """
if [ "${CURL_STATUS:-0}" != 0 ]; then exit "$CURL_STATUS"; fi
case "$*" in *--aws-sigv4*) echo 200 ;; *) echo 403 ;; esac
""")

    def run_script(self, body, **overrides):
        env = os.environ.copy()
        for name in ("GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "PEPPY_TEST_COMPOSE_PROJECT",
                     "PEPPY_TEST_POSTGRES_PORT", "PEPPY_TEST_S3_PORT"):
            env.pop(name, None)
        env.update(PATH=f"{self.bin}:{env['PATH']}", COMMAND_LOG=str(self.log), YAML_LOG=str(self.yaml), **overrides)
        self.log.unlink(missing_ok=True)
        result = subprocess.run(
            ["/bin/bash", "-c", f'set -euo pipefail; source "$1"; {body}', "test", str(self.checkout / "infra/compose/test-env.sh")],
            env=env, capture_output=True, text=True,
        )
        project = env.get("PEPPY_TEST_COMPOSE_PROJECT")
        if not project:
            project = (f"peppy-test-{env['GITHUB_RUN_ID']}-{env.get('GITHUB_RUN_ATTEMPT', '0')}"
                       if env.get("GITHUB_RUN_ID") else
                       "peppy-test-" + hashlib.sha256(str(self.checkout.resolve()).encode()).hexdigest()[:12])
        if self.log.exists():
            for command in self.log.read_text().splitlines():
                self.assertTrue(command.startswith(f"compose --project-name {project} -f - "), command)
        return result

    def test_local_identity_stable_and_checkout_specific(self):
        first = self.run_script('echo "$PEPPY_TEST_COMPOSE_PROJECT"')
        self.assertEqual(first.stdout.strip(), "peppy-test-" + hashlib.sha256(str(self.checkout.resolve()).encode()).hexdigest()[:12])
        self.assertEqual(first.stdout, self.run_script('echo "$PEPPY_TEST_COMPOSE_PROJECT"').stdout)
        other = self.root / "other"
        shutil.copytree(self.checkout, other)
        self.checkout = other
        self.assertNotEqual(first.stdout, self.run_script('echo "$PEPPY_TEST_COMPOSE_PROJECT"').stdout)

    def test_override_and_ci_identity(self):
        self.assertEqual(self.run_script('echo "$PEPPY_TEST_COMPOSE_PROJECT"', PEPPY_TEST_COMPOSE_PROJECT="custom").stdout.strip(), "custom")
        self.assertEqual(self.run_script('echo "$PEPPY_TEST_COMPOSE_PROJECT"', GITHUB_RUN_ID="42", GITHUB_RUN_ATTEMPT="3").stdout.strip(), "peppy-test-42-3")

    def test_running_stack_refused_without_down(self):
        result = self.run_script("peppy_test_infra_up", RUNNING="postgres")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("already has containers", result.stderr)
        self.assertNotIn(" down ", self.log.read_text())

    def test_preclean_ports_entrypoint_and_down(self):
        result = self.run_script('peppy_test_infra_up || exit; echo "$PEPPY_TEST_SEAWEED_ENTRYPOINT|$DATABASE_URL|$TEST_DATABASE_URL|$S3_INTERNAL_ENDPOINT"; peppy_test_infra_down; peppy_test_infra_down')
        self.assertEqual(result.returncode, 0, result.stderr)
        entry, database, test_database, endpoint = result.stdout.strip().split("|")
        self.assertTrue(entry.startswith(str(self.checkout.resolve() / "target/test-tmp") + "/"))
        self.assertFalse(Path(entry).exists())
        self.assertEqual(database, test_database)
        self.assertIn(":15432/", database)
        self.assertEqual(endpoint, "http://127.0.0.1:18333")
        commands = self.log.read_text().splitlines()
        self.assertEqual(sum(" down " in line for line in commands), 2)
        self.assertTrue(all("peppy-test-local-0" not in line for line in commands))
        self.assertLess(next(i for i, line in enumerate(commands) if " down " in line), next(i for i, line in enumerate(commands) if " up " in line))
        self.assertIn("127.0.0.1:${PEPPY_TEST_POSTGRES_PORT}:5432", self.yaml.read_text())
        self.assertIn("127.0.0.1:${PEPPY_TEST_S3_PORT}:8333", self.yaml.read_text())

    def test_failed_up_logs_and_cleans_preserving_status(self):
        result = self.run_script("peppy_test_infra_up", UP_STATUS="37")
        self.assertEqual(result.returncode, 37)
        commands = self.log.read_text()
        self.assertIn(" logs", commands)
        self.assertEqual(commands.count(" down "), 2)
        self.assertEqual(list((self.checkout / "target/test-tmp").iterdir()), [])

    def test_failed_discovery_and_probe_preserve_status(self):
        for variable in ("PORT_STATUS", "CURL_STATUS"):
            with self.subTest(variable=variable):
                result = self.run_script("peppy_test_infra_up", **{variable: "38"})
                self.assertEqual(result.returncode, 38)
                self.assertIn(" logs", self.log.read_text())
                self.assertEqual(list((self.checkout / "target/test-tmp").iterdir()), [])

    def test_ci_ports_and_override_defaults(self):
        result = self.run_script('echo "$PEPPY_TEST_POSTGRES_PORT|$PEPPY_TEST_S3_PORT"', GITHUB_RUN_ID="1")
        self.assertEqual(result.stdout.strip(), "5432|8333")
        result = self.run_script('echo "$PEPPY_TEST_POSTGRES_PORT|$PEPPY_TEST_S3_PORT"', PEPPY_TEST_POSTGRES_PORT="1234", PEPPY_TEST_S3_PORT="5678")
        self.assertEqual(result.stdout.strip(), "1234|5678")

    def test_yaml_port_mappings_and_ci_parity(self):
        # Compose interpolates the captured environment variables in these mappings.
        for env, postgres, s3 in (({}, "", ""), ({"GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "2"}, "5432", "8333")):
            with self.subTest(env=env):
                self.yaml.unlink(missing_ok=True)
                result = self.run_script('peppy_test_infra_up; echo "$PEPPY_TEST_COMPOSE_PROJECT|$DATABASE_URL|$TEST_DATABASE_URL|$S3_INTERNAL_ENDPOINT"; peppy_test_infra_down', **env)
                self.assertEqual(result.returncode, 0, result.stderr)
                yaml = self.yaml.read_text().replace("${PEPPY_TEST_POSTGRES_PORT}", postgres).replace("${PEPPY_TEST_S3_PORT}", s3)
                self.assertIn(f"127.0.0.1:{postgres}:5432", yaml)
                self.assertIn(f"127.0.0.1:{s3}:8333", yaml)
                if env:
                    db = "postgres://peppy_test:peppy-test-password@127.0.0.1:5432/peppy_test"
                    self.assertEqual(result.stdout.strip(), f"peppy-test-42-2|{db}|{db}|http://127.0.0.1:8333")

    def test_fresh_down_and_refusal_trap(self):
        result = self.run_script("peppy_test_infra_down")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(" down --volumes --remove-orphans", self.log.read_text())
        result = self.run_script("trap peppy_test_infra_down EXIT; peppy_test_infra_up", RUNNING="created-container")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("docker compose --project-name ", result.stderr)
        self.assertIn(" down --volumes --remove-orphans", result.stderr)
        self.assertIn(" ps --all --quiet", self.log.read_text())
        self.assertNotIn(" down ", self.log.read_text())

    def test_local_urls_unset_before_and_after_failure(self):
        result = self.run_script('echo "${DATABASE_URL-unset}|${TEST_DATABASE_URL-unset}|${S3_INTERNAL_ENDPOINT-unset}"; if peppy_test_infra_up; then exit 1; fi; echo "${DATABASE_URL-unset}|${TEST_DATABASE_URL-unset}|${S3_INTERNAL_ENDPOINT-unset}"', CURL_STATUS="38")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["unset|unset|unset"] * 2)

    def test_invalid_port_and_failed_down(self):
        result = self.run_script("peppy_test_infra_up", POSTGRES_OUTPUT="invalid")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid compose port", result.stderr)
        self.assertIn(" logs", self.log.read_text())
        result = self.run_script("peppy_test_infra_down", DOWN_STATUS="41")
        self.assertEqual(result.returncode, 41)
        self.assertEqual(list((self.checkout / "target/test-tmp").iterdir()), [])

    def test_ci_source_exports_original_urls(self):
        result = self.run_script('echo "$DATABASE_URL|$TEST_DATABASE_URL|$S3_INTERNAL_ENDPOINT"', GITHUB_RUN_ID="1")
        db = "postgres://peppy_test:peppy-test-password@127.0.0.1:5432/peppy_test"
        self.assertEqual(result.stdout.strip(), f"{db}|{db}|http://127.0.0.1:8333")
