#!/usr/bin/env bash
# Source this file, then use peppy_test_infra_up before integration tests.
# The stack is intentionally private and disposable; credentials are test-only.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "source infra/compose/test-env.sh; peppy_test_infra_up" >&2
  exit 2
fi

export PEPPY_ENV=test
if [[ -n "${GITHUB_RUN_ID:-}" ]]; then
  export DATABASE_URL='postgres://peppy_test:peppy-test-password@127.0.0.1:5432/peppy_test'
  export TEST_DATABASE_URL="$DATABASE_URL"
  export S3_INTERNAL_ENDPOINT='http://127.0.0.1:8333'
else
  unset DATABASE_URL TEST_DATABASE_URL S3_INTERNAL_ENDPOINT
fi
export S3_ACCESS_KEY='peppy-test-access'
export S3_SECRET_KEY='peppy-test-secret'
export S3_BUCKET='peppy-test'
export PUBLIC_API_URL='http://127.0.0.1:8080'

PEPPY_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
export PEPPY_REPO_ROOT

if [[ -z "${PEPPY_TEST_COMPOSE_PROJECT:-}" ]]; then
  if [[ -n "${GITHUB_RUN_ID:-}" ]]; then
    PEPPY_TEST_COMPOSE_PROJECT="peppy-test-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}"
  else
    PEPPY_TEST_COMPOSE_PROJECT="peppy-test-$(python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.argv[1].encode()).hexdigest()[:12])' "$PEPPY_REPO_ROOT")"
  fi
fi
export PEPPY_TEST_COMPOSE_PROJECT

# Set either variable to a host port to override the local ephemeral or CI fixed default.
: "${PEPPY_TEST_POSTGRES_PORT:=${GITHUB_RUN_ID:+5432}}"
: "${PEPPY_TEST_S3_PORT:=${GITHUB_RUN_ID:+8333}}"
export PEPPY_TEST_POSTGRES_PORT PEPPY_TEST_S3_PORT
unset PEPPY_TEST_SEAWEED_ENTRYPOINT
_peppy_test_owned_entrypoint=
_peppy_test_cleaned=0
_peppy_test_refused=0

peppy_test_entrypoint_create() {
  mkdir -p "$PEPPY_REPO_ROOT/target/test-tmp" || return
  _peppy_test_owned_entrypoint=$(mktemp "$PEPPY_REPO_ROOT/target/test-tmp/seaweed-entrypoint.XXXXXXXX") || return
  export PEPPY_TEST_SEAWEED_ENTRYPOINT="$_peppy_test_owned_entrypoint"
  python3 "$PEPPY_REPO_ROOT/infra/compose/extract-config.py" "$PEPPY_REPO_ROOT/docker-compose.yml" seaweed-entrypoint > "$PEPPY_TEST_SEAWEED_ENTRYPOINT"
}

peppy_test_entrypoint_remove() {
  if [[ -n "$_peppy_test_owned_entrypoint" ]]; then
    rm -f -- "$_peppy_test_owned_entrypoint"
    _peppy_test_owned_entrypoint=
  fi
  unset PEPPY_TEST_SEAWEED_ENTRYPOINT
}

peppy_test_compose() {
  docker compose --project-name "$PEPPY_TEST_COMPOSE_PROJECT" -f - "$@" <<'YAML'
services:
  postgres:
    image: postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
    environment:
      POSTGRES_DB: peppy_test
      POSTGRES_USER: peppy_test
      POSTGRES_PASSWORD: peppy-test-password
    ports: ["127.0.0.1:${PEPPY_TEST_POSTGRES_PORT}:5432"]
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $$POSTGRES_USER -d $$POSTGRES_DB"]
      interval: 2s
      timeout: 2s
      retries: 30
  seaweedfs:
    image: chrislusf/seaweedfs:4.48@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d
    entrypoint: ["/bin/sh", "/usr/local/bin/peppy-seaweedfs-entrypoint.sh"]
    command: >-
      server -s3 -s3.port=8333 -s3.config=/etc/seaweedfs/s3.json -dir=/data -master.volumeSizeLimitMB=64
      -master.port=9333 -volume.port=8080 -filer -filer.port=8888 -filer.encryptVolumeData -ip=seaweedfs
    ports: ["127.0.0.1:${PEPPY_TEST_S3_PORT}:8333"]
    configs:
      - source: s3-config
        target: /etc/seaweedfs/s3.json
      - source: seaweed-entrypoint
        target: /usr/local/bin/peppy-seaweedfs-entrypoint.sh
    healthcheck:
      test: ["CMD-SHELL", "wget -S -O /dev/null http://127.0.0.1:8333/ 2>&1 | grep -Eq 'HTTP/.* (401|403)'"]
      interval: 2s
      timeout: 2s
      retries: 30
configs:
  seaweed-entrypoint:
    file: ${PEPPY_TEST_SEAWEED_ENTRYPOINT}
  s3-config:
    content: |
      {"identities":[{"name":"peppy-test","credentials":[{"accessKey":"peppy-test-access","secretKey":"peppy-test-secret"}],"actions":["Read","Write","List","Tagging","Admin"]}]}
YAML
}

