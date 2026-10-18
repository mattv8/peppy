#!/usr/bin/env bash
set -euo pipefail

readonly IMAGE='chrislusf/seaweedfs:4.48@sha256:4e61d15fd35994cb1e43e1e553dff106794841fd9a99ade2fc8c8bfce4d7872d'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
readonly ROOT
readonly ENTRYPOINT="$ROOT/infra/docker/seaweedfs-entrypoint.sh"
readonly CONTAINER_ENTRYPOINT='/usr/local/bin/peppy-seaweedfs-entrypoint.sh'

scratch_root="$ROOT/.opencode/sessions"
mkdir -p "$scratch_root"
fixture=$(mktemp -d "$scratch_root/seaweed-entrypoint.XXXXXX")

cleanup_fixture() {
  docker run --rm \
    --mount "type=bind,src=$fixture,dst=/fixture" \
    --entrypoint /bin/sh "$IMAGE" \
    -c 'find /fixture -mindepth 1 -maxdepth 1 -type d -exec rm -rf {} +' >/dev/null 2>&1 || true
  find "$fixture" -type d -exec chmod u+rwx {} + >/dev/null 2>&1 || true
  rm -rf "$fixture"
}

trap cleanup_fixture EXIT
state="$fixture/state"
data="$fixture/data"
stub="$fixture/entrypoint.sh"
mkdir "$state" "$data"

cat >"$stub" <<'SH'
#!/bin/sh
test "$(cat /root/.seaweedfs/.peppy-s3-sse-kek)" = "$WEED_S3_SSE_KEK"
test "$1" = seaweedfs-test-command
test "$2" = retained-argument
SH
chmod 700 "$stub"

fail_test() {
  echo "SeaweedFS lifecycle assertion failed: $1" >&2
  exit 1
}

run_entrypoint() {
  docker run --rm \
    --mount "type=bind,src=$1,dst=/root/.seaweedfs" \
    --mount "type=bind,src=$2,dst=/data" \
    --mount "type=bind,src=$ENTRYPOINT,dst=$CONTAINER_ENTRYPOINT,readonly" \
    --mount "type=bind,src=$stub,dst=/entrypoint.sh,readonly" \
    --entrypoint /bin/sh "$IMAGE" "$CONTAINER_ENTRYPOINT" seaweedfs-test-command retained-argument
}

run_entrypoint_with_external_kek() {
  docker run --rm -e WEED_S3_SSE_KEK=externally-supplied-value \
    --mount "type=bind,src=$1,dst=/root/.seaweedfs" \
    --mount "type=bind,src=$2,dst=/data" \
    --mount "type=bind,src=$ENTRYPOINT,dst=$CONTAINER_ENTRYPOINT,readonly" \
    --mount "type=bind,src=$stub,dst=/entrypoint.sh,readonly" \
    --entrypoint /bin/sh "$IMAGE" "$CONTAINER_ENTRYPOINT" seaweedfs-test-command retained-argument
}

run_entrypoint_with_image_data_skeleton() {
  docker run --rm \
    --mount "type=bind,src=$1,dst=/root/.seaweedfs" \
    --mount "type=bind,src=$ENTRYPOINT,dst=$CONTAINER_ENTRYPOINT,readonly" \
    --mount "type=bind,src=$stub,dst=/entrypoint.sh,readonly" \
    --entrypoint /bin/sh "$IMAGE" "$CONTAINER_ENTRYPOINT" seaweedfs-test-command retained-argument
}

run_silently() {
  local output
  output=$(run_entrypoint "$1" "$2" 2>&1) || return 1
  [[ -z "$output" ]] || fail_test 'successful entrypoint emitted output'
}

read_key() {
  docker run --rm \
    --mount "type=bind,src=$1,dst=/root/.seaweedfs,readonly" \
    --entrypoint /bin/sh "$IMAGE" \
    -c 'cat /root/.seaweedfs/.peppy-s3-sse-kek'
}

set_key_permissions() {
  docker run --rm \
    --mount "type=bind,src=$1,dst=/root/.seaweedfs" \
    --entrypoint /bin/sh "$IMAGE" \
    -c 'chmod "$1" /root/.seaweedfs/.peppy-s3-sse-kek' -- "$2"
}

key_contents_match() {
  docker run --rm \
    --mount "type=bind,src=$fixture,dst=/fixture,readonly" \
    --entrypoint /bin/sh "$IMAGE" \
    -c 'test "$(od -An -tx1 "$1")" = "$(od -An -tx1 "$2")"' -- "$1" "$2"
}

expect_entrypoint_failure() {
  local state_dir=$1
  local data_dir=$2
  local output
  if output=$(run_entrypoint "$state_dir" "$data_dir" 2>&1); then
    echo 'entrypoint unexpectedly succeeded' >&2
    exit 1
  fi
  [[ "$output" == *'SeaweedFS KEK bootstrap failed:'* ]] || fail_test 'negative case missed helper diagnostic'
  [[ "$output" != *"$key"* ]] || fail_test 'negative case exposed a generated key'
}

run_silently "$state" "$data"
key=$(read_key "$state")
[[ "$key" =~ ^[0-9a-f]{64}$ ]] || fail_test 'generated key format is invalid'
docker run --rm --mount "type=bind,src=$state,dst=/root/.seaweedfs,readonly" \
  --entrypoint /bin/sh "$IMAGE" -c 'test "$(stat -c %a /root/.seaweedfs/.peppy-s3-sse-kek)" = 600' \
  || fail_test 'generated key permissions are not 600'

run_silently "$state" "$data"
[[ "$(read_key "$state")" == "$key" ]] || fail_test 'restart changed the generated key'

