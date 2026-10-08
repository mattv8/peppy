#!/usr/bin/env python3
"""Safely reconcile the local development database before starting it."""

import argparse
import contextlib
import fcntl
import hashlib
import os
import re
import shlex
import subprocess
import sys
import tempfile
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator, Optional, Sequence


ADVISORY_MULTIPLIER = 0x3D32AD9E
MIGRATION_NAME = re.compile(r"(?P<version>\d+)_(?P<name>.+)\.sql$")
TRANSACTION_KEYWORDS = {"ABORT", "BEGIN", "COMMIT", "END", "PREPARE", "RELEASE", "ROLLBACK", "SAVEPOINT", "START"}


class RepairError(RuntimeError):
    """Raised before an unsafe database transition can occur."""


@dataclass(frozen=True)
class AppliedMigration:
    version: int
    checksum: bytes
    success: bool


@dataclass(frozen=True)
class MigrationFile:
    version: int
    name: str
    up: bytes
    down: Optional[bytes]
    provenance: str

    @property
    def checksum(self) -> bytes:
        return hashlib.sha384(self.up).digest()


@dataclass(frozen=True)
class RepairPlan:
    reversions: tuple[MigrationFile, ...]


def plan_repair(applied: Sequence[AppliedMigration], target: Sequence[MigrationFile], candidates: Sequence[MigrationFile]) -> RepairPlan:
    applied_by_version = checked_history(applied)
    target_by_version = {migration.version: migration for migration in target}
    if len(target_by_version) != len(target):
        raise RepairError("target migrations contain duplicate versions")
    if not applied:
        return RepairPlan(())
    maximum = max(applied_by_version)
    boundaries = [version for version, migration in applied_by_version.items() if version not in target_by_version or target_by_version[version].checksum != migration.checksum]
    boundaries.extend(version for version in target_by_version if version <= maximum and version not in applied_by_version)
    if not boundaries:
        return RepairPlan(())
    boundary = min(boundaries)
    required = sorted((entry for entry in applied if entry.version >= boundary), key=lambda entry: entry.version, reverse=True)
    return RepairPlan(tuple(find_exact_candidate(entry, candidates) for entry in required))


def checked_history(applied: Sequence[AppliedMigration]) -> dict[int, AppliedMigration]:
    history: dict[int, AppliedMigration] = {}
    for migration in applied:
        if not migration.success:
            raise RepairError(f"migration {migration.version} has a failed history row")
        if migration.version in history:
            raise RepairError(f"migration {migration.version} has duplicate history rows")
        history[migration.version] = migration
    return history


def find_exact_candidate(applied: AppliedMigration, candidates: Sequence[MigrationFile]) -> MigrationFile:
    for tier in ("selected:", "worktree:", "branch:"):
        matches = [candidate for candidate in candidates if candidate.provenance.startswith(tier) and candidate.version == applied.version and candidate.checksum == applied.checksum and candidate.down is not None]
        distinct = {(match.up, match.down): match for match in matches}
        if len(distinct) == 1:
            return next(iter(distinct.values()))
        if distinct:
            raise RepairError(f"migration {applied.version} has ambiguous exact-checksum up/down pairs in {tier.removesuffix(':')} sources")
    raise RepairError(f"migration {applied.version} has no unambiguous exact-checksum up/down pair")


def discover_migrations(root: Path, provenance: str) -> list[MigrationFile]:
    migrations, downs = root / "services/server/migrations", root / "services/server/migrations/down"
    found: list[MigrationFile] = []
    for up_path in sorted(migrations.glob("*.sql")):
        match = MIGRATION_NAME.fullmatch(up_path.name)
        if match is None:
            continue
        version, name = int(match["version"]), match["name"]
        down_path = downs / f"{version:04d}_{name}.down.sql"
        found.append(MigrationFile(version, name, up_path.read_bytes(), down_path.read_bytes() if down_path.is_file() else None, provenance))
    return found


