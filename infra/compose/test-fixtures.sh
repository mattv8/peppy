#!/usr/bin/env bash
# Behavioral recovery-script fixtures. Uses only randomly named Compose resources.
set -u

repo=$(cd "$(dirname "$0")/../.." && pwd -P)
scratch_root="$repo/.opencode/sessions/messaging-foundation"
mkdir -p "$scratch_root"
fixture=$(mktemp -d "$scratch_root/recovery-hardening.XXXXXX")
suffix=$(openssl rand -hex 4)
source_project="recoveryhardening${suffix}"
compose=(docker compose --project-name "$source_project" --project-directory "$fixture" --env-file "$fixture/.env" -f "$fixture/docker-compose.yml")
created_projects="$source_project"
real_docker=$(command -v docker)

cleanup() {
  for project in $created_projects; do
    docker compose --project-name "$project" --project-directory "$fixture" --env-file "$fixture/.env" -f "$fixture/docker-compose.yml" down -v --remove-orphans --rmi local >/dev/null 2>&1 || true
  done
  rm -rf "$fixture"
}
trap cleanup EXIT HUP INT TERM

cat > "$fixture/.env" <<'ENV'
POSTGRES_DB=recovery
POSTGRES_USER=recovery
POSTGRES_PASSWORD=recovery-fixture-password
POSTGRES_IMAGE=postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
SEAWEED_COMMAND="exit 0"
API_COMMAND="exit 0"
API_RESTART=no
ENV
chmod 600 "$fixture/.env"
cat > "$fixture/Dockerfile" <<'DOCKERFILE'
FROM postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
DOCKERFILE
printf 'name: %s\n' "$source_project" > "$fixture/docker-compose.yml"
cat >> "$fixture/docker-compose.yml" <<'YAML'
services:
  postgres:
    image: ${POSTGRES_IMAGE}
    environment:
      POSTGRES_DB: ${POSTGRES_DB}
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
    volumes: [postgres-data:/var/lib/postgresql]
    healthcheck:
      test: [CMD-SHELL, 'pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB']
      interval: 1s
      timeout: 1s
      retries: 30
  seaweedfs:
    image: postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
    command: [sh, -c, '${SEAWEED_COMMAND}']
    volumes:
      - seaweed-volume-data:/data
      - seaweed-filer-data:/root/.seaweedfs
  api:
    build: .
    command: [sh, -c, '${API_COMMAND}']
    restart: ${API_RESTART}
  migrate:
    image: postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
    command: [sh, -c, 'exit 0']
volumes:
  postgres-data:
  seaweed-volume-data:
  seaweed-filer-data:
YAML

set_env() {
  key=$1 value=$2
  python3 - "$fixture/.env" "$key" "$value" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
key, value = sys.argv[2:]
lines = p.read_text().splitlines()
replacements = [index for index, line in enumerate(lines) if line.partition("=")[0] == key]
if len(replacements) != 1:
    raise SystemExit(f"expected exactly one {key} entry")
lines[replacements[0]] = f"{key}={value}"
p.write_text("\n".join(lines) + "\n")
PY
}

remove_last_line() {
  python3 - "$1" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
lines = p.read_text().splitlines()
if not lines:
    raise SystemExit("cannot remove a line from an empty file")
p.write_text("\n".join(lines[:-1]) + ("\n" if len(lines) > 1 else ""))
PY
}

run_capture() {
  output=$1; shift
  set +e
  "$@" >"$output" 2>&1
  command_status=$?
  set -e
}

failures=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; failures=$((failures + 1)); }

set -e
"${compose[@]}" create api >/dev/null
"${compose[@]}" up -d --wait postgres >/dev/null
"${compose[@]}" up -d seaweedfs >/dev/null
"${compose[@]}" exec -T postgres psql -U recovery -d recovery -c "create table recovery_sentinel (value text); insert into recovery_sentinel values ('backup-sentinel');" >/dev/null
docker run --rm -v "${source_project}_seaweed-volume-data:/data" postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f sh -c 'printf volume-sentinel > /data/backup-sentinel'
docker run --rm -v "${source_project}_seaweed-filer-data:/data" postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f sh -c 'printf filer-sentinel > /data/backup-sentinel'
mkdir "$fixture/accepted backup" "$fixture/just archive" "$fixture/refusal output" "$fixture/cleanup warning"

