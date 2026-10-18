#!/usr/bin/env bash
# Source this file, then use peppy_test_infra_up before integration tests.
# The stack is intentionally private and disposable; credentials are test-only.

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  echo "source infra/compose/test-env.sh; peppy_test_infra_up" >&2
  exit 2
fi

export PEPPY_ENV=test
export DATABASE_URL='postgres://peppy_test:peppy-test-password@127.0.0.1:5432/peppy_test'
export TEST_DATABASE_URL="$DATABASE_URL"
export S3_INTERNAL_ENDPOINT='http://127.0.0.1:8333'
export S3_ACCESS_KEY='peppy-test-access'
export S3_SECRET_KEY='peppy-test-secret'
export S3_BUCKET='peppy-test'
export PUBLIC_API_URL='http://127.0.0.1:8080'
export PUBLIC_ATTACHMENT_URL='http://127.0.0.1:8080'

PEPPY_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PEPPY_REPO_ROOT

: "${PEPPY_TEST_COMPOSE_PROJECT:=peppy-test-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}}"
export PEPPY_TEST_COMPOSE_PROJECT

peppy_test_compose() {
  docker compose --project-name "$PEPPY_TEST_COMPOSE_PROJECT" -f - "$@" <<'YAML'
services:
  postgres:
    image: postgres:18.1@sha256:1090bc3a8ccfb0b55f78a494d76f8d603434f7e4553543d6e807bc7bd6bbd17f
    environment:
      POSTGRES_DB: peppy_test
      POSTGRES_USER: peppy_test
      POSTGRES_PASSWORD: peppy-test-password
    ports: ["127.0.0.1:5432:5432"]
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
    ports: ["127.0.0.1:8333:8333"]
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
    file: ${PEPPY_REPO_ROOT}/infra/docker/seaweedfs-entrypoint.sh
  s3-config:
    content: |
      {"identities":[{"name":"peppy-test","credentials":[{"accessKey":"peppy-test-access","secretKey":"peppy-test-secret"}],"actions":["Read","Write","List","Tagging","Admin"]}]}
YAML
}

peppy_test_infra_up() {
  peppy_test_compose up --detach --wait
  peppy_test_bucket_probe
}

peppy_test_bucket_probe() {
  local status
  status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    --aws-sigv4 'aws:amz:us-east-1:s3' \
    --user "$S3_ACCESS_KEY:$S3_SECRET_KEY" \
    --request PUT "$S3_INTERNAL_ENDPOINT/$S3_BUCKET")
  case "$status" in
    200|201|204|409) ;;
    *) echo "failed to create test S3 bucket (HTTP $status)" >&2; return 1 ;;
  esac

  status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
    "$S3_INTERNAL_ENDPOINT/$S3_BUCKET")
  case "$status" in
    401|403) ;;
    *) echo "test S3 bucket allowed anonymous access (HTTP $status)" >&2; return 1 ;;
  esac
}

peppy_test_infra_down() {
  peppy_test_compose down --volumes --remove-orphans
}