peppy_test_infra_up() {
  local running status postgres_port s3_port
  if [[ -z "${GITHUB_RUN_ID:-}" ]]; then
    unset DATABASE_URL TEST_DATABASE_URL S3_INTERNAL_ENDPOINT
  fi
  if [[ -n "$_peppy_test_owned_entrypoint" ]]; then
    echo "test infrastructure is already owned by this shell; run peppy_test_infra_down first" >&2
    return 1
  fi
  _peppy_test_refused=1
  if peppy_test_entrypoint_create; then :; else
    status=$?
    peppy_test_entrypoint_remove
    return "$status"
  fi
  if running=$(peppy_test_compose ps --all --quiet); then :; else
    status=$?
    peppy_test_entrypoint_remove
    return "$status"
  fi
  if [[ -n "$running" ]]; then
    echo "test project $PEPPY_TEST_COMPOSE_PROJECT already has containers; stop it from its owning shell, or use another PEPPY_TEST_COMPOSE_PROJECT." >&2
    printf 'To remove a stale stack manually: docker compose --project-name %q down --volumes --remove-orphans\n' "$PEPPY_TEST_COMPOSE_PROJECT" >&2
    _peppy_test_refused=1
    peppy_test_entrypoint_remove
    return 1
  fi
  if peppy_test_compose down --volumes --remove-orphans; then :; else
    status=$?
    peppy_test_entrypoint_remove
    return "$status"
  fi
  _peppy_test_refused=0
  _peppy_test_cleaned=0
  if peppy_test_compose up --detach --wait &&
    postgres_port=$(peppy_test_compose port postgres 5432) &&
    s3_port=$(peppy_test_compose port seaweedfs 8333); then
    postgres_port=${postgres_port##*:}
    s3_port=${s3_port##*:}
    if [[ "$postgres_port" =~ ^[0-9]+$ && "$s3_port" =~ ^[0-9]+$ ]]; then
      export DATABASE_URL="postgres://peppy_test:peppy-test-password@127.0.0.1:$postgres_port/peppy_test"
      export TEST_DATABASE_URL="$DATABASE_URL"
      export S3_INTERNAL_ENDPOINT="http://127.0.0.1:$s3_port"
      if peppy_test_bucket_probe; then return 0; else status=$?; fi
    else
      echo "invalid compose port discovery output" >&2
      status=1
    fi
  else
    status=$?
  fi
  peppy_test_compose logs || echo "failed to collect test infrastructure logs" >&2
  peppy_test_infra_down || echo "failed to clean up test infrastructure" >&2
  if [[ -z "${GITHUB_RUN_ID:-}" ]]; then
    unset DATABASE_URL TEST_DATABASE_URL S3_INTERNAL_ENDPOINT
  fi
  return "$status"
}

peppy_test_bucket_probe() {
  local status
  status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --aws-sigv4 'aws:amz:us-east-1:s3' \
    --user "$S3_ACCESS_KEY:$S3_SECRET_KEY" \
    --request PUT "$S3_INTERNAL_ENDPOINT/$S3_BUCKET") || return $?
  case "$status" in
    200|201|204|409) ;;
    *) echo "failed to create test S3 bucket (HTTP $status)" >&2; return 1 ;;
  esac

  status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    "$S3_INTERNAL_ENDPOINT/$S3_BUCKET") || return $?
  case "$status" in
    401|403) ;;
    *) echo "test S3 bucket allowed anonymous access (HTTP $status)" >&2; return 1 ;;
  esac
}

peppy_test_infra_down() {
  local status=0
  if [[ "$_peppy_test_refused" == 1 ]]; then
    return 0
  fi
  if [[ "$_peppy_test_cleaned" != 1 ]]; then
    if [[ -z "$_peppy_test_owned_entrypoint" ]]; then
      peppy_test_entrypoint_create || { status=$?; peppy_test_entrypoint_remove; return "$status"; }
    fi
    if peppy_test_compose down --volumes --remove-orphans; then
      _peppy_test_cleaned=1
    else
      status=$?
    fi
  fi
  peppy_test_entrypoint_remove
  return "$status"
}