# Created API and exited SeaweedFS are clean stopped states, and must complete a real backup.
run_capture "$fixture/accepted.out" bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$fixture/accepted backup"
if test "$command_status" = 0 && test -f "$(find "$fixture/accepted backup" -name manifest.json -print -quit)"; then
  pass "backup accepts actual created/exited clean states"
else
  fail "backup did not accept created/exited clean states (rc=$command_status; $(tail -1 "$fixture/accepted.out"))"
fi
archive=$(find "$fixture/accepted backup" -mindepth 1 -maxdepth 1 -type d -print -quit)
if python3 - "$archive" "$(id -u)" <<'PY'
from pathlib import Path
import stat, sys
archive, owner = Path(sys.argv[1]), int(sys.argv[2])
files = list(archive.iterdir())
if stat.S_IMODE(archive.stat().st_mode) != 0o700 or not files:
    raise SystemExit(1)
if any(stat.S_IMODE(path.stat().st_mode) != 0o600 or path.stat().st_uid != owner for path in files):
    raise SystemExit(1)
PY
then
  pass "backup artifacts are private and owned by the invoking operator"
else
  fail "backup artifacts are not private and owned by the invoking operator"
fi

# The effective configuration must retain a caller-selected overlay while the API
# is build-only and has no explicit image field.
cat > "$fixture/docker-compose.overlay.yml" <<'YAML'
services:
  api:
    environment:
      PEPPY_FIXTURE_OVERLAY: "1"
YAML
mkdir "$fixture/overlay backup"
run_capture "$fixture/overlay.out" env COMPOSE_FILE=docker-compose.yml:docker-compose.overlay.yml bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$fixture/overlay backup"
if test "$command_status" = 0 && test -f "$(find "$fixture/overlay backup" -name manifest.json -print -quit)"; then
  pass "backup uses the caller-selected Compose overlay with a build-only API"
else
  fail "backup did not accept the effective build-only overlay configuration (rc=$command_status; $(tail -1 "$fixture/overlay.out"))"
fi

# Just must pass an adversarial path as one literal argument, without command substitution.
mkdir -p "$fixture/infra/compose"
ln -s "$repo/infra/compose/backup.sh" "$fixture/infra/compose/backup.sh"
ln -s "$repo/infra/compose/restore.sh" "$fixture/infra/compose/restore.sh"
weird='just archive ";$(touch just-injected); echo "'
mv "$fixture/just archive" "$fixture/$weird"
run_capture "$fixture/just.out" just --justfile "$repo/justfile" --working-directory "$fixture" backup "$weird"
if test "$command_status" = 0 && test ! -e "$fixture/just-injected" && test -n "$(find "$fixture/$weird" -name manifest.json -print -quit)"; then
  pass "just backup passes shell metacharacters literally"
else
  cat "$fixture/just.out" >&2
  fail "just backup argument was not passed literally (rc=$command_status, injected=$(test -e "$fixture/just-injected" && echo yes || echo no))"
fi

# Restore must reject an incomplete checksum index before creating any target volume.
truncated="$fixture/truncated archive"
cp -R "$archive" "$truncated"
remove_last_line "$truncated/manifest.sha256"
target="recoverytruncated${suffix}"; created_projects="$created_projects $target"
run_capture "$fixture/truncated.out" bash -c 'cd "$1" && "$2" "$3" "$4"' _ "$fixture" "$repo/infra/compose/restore.sh" "$truncated" "$target"
if test "$command_status" != 0 && test -z "$(docker volume ls --format '{{.Name}}' | grep "^${target}_" || true)"; then
  pass "restore rejects incomplete checksum index before target creation"
else
  fail "restore accepted incomplete checksum index or created target volumes (rc=$command_status)"
