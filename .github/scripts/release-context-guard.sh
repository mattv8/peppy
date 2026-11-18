#!/usr/bin/env bash
set -euo pipefail

: "${CHANNEL:?CHANNEL is required}"
: "${REQUESTED_SHA:?REQUESTED_SHA is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_EVENT_NAME:?GITHUB_EVENT_NAME is required}"
: "${GITHUB_REF:?GITHUB_REF is required}"

if [[ "${RELEASE_CONTEXT_MODE:-}" == production-ancestor ]]; then
  case "$GITHUB_EVENT_NAME:$GITHUB_REF:$CHANNEL" in
    workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable) ;;
    *) echo 'App Store builds require a production stable dispatch/push' >&2; exit 1 ;;
  esac
  [[ "$REQUESTED_SHA" =~ ^[0-9a-fA-F]{40}$ ]] || {
    echo 'requested App Store SHA must be a full 40-hex commit SHA' >&2
    exit 1
  }
  actual_sha=$(git rev-parse HEAD)
  [[ "$actual_sha" = "$(printf '%s' "$REQUESTED_SHA" | tr 'A-F' 'a-f')" ]] || {
    echo 'checked-out App Store SHA must equal the requested SHA' >&2
    exit 1
  }
  git merge-base --is-ancestor "$REQUESTED_SHA" "$GITHUB_SHA" || {
    echo 'requested App Store SHA must be an ancestor of the triggering SHA' >&2
    exit 1
  }
  exit 0
fi
[[ -z "${RELEASE_CONTEXT_MODE:-}" ]] || { echo 'unknown release context mode' >&2; exit 1; }

[[ "$REQUESTED_SHA" = "$GITHUB_SHA" ]] || {
  echo 'requested release SHA must equal the triggering SHA' >&2
  exit 1
}

actual_sha=$(git rev-parse HEAD)
[[ "$actual_sha" = "$GITHUB_SHA" ]] || {
  echo 'checked-out release SHA must equal the triggering SHA' >&2
  exit 1
}

case "$GITHUB_EVENT_NAME:$GITHUB_REF:$CHANNEL" in
  workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable|push:refs/heads/staging:staging) ;;
  *)
    echo 'trusted releases require a production stable dispatch/push or a staging push' >&2
    exit 1
    ;;
esac
