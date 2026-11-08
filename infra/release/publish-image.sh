#!/usr/bin/env bash
set -euo pipefail

: "${IMAGE:?}" "${CONTEXT:?}" "${DOCKERFILE:?}" "${TAG:?}" "${SHA:?}"
: "${COSIGN_PRIVATE_KEY:?}" "${COSIGN_PASSWORD:?}" "${COSIGN_PUBLIC_KEY:?}"

# Enforce public library namespace only (no private-library)
[[ "$IMAGE" =~ ^hub\.docker\.visnovsky\.us/library/[a-z0-9][a-z0-9._/-]*$ ]] || { echo "Invalid public image name: must be hub.docker.visnovsky.us/library/*" >&2; exit 1; }

# Seconds to wait before each signing retry; the default allows 5 attempts of at most
# 2 minutes each, which fits the 30-minute publish job alongside the image build.
# PUBLISH_SIGN_RETRY_DELAYS exists so tests can shorten the schedule.
sign_retry_input=${PUBLISH_SIGN_RETRY_DELAYS-10 20 40 80}
reject_sign_retry_delays() {
    echo "PUBLISH_SIGN_RETRY_DELAYS must be a non-empty single-line list of integers from 0 to 300" >&2
    exit 1
}
[[ "$sign_retry_input" != *$'\n'* ]] || reject_sign_retry_delays
read -r -a sign_retry_delays <<<"$sign_retry_input"
((${#sign_retry_delays[@]} > 0)) || reject_sign_retry_delays
for delay in "${sign_retry_delays[@]}"; do
    if [[ ! "$delay" =~ ^[0-9]+$ ]] || ((10#$delay > 300)); then
        reject_sign_retry_delays
    fi
done

inspect_error=$(mktemp)
tag_error=$(mktemp)
trap 'rm -f "$inspect_error" "$tag_error"' EXIT
immutable="$IMAGE:sha-$SHA"
digest=
digest_format='{{json .Manifest.Digest}}'

# Buildx prints a human-readable summary for a bare nested template; its JSON
# form is a quoted string that must contain exactly one sha256 digest.
decode_digest() {
    [[ "$1" =~ ^\"(sha256:[0-9a-f]{64})\"$ ]] && printf '%s' "${BASH_REMATCH[1]}"
}

report_invalid_digest() {
    echo "Registry returned an invalid digest for $immutable" >&2
    exit 1
}

# Retries only the signature of an image this run just pushed; an existing tag is
# verified, never signed. Prints attempt numbers and messages only, never key material.
sign_new_image() {
    local attempt=1 max_attempts=$((${#sign_retry_delays[@]} + 1)) delay
    while true; do
        echo "cosign sign attempt $attempt of $max_attempts for $immutable" >&2
        if cosign sign --timeout 2m --yes --key env://COSIGN_PRIVATE_KEY "$IMAGE@$digest"; then
            return 0
        fi
        if ((attempt >= max_attempts)); then
            echo "cosign sign failed after $attempt attempts; refusing to promote unsigned image: $immutable" >&2
            return 1
        fi
        delay=${sign_retry_delays[attempt-1]}
        echo "retrying cosign sign in ${delay}s" >&2
        sleep "$delay"
        attempt=$((attempt + 1))
    done
}

report_inspect_error() {
    echo "Unable to determine whether immutable image exists; refusing to rebuild: $immutable" >&2
    head -c 4096 "$inspect_error" >&2
    echo >&2
}

if inspected=$(docker buildx imagetools inspect --format "$digest_format" "$immutable" 2>"$inspect_error"); then
    digest=$(decode_digest "$inspected") || report_invalid_digest
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
    inspected=$(docker buildx imagetools inspect --format "$digest_format" "$immutable")
    digest=$(decode_digest "$inspected") || report_invalid_digest
    sign_new_image
fi

if [[ "$TAG" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    if tagged=$(docker buildx imagetools inspect --format "$digest_format" "$IMAGE:$TAG" 2>"$tag_error"); then
        tagged_digest=$(decode_digest "$tagged") || report_invalid_digest
        if [[ "$tagged_digest" != "$digest" ]]; then
            echo "Stable image tag $IMAGE:$TAG already points at a different digest; refusing to overwrite it" >&2
            exit 1
        fi
    else
        tag_inspect_message=$(tr '[:upper:]' '[:lower:]' < "$tag_error")
        if [[ "$tag_inspect_message" != *"manifest unknown"* && "$tag_inspect_message" != *": not found"* && "$tag_inspect_message" != *"not found:"* ]]; then
            echo "Unable to determine whether stable image tag exists; refusing to promote: $IMAGE:$TAG" >&2
            head -c 4096 "$tag_error" >&2
            echo >&2
            exit 1
        fi
    fi
fi

promote() {
    local alias=$1 promoted promoted_digest
    docker buildx imagetools create --prefer-index=false --tag "$IMAGE:$alias" "$IMAGE@$digest"
    if ! promoted=$(docker buildx imagetools inspect --format "$digest_format" "$IMAGE:$alias" 2>"$inspect_error"); then
        echo "Unable to verify promoted image digest: $IMAGE:$alias" >&2
        head -c 4096 "$inspect_error" >&2
        echo >&2
        exit 1
    fi
    promoted_digest=$(decode_digest "$promoted") || {
        echo "Registry returned an invalid promoted image digest for $IMAGE:$alias" >&2
        exit 1
    }
    if [[ "$promoted_digest" != "$digest" ]]; then
        echo "Promoted image digest does not match verified source digest: $IMAGE:$alias" >&2
        exit 1
    fi
}
promote "$TAG"
if [[ "$TAG" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    IFS=. read -r major minor _ <<<"$TAG"
    promote "$major.$minor"
    promote latest
    if ((major >= 1)); then promote "$major"; fi
fi
printf 'digest=%s\n' "$digest" >> "$GITHUB_OUTPUT"