def historical_candidates(root: Path, selected: Path) -> list[MigrationFile]:
    candidates = complete_pairs(discover_migrations(selected, f"selected:{selected}"))
    worktrees = subprocess.run(["git", "-C", str(root), "worktree", "list", "--porcelain"], text=True, capture_output=True)
    if worktrees.returncode == 0:
        for line in worktrees.stdout.splitlines():
            if line.startswith("worktree "):
                path = Path(line.removeprefix("worktree "))
                if path.is_dir() and path.resolve() != selected.resolve():
                    candidates.extend(complete_pairs(discover_migrations(path, f"worktree:{path}")))
    branches = subprocess.run(["git", "-C", str(root), "for-each-ref", "--format=%(refname)", "refs/heads"], text=True, capture_output=True)
    if branches.returncode == 0:
        for branch in branches.stdout.splitlines():
            candidates.extend(branch_migrations(root, branch))
    return candidates


def complete_pairs(migrations: Sequence[MigrationFile]) -> list[MigrationFile]:
    return [migration for migration in migrations if migration.down is not None]


def branch_migrations(root: Path, branch: str) -> list[MigrationFile]:
    paths = subprocess.run(["git", "-C", str(root), "ls-tree", "-r", "--name-only", branch, "services/server/migrations"], text=True, capture_output=True)
    if paths.returncode:
        return []
    entries: list[MigrationFile] = []
    for path in paths.stdout.splitlines():
        match = MIGRATION_NAME.fullmatch(Path(path).name)
        if match is None or "/down/" in path:
            continue
        down_path = f"services/server/migrations/down/{int(match['version']):04d}_{match['name']}.down.sql"
        up, down = git_file(root, branch, path), git_file(root, branch, down_path)
        if up is not None and down is not None:
            entries.append(MigrationFile(int(match["version"]), match["name"], up, down, f"branch:{branch}"))
    return entries


def git_file(root: Path, revision: str, path: str) -> Optional[bytes]:
    result = subprocess.run(["git", "-C", str(root), "show", f"{revision}:{path}"], capture_output=True)
    return result.stdout if result.returncode == 0 else None


def validate_forward_pairs(applied: Sequence[AppliedMigration], target: Sequence[MigrationFile]) -> None:
    history = checked_history(applied)
    for migration in target:
        old = history.get(migration.version)
        if (old is None or old.checksum != migration.checksum) and migration.down is None:
            raise RepairError(f"target migration {migration.version} has no down SQL; refusing to apply an unrecoverable forward migration")


def repair_is_required(applied: Sequence[AppliedMigration], target: Sequence[MigrationFile]) -> bool:
    history = checked_history(applied)
    if not history:
        return False
    target_by_version = {migration.version: migration for migration in target}
    maximum = max(history)
    return any(version not in target_by_version or target_by_version[version].checksum != migration.checksum for version, migration in history.items()) or any(version <= maximum and version not in history for version in target_by_version)


def validate_down_sql(sql: bytes) -> None:
    if re.search(rb"(?im)^\s*--\s*no-transaction\b", sql):
        raise RepairError("down SQL declares -- no-transaction")
    code = top_level_sql(sql.decode("utf-8", errors="strict"))
    if "\\" in code:
        raise RepairError("down SQL contains a psql metacommand")
    for statement in code.split(";"):
        match = re.match(r"\s*([A-Za-z_]+)", statement)
        if match and match.group(1).upper() in TRANSACTION_KEYWORDS:
            raise RepairError(f"down SQL contains transaction control: {match.group(1).upper()}")