fi

# A duplicate checksum entry must not substitute for a required artifact.
duplicated="$fixture/duplicated checksum archive"
cp -R "$archive" "$duplicated"
remove_last_line "$duplicated/manifest.sha256"
head -1 "$duplicated/manifest.sha256" >> "$duplicated/manifest.sha256"
duplicate_target="recoveryduplicate${suffix}"; created_projects="$created_projects $duplicate_target"
run_capture "$fixture/duplicate.out" bash -c 'cd "$1" && "$2" "$3" "$4"' _ "$fixture" "$repo/infra/compose/restore.sh" "$duplicated" "$duplicate_target"
if test "$command_status" != 0 && test -z "$(docker volume ls --format '{{.Name}}' | grep "^${duplicate_target}_" || true)"; then
  pass "restore rejects duplicate checksum entries before target creation"
else
  fail "restore accepted duplicate checksum entries or created target volumes (rc=$command_status)"
fi

# The restore wrapper must also pass an adversarial archive path as one literal argument.
restore_weird="restore archive '\";\$(touch restore-injected); echo \""
cp -R "$archive" "$fixture/$restore_weird"
restore_quoted="recoveryquoted${suffix}"; created_projects="$created_projects $restore_quoted"
run_capture "$fixture/just-restore.out" just --justfile "$repo/justfile" --working-directory "$fixture" restore "$restore_weird" "$restore_quoted"
if test "$command_status" = 0 && test ! -e "$fixture/restore-injected" && test "$(docker inspect --format '{{.State.Status}}' "${restore_quoted}-postgres-1")" = running && test "$(docker exec "${restore_quoted}-postgres-1" psql -U recovery -d recovery -tAc 'select value from recovery_sentinel')" = backup-sentinel && test "$(docker run --rm -v "${restore_quoted}_seaweed-volume-data:/data" postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f cat /data/backup-sentinel)" = volume-sentinel && test "$(docker run --rm -v "${restore_quoted}_seaweed-filer-data:/data" postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f cat /data/backup-sentinel)" = filer-sentinel; then
  pass "just restore passes shell metacharacters literally and restores database and volume sentinels"
else
  cat "$fixture/just-restore.out" >&2
  fail "restore did not preserve literal arguments and all storage sentinels (rc=$command_status, injected=$(test -e "$fixture/restore-injected" && echo yes || echo no))"
fi

# A paused source service must be refused before a backup directory is created.
set_env SEAWEED_COMMAND 'sleep infinity'
"${compose[@]}" up -d --force-recreate seaweedfs >/dev/null
docker pause "${source_project}-seaweedfs-1" >/dev/null
paused_dest="$fixture/paused refusal"; mkdir "$paused_dest"
run_capture "$fixture/paused.out" bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$paused_dest"
if test "$command_status" != 0 && grep -q 'invalid state.*paused\|paused.*invalid state' "$fixture/paused.out" && test -z "$(find "$paused_dest" -mindepth 1 -print -quit)"; then
  pass "backup rejects an actual paused service"
else
  fail "backup did not explicitly reject paused state (rc=$command_status)"
fi
docker unpause "${source_project}-seaweedfs-1" >/dev/null
"${compose[@]}" stop seaweedfs >/dev/null

# A crash-looping API must be refused before source services are stopped.
set_env API_COMMAND 'exit 1'
set_env API_RESTART always
"${compose[@]}" up -d --force-recreate api >/dev/null
restart_dest="$fixture/restarting refusal"; mkdir "$restart_dest"
run_capture "$fixture/restarting.out" bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$restart_dest"
api_state=$(docker inspect --format '{{.State.Status}}' "${source_project}-api-1")
if test "$command_status" != 0 && grep -q 'invalid state.*restarting\|restarting.*invalid state' "$fixture/restarting.out" && test "$api_state" = restarting; then
  pass "backup rejects an actual crash-looping service"
else
  fail "backup did not explicitly reject crash-loop state (rc=$command_status, state=$api_state)"
