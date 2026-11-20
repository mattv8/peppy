#!/usr/bin/env python3
"""Regression checks for the reusable container validation workflow."""

import copy
import json
import posixpath
import re
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).parents[2]
WORKFLOW = Path(__file__).parents[1] / "workflows" / "build-container.yml"
SERVER_DOCKERFILE = ROOT / "infra" / "docker" / "server.Dockerfile"
DOCKERIGNORE = ROOT / ".dockerignore"
LOCKFILE = ROOT / "pnpm-lock.yaml"
IMAGES = {
    "server": ("peppy-server", "infra/docker/server.Dockerfile"),
    "relay": ("peppy-push-relay", "infra/docker/push-relay.Dockerfile"),
    "mcp": ("peppy-mcp", "infra/docker/mcp.Dockerfile"),
}
RESULTS_THAT_MUST_FAIL = ("failure", "skipped", "cancelled", "")
RUBY_YAML_JOBS = (
    "require 'yaml'; require 'json'; "
    "workflow = YAML.safe_load(File.read(ARGV.fetch(0)), aliases: true); "
    "puts JSON.generate(workflow.fetch('jobs'))"
)


def load_jobs():
    result = subprocess.run(
        ["ruby", "-e", RUBY_YAML_JOBS, str(WORKFLOW)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def aggregate_script(jobs):
    return jobs["complete"]["steps"][0]["run"]


def aggregate_result(script, results):
    environment = {f"{job.upper()}_RESULT": result for job, result in results.items()}
    return subprocess.run(["bash", "-ec", script], env=environment, capture_output=True, text=True)


def lockfile_importers():
    """Workspace directories pnpm checks with --frozen-lockfile: the importers keys except the root."""
    importers = []
    in_importers = False
    for line in LOCKFILE.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        if not line.startswith(" "):
            in_importers = line == "importers:"
            continue
        match = re.fullmatch(r"  ([^ ].*?):(?: \{\})?", line)
        if in_importers and match and match.group(1) != ".":
            importers.append(match.group(1))
    if not importers:
        raise AssertionError("no workspace importers found in pnpm-lock.yaml")
    return importers


def web_build_copies():
    """(source, destination) pairs of the COPY lines in the server Dockerfile's web-build stage."""
    copies = []
    in_web_build = False
    for line in SERVER_DOCKERFILE.read_text(encoding="utf-8").splitlines():
        if line.startswith("FROM "):
            in_web_build = line.endswith(" AS web-build")
        elif in_web_build and line.startswith("COPY "):
            tokens = line.split()
            copies.extend((source, tokens[-1]) for source in tokens[1:-1])
    return copies


def is_copied(importer, copies):
    for source, destination in copies:
        if importer == source or importer.startswith(source + "/"):
            relative = importer[len(source) :].lstrip("/")
            if posixpath.normpath(f"{destination}/{relative}") == importer:
                return True
    return False


class WebBuildContextTests(unittest.TestCase):
    """Every pnpm workspace importer must reach the web-build context.

    .dockerignore excludes packages/** and apps/* and re-includes each needed directory with an
    explicit `!<dir>/` line (no pattern engine is applied here), so a new workspace package needs
    both a web-build COPY line and such a re-include.
    """

    def test_every_lockfile_importer_is_copied_and_not_excluded(self):
        copies = web_build_copies()
        ignore_lines = set(DOCKERIGNORE.read_text(encoding="utf-8").splitlines())
        for importer in lockfile_importers():
            with self.subTest(importer=importer):
                self.assertTrue(
                    is_copied(importer, copies),
                    f"{importer} is not copied into the web-build stage of server.Dockerfile",
                )
                if importer.startswith(("packages/", "apps/")):
                    self.assertIn(f"!{importer}/", ignore_lines, f"{importer} is excluded by .dockerignore")


class ContainerWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.jobs = load_jobs()

    def assert_required_jobs_are_unconditional(self, jobs):
        for job in IMAGES:
            self.assertNotIn("if", jobs[job], f"{job} must always run")

    def assert_aggregate_env_wiring(self, jobs):
        environment = jobs["complete"]["steps"][0]["env"]
        self.assertEqual(
            environment,
            {
                "SERVER_RESULT": "${{ needs.server.result }}",
                "RELAY_RESULT": "${{ needs.relay.result }}",
                "MCP_RESULT": "${{ needs.mcp.result }}",
            },
        )

    def assert_aggregate_rejects_non_success(self, script):
        success = {job: "success" for job in IMAGES}
        self.assertEqual(aggregate_result(script, success).returncode, 0)
        for job in IMAGES:
            for result in RESULTS_THAT_MUST_FAIL:
                with self.subTest(job=job, result=result):
                    outcomes = success | {job: result}
                    self.assertNotEqual(aggregate_result(script, outcomes).returncode, 0)

    def test_builds_exactly_the_three_required_images_with_hardening_smokes(self):
        self.assertEqual(set(self.jobs), {*IMAGES, "complete"})
        self.assert_required_jobs_are_unconditional(self.jobs)
        for job, (image, dockerfile) in IMAGES.items():
            steps = self.jobs[job]["steps"]
            build = next(step for step in steps if step.get("uses", "").startswith("docker/build-push-action@"))
            self.assertEqual(build["with"]["file"], dockerfile)
            self.assertEqual(build["with"]["tags"], f"{image}:ci")
            self.assertEqual(build["with"]["push"], False)
            self.assertNotIn("secrets", self.jobs[job])
        for job, image in (("server", "peppy-server"), ("relay", "peppy-push-relay")):
            smoke = self.jobs[job]["steps"][-1]["run"]
            self.assertIn(f"{image}:ci -ec", smoke)
            self.assertIn("--network none --read-only --cap-drop ALL", smoke)
            self.assertIn("--security-opt no-new-privileges:true", smoke)
            self.assertIn('test "$(id -u)" = 10001', smoke)
            self.assertIn("ca-certificates.crt", smoke)
            self.assertIn('grep "not found"', smoke)
        self.assertEqual(self.jobs["mcp"]["steps"][-1]["run"], "bash infra/docker/test-mcp-image.sh peppy-mcp:ci")

    def test_preserves_source_revision_and_version_labels(self):
        for job, (image, _) in IMAGES.items():
            steps = self.jobs[job]["steps"]
            checkout = next(step for step in steps if step.get("uses", "").startswith("actions/checkout@"))
            buildx = next(step for step in steps if step.get("uses", "").startswith("docker/setup-buildx-action@"))
            build = next(step for step in steps if step.get("uses", "").startswith("docker/build-push-action@"))
            metadata = next(step for step in steps if step.get("id") == "metadata")["run"]
            self.assertEqual(checkout["with"]["ref"], "${{ inputs.ref || github.sha }}")
            self.assertEqual(buildx["uses"], "docker/setup-buildx-action@f87e5991a6d7451dcb8d9637bfbc97413f497069")
            self.assertEqual(build["uses"], "docker/build-push-action@c3c9e263c25d99ce0380d002d59b67737d91b0dc")
            self.assertIn("version must match X.Y.Z or X.Y.Z-prerelease", metadata)
            self.assertIn("revision=$(git rev-parse HEAD)", metadata)
            labels = build["with"]["labels"]
            self.assertIn("org.opencontainers.image.revision=${{ steps.metadata.outputs.revision }}", labels)
            self.assertIn(f"org.opencontainers.image.title={image}", labels)
            self.assertIn("org.opencontainers.image.licenses=Apache-2.0", labels)
            self.assertIn("org.opencontainers.image.version=$VERSION", metadata)

    def test_cache_scopes_restore_for_all_and_write_only_from_production_pushes(self):
        production_write = "github.event_name == 'push' && github.ref == 'refs/heads/production'"
        for job in IMAGES:
            build = next(step for step in self.jobs[job]["steps"] if step.get("uses", "").startswith("docker/build-push-action@"))
            self.assertEqual(build["with"]["cache-from"], f"type=gha,scope=container-{job}")
            self.assertIn(production_write, build["with"]["cache-to"])
            self.assertIn(f"type=gha,mode=min,scope=container-{job}", build["with"]["cache-to"])
            self.assertNotIn("mode=max", build["with"]["cache-to"])

    def test_complete_fails_for_every_non_success_dependency_result(self):
        complete = self.jobs["complete"]
        self.assertEqual(complete["if"], "${{ always() }}")
        self.assertEqual(complete["needs"], ["server", "relay", "mcp"])
        self.assert_aggregate_env_wiring(self.jobs)
        self.assert_aggregate_rejects_non_success(aggregate_script(self.jobs))

    def test_mutations_are_rejected(self):
        conditional_server = copy.deepcopy(self.jobs)
        conditional_server["server"]["if"] = False
        with self.assertRaises(AssertionError):
            self.assert_required_jobs_are_unconditional(conditional_server)

        wrong_mcp_wiring = copy.deepcopy(self.jobs)
        wrong_mcp_wiring["complete"]["steps"][0]["env"]["MCP_RESULT"] = "${{ needs.server.result }}"
        with self.assertRaises(AssertionError):
            self.assert_aggregate_env_wiring(wrong_mcp_wiring)

        missing_mcp = aggregate_script(self.jobs).replace('"$MCP_RESULT"', "", 1)
        results = {job: "success" for job in IMAGES} | {"mcp": "failure"}
        self.assertEqual(aggregate_result(missing_mcp, results).returncode, 0)
        self.assertNotEqual(aggregate_result(aggregate_script(self.jobs), results).returncode, 0)


if __name__ == "__main__":
    unittest.main()
