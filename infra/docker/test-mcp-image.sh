#!/usr/bin/env bash
# Verify the MCP worker can initialize and restart with fresh private volumes.
set -euo pipefail

image=${1:-peppy-mcp:ci}
name="peppy-mcp-image-smoke-$$"
state_volume="${name}-state"
control_volume="${name}-control"
verification_volume="${name}-verification"

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm -f "$state_volume" "$control_volume" "$verification_volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT

health_failure() {
  echo "MCP image health check did not become ready; sanitized container logs follow:" >&2
  docker logs "$name" 2>&1 \
    | sed -E 's/([[:alnum:]_.-]*(token|key|secret|password)[[:alnum:]_.-]*[=:])[[:graph:]]+/\1[REDACTED]/Ig' \
    | sed -E 's/[[:alnum:]+\/_=-]{20,}/[REDACTED]/g' >&2 \
    || true
  exit 1
}

docker volume create "$state_volume" >/dev/null
docker volume create "$control_volume" >/dev/null
docker volume create "$verification_volume" >/dev/null

docker run --rm --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges:true --entrypoint /bin/sh "$image" -ec '
    test "$(id -u)" = 10001
    test -s /etc/ssl/certs/ca-certificates.crt
    test -x /usr/local/bin/peppy-mcp
    command -v ldd >/dev/null
    ! ldd /usr/local/bin/peppy-mcp | grep "not found"
  '

# Match the Compose initializer without starting API, database, or user services.
docker run --rm --read-only --cap-drop ALL --cap-add CHOWN --cap-add FOWNER \
  --security-opt no-new-privileges:true --user 0:0 \
  -v "$state_volume:/var/lib/peppy-mcp" \
  -v "$control_volume:/run/peppy-mcp" \
  -v "$verification_volume:/verification" \
  --entrypoint /bin/sh "$image" -ec '
    mkdir -p /var/lib/peppy-mcp /run/peppy-mcp /verification
    chown 10001:10001 /var/lib/peppy-mcp /run/peppy-mcp /verification
    chmod 0700 /var/lib/peppy-mcp /run/peppy-mcp /verification
  '

docker run -d --name "$name" --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges:true --tmpfs /tmp \
  -v "$state_volume:/var/lib/peppy-mcp" \
  -v "$control_volume:/run/peppy-mcp" \
  -v "$verification_volume:/verification" \
  -e PEPPY_MCP_BIND_ADDR=0.0.0.0:8091 \
  -e PEPPY_MCP_STATE_DIR=/var/lib/peppy-mcp \
  -e PEPPY_MCP_API_URL=http://api:8080 \
  -e PUBLIC_API_URL=https://ci-smoke.invalid \
  -e PEPPY_MCP_WORKER_TOKEN_FILE=/run/peppy-mcp/control-token \
  -e PEPPY_MCP_KEY_FILE=/var/lib/peppy-mcp/storage-key \
  "$image" >/dev/null

healthy=0
for _ in {1..10}; do
  if docker exec "$name" curl --fail --silent --show-error http://127.0.0.1:8091/healthz >/dev/null; then
    healthy=1
    break
  fi
  sleep 1
done
test "$healthy" = 1 || health_failure
docker exec "$name" /bin/sh -ec '
  test "$(id -u)" = 10001
  test -s /run/peppy-mcp/control-token
  test -s /var/lib/peppy-mcp/storage-key
  test "$(stat -c %u:%a /run/peppy-mcp/control-token)" = 10001:600
  test "$(stat -c %u:%a /var/lib/peppy-mcp/storage-key)" = 10001:600
  test "$(stat -c %u:%a /run/peppy-mcp)" = 10001:700
  test "$(stat -c %u:%a /var/lib/peppy-mcp)" = 10001:700
  test "$(stat -c %s /var/lib/peppy-mcp/storage-key)" = 32
  ! cmp -s /run/peppy-mcp/control-token /var/lib/peppy-mcp/storage-key
  sha256sum /run/peppy-mcp/control-token /var/lib/peppy-mcp/storage-key > /verification/secret-digests
  chmod 0600 /verification/secret-digests
'

docker restart "$name" >/dev/null
docker exec "$name" curl --fail --silent --show-error --retry 10 --retry-connrefused --retry-delay 1 \
  http://127.0.0.1:8091/healthz >/dev/null
docker exec "$name" /bin/sh -ec '
  sha256sum --check --status /verification/secret-digests
  rm -f /verification/secret-digests
'
