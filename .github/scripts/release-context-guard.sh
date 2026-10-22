#!/usr/bin/env bash
set -euo pipefail

: "${CHANNEL:?CHANNEL is required}"
: "${REQUESTED_SHA:?REQUESTED_SHA is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_EVENT_NAME:?GITHUB_EVENT_NAME is required}"
: "${GITHUB_REF:?GITHUB_REF is required}"

[[ "$REQUESTED_SHA" = "$GITHUB_SHA" ]] || {
  echo 'requested release SHA must equal the triggering SHA' >&2
  exit 1
}

case "$GITHUB_EVENT_NAME:$GITHUB_REF:$CHANNEL" in
  workflow_dispatch:refs/heads/production:stable|push:refs/heads/production:stable|push:refs/heads/staging:staging) ;;
  *)
    echo 'trusted releases require a production stable dispatch/push or a staging push' >&2
    exit 1
    ;;
esac
