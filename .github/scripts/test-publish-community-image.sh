#!/usr/bin/env bash
set -euo pipefail
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir "$root/bin"
cat > "$root/bin/docker" <<'MOCK'
#!/usr/bin/env bash
printf 'docker' >> "$LOG"; printf ' %q' "$@" >> "$LOG"; printf '\n' >> "$LOG"
if [[ ${1:-} == buildx && ${2:-} == imagetools && ${3:-} == inspect ]]; then
  if [[ ${4:-} != --format || ${5:-} != '{{.Manifest.Digest}}' || $# -ne 6 ]]; then
    printf 'unexpected inspect arguments:' >&2; printf ' %q' "$@" >&2; printf '\n' >&2; exit 64
  fi
  if [[ -f "$BUILD_STATE_FILE" ]]; then
    echo sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; exit 0
  fi
  case ${INSPECT_MODE:-exists} in
    exists) echo sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
    missing) echo 'ERROR: image not found: manifest unknown' >&2; exit 1 ;;
    uncertain) echo 'ERROR: failed to do request: i/o timeout' >&2; exit 1 ;;
    invalid) echo sha256:existing ;;
  esac
  exit 0
fi
if [[ ${1:-} == buildx && ${2:-} == build ]]; then touch "$BUILD_STATE_FILE"; fi
MOCK
cat > "$root/bin/cosign" <<'MOCK'
#!/usr/bin/env bash
printf 'cosign' >> "$LOG"; printf ' %q' "$@" >> "$LOG"; printf '\n' >> "$LOG"
[[ !( ${1:-} == verify && ${VERIFY_FAIL:-false} == true ) ]]
MOCK
chmod +x "$root/bin/docker" "$root/bin/cosign"
reset_case() { : > "$root/log"; : > "$root/output"; rm -f "$root/build_state"; }
run_publisher() {
  env -u HARBOR_USERNAME -u HARBOR_PASSWORD PATH="$root/bin:$PATH" LOG="$root/log" GITHUB_OUTPUT="$root/output" BUILD_STATE_FILE="$root/build_state" GITHUB_REPOSITORY=test/peppy IMAGE="${IMAGE:-hub.docker.visnovsky.us/library/peppy-server}" CONTEXT=. DOCKERFILE=infra/docker/server.Dockerfile TAG=staging SHA=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa COSIGN_PRIVATE_KEY=x COSIGN_PASSWORD=x COSIGN_PUBLIC_KEY=x infra/release/publish-image.sh
}
assert_no_mutation() {
  if grep -Eq 'docker buildx build|docker buildx imagetools create|cosign sign' "$root/log"; then echo 'unexpected registry mutation' >&2; cat "$root/log" >&2; exit 1; fi
}
reset_case
INSPECT_MODE=exists run_publisher
grep -Fq 'docker buildx imagetools inspect --format \{\{.Manifest.Digest\}\}' "$root/log"
grep -Fq 'cosign verify --key env://COSIGN_PUBLIC_KEY hub.docker.visnovsky.us/library/peppy-server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/log"
grep -Fq 'docker buildx imagetools create --tag hub.docker.visnovsky.us/library/peppy-server:staging hub.docker.visnovsky.us/library/peppy-server@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/log"
if grep -Fq 'docker buildx build' "$root/log"; then echo 'unexpected docker buildx build in log' >&2; exit 1; fi
grep -Fq 'digest=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' "$root/output"
reset_case
INSPECT_MODE=missing run_publisher
grep -Fq 'docker buildx build --push' "$root/log"
grep -Fq 'cosign sign --yes --key env://COSIGN_PRIVATE_KEY hub.docker.visnovsky.us/library/peppy-server@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' "$root/log"
sign_line=$(grep -n 'cosign sign' "$root/log" | cut -d: -f1); promote_line=$(grep -n 'docker buildx imagetools create' "$root/log" | head -1 | cut -d: -f1); ((sign_line < promote_line))
reset_case
if INSPECT_MODE=uncertain run_publisher >/dev/null 2>"$root/stderr"; then echo 'uncertain inspect failure should fail closed' >&2; exit 1; fi
grep -Fq 'i/o timeout' "$root/stderr"; assert_no_mutation
reset_case
if INSPECT_MODE=exists VERIFY_FAIL=true run_publisher >/dev/null 2>"$root/stderr"; then echo 'signature failure should fail' >&2; exit 1; fi
assert_no_mutation
reset_case
if INSPECT_MODE=invalid run_publisher >/dev/null 2>"$root/stderr"; then echo 'invalid digest should fail' >&2; exit 1; fi
assert_no_mutation; if grep -Fq 'cosign verify' "$root/log"; then echo 'unexpected cosign verify in log' >&2; exit 1; fi
reset_case
if IMAGE=hub.docker.visnovsky.us/private-library/peppy-server INSPECT_MODE=exists run_publisher >/dev/null 2>"$root/stderr"; then echo 'private namespace should fail' >&2; exit 1; fi
assert_no_mutation
echo 'community image publisher tests passed'
