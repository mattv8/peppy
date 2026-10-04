#!/usr/bin/env bash
set -euo pipefail

: "${IMAGE:?}" "${CONTEXT:?}" "${DOCKERFILE:?}" "${TAG:?}" "${SHA:?}"
: "${COSIGN_PRIVATE_KEY:?}" "${COSIGN_PASSWORD:?}" "${COSIGN_PUBLIC_KEY:?}"

# Enforce public library namespace only (no private-library)
[[ "$IMAGE" =~ ^hub\.docker\.visnovsky\.us/library/[a-z0-9][a-z0-9._/-]*$ ]] || { echo "Invalid public image name: must be hub.docker.visnovsky.us/library/*" >&2; exit 1; }

inspect_error=$(mktemp)
trap 'rm -f "$inspect_error"' EXIT
immutable="$IMAGE:sha-$SHA"
digest=

valid_digest() {
    [[ "$1" =~ ^sha256:[0-9a-f]{64}$ ]]
}

report_inspect_error() {
    echo "Unable to determine whether immutable image exists; refusing to rebuild: $immutable" >&2
    head -c 4096 "$inspect_error" >&2
    echo >&2
}

if digest=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$immutable" 2>"$inspect_error"); then
    if ! valid_digest "$digest"; then
        echo "Registry returned an invalid digest for $immutable" >&2
        exit 1
    fi
    # An immutable SHA tag may only be promoted after its existing signature
    # verifies; never rebuild or overwrite it during staging/promotion/stable.
    cosign verify --key env://COSIGN_PUBLIC_KEY "$IMAGE@$digest" >/dev/null
else
    inspect_message=$(tr '[:upper:]' '[:lower:]' < "$inspect_error")
    if [[ "$inspect_message" != *"manifest unknown"* && "$inspect_message" != *": not found"* && "$inspect_message" != *"not found:"* ]]; then
        report_inspect_error
        exit 1
    fi
    docker buildx build --push --file "$DOCKERFILE" \
        --label "org.opencontainers.image.source=https://github.com/$GITHUB_REPOSITORY" \
        --label "org.opencontainers.image.revision=$SHA" \
        --tag "$immutable" "$CONTEXT"
    digest=$(docker buildx imagetools inspect --format '{{.Manifest.Digest}}' "$immutable")
    if ! valid_digest "$digest"; then
        echo "Registry returned an invalid digest for $immutable" >&2
        exit 1
    fi
    cosign sign --yes --key env://COSIGN_PRIVATE_KEY "$IMAGE@$digest"
fi

promote() { docker buildx imagetools create --tag "$IMAGE:$1" "$IMAGE@$digest"; }
promote "$TAG"
if [[ "$TAG" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    IFS=. read -r major minor _ <<<"$TAG"
    promote "$major.$minor"
    promote latest
    if ((major >= 1)); then promote "$major"; fi
fi
printf 'digest=%s\n' "$digest" >> "$GITHUB_OUTPUT"