def top_level_sql(sql: str) -> str:
    """Blank quoted and comment bodies, rejecting unclosed SQL constructs."""
    result: list[str] = []
    index = 0
    while index < len(sql):
        if sql.startswith("--", index):
            end = sql.find("\n", index)
            if end < 0:
                break
            result.append("\n")
            index = end + 1
        elif sql.startswith("/*", index):
            index = nested_comment_end(sql, index)
            result.append(" ")
        elif sql[index] in "'\"":
            is_escape_string = sql[index] == "'" and index > 0 and sql[index - 1] in "eE" and (index == 1 or not (sql[index - 2].isalnum() or sql[index - 2] == "_"))
            index = quoted_end(sql, index, sql[index], is_escape_string)
            result.append(" ")
        elif sql[index] == "$":
            marker = re.match(r"\$[A-Za-z_][A-Za-z0-9_]*\$|\$\$", sql[index:])
            if marker is None:
                result.append(sql[index]); index += 1
            else:
                token = marker.group(0)
                end = sql.find(token, index + len(token))
                if end < 0:
                    raise RepairError("down SQL has an unclosed dollar-quoted string")
                result.append(" ")
                index = end + len(token)
        else:
            result.append(sql[index]); index += 1
    return "".join(result)


def nested_comment_end(sql: str, start: int) -> int:
    depth, index = 1, start + 2
    while index < len(sql):
        if sql.startswith("/*", index):
            depth += 1; index += 2
        elif sql.startswith("*/", index):
            depth -= 1; index += 2
            if depth == 0:
                return index
        else:
            index += 1
    raise RepairError("down SQL has an unclosed block comment")


def quoted_end(sql: str, start: int, quote: str, escape_backslashes: bool) -> int:
    index = start + 1
    while index < len(sql):
        if escape_backslashes and sql[index] == "\\":
            index += 2
        elif sql[index] == quote:
            if index + 1 < len(sql) and sql[index + 1] == quote:
                index += 2
            else:
                return index + 1
        else:
            index += 1
    raise RepairError("down SQL has an unclosed quoted string")


@contextlib.contextmanager
def transition_lock(root: Path, nonblocking: bool = False) -> Iterator[None]:
    result = subprocess.run(["git", "-C", str(root), "rev-parse", "--path-format=absolute", "--git-common-dir"], text=True, capture_output=True)
    lock_directory = Path(result.stdout.strip()) if result.returncode == 0 else root / ".opencode/dev"
    lock_directory.mkdir(parents=True, exist_ok=True)
    lock_path = lock_directory / "peppy-migration-transition.lock"
    with lock_path.open("a+") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | (fcntl.LOCK_NB if nonblocking else 0))
        except BlockingIOError as error:
            raise RepairError("a development transition is already in progress") from error
        yield


def is_local_docker_host(host: str) -> bool:
    return host.startswith("unix://") or host.startswith("npipe://")


def docker_endpoint(environment: dict[str, str]) -> str:
    result = subprocess.run(["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"], text=True, capture_output=True, env=environment)
    endpoint = result.stdout.strip()
    if result.returncode or not endpoint:
        raise RepairError("cannot inspect the effective Docker endpoint")
    return endpoint


def compose_command(root: Path) -> list[str]:
    return ["docker", "compose", "--env-file", str(root / ".env"), "-f", str(root / "docker-compose.yml"), "-f", str(root / "infra/compose/compose.dev.yml")]


def database_credentials(root: Path) -> tuple[str, str]:
    result = subprocess.run(compose_command(root) + ["exec", "-T", "postgres", "sh", "-ec", 'printf "%s\\t%s" "$POSTGRES_USER" "$POSTGRES_DB"'], text=True, capture_output=True)
    if result.returncode or "\t" not in result.stdout:
        raise RepairError(f"could not read PostgreSQL container configuration: {result.stderr.strip()}")
    user, database = result.stdout.strip().split("\t", 1)
    return user, database


def psql(root: Path, sql: str) -> subprocess.CompletedProcess[str]:
    user, database = database_credentials(root)
    return subprocess.run(compose_command(root) + ["exec", "-T", "postgres", "psql", "-X", "-v", "ON_ERROR_STOP=1", "-U", user, "-d", database, "-At", "-F", "\t"], input=sql, text=True, capture_output=True)


