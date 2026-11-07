import json
import os
import pathlib
import stat
import subprocess
import tempfile
import unittest
from hashlib import sha256


ROOT = pathlib.Path(__file__).resolve().parents[2]
BACKUP = ROOT / "infra/compose/backup.sh"
RESTORE = ROOT / "infra/compose/restore.sh"

POSTGRES_ID = "sha256:" + "a" * 64
SEAWEED_ID = "sha256:" + "b" * 64
API_ID = "sha256:" + "c" * 64
STALE_API_ID = "sha256:" + "d" * 64


FAKE_DOCKER = r'''#!/usr/bin/env python3
import json, os, subprocess, sys

args = sys.argv[1:]
with open(os.environ["DOCKER_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] == "compose":
    if "config" in args:
        print(os.environ["COMPOSE_CONFIG"])
    elif "ps" in args and "--status" in args:
        print("postgres")
    elif "ps" in args and "--format" in args:
        print(json.dumps([
            {"Service": "postgres", "State": "running"},
            {"Service": "api", "State": "exited"},
            {"Service": "seaweedfs", "State": "exited"},
        ]))
    elif "ps" in args:
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
    sys.exit(0 if os.environ.get("VOLUMES_EXIST", "1") == "1" else 1)
if args[0] == "inspect":
    service = args[-1].removeprefix("container-")
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


def compose_config(api_image: str | None, seaweed_image: str) -> str:
    api = {"image": api_image} if api_image else {"build": {"context": "."}}
    return json.dumps(
        {
            "name": "peppy",
            "services": {
                "postgres": {"image": "postgres:fixture", "volumes": [{"type": "volume", "source": "postgres", "target": "/var/lib/postgresql"}]},
                "seaweedfs": {"image": seaweed_image, "volumes": [{"type": "volume", "source": "seaweed-data", "target": "/data"}, {"type": "volume", "source": "seaweed-home", "target": "/root/.seaweedfs"}]},
                "api": api,
            },
            "volumes": {"postgres": {"name": "peppy_postgres"}, "seaweed-data": {"name": "peppy_seaweed-data"}, "seaweed-home": {"name": "peppy_seaweed-home"}},
        }
    )


class ComposeRecoveryScriptTests(unittest.TestCase):
    def backup_fixture(self, api_image: str | None, seaweed_image: str, api_id: str = API_ID):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        (root / ".env").write_text("fixture=true\n")
        destination = root / "backups"
        destination.mkdir()
        tools = root / "tools"
        tools.mkdir()
        docker = tools / "docker"
        docker.write_text(FAKE_DOCKER)
        docker.chmod(0o755)
        log = root / "docker.log"
        images = {"postgres:fixture": POSTGRES_ID, seaweed_image: SEAWEED_ID, api_image or "peppy-api": API_ID}
        environment = os.environ | {
            "PATH": f"{tools}:{os.environ['PATH']}",
            "DOCKER_LOG": str(log),
            "COMPOSE_CONFIG": compose_config(api_image, seaweed_image),
            "RUNNING_IDS": json.dumps({"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "api": api_id}),
            "IMAGE_IDS": json.dumps(images),
        }
        return root, destination, log, environment

    def run_backup(self, root, destination, environment):
        return subprocess.run(["bash", str(BACKUP), str(destination)], cwd=root, env=environment, text=True, capture_output=True)

    def restore_fixture(self, readiness_failures: int):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = pathlib.Path(directory.name)
        (root / ".env").write_text("fixture=true\n")
        archive = root / "archive"
        archive.mkdir()
        manifest = {
            "format_version": 2,
            "source_project": "source",
            "images": {
                "configured": {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture"},
                "running_ids": {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID},
            },
            "artifacts": ["postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"],
        }
        (archive / "manifest.json").write_text(json.dumps(manifest))
        for name in ("postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"):
            (archive / name).write_bytes(b"fixture")
        checksums = [
            f"{sha256((archive / name).read_bytes()).hexdigest()}  {name}"
            for name in ("manifest.json", "postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz")
        ]
        (archive / "manifest.sha256").write_text("\n".join(checksums) + "\n")
        tools = root / "tools"
        tools.mkdir()
        for name, contents in (("docker", FAKE_DOCKER), ("psql", FAKE_PSQL), ("sleep", FAKE_SLEEP)):
            tool = tools / name
            tool.write_text(contents)
            tool.chmod(0o755)
        log = root / "docker.log"
        password = "recovery_fixture_password"
        environment = os.environ | {
            "PATH": f"{tools}:{os.environ['PATH']}",
            "DOCKER_LOG": str(log),
            "COMPOSE_CONFIG": compose_config(None, "seaweed:fixture"),
            "RUNNING_IDS": json.dumps({"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID}),
            "IMAGE_IDS": json.dumps({"postgres:fixture": POSTGRES_ID, POSTGRES_ID: POSTGRES_ID}),
            "VOLUMES_EXIST": "0",
            "READINESS_FAILURES": str(readiness_failures),
            "PSQL_ATTEMPT_FILE": str(root / "psql-attempts"),
            "EXPECTED_POSTGRES_PASSWORD": password,
        }
        return root, archive, log, environment, password

    def run_restore(self, root, archive, environment):
        return subprocess.run(
            ["bash", str(RESTORE), str(archive), "target"],
            cwd=root,
            env=environment,
            text=True,
            capture_output=True,
        )

    @staticmethod
    def recovery_events(log):
        return [json.loads(line) for line in log.read_text().splitlines()]

    def test_restore_waits_for_authenticated_tcp_database_without_retrying_restore(self):
        root, archive, log, environment, password = self.restore_fixture(readiness_failures=2)

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
        root, archive, log, environment, _password = self.restore_fixture(readiness_failures=100)

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

    def test_stale_build_only_tag_fails_before_any_service_stop_or_backup_creation(self):
        root, destination, log, environment = self.backup_fixture(None, "seaweed:root", STALE_API_ID)

        result = self.run_backup(root, destination, environment)

        self.assertEqual(result.returncode, 67)
        self.assertIn("api container image mismatch", result.stderr)
        self.assertNotIn('"stop"', log.read_text())
        self.assertEqual(list(destination.iterdir()), [])

    def test_untrusted_manifest_image_never_reaches_docker_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / ".env").write_text("fixture=true\n")
            archive = root / "archive"
            archive.mkdir()
            manifest = {
                "format_version": 2,
                "source_project": "source",
                "images": {"configured": {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture"}, "running_ids": {"postgres": "attacker/image:tag", "seaweedfs": SEAWEED_ID}},
                "artifacts": ["postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"],
            }
            (archive / "manifest.json").write_text(json.dumps(manifest))
            for name in ("postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"):
                (archive / name).write_bytes(b"fixture")
            entries = []
            for name in ("manifest.json", "postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz"):
                entries.append(f"{sha256((archive / name).read_bytes()).hexdigest()}  {name}")
            (archive / "manifest.sha256").write_text("\n".join(entries) + "\n")
            tools = root / "tools"
            tools.mkdir()
            docker = tools / "docker"
            docker.write_text(FAKE_DOCKER)
            docker.chmod(0o755)
            log = root / "docker.log"
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "DOCKER_LOG": str(log), "COMPOSE_CONFIG": "{}", "RUNNING_IDS": "{}", "IMAGE_IDS": "{}"}

            result = subprocess.run(["bash", str(RESTORE), str(archive), "target"], cwd=root, env=environment, text=True, capture_output=True)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("pinned sha256", result.stderr)
            self.assertFalse(log.exists())


if __name__ == "__main__":
    unittest.main()
