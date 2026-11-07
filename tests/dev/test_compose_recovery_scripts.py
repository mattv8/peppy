from __future__ import annotations

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
import json, os, sys

args = sys.argv[1:]
with open(os.environ["DOCKER_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
if args[0] == "compose":
    if "config" in args:
        print(os.environ["COMPOSE_CONFIG"])
    elif "ps" in args and "--status" in args:
        print("postgres" + ("\nmcp" if os.environ.get("MCP_OVERLAY") == "1" else ""))
    elif "ps" in args and "--format" in args:
        services = [
            {"Service": "postgres", "State": "running"},
            {"Service": "api", "State": "exited"},
            {"Service": "seaweedfs", "State": "exited"},
        ]
        if os.environ.get("MCP_OVERLAY") == "1":
            services.extend([
                {"Service": "mcp", "State": "running"},
                {"Service": "mcp-state-init", "State": "exited"},
            ])
        print(json.dumps(services))
    elif "ps" in args:
        print("container-" + args[-1])
    elif "exec" in args:
        sys.stdout.buffer.write(b"postgres dump")
    sys.exit(0)
if args[:2] == ["volume", "inspect"]:
    if os.environ.get("VOLUME_INSPECT_MODE") == "absent":
        sys.exit(1)
    sys.exit(0)
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


class ComposeRecoveryScriptTests(unittest.TestCase):
    def backup_fixture(self, api_image: str | None, seaweed_image: str, api_id: str = API_ID, mcp_overlay: bool = False):
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
        images = {"postgres:fixture": POSTGRES_ID, seaweed_image: SEAWEED_ID, api_image or "peppy-api": API_ID, "mcp:fixture": API_ID}
        environment = os.environ | {
            "PATH": f"{tools}:{os.environ['PATH']}",
            "DOCKER_LOG": str(log),
            "COMPOSE_CONFIG": compose_config(api_image, seaweed_image, mcp_overlay),
            "RUNNING_IDS": json.dumps({"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "api": api_id, "mcp": API_ID}),
            "IMAGE_IDS": json.dumps(images),
            "MCP_OVERLAY": "1" if mcp_overlay else "0",
        }
        return root, destination, log, environment

    def run_backup(self, root, destination, environment):
        return subprocess.run(["bash", str(BACKUP), str(destination)], cwd=root, env=environment, text=True, capture_output=True)

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

        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('"stop"', log.read_text())
        self.assertEqual(list(destination.iterdir()), [])

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

    def test_mcp_restore_requires_matching_overlay_before_volume_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / ".env").write_text("fixture=true\n")
            archive = root / "archive"
            archive.mkdir()
            manifest = {
                "format_version": 2,
                "source_project": "source",
                "images": {"configured": {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture", "mcp": "mcp:fixture"}, "running_ids": {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "mcp": API_ID}},
                "artifacts": ["postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz", "mcp-state.tar.gz"],
            }
            (archive / "manifest.json").write_text(json.dumps(manifest))
            for name in manifest["artifacts"]:
                (archive / name).write_bytes(b"fixture")
            entries = [f"{sha256((archive / name).read_bytes()).hexdigest()}  {name}" for name in ("manifest.json", *manifest["artifacts"])]
            (archive / "manifest.sha256").write_text("\n".join(entries) + "\n")
            tools = root / "tools"
            tools.mkdir()
            docker = tools / "docker"
            docker.write_text(FAKE_DOCKER)
            docker.chmod(0o755)
            log = root / "docker.log"
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "DOCKER_LOG": str(log), "COMPOSE_CONFIG": compose_config("server:fixture", "seaweed:fixture"), "RUNNING_IDS": "{}", "IMAGE_IDS": json.dumps({POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID})}

            result = subprocess.run(["bash", str(RESTORE), str(archive), "target"], cwd=root, env=environment, text=True, capture_output=True)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("MCP overlay", result.stderr)
            self.assertNotIn('"create"', log.read_text())

    def test_mcp_restore_extracts_state_without_starting_worker(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / ".env").write_text("fixture=true\n")
            archive = root / "archive"
            archive.mkdir()
            manifest = {
                "format_version": 2,
                "source_project": "source",
                "images": {"configured": {"postgres": "postgres:fixture", "seaweedfs": "seaweed:fixture", "mcp": "mcp:fixture"}, "running_ids": {"postgres": POSTGRES_ID, "seaweedfs": SEAWEED_ID, "mcp": API_ID}},
                "artifacts": ["postgres.dump", "seaweed-data.tar.gz", "seaweed-home.tar.gz", "mcp-state.tar.gz"],
            }
            (archive / "manifest.json").write_text(json.dumps(manifest))
            for name in manifest["artifacts"]:
                (archive / name).write_bytes(b"fixture")
            entries = [f"{sha256((archive / name).read_bytes()).hexdigest()}  {name}" for name in ("manifest.json", *manifest["artifacts"])]
            (archive / "manifest.sha256").write_text("\n".join(entries) + "\n")
            tools = root / "tools"
            tools.mkdir()
            docker = tools / "docker"
            docker.write_text(FAKE_DOCKER)
            docker.chmod(0o755)
            log = root / "docker.log"
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "DOCKER_LOG": str(log), "COMPOSE_CONFIG": compose_config("server:fixture", "seaweed:fixture", True), "RUNNING_IDS": "{}", "IMAGE_IDS": json.dumps({POSTGRES_ID: POSTGRES_ID, "postgres:fixture": POSTGRES_ID}), "VOLUME_INSPECT_MODE": "absent"}

            result = subprocess.run(["bash", str(RESTORE), str(archive), "target"], cwd=root, env=environment, text=True, capture_output=True)

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