def read_applied(root: Path) -> list[AppliedMigration]:
    exists = psql(root, "SELECT to_regclass('_sqlx_migrations');")
    if exists.returncode:
        raise RepairError(f"could not check _sqlx_migrations: {exists.stderr.strip()}")
    if not exists.stdout.strip():
        return []
    result = psql(root, "SELECT version, encode(checksum, 'hex'), success FROM _sqlx_migrations ORDER BY version;")
    if result.returncode:
        raise RepairError(f"could not read _sqlx_migrations: {result.stderr.strip()}")
    return [AppliedMigration(int(version), bytes.fromhex(checksum), success == "t") for version, checksum, success in (line.split("\t") for line in result.stdout.splitlines())]


def current_database(root: Path) -> str:
    result = psql(root, "SELECT current_database();")
    if result.returncode or not result.stdout.strip():
        raise RepairError(f"could not determine current database: {result.stderr.strip()}")
    return result.stdout.strip()


def advisory_key(database_name: str) -> int:
    return ADVISORY_MULTIPLIER * (zlib.crc32(database_name.encode()) & 0xFFFFFFFF)


def backup_database(root: Path) -> Optional[Path]:
    directory = root / ".opencode/dev/db-backups"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    directory.chmod(0o700)
    descriptor, temporary = tempfile.mkstemp(prefix="migration-", suffix=".dump", dir=directory)
    os.close(descriptor)
    backup = Path(temporary)
    backup.chmod(0o600)
    user, database = database_credentials(root)
    with backup.open("wb") as output:
        dump = subprocess.run(compose_command(root) + ["exec", "-T", "postgres", "pg_dump", "-U", user, "-d", database, "--format=custom"], stdout=output, stderr=subprocess.PIPE)
    if dump.returncode:
        backup.unlink(missing_ok=True)
        raise RepairError(f"pg_dump failed: {dump.stderr.decode(errors='replace').strip()}")
    check = subprocess.run(compose_command(root) + ["exec", "-T", "postgres", "pg_restore", "--list"], input=backup.read_bytes(), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if check.returncode:
        backup.unlink(missing_ok=True)
        raise RepairError(f"pg_restore validation failed: {check.stderr.decode(errors='replace').strip()}")
    return backup


def require_verified_backup(create_backup) -> Path:
    backup = create_backup()
    if backup is None or not backup.is_file() or backup.stat().st_size == 0:
        raise RepairError("database backup failed or is empty; no migrations were reverted")
    return backup


def restore_command(root: Path, backup: Path) -> str:
    user, database = database_credentials(root)
    command = compose_command(root) + ["exec", "-T", "postgres", "pg_restore", "--clean", "--if-exists", "-U", user, "-d", database]
    return f"{shlex.join(command)} < {shlex.quote(str(backup))}"


def repair(root: Path, plan: RepairPlan, applied: Sequence[AppliedMigration], database_name: str) -> Optional[Path]:
    if not plan.reversions:
        return None
    for migration in plan.reversions:
        if migration.down is None:
            raise RepairError(f"migration {migration.version} has no down SQL")
        validate_down_sql(migration.down)
    backup = require_verified_backup(lambda: backup_database(root))
    expected = ", ".join(f"({entry.version}, decode('{entry.checksum.hex()}', 'hex'), {str(entry.success).lower()})" for entry in applied)
    reversions = ", ".join(str(entry.version) for entry in plan.reversions)
    down_sql = "\n".join(entry.down.decode("utf-8", errors="strict") for entry in plan.reversions if entry.down is not None)
    script = f"""SELECT pg_advisory_lock({advisory_key(database_name)});
BEGIN;
CREATE TEMP TABLE peppy_migration_transaction_sentinel () ON COMMIT DROP;
DO $$ BEGIN
  IF EXISTS ((SELECT version, checksum, success FROM _sqlx_migrations EXCEPT SELECT * FROM (VALUES {expected}) AS expected(version, checksum, success)) UNION ALL (SELECT * FROM (VALUES {expected}) AS expected(version, checksum, success) EXCEPT SELECT version, checksum, success FROM _sqlx_migrations)) THEN RAISE EXCEPTION 'migration history changed during repair'; END IF;
END $$;
{down_sql}
DO $$ BEGIN IF to_regclass('pg_temp.peppy_migration_transaction_sentinel') IS NULL THEN RAISE EXCEPTION 'migration repair escaped its transaction'; END IF; END $$;
DO $$ DECLARE deleted_count integer; BEGIN DELETE FROM _sqlx_migrations WHERE version IN ({reversions}); GET DIAGNOSTICS deleted_count = ROW_COUNT; IF deleted_count <> {len(plan.reversions)} THEN RAISE EXCEPTION 'migration metadata deletion count changed'; END IF; END $$;
COMMIT;
"""
    result = psql(root, script)
    if result.returncode:
        restore = restore_command(root, backup)
        raise RepairError(f"migration repair failed; code remains stopped. Restore with: {restore}\n{result.stderr.strip()}")
    return backup


def run_just(root: Path, recipe: str) -> None:
    result = subprocess.run(["just", recipe], cwd=root)
    if result.returncode:
        raise RepairError(f"{recipe} failed")


def verify_development_compose(root: Path) -> None:
    result = subprocess.run(compose_command(root) + ["config", "--quiet"], text=True, capture_output=True)
    if result.returncode:
        raise RepairError(f"development Compose configuration is invalid: {result.stderr.strip()}")


def verify_writers_stopped(root: Path) -> None:
    result = subprocess.run(compose_command(root) + ["ps", "--status", "running", "--services"], text=True, capture_output=True)
    if result.returncode:
        raise RepairError(f"could not verify development writers are stopped: {result.stderr.strip()}")
    writers = {line.strip() for line in result.stdout.splitlines()} & {"api", "migrate", "dev", "web"}
    if writers:
        raise RepairError(f"refusing migration repair while code writers are running: {', '.join(sorted(writers))}")


def dev_up(root: Path) -> None:
    if not is_local_docker_host(docker_endpoint(dict(os.environ))):
        raise RepairError("refusing migration repair against a non-local Docker endpoint")
    with transition_lock(root):
        run_just(root, "_dev-up-build")
        verify_development_compose(root)
        subprocess.run(compose_command(root) + ["up", "--detach", "--wait", "postgres"], check=True)
        applied = read_applied(root)
        selected = Path(os.environ.get("PEPPY_SOURCE_TREE", root)).resolve()
        if not (selected / "services/server/migrations").is_dir():
            raise RepairError(f"selected source tree has no server migrations: {selected}")
        target = discover_migrations(selected, "selected source tree")
        validate_forward_pairs(applied, target)
        plan = plan_repair(applied, target, historical_candidates(root, selected)) if repair_is_required(applied, target) else RepairPlan(())
        if plan.reversions:
            print(f"Reverting migrations: {', '.join(str(item.version) for item in plan.reversions)}", file=sys.stderr)
        run_just(root, "_dev-up-stop-writers")
        verify_writers_stopped(root)
        if discover_migrations(selected, "selected source tree") != target:
            raise RepairError("target migration files changed while writers were stopped")
        backup = repair(root, plan, applied, current_database(root))
        if backup:
            print(f"Migration backup retained at {backup}", file=sys.stderr)
        try:
            run_just(root, "_dev-up-start")
        except RepairError as error:
            if backup:
                raise RepairError(f"forward migration or startup failed; code remains stopped. Restore retained backup before retrying: {restore_command(root, backup)}") from error
            raise


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("command", choices=("dev-up", "dev-down"))
    arguments = parser.parse_args()
    root = Path.cwd().resolve()
    try:
        if arguments.command == "dev-up":
            dev_up(root)
        else:
            with transition_lock(root):
                run_just(root, "_dev-down-raw")
    except (RepairError, subprocess.CalledProcessError) as error:
        print(f"development transition: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
