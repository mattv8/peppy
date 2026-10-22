#!/usr/bin/env bash
set -euo pipefail
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir "$root/bin"
cat > "$root/bin/docker" <<'MOCK'
#!/usr/bin/env bash
printf 'docker' >> "$LOG"; printf ' %q' "$@" >> "$LOG"; printf '\n' >> "$LOG"
state_path() { printf '%s/%s' "$REGISTRY_STATE_DIR" "$(printf '%s' "$1" | tr '/:@' '___')"; }
if [[ ${1:-} == buildx && ${2:-} == imagetools && ${3:-} == inspect ]]; then
  if [[ ${4:-} != --format || ${5:-} != '{{json .Manifest.Digest}}' || $# -ne 6 ]]; then
    printf 'unexpected inspect arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
  fi
  state=$(state_path "$6")
  if [[ -f "$state" ]]; then
    if [[ $(cat "$state") == ERROR ]]; then echo 'ERROR: failed to inspect promoted digest' >&2; exit 1; fi
    cat "$state"
    exit 0
  fi
  if [[ ${6:-} =~ :[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    case ${VERSION_INSPECT_MODE:-missing} in
      missing) echo 'ERROR: image not found: manifest unknown' >&2; exit 1 ;;
      same) echo '"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' ;;
      different) echo '"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"' ;;
      uncertain) echo 'ERROR: failed to do request: i/o timeout' >&2; exit 1 ;;
      *) echo 'unknown VERSION_INSPECT_MODE' >&2; exit 64 ;;
    esac
    exit 0
  fi
  if [[ -f "$BUILD_STATE_FILE" ]]; then
    echo '"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"'; exit 0
  fi
  case ${INSPECT_MODE:-exists} in
    exists) echo '"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' ;;
    missing) echo 'ERROR: image not found: manifest unknown' >&2; exit 1 ;;
    uncertain) echo 'ERROR: failed to do request: i/o timeout' >&2; exit 1 ;;
    invalid) echo '"sha256:existing"' ;;
    unquoted) echo sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
    summary) printf 'Name:      %s\nMediaType: application/vnd.oci.image.index.v1+json\nDigest:    sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' "$6" ;;
    *) echo 'unknown INSPECT_MODE' >&2; exit 64 ;;
  esac
  exit 0
fi
if [[ ${1:-} == buildx && ${2:-} == imagetools && ${3:-} == create ]]; then
  if [[ ${4:-} == --prefer-index=false && ${5:-} == --tag && $# -eq 7 ]]; then
    target=$6 source=$7
  elif [[ ${4:-} == --tag && $# -eq 6 ]]; then
    target=$5 source=$6
  else
    printf 'unexpected create arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
  fi
  source_digest=${source##*@}
  promotion_mode=${PROMOTION_DIGEST_MODE:-same}
  if [[ -n ${PROMOTION_BAD_TAG:-} && $target != *":$PROMOTION_BAD_TAG" ]]; then promotion_mode=same; fi
  case $promotion_mode in
    same) printf '"%s"\n' "$source_digest" > "$(state_path "$target")" ;;
    different) echo '"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"' > "$(state_path "$target")" ;;
    malformed) echo '"sha256:bad"' > "$(state_path "$target")" ;;
    error) echo ERROR > "$(state_path "$target")" ;;
    *) echo 'unknown PROMOTION_DIGEST_MODE' >&2; exit 64 ;;
  esac
  exit 0
