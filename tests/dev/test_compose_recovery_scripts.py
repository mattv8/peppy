from __future__ import annotations

import json
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest
from collections.abc import Callable
from hashlib import sha256


ROOT = pathlib.Path(__file__).resolve().parents[2]
BACKUP = ROOT / "infra/compose/backup.sh"
RESTORE = ROOT / "infra/compose/restore.sh"

POSTGRES_ID = "sha256:" + "a" * 64
SEAWEED_ID = "sha256:" + "b" * 64
API_ID = "sha256:" + "c" * 64
STALE_API_ID = "sha256:" + "d" * 64

BASELINE_ARTIFACTS = ("postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz")
MCP_ARTIFACTS = (*BASELINE_ARTIFACTS, "mcp-state.tar.gz")
MCP_CONFIGURED = {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture", "mcp": "mcp:fixture"}
MCP_RUNNING = {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "mcp": API_ID}
MCP_RESTORE_EXISTING_VOLUME = 73
BASELINE_CONFIGURED = {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture"}
BASELINE_RUNNING = {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID}
RESTORE_FIXTURE_PASSWORD = "recovery_fixture_password"


FAKE_DOCKER = r'''#!/usr/bin/env python3
import json, os, subprocess, sys

args = sys.argv[1:]
with open(os.environ["DOCKER_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] == "compose":
    if "config" in args:
        print(os.environ["COMPOSE_CONFIG"])
    elif "ps" in args and "--status" in args:
        print(os.environ["PS_RUNNING"])
    elif "ps" in args and "--format" in args:
        print(os.environ["PS_ALL"])
    elif "ps" in args and "-q" in args:
        listed = {s["Service"] for s in json.loads(os.environ["PS_ALL"])}
        if args[-1] in listed:
            print("container-" + args[-1])
    elif "exec" in args:
        command = args[-1]
        if "psql --no-password -h 127.0.0.1" in command:
            container_environment = os.environ | {
                "POSTGRES_DB": "recovery",
                "POSTGRES_USER": "recovery",
                "POSTGRES_PASSWORD": os.environ["EXPECTED_POSTGRES_PASSWORD"],
            }
            result = subprocess.run(["sh", "-ec", command], env=container_environment)
            sys.exit(result.returncode)
        if "pg_restore" not in command:
            sys.stdout.buffer.write(b"postgres dump")
    sys.exit(0)
if args[:2] == ["volume", "inspect"]:
    name = args[-1]
    explicit = json.loads(os.environ.get("EXISTING_VOLUMES", "{}"))
    if name in explicit:
        sys.exit(0 if explicit[name] else 1)
    if os.environ.get("VOLUME_INSPECT_MODE") == "absent":
        sys.exit(1)
    sys.exit(0 if os.environ.get("VOLUMES_EXIST", "1") == "1" else 1)
if args[0] == "inspect":
    service = args[-1].removeprefix("container-")
    if "{{.State.ExitCode}}" in args:
        print(json.loads(os.environ["EXIT_CODES"])[service])
        sys.exit(0)
    print(json.loads(os.environ["RUNNING_IDS"])[service])
    sys.exit(0)
if args[:2] == ["image", "inspect"]:
    image = args[-1]
    image_id = json.loads(os.environ["IMAGE_IDS"]).get(image)
    if image_id is None:
        sys.exit(1)
    print(image_id)
    sys.exit(0)
if args[0] == "run":
    sys.stdout.buffer.write(b"archive")
    sys.exit(0)
sys.exit(0)
'''


FAKE_PSQL = r'''#!/usr/bin/env python3
import json, os, pathlib, sys

args = sys.argv[1:]
attempt_path = pathlib.Path(os.environ["PSQL_ATTEMPT_FILE"])
attempt = int(attempt_path.read_text()) + 1 if attempt_path.exists() else 1
attempt_path.write_text(str(attempt))
ready = attempt > int(os.environ["READINESS_FAILURES"])
password_from_environment = os.environ.get("PGPASSWORD") == os.environ["EXPECTED_POSTGRES_PASSWORD"]
connect_timeout = os.environ.get("PGCONNECT_TIMEOUT")
event = {
    "tool": "psql",
    "args": args,
    "attempt": attempt,
    "ready": ready,
    "password_from_environment": password_from_environment,
    "connect_timeout": connect_timeout,
}
with open(os.environ["DOCKER_LOG"], "a") as log:
    log.write(json.dumps(event) + "\n")
expected = ["--no-password", "-h", "127.0.0.1", "-U", "recovery", "-d", "recovery", "-Atqc", "SELECT 1"]
if args != expected or not password_from_environment or connect_timeout != "2":
    sys.exit(64)
if not ready:
    sys.exit(1)
print("1")
'''


FAKE_SLEEP = "#!/bin/sh\nexit 0\n"


def compose_config(api_image: str | None, seaweed_image: str, mcp_overlay: bool = False) -> str:
    api = {"image": api_image} if api_image else {"build": {"context": "."}}
    config = {
            "name": "peppy",
            "services": {
                "postgres": {"image": "postgres:fixture", "volumes": [{"type": "volume", "source": "postgres", "target": "/var/lib/postgresql"}]},
                "seaweedfs": {"image": seaweed_image, "volumes": [{"type": "volume", "source": "seaweed-data", "target": "/data"}, {"type": "volume", "source": "seaweed-home", "target": "/root/.seaweedfs"}]},
                "api": api,
            },
            "volumes": {"postgres": {"name": "peppy_postgres"}, "seaweed-data": {"name": "peppy_seaweed-data"}, "seaweed-home": {"name": "peppy_seaweed-home"}},
        }
    if mcp_overlay:
        config["services"]["mcp"] = {
            "image": "mcp:fixture",
            "volumes": [
                {"type": "volume", "source": "mcp-state", "target": "/var/lib/peppy-mcp"},
                {"type": "volume", "source": "mcp-control", "target": "/run/peppy-mcp"},
            ],
        }
        config["services"]["mcp-state-init"] = {"image": "mcp:fixture"}
        config["volumes"].update({"mcp-state": {"name": "peppy_mcp-state"}, "mcp-control": {"name": "peppy_mcp-control"}})
    return json.dumps(config)


def ps_state(mcp_states: dict[str, str]) -> tuple[str, str]:
    services = [
        {"Service": "postgres", "State": "running"},
        {"Service": "api", "State": "exited"},
        {"Service": "seaweedfs", "State": "exited"},
        *({"Service": name, "State": state} for name, state in mcp_states.items()),
    ]
    running = "\n".join(service["Service"] for service in services if service["State"] == "running")
    return running, json.dumps(services)


def share_volume_name(key: str, name: str) -> Callable[[dict], None]:
    def mutate(config: dict) -> None:
        config["volumes"][key]["name"] = name
    return mutate


def map_state_to_control(config: dict) -> None:
    config["services"]["mcp"]["volumes"][0]["source"] = "mcp-control"


ALIAS_MUTATIONS = {
    "control shares state physical name": share_volume_name("mcp-control", "peppy_mcp-state"),
    "state shares postgres physical name": share_volume_name("mcp-state", "peppy_postgres"),
    "control shares seaweed data physical name": share_volume_name("mcp-control", "peppy_seaweed-data"),
    "state mount resolves to control volume": map_state_to_control,
}


def write_restore_archive(root: pathlib.Path, artifacts: tuple[str, ...], configured: dict, running: dict) -> pathlib.Path:
    archive = root / "archive"
    archive.mkdir()
    manifest = {
        "format_version": 2,
        "source_project": "source",
        "images": {"configured": configured, "running_ids": running},
        "artifacts": list(artifacts),
    }
    (archive / "manifest.json").write_text(json.dumps(manifest))
    for name in artifacts:
        (archive / name).write_bytes(b"fixture")
    entries = [f"{sha256((archive / name).read_bytes()).hexdigest()}  {name}" for name in ("manifest.json", *artifacts)]
    (archive / "manifest.sha256").write_text("\n".join(entries) + "\n")
    return archive


class ComposeRecoveryScriptTests(unittest.TestCase):
    def fake_docker_root(self) -> tuple[pathlib.Path, pathlib.Path, pathlib.Path]:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        (root / ".env").write_text("fixture=true\n")
        tools = root / "tools"
        tools.mkdir()
        for name, contents in (("docker", FAKE_DOCKER), ("psql", FAKE_PSQL), ("sleep", FAKE_SLEEP)):
            tool = tools / name
            tool.write_text(contents)
            tool.chmod(0o755)
        return root, tools, root / "docker.log"

    def backup_fixture(
        self,
        api_image: str | None,
        seaweed_image: str,
        api_id: str = API_ID,
        mcp_overlay: bool = False,
        mcp_states: dict[str, str] | None = None,
        exit_codes: dict[str, int] | None = None,
    ):
        root, tools, log = self.fake_docker_root()
        destination = root / "backups"
        destination.mkdir()
        if mcp_states is None:
            mcp_states = {"mcp": "running", "mcp-state-init": "exited"} if mcp_overlay else {}
        running, ps_all = ps_state(mcp_states)
        images = {"postgres:fixture": POSTGRES_ID, seaweed_image: SEAWEED_ID, api_image or "peppy-api": API_ID, "mcp:fixture": API_ID}
        environment = os.environ | {
            "PATH": f"{tools}:{os.environ['PATH']}",
            "DOCKER_LOG": str(log),
            "COMPOSE_CONFIG": compose_config(api_image, seaweed_image, mcp_overlay),
            "RUNNING_IDS": json.dumps({"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "api": api_id, "mcp": API_ID}),
            "IMAGE_IDS": json.dumps(images),
            "PS_RUNNING": running,
            "PS_ALL": ps_all,
            "EXIT_CODES": json.dumps(exit_codes or {"mcp-state-init": 0}),
        }
        return root, destination, log, environment

    def restore_fixture(
        self,
        artifacts: tuple[str, ...] = BASELINE_ARTIFACTS,
        configured: dict = BASELINE_CONFIGURED,
        running: dict = BASELINE_RUNNING,
        config_json: str | None = None,
        image_ids: dict | None = None,
        existing_volumes: dict[str, bool] | None = None,
        readiness_failures: int = 0,
    ):
        root, tools, log = self.fake_docker_root()
        archive = write_restore_archive(root, artifacts, configured, running)
        environment = os.environ | {
            "PATH": f"{tools}:{os.environ['PATH']}",
            "DOCKER_LOG": str(log),
            "COMPOSE_CONFIG": config_json or compose_config(None, "seaweed:fixture"),
            "RUNNING_IDS": "{}",
            "IMAGE_IDS": json.dumps(image_ids or {"postgres:fixture": POSTGRES_ID, POSTGRES_ID: POSTGRES_ID}),
            "VOLUMES_EXIST": "0",
            "EXISTING_VOLUMES": json.dumps(existing_volumes or {}),
            "PS_RUNNING": "",
            "PS_ALL": "[]",
            "EXIT_CODES": "{}",
            "READINESS_FAILURES": str(readiness_failures),
            "PSQL_ATTEMPT_FILE": str(root / "psql-attempts"),
            "EXPECTED_POSTGRES_PASSWORD": RESTORE_FIXTURE_PASSWORD,
        }
        return root, archive, log, environment

    def run_backup(self, root, destination, environment):
        return subprocess.run(["bash", str(BACKUP), str(destination)], cwd=root, env=environment, text=True, capture_output=True)

    def run_restore(self, root, archive, environment, target="target"):
        return subprocess.run(["bash", str(RESTORE), str(archive), target], cwd=root, env=environment, text=True, capture_output=True)

    def assert_backup_refused_before_stop(self, result, log, destination, expected_code: int | None = None):
        if expected_code is None:
            self.assertNotEqual(result.returncode, 0, result.stderr)
        else:
            self.assertEqual(result.returncode, expected_code, result.stderr)
        self.assertNotIn('"stop"', log.read_text())
        self.assertEqual(list(destination.iterdir()), [])

    @staticmethod
    def recovery_events(log):
        return [json.loads(line) for line in log.read_text().splitlines()]

    def test_restore_waits_for_authenticated_tcp_database_without_retrying_restore(self):
        root, archive, log, environment = self.restore_fixture(readiness_failures=2)
        password = environment["EXPECTED_POSTGRES_PASSWORD"]

        result = self.run_restore(root, archive, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        events = self.recovery_events(log)
        readiness = [event for event in events if isinstance(event, dict) and event.get("tool") == "psql"]
        self.assertEqual([event["ready"] for event in readiness], [False, False, True])
        self.assertTrue(all(event["password_from_environment"] for event in readiness))
        self.assertTrue(all(event["connect_timeout"] == "2" for event in readiness))
        restore_calls = [
            event
            for event in events
            if isinstance(event, list) and any("pg_restore" in argument for argument in event)
        ]
        self.assertEqual(len(restore_calls), 1)
        self.assertGreater(events.index(restore_calls[0]), events.index(readiness[-1]))
        self.assertNotIn(password, log.read_text())

    def test_restore_refuses_pg_restore_when_target_database_never_becomes_ready(self):
        root, archive, log, environment = self.restore_fixture(readiness_failures=100)

        result = self.run_restore(root, archive, environment)

        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertIn("target database did not become ready", result.stderr)
        events = self.recovery_events(log)
        readiness = [event for event in events if isinstance(event, dict) and event.get("tool") == "psql"]
        self.assertEqual(len(readiness), 60)
        self.assertFalse(any(event["ready"] for event in readiness))
        self.assertFalse(
            any(
                isinstance(event, list) and any("pg_restore" in argument for argument in event)
                for event in events
            )
        )

    def test_root_build_only_shape_uses_compose_default_tag_and_private_host_artifacts(self):
        root, destination, log, environment = self.backup_fixture(None, "seaweed:root")

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = log.read_text()
        self.assertNotIn('"-f"', calls)
        self.assertIn("peppy-api", calls)
        backup = next(destination.iterdir())
        self.assertEqual(stat.S_IMODE(backup.stat().st_mode), 0o700)
        self.assertTrue(all(stat.S_IMODE(path.stat().st_mode) == 0o600 and path.stat().st_uid == os.getuid() for path in backup.iterdir()))

    def test_community_overlay_records_effective_images_without_root_file_flag(self):
        root, destination, log, environment = self.backup_fixture("server:community", "seaweed:community")

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((next(destination.iterdir()) / "manifest.json").read_text())
        self.assertEqual(manifest["images"]["configured"]["seaweedfs"], "seaweed:community")
        self.assertNotIn('"-f"', log.read_text())

    def test_mcp_overlay_archives_only_state_after_stopping_worker(self):
        root, destination, log, environment = self.backup_fixture("server:community", "seaweed:community", mcp_overlay=True)

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        backup = next(destination.iterdir())
        manifest = json.loads((backup / "manifest.json").read_text())
        self.assertEqual(manifest["format_version"], 2)
        self.assertIn("mcp-state.tar.gz", manifest["artifacts"])
        self.assertNotIn("mcp-control", json.dumps(manifest))
        calls = log.read_text().splitlines()
        stop_index = next(index for index, call in enumerate(calls) if '"stop", "mcp"' in call)
        archive_index = next(index for index, call in enumerate(calls) if '"peppy_mcp-state:/source:ro"' in call)
        self.assertLess(stop_index, archive_index)
        self.assertNotIn("mcp-control:/source", log.read_text())

    def test_malformed_mcp_mount_fails_before_stopping_services(self):
        root, destination, log, environment = self.backup_fixture("server:community", "seaweed:community", mcp_overlay=True)
        config = json.loads(environment["COMPOSE_CONFIG"])
        config["services"]["mcp"]["volumes"].pop()
        environment["COMPOSE_CONFIG"] = json.dumps(config)

        result = self.run_backup(root, destination, environment)

        self.assert_backup_refused_before_stop(result, log, destination)

    def test_mcp_volume_aliases_fail_before_any_stop_or_archive(self):
        for name, mutate in ALIAS_MUTATIONS.items():
            with self.subTest(alias=name):
                root, destination, log, environment = self.backup_fixture("server:community", "seaweed:community", mcp_overlay=True)
                config = json.loads(environment["COMPOSE_CONFIG"])
                mutate(config)
                environment["COMPOSE_CONFIG"] = json.dumps(config)

                result = self.run_backup(root, destination, environment)

                self.assert_backup_refused_before_stop(result, log, destination)
                self.assertNotIn('"run"', log.read_text())

    def test_orphan_mcp_containers_without_overlay_keep_baseline_behavior(self):
        root, destination, log, environment = self.backup_fixture(
            "server:community",
            "seaweed:community",
            mcp_states={"mcp": "running", "mcp-state-init": "exited"},
            exit_codes={"mcp-state-init": 1},
        )

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = log.read_text()
        self.assertNotIn('"stop", "mcp"', calls)
        self.assertNotIn('"start", "mcp"', calls)
        self.assertNotIn("peppy_mcp-state", calls)
        manifest = json.loads((next(destination.iterdir()) / "manifest.json").read_text())
        self.assertEqual(tuple(manifest["artifacts"]), BASELINE_ARTIFACTS)
        self.assertNotIn("mcp", manifest["images"]["configured"])
        self.assertNotIn("mcp", manifest["images"]["running_ids"])

    def test_mcp_initializer_must_exit_cleanly_before_any_stop(self):
        for code in (0, 1, 137):
            with self.subTest(exit_code=code):
                root, destination, log, environment = self.backup_fixture(
                    "server:community", "seaweed:community", mcp_overlay=True, exit_codes={"mcp-state-init": code}
                )

                result = self.run_backup(root, destination, environment)

                if code == 0:
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assert_backup_refused_before_stop(result, log, destination, expected_code=67)
                    self.assertIn("MCP state initializer", result.stderr)

    def test_mcp_overlay_requires_initializer_container_before_any_stop(self):
        root, destination, log, environment = self.backup_fixture(
            "server:community", "seaweed:community", mcp_overlay=True, mcp_states={"mcp": "running"}
        )

        result = self.run_backup(root, destination, environment)

        self.assert_backup_refused_before_stop(result, log, destination, expected_code=67)

    def test_stale_build_only_tag_fails_before_any_service_stop_or_backup_creation(self):
        root, destination, log, environment = self.backup_fixture(None, "seaweed:root", STALE_API_ID)

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 67)
        self.assertIn("api container image mismatch", result.stderr)
        self.assertNotIn('"stop"', log.read_text())
        self.assertEqual(list(destination.iterdir()), [])

    def test_untrusted_manifest_image_never_reaches_docker_run(self):
        root, archive, log, environment = self.restore_fixture(
            BASELINE_ARTIFACTS,
            {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture"},
            {"postgres": "attacker/image:tag", "seaweedfs": SEAWEED_ID},
            "{}",
            {},
        )

        result = self.run_restore(root, archive, environment, target="target")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("pinned sha256", result.stderr)
        self.assertFalse(log.exists())

    def test_mcp_restore_requires_matching_overlay_before_volume_creation(self):
        root, archive, log, environment = self.restore_fixture(
            MCP_ARTIFACTS, MCP_CONFIGURED, MCP_RUNNING,
            compose_config("server:fixture", "seaweed:fixture"),
            {POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID},
        )

        result = self.run_restore(root, archive, environment)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("MCP overlay", result.stderr)
        self.assertNotIn('"create"', log.read_text())

    def test_mcp_overlay_requires_matching_artifact_before_volume_creation(self):
        root, archive, log, environment = self.restore_fixture(
            BASELINE_ARTIFACTS,
            {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture"},
            {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID},
            compose_config("server:fixture", "seaweed:fixture", True),
            {POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID},
        )

        result = self.run_restore(root, archive, environment)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("MCP overlay", result.stderr)
        self.assertNotIn('"create"', log.read_text())

    def test_mcp_restore_rejects_aliased_volumes_before_creation(self):
        for name, mutate in ALIAS_MUTATIONS.items():
            with self.subTest(alias=name):
                config = json.loads(compose_config("server:fixture", "seaweed:fixture", True))
                mutate(config)
                root, archive, log, environment = self.restore_fixture(
                    MCP_ARTIFACTS, MCP_CONFIGURED, MCP_RUNNING,
                    json.dumps(config),
                    {POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID},
                )

                result = self.run_restore(root, archive, environment)

                self.assertNotEqual(result.returncode, 0, result.stderr)
                calls = log.read_text()
                self.assertNotIn('"create"', calls)
                self.assertNotIn('"run"', calls)

    def test_mcp_restore_refuses_existing_mcp_volumes_before_creation(self):
        for volume in ("peppy_mcp-state", "peppy_mcp-control"):
            with self.subTest(volume=volume):
                root, archive, log, environment = self.restore_fixture(
                    MCP_ARTIFACTS, MCP_CONFIGURED, MCP_RUNNING,
                    compose_config("server:fixture", "seaweed:fixture", True),
                    {POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID},
                    existing_volumes={volume: True},
                )

                result = self.run_restore(root, archive, environment)

                self.assertEqual(result.returncode, MCP_RESTORE_EXISTING_VOLUME, result.stderr)
                self.assertIn(volume, result.stderr)
                self.assertNotIn('"create"', log.read_text())

    def test_mcp_restore_extracts_state_without_starting_worker(self):
        root, archive, log, environment = self.restore_fixture(
            MCP_ARTIFACTS, MCP_CONFIGURED, MCP_RUNNING,
            compose_config("server:fixture", "seaweed:fixture", True),
            {POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID},
        )

        result = self.run_restore(root, archive, environment)

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = log.read_text()
        self.assertIn('"create", "mcp-state-init"', calls)
        self.assertNotIn('"volume", "create", "peppy_mcp-state"', calls)
        self.assertIn("mcp-state.tar.gz", calls)
        self.assertIn("chown 10001:10001", calls)
        self.assertNotIn('"start", "mcp"', calls)
        self.assertNotIn('"up", "--wait", "mcp"', calls)


if __name__ == "__main__":
    unittest.main()