image_skeleton_state="$fixture/image-skeleton-state"
mkdir "$image_skeleton_state"
output=$(run_entrypoint_with_image_data_skeleton "$image_skeleton_state" 2>&1)
[[ -z "$output" ]] || fail_test 'fresh image skeleton emitted output'
[[ "$(read_key "$image_skeleton_state")" =~ ^[0-9a-f]{64}$ ]] || fail_test 'fresh image skeleton did not create a key'

output=$(run_entrypoint_with_external_kek "$state" "$data" 2>&1)
[[ -z "$output" ]] || fail_test 'external KEK run emitted output'
[[ "$(read_key "$state")" == "$key" ]] || fail_test 'external KEK changed the persisted key'

malformed_state="$fixture/malformed"
malformed_data="$fixture/malformed-data"
expected_malformed_key="$fixture/expected-malformed-key"
mkdir "$malformed_state" "$malformed_data"
printf '%s\n' "$key" >"$malformed_state/.peppy-s3-sse-kek"
cp "$malformed_state/.peppy-s3-sse-kek" "$expected_malformed_key"
expect_entrypoint_failure "$malformed_state" "$malformed_data"
key_contents_match /fixture/malformed/.peppy-s3-sse-kek /fixture/expected-malformed-key \
  || fail_test 'malformed stored key changed'

skeleton_data_state="$fixture/skeleton-data-state"
skeleton_data="$fixture/skeleton-data"
mkdir "$skeleton_data_state" "$skeleton_data" "$skeleton_data/filerldb2"
touch "$skeleton_data/filerldb2/metadata"
expect_entrypoint_failure "$skeleton_data_state" "$skeleton_data"
[[ ! -e "$skeleton_data_state/.peppy-s3-sse-kek" ]] || fail_test 'skeleton state generated a key'

missing_key_state="$fixture/missing-key"
missing_key_data="$fixture/missing-key-data"
mkdir "$missing_key_state" "$missing_key_data"
touch "$missing_key_state/filer.toml"
expect_entrypoint_failure "$missing_key_state" "$missing_key_data"
[[ ! -e "$missing_key_state/.peppy-s3-sse-kek" ]] || fail_test 'initialized filer state generated a key'

missing_key_data_state="$fixture/missing-key-data-state"
missing_key_data_filer="$fixture/missing-key-data-filer"
mkdir "$missing_key_data_state" "$missing_key_data_filer"
touch "$missing_key_data_state/1.dat"
expect_entrypoint_failure "$missing_key_data_filer" "$missing_key_data_state"
[[ ! -e "$missing_key_data_filer/.peppy-s3-sse-kek" ]] || fail_test 'initialized volume state generated a key'

symlink_state="$fixture/symlink"
symlink_data="$fixture/symlink-data"
mkdir "$symlink_state" "$symlink_data"
printf '%s' "$key" >"$symlink_state/target"
ln -s target "$symlink_state/.peppy-s3-sse-kek"
expect_entrypoint_failure "$symlink_state" "$symlink_data"
[[ -L "$symlink_state/.peppy-s3-sse-kek" ]] || fail_test 'symlinked key was replaced'

set_key_permissions "$state" 644
expect_entrypoint_failure "$state" "$data"
[[ "$(read_key "$state")" == "$key" ]] || fail_test 'insecure key was changed'
set_key_permissions "$state" 600

readonly_state="$fixture/readonly-state"
readonly_data="$fixture/readonly-data"
mkdir "$readonly_state" "$readonly_data"
if output=$(docker run --rm \
  --mount "type=bind,src=$readonly_state,dst=/root/.seaweedfs,readonly" \
  --mount "type=bind,src=$readonly_data,dst=/data,readonly" \
  --mount "type=bind,src=$ENTRYPOINT,dst=$CONTAINER_ENTRYPOINT,readonly" \
  --mount "type=bind,src=$stub,dst=/entrypoint.sh,readonly" \
  --entrypoint /bin/sh "$IMAGE" "$CONTAINER_ENTRYPOINT" seaweedfs-test-command retained-argument 2>&1); then
  exit 1
fi
[[ "$output" == *'SeaweedFS KEK bootstrap failed:'* ]] || fail_test 'readonly case missed helper diagnostic'
[[ ! -e "$readonly_state/.peppy-s3-sse-kek" ]] || fail_test 'readonly state generated a key'

restricted_root="$fixture/restricted-root"
restricted_state="$restricted_root/.seaweedfs"
restricted_data="$fixture/restricted-data"
mkdir -p "$restricted_state"
mkdir "$restricted_data"
chmod 755 "$restricted_root"
chmod 400 "$restricted_state"
if output=$(docker run --rm --user 65534:65534 \
  --mount "type=bind,src=$restricted_root,dst=/root" \
  --mount "type=bind,src=$restricted_data,dst=/data" \
  --mount "type=bind,src=$ENTRYPOINT,dst=$CONTAINER_ENTRYPOINT,readonly" \
  --mount "type=bind,src=$stub,dst=/entrypoint.sh,readonly" \
  --entrypoint /bin/sh "$IMAGE" "$CONTAINER_ENTRYPOINT" seaweedfs-test-command retained-argument 2>&1); then
  exit 1
fi
[[ "$output" == *'SeaweedFS KEK bootstrap failed: state directory is unreadable'* ]] \
  || fail_test 'restricted state did not reach the readability guard'
[[ ! -e "$restricted_state/.peppy-s3-sse-kek" ]] || fail_test 'restricted state generated a key'

echo 'SeaweedFS KEK entrypoint lifecycle checks passed.'