fi
if [[ ${1:-} == buildx && ${2:-} == build ]]; then
  if [[ ${3:-} != --push || ${4:-} != --file || ${6:-} != --label || ${8:-} != --label || ${10:-} != --tag || $# -ne 12 ]]; then
    printf 'unexpected build arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
  fi
  touch "$BUILD_STATE_FILE"; exit 0
fi
printf 'unexpected docker arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
MOCK
cat > "$root/bin/cosign" <<'MOCK'
#!/usr/bin/env bash
printf 'cosign' >> "$LOG"; printf ' %q' "$@" >> "$LOG"; printf '\n' >> "$LOG"
if [[ ${1:-} == verify && ${2:-} == --key && ${3:-} == env://COSIGN_PUBLIC_KEY && $# -eq 4 ]]; then
  [[ ${VERIFY_FAIL:-false} != true ]]
  exit
fi
if [[ ${1:-} == sign && ${2:-} == --yes && ${3:-} == --key && ${4:-} == env://COSIGN_PRIVATE_KEY && $# -eq 5 ]]; then exit 0; fi
printf 'unexpected cosign arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
MOCK
chmod +x "$root/bin/docker" "$root/bin/cosign"
reset_case() { : > "$root/log"; : > "$root/output"; rm -f "$root/build_state"; rm -rf "$root/registry"; mkdir "$root/registry"; }
run_publisher() {
  env -u HARBOR_USERNAME -u HARBOR_PASSWORD PATH="$root/bin:$PATH" LOG="$root/log" GITHUB_OUTPUT="$root/output" BUILD_STATE_FILE="$root/build_state" REGISTRY_STATE_DIR="$root/registry" GITHUB_REPOSITORY=test/peppy IMAGE="${IMAGE:-hub.docker.visnovsky.us/library/peppy-server}" CONTEXT=. DOCKERFILE=infra/docker/server.Dockerfile TAG="${TAG:-staging}" SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa COSIGN_PRIVATE_KEY=x COSIGN_PASSWORD=x COSIGN_PUBLIC_KEY=x infra/release/publish-image.sh
}
assert_no_mutation() {
  if grep -Eq 'docker buildx build|docker buildx imagetools create|cosign sign' "$root/log"; then echo 'unexpected registry mutation' >&2; cat "$root/log" >&2; exit 1; fi
}
reset_case
INSPECT_MODE=exists run_publisher
grep -Fq 'docker buildx imagetools inspect --format \{\{json\ .Manifest.Digest\}\}' "$root/log"
grep -Fq 'cosign verify --key env://COSIGN_PUBLIC_KEY hub.docker.visnovsky.us/library/peppy-server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/log"
grep -Fq 'docker buildx imagetools create --prefer-index=false --tag hub.docker.visnovsky.us/library/peppy-server:staging hub.docker.visnovsky.us/library/peppy-server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/log"
if grep -Fq 'docker buildx build' "$root/log"; then echo 'unexpected docker buildx build in log' >&2; exit 1; fi
grep -Fq 'digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/output"
reset_case
if INSPECT_MODE=exists PROMOTION_DIGEST_MODE=different run_publisher >/dev/null 2>"$root/stderr"; then echo 'different promoted digest should fail closed' >&2; exit 1; fi
if [[ -s "$root/output" ]]; then echo 'digest output must remain empty after a bad promotion' >&2; exit 1; fi
grep -Fq 'Promoted image digest does not match verified source digest' "$root/stderr"
for mode in malformed error; do
  reset_case
  if INSPECT_MODE=exists PROMOTION_DIGEST_MODE=$mode run_publisher >/dev/null 2>"$root/stderr"; then echo "$mode promoted digest should fail closed" >&2; exit 1; fi
  if [[ -s "$root/output" ]]; then echo "digest output must remain empty after $mode promotion inspection" >&2; exit 1; fi
done
reset_case
if TAG=1.2.3 INSPECT_MODE=exists VERSION_INSPECT_MODE=missing PROMOTION_DIGEST_MODE=different PROMOTION_BAD_TAG=1.2 run_publisher >/dev/null 2>"$root/stderr"; then echo 'different minor alias digest should fail before latest' >&2; exit 1; fi
if grep -Fq 'peppy-server:latest' "$root/log"; then echo 'latest promotion must not run after minor digest mismatch' >&2; exit 1; fi
if [[ -s "$root/output" ]]; then echo 'digest output must remain empty after minor alias mismatch' >&2; exit 1; fi
reset_case
INSPECT_MODE=missing run_publisher
grep -Fq 'docker buildx build --push' "$root/log"
grep -Fq 'cosign sign --yes --key env://COSIGN_PRIVATE_KEY hub.docker.visnovsky.us/library/peppy-server@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' "$root/log"
sign_line=$(grep -n 'cosign sign' "$root/log" | cut -d: -f1); promote_line=$(grep -n 'docker buildx imagetools create' "$root/log" | head -1 | cut -d: -f1); ((sign_line < promote_line))
grep -Fq 'docker buildx imagetools inspect --format \{\{json\ .Manifest.Digest\}\} hub.docker.visnovsky.us/library/peppy-server:staging' "$root/log"
grep -Fq 'digest=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' "$root/output"
reset_case
if INSPECT_MODE=uncertain run_publisher >/dev/null 2>"$root/stderr"; then echo 'uncertain inspect failure should fail closed' >&2; exit 1; fi
grep -Fq 'i/o timeout' "$root/stderr"; assert_no_mutation
reset_case
if INSPECT_MODE=exists VERIFY_FAIL=true run_publisher >/dev/null 2>"$root/stderr"; then echo 'signature failure should fail' >&2; exit 1; fi
assert_no_mutation
reset_case
if INSPECT_MODE=invalid run_publisher >/dev/null 2>"$root/stderr"; then echo 'invalid digest should fail' >&2; exit 1; fi
assert_no_mutation; if grep -Fq 'cosign verify' "$root/log"; then echo 'unexpected cosign verify in log' >&2; exit 1; fi
for mode in summary unquoted; do
  reset_case
  if INSPECT_MODE=$mode run_publisher >/dev/null 2>"$root/stderr"; then echo "$mode inspect output should fail" >&2; exit 1; fi
  grep -Fq 'invalid digest' "$root/stderr"; assert_no_mutation
  if grep -Fq 'cosign verify' "$root/log"; then echo "unexpected cosign verify after $mode output" >&2; exit 1; fi
done
reset_case
if IMAGE=hub.docker.visnovsky.us/private-library/peppy-server INSPECT_MODE=exists run_publisher >/dev/null 2>"$root/stderr"; then echo 'private namespace should fail' >&2; exit 1; fi
assert_no_mutation
reset_case
if TAG=1.2.3 INSPECT_MODE=exists VERSION_INSPECT_MODE=different run_publisher >/dev/null 2>"$root/stderr"; then echo 'different existing stable tag should fail' >&2; exit 1; fi
grep -Fq 'already points at a different digest' "$root/stderr"; assert_no_mutation
reset_case
TAG=1.2.3 INSPECT_MODE=exists VERSION_INSPECT_MODE=same run_publisher
grep -Fq 'docker buildx imagetools create --prefer-index=false --tag hub.docker.visnovsky.us/library/peppy-server:1.2.3' "$root/log"
reset_case
if TAG=1.2.3 INSPECT_MODE=exists VERSION_INSPECT_MODE=uncertain run_publisher >/dev/null 2>"$root/stderr"; then echo 'uncertain stable tag inspection should fail' >&2; exit 1; fi
grep -Fq 'refusing to promote' "$root/stderr"; assert_no_mutation
echo 'community image publisher tests passed'