fi
"${compose[@]}" rm -sf api >/dev/null
set_env API_COMMAND 'exit 0'; set_env API_RESTART no
"${compose[@]}" create api >/dev/null

# Running/configured source image mismatch must fail before PostgreSQL is stopped.
set_env POSTGRES_IMAGE postgres:16
mismatch_dest="$fixture/image mismatch"; mkdir "$mismatch_dest"
run_capture "$fixture/image.out" bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$mismatch_dest"
pg_state=$(docker inspect --format '{{.State.Status}}' "${source_project}-postgres-1")
if test "$command_status" = 67 && test "$pg_state" = running && test -z "$(find "$mismatch_dest" -mindepth 1 -print -quit)"; then
  pass "backup rejects PostgreSQL image mismatch before stop"
else
  fail "backup did not reject PostgreSQL image mismatch before stop (rc=$command_status, state=$pg_state)"
fi
set_env POSTGRES_IMAGE 'postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f'

# Restore must abort immediately when PostgreSQL cannot start.
set_env SEAWEED_COMMAND 'exit 0'
python3 - "$fixture/docker-compose.yml" <<'PY'
from pathlib import Path
import sys
p=Path(sys.argv[1]); s=p.read_text()
s=s.replace("    healthcheck:\n      test: [CMD-SHELL, 'pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB']", "    command: [sh, -c, 'exit 1']\n    healthcheck:\n      test: [CMD-SHELL, 'exit 1']")
p.write_text(s)
PY
startfail="recoverystartfail${suffix}"; created_projects="$created_projects $startfail"
run_capture "$fixture/startfail.out" bash -c 'cd "$1" && "$2" "$3" "$4"' _ "$fixture" "$repo/infra/compose/restore.sh" "$archive" "$startfail"
if test "$command_status" = 74 && grep -q 'restore aborted' "$fixture/startfail.out" && ! grep -q 'pg_restore:' "$fixture/startfail.out"; then
  pass "restore aborts before pg_restore when PostgreSQL startup fails"
else
  fail "restore did not fail closed on PostgreSQL startup failure (rc=$command_status)"
fi
# Restore normal fixture definition for cleanup and warning test.
python3 - "$fixture/docker-compose.yml" <<'PY'
from pathlib import Path
import sys
p=Path(sys.argv[1]); s=p.read_text()
s=s.replace("    command: [sh, -c, 'exit 1']\n    healthcheck:\n      test: [CMD-SHELL, 'exit 1']", "    healthcheck:\n      test: [CMD-SHELL, 'pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB']")
p.write_text(s)
PY

# Backup cleanup restart failure must be visible while preserving the successful backup status.
set_env SEAWEED_COMMAND 'sleep infinity'
"${compose[@]}" up -d --force-recreate seaweedfs >/dev/null
mkdir "$fixture/fakebin"
cat > "$fixture/fakebin/docker" <<EOF_DOCKER
#!/usr/bin/env bash
for arg in "\$@"; do
  test "\$arg" = start && saw_start=1
  if test "\${saw_start:-0}" = 1 && test "\$arg" = seaweedfs; then exit 42; fi
done
exec "$real_docker" "\$@"
EOF_DOCKER
chmod +x "$fixture/fakebin/docker"
run_capture "$fixture/cleanup-warning.out" env PATH="$fixture/fakebin:$PATH" bash -c 'cd "$1" && "$2" "$3"' _ "$fixture" "$repo/infra/compose/backup.sh" "$fixture/cleanup warning"
if test "$command_status" = 0 && grep -q 'warning: failed to restart seaweedfs' "$fixture/cleanup-warning.out"; then
  pass "backup warns when cleanup cannot restart a previously running service"
else
  fail "backup cleanup restart failure was silent or changed success status (rc=$command_status)"
fi
"${compose[@]}" start seaweedfs >/dev/null

if test "$failures" = 0; then
  echo "All behavioral recovery fixtures passed"
  exit 0
fi
echo "$failures behavioral recovery fixture(s) failed" >&2
exit 1
