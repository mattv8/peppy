#!/bin/sh
set -eu

state_dir=/root/.seaweedfs
data_dir=/data
key_path=$state_dir/.peppy-s3-sse-kek

fail() {
  echo "SeaweedFS KEK bootstrap failed: $1" >&2
  exit 1
}

# SeaweedFS accepts this variable internally. Clear any supplied encryption
# variables so the persisted machine key is the only possible source.
for variable in $(env | sed -n 's/^\(WEED_[A-Za-z0-9_]*\)=.*/\1/p'); do
  case "$variable" in
    *SSE*|*KEK*|*ENCRYPT*) unset "$variable" ;;
  esac
done

if [ -L "$state_dir" ]; then
  fail 'state directory is a symlink'
fi
if [ ! -e "$state_dir" ]; then
  umask 077
  mkdir "$state_dir" || fail 'cannot create state directory'
fi
if [ ! -d "$state_dir" ]; then
  fail 'state path is not a directory'
fi
if [ ! -r "$state_dir" ] || [ ! -x "$state_dir" ]; then
  fail 'state directory is unreadable'
fi
if [ -L "$key_path" ]; then
  fail 'stored key is a symlink'
fi

validate_stored_key() {
  [ "$(wc -c <"$key_path" | tr -d ' ')" = 64 ] || return 1
  key=$(cat "$key_path") || return 1
  [ "${#key}" = 64 ] || return 1
  case "$key" in
    *[!0-9a-f]*) return 1 ;;
  esac
  [ "$(stat -c %a "$key_path")" = 600 ] || return 1
}

has_initialized_filer_state() {
  entries=$(find "$1" -mindepth 1 -maxdepth 1 -print -quit) || fail 'cannot enumerate state directory'
  [ -n "$entries" ]
}

has_initialized_data_state() {
  [ -e "$data_dir" ] || return 1
  [ -d "$data_dir" ] || return 0
  [ -r "$data_dir" ] && [ -x "$data_dir" ] || fail 'data directory is unreadable'

  for entry in "$data_dir"/* "$data_dir"/.[!.]* "$data_dir"/..?*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    if [ "$entry" = "$data_dir/filerldb2" ] && [ -d "$entry" ] && [ ! -L "$entry" ]; then
      [ -r "$entry" ] && [ -x "$entry" ] || fail 'filer metadata directory is unreadable'
      entries=$(find "$entry" -mindepth 1 -print -quit) || fail 'cannot enumerate filer metadata directory'
      [ -n "$entries" ] && return 0
    else
      return 0
    fi
  done
  return 1
}

if [ -e "$key_path" ]; then
  validate_stored_key || fail 'stored key is malformed or insecure'
else
  for stale_key in "$state_dir"/.peppy-s3-sse-kek.*; do
    [ -e "$stale_key" ] || continue
    [ ! -L "$stale_key" ] && [ -f "$stale_key" ] || fail 'temporary key state is unsafe'
    rm -f "$stale_key" || fail 'cannot remove incomplete temporary key'
  done
  sync -f "$state_dir" || fail 'cannot sync state directory'
  if has_initialized_filer_state "$state_dir" || has_initialized_data_state; then
    fail 'stored key is missing from initialized state'
  fi

  umask 077
  temporary_key=$(mktemp "$state_dir/.peppy-s3-sse-kek.XXXXXX") || fail 'cannot create temporary key'
  key=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n') || fail 'cannot generate stored key'
  [ "${#key}" = 64 ] || fail 'generated key is malformed'
  case "$key" in
    *[!0-9a-f]*) fail 'generated key is malformed' ;;
  esac
  printf '%s' "$key" >"$temporary_key" || fail 'cannot write stored key'
  chmod 600 "$temporary_key" || fail 'cannot secure stored key permissions'
  sync -f "$temporary_key" || fail 'cannot sync temporary key'
  if ! mv -n "$temporary_key" "$key_path"; then
    rm -f "$temporary_key"
    fail 'cannot atomically install stored key'
  fi
  if [ -e "$temporary_key" ]; then
    rm -f "$temporary_key"
  fi
  if [ -L "$key_path" ] || ! validate_stored_key; then
    fail 'stored key is malformed or insecure'
  fi
  sync -f "$key_path" || fail 'cannot sync stored key'
  sync -f "$state_dir" || fail 'cannot sync state directory'
fi

export WEED_S3_SSE_KEK="$key"
exec /entrypoint.sh "$@"
