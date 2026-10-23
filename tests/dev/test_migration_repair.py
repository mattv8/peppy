import hashlib
import contextlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "infra/dev"))
import migration_repair as repair


def checksum(text: bytes) -> bytes:
    return hashlib.sha384(text).digest()


class MigrationRepairTests(unittest.TestCase):
    def candidate(self, version: int, up: bytes, provenance: str = "selected:test"):
        return repair.MigrationFile(version, f"migration_{version}", up, b"DROP TABLE example;\n", provenance)

    def test_exact_checksum_match_does_not_repair(self):
        up = b"CREATE TABLE example ();\n"
        candidate = self.candidate(1, up)
        plan = repair.plan_repair([repair.AppliedMigration(1, checksum(up), True)], [candidate], [candidate])
        self.assertEqual(plan.reversions, ())

    def test_old_target_without_down_is_a_noop_but_new_forward_requires_one(self):
        up = b"CREATE TABLE example ();"
        old_target = repair.MigrationFile(1, "migration_1", up, None, "selected:old")
        applied = [repair.AppliedMigration(1, checksum(up), True)]
        self.assertEqual(repair.plan_repair(applied, [old_target], []).reversions, ())
        repair.validate_forward_pairs(applied, [old_target])
        with self.assertRaisesRegex(repair.RepairError, "no down SQL"):
            repair.validate_forward_pairs([], [old_target])

    def test_missing_or_changed_early_migration_reverts_complete_suffix(self):
        old, second = b"old\r\n", b"two\n"
        applied = [repair.AppliedMigration(1, checksum(old), True), repair.AppliedMigration(2, checksum(second), True)]
        target = [self.candidate(1, b"new\r\n"), self.candidate(2, second)]
        plan = repair.plan_repair(applied, target, [self.candidate(1, old, "branch:refs/heads/old"), target[1]])
        self.assertEqual([entry.version for entry in plan.reversions], [2, 1])

    def test_target_insertion_before_applied_suffix_reverts_suffix(self):
        one, three = b"one", b"three"
        target = [self.candidate(1, one), self.candidate(2, b"two"), self.candidate(3, three)]
        plan = repair.plan_repair(
            [repair.AppliedMigration(1, checksum(one), True), repair.AppliedMigration(3, checksum(three), True)],
            target, target,
        )
        self.assertEqual([entry.version for entry in plan.reversions], [3])

    def test_failed_or_duplicate_history_is_refused(self):
        up = b"one"
        with self.assertRaisesRegex(repair.RepairError, "failed"):
            repair.plan_repair([repair.AppliedMigration(1, checksum(up), False)], [self.candidate(1, up)], [])
        with self.assertRaisesRegex(repair.RepairError, "duplicate"):
            repair.plan_repair([repair.AppliedMigration(1, checksum(up), True)] * 2, [self.candidate(1, up)], [])

    def test_raw_crlf_checksum_and_down_provenance_are_preserved(self):
        up = b"CREATE TABLE x ();\r\n"
        candidate = self.candidate(1, up, "branch:repair")
        self.assertEqual(candidate.checksum, checksum(up))
        self.assertEqual(candidate.down, b"DROP TABLE example;\n")
        self.assertEqual(candidate.provenance, "branch:repair")

    def test_sql_validator_rejects_transactions_and_metacommands_but_not_function_body(self):
        repair.validate_down_sql(b"CREATE FUNCTION f() RETURNS void AS $$ BEGIN PERFORM 1; END; $$ LANGUAGE plpgsql; E'COMMIT \\\\ ignored'; /* outer /* nested */ comment */")
        for prohibited in (b"BEGIN;", b"END;", b"ABORT;", b"COMMIT;", b"\\set x 1", b"-- no-transaction\nDROP TABLE x;"):
            with self.subTest(prohibited=prohibited):
                with self.assertRaises(repair.RepairError):
                    repair.validate_down_sql(prohibited)
        for unsafe in (b"/* never closes", b"$$ never closes", b"E'never closes"):
            with self.subTest(unsafe=unsafe):
                with self.assertRaises(repair.RepairError):
                    repair.validate_down_sql(unsafe)
        with self.assertRaises(repair.RepairError):
            repair.validate_down_sql(b"DROP TABLE x; \\set unsafe")

    def test_selected_exact_pair_beats_historical_pair(self):
        up = b"CREATE TABLE example ();"
        selected = repair.MigrationFile(1, "migration_1", up, b"DROP TABLE selected;", "selected:current")
        old = repair.MigrationFile(1, "migration_1", up, b"DROP TABLE old;", "branch:refs/heads/old")
        self.assertIs(repair.find_exact_candidate(repair.AppliedMigration(1, checksum(up), True), [selected, old]), selected)

    def test_backup_failure_stops_before_downs(self):
        calls = []
        with self.assertRaises(repair.RepairError):
            repair.require_verified_backup(lambda: calls.append("backup") or None)
        self.assertEqual(calls, ["backup"])

    def test_repair_does_not_execute_downs_when_backup_cannot_be_verified(self):
        migration = self.candidate(1, b"CREATE TABLE example ();\n")
        plan = repair.RepairPlan((migration,))
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(repair, "backup_database", return_value=None):
                with mock.patch.object(repair, "psql") as psql:
                    with self.assertRaises(repair.RepairError):
                        repair.repair(Path(directory), plan, [repair.AppliedMigration(1, migration.checksum, True)], "peppy")
        psql.assert_not_called()

    def test_repair_holds_advisory_lock_and_revalidates_history_before_down_sql(self):
        migration = self.candidate(1, b"CREATE TABLE example ();\n")
        plan = repair.RepairPlan((migration,))
        result = type("Result", (), {"returncode": 0, "stderr": ""})()
        with tempfile.TemporaryDirectory() as directory:
            backup = Path(directory) / "backup.dump"; backup.write_bytes(b"x")
            with mock.patch.object(repair, "backup_database", return_value=backup):
                with mock.patch.object(repair, "psql", return_value=result) as psql:
                    repair.repair(Path(directory), plan, [repair.AppliedMigration(1, migration.checksum, True)], "peppy")
        script = psql.call_args.args[1]
        self.assertLess(script.index("pg_advisory_lock"), script.index("BEGIN;"))
        self.assertLess(script.index("migration history changed"), script.index("DROP TABLE example"))

    def test_every_server_migration_has_one_down_pair_and_valid_down_sql(self):
        ups = sorted((ROOT / "services/server/migrations").glob("*.sql"))
        downs = sorted((ROOT / "services/server/migrations/down").glob("*.down.sql"))
        self.assertEqual([path.stem for path in ups], [path.name.removesuffix(".down.sql") for path in downs])
        for down in downs:
            with self.subTest(down=down.name):
                repair.validate_down_sql(down.read_bytes())

    def test_historical_candidates_preserve_dirty_pairs_and_full_ref_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo with spaces"
            root.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.email", "repair@example.test"], cwd=root, check=True)
            subprocess.run(["git", "config", "user.name", "Migration Repair"], cwd=root, check=True)
            migration_root = root / "services/server/migrations"
            down_root = migration_root / "down"
            down_root.mkdir(parents=True)
            up_path = migration_root / "0001_example.sql"
            down_path = down_root / "0001_example.down.sql"
            up_path.write_bytes(b"CREATE TABLE original ();\n")
            down_path.write_bytes(b"DROP TABLE original;\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "commit", "-qm", "original migration"], cwd=root, check=True)
            subprocess.run(["git", "branch", "history"], cwd=root, check=True)

            worktree = Path(directory) / "registered worktree"
            subprocess.run(["git", "worktree", "add", "-qb", "feature/history", str(worktree)], cwd=root, check=True)
            worktree_up = worktree / "services/server/migrations/0001_example.sql"
            worktree_down = worktree / "services/server/migrations/down/0001_example.down.sql"
            worktree_up.write_bytes(b"CREATE TABLE worktree ();\r\n")
            worktree_down.write_bytes(b"DROP TABLE worktree;\r\n")
            up_path.write_bytes(b"CREATE TABLE selected ();\r\n")
            down_path.write_bytes(b"DROP TABLE selected;\r\n")

            candidates = repair.historical_candidates(root, root)
            self.assertIn("branch:refs/heads/history", {candidate.provenance for candidate in candidates})
            selected = repair.find_exact_candidate(
                repair.AppliedMigration(1, checksum(b"CREATE TABLE selected ();\r\n"), True), candidates
            )
            self.assertEqual((selected.up, selected.down), (b"CREATE TABLE selected ();\r\n", b"DROP TABLE selected;\r\n"))
            registered = repair.find_exact_candidate(
                repair.AppliedMigration(1, checksum(b"CREATE TABLE worktree ();\r\n"), True), candidates
            )
            self.assertTrue(registered.provenance.startswith("worktree:"))
            self.assertEqual((registered.up, registered.down), (b"CREATE TABLE worktree ();\r\n", b"DROP TABLE worktree;\r\n"))

    def test_remote_docker_endpoint_is_refused(self):
        self.assertFalse(repair.is_local_docker_host("tcp://db.example.test:2376"))
        self.assertTrue(repair.is_local_docker_host("unix:///var/run/docker.sock"))
        self.assertTrue(repair.is_local_docker_host("npipe:////./pipe/docker_engine"))

    def test_psql_uses_resolved_container_credentials(self):
        result = type("Result", (), {"returncode": 0, "stdout": "", "stderr": ""})()
        with mock.patch.object(repair, "database_credentials", return_value=("actual user", "actual db")):
            with mock.patch.object(repair.subprocess, "run", return_value=result) as run:
                repair.psql(Path("/repo"), "SELECT 1;")
        command = run.call_args.args[0]
        self.assertEqual(command[-14:], ["exec", "-T", "postgres", "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", "actual user", "-d", "actual db", "-At", "-F", "\t"])

    def test_missing_history_table_is_an_empty_history(self):
        missing = type("Result", (), {"returncode": 0, "stdout": "\n", "stderr": ""})()
        with mock.patch.object(repair, "psql", return_value=missing) as psql:
            self.assertEqual(repair.read_applied(Path("/repo")), [])
        psql.assert_called_once()

    def test_effective_docker_endpoint_uses_unnamed_context_for_every_environment(self):
        result = type("Result", (), {"returncode": 0, "stdout": "tcp://127.0.0.1:9\n", "stderr": ""})()
        for environment in ({"DOCKER_HOST": "unix:///ignored", "DOCKER_CONTEXT": "ignored"}, {"DOCKER_HOST": "unix:///ignored"}, {"DOCKER_CONTEXT": "ignored"}, {}):
            with self.subTest(environment=environment):
                with mock.patch.object(repair.subprocess, "run", return_value=result) as run:
                    self.assertEqual(repair.docker_endpoint(environment), "tcp://127.0.0.1:9")
                self.assertEqual(run.call_args.args[0], ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"])
                self.assertEqual(run.call_args.kwargs["env"], environment)

    def test_restore_command_quotes_actual_compose_credentials_and_backup(self):
        with mock.patch.object(repair, "database_credentials", return_value=("actual user", "actual db")):
            command = repair.restore_command(Path("/repo root"), Path("/backup dir/file.dump"))
        self.assertIn("'actual user'", command)
        self.assertIn("'actual db'", command)
        self.assertIn("'/backup dir/file.dump'", command)

    def test_dev_up_builds_before_stopping_and_never_starts_after_repair_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            migrations = root / "services/server/migrations/down"
            migrations.mkdir(parents=True)
            (migrations.parent / "0001_example.sql").write_text("CREATE TABLE example ();\n")
            (migrations / "0001_example.down.sql").write_text("DROP TABLE example;\n")
            calls = []
            success = type("Result", (), {"returncode": 0})()
            with mock.patch.object(repair, "docker_endpoint", return_value="unix:///docker.sock"), \
                 mock.patch.object(repair, "transition_lock", return_value=contextlib.nullcontext()), \
                 mock.patch.object(repair, "run_just", side_effect=lambda _root, recipe: calls.append(recipe)), \
                 mock.patch.object(repair, "verify_development_compose", side_effect=lambda _root: calls.append("compose")), \
                 mock.patch.object(repair, "verify_writers_stopped", side_effect=lambda _root: calls.append("writers")), \
                 mock.patch.object(repair, "read_applied", return_value=[]), \
                 mock.patch.object(repair, "current_database", return_value="peppy"), \
                 mock.patch.object(repair, "repair", return_value=None), \
                 mock.patch.object(repair.subprocess, "run", return_value=success):
                repair.dev_up(root)
            self.assertEqual(calls, ["_dev-up-build", "compose", "_dev-up-stop-writers", "writers", "_dev-up-start"])

            calls.clear()
            with mock.patch.object(repair, "docker_endpoint", return_value="unix:///docker.sock"), \
                 mock.patch.object(repair, "transition_lock", return_value=contextlib.nullcontext()), \
                 mock.patch.object(repair, "run_just", side_effect=lambda _root, recipe: calls.append(recipe)), \
                 mock.patch.object(repair, "verify_development_compose"), \
                 mock.patch.object(repair, "verify_writers_stopped"), \
                 mock.patch.object(repair, "read_applied", return_value=[]), \
                 mock.patch.object(repair, "current_database", return_value="peppy"), \
                 mock.patch.object(repair, "repair", side_effect=repair.RepairError("backup failed")), \
                 mock.patch.object(repair.subprocess, "run", return_value=success):
                with self.assertRaisesRegex(repair.RepairError, "backup failed"):
                    repair.dev_up(root)
            self.assertNotIn("_dev-up-start", calls)

    def test_transition_lock_serializes_callers(self):
        with tempfile.TemporaryDirectory() as directory:
            subprocess.run(["git", "init", "-q"], cwd=directory, check=True)
            with repair.transition_lock(Path(directory)):
                with self.assertRaisesRegex(repair.RepairError, "already in progress"):
                    with repair.transition_lock(Path(directory), nonblocking=True):
                        pass
