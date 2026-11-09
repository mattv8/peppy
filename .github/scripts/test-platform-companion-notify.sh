#!/usr/bin/env bash
# shellcheck disable=SC2016 # Workflow ${{ }} expressions are literal text in this contract test.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTIFIER="$SCRIPT_DIR/notify-platform-companion.sh"
WORKFLOW="$SCRIPT_DIR/../workflows/platform-companion.yml"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/companion-sync.yml/dispatches'
readonly token='ghs_platform_companion_test_token'
readonly expected_payload='{"ref":"production","inputs":{"public_pr":"42"}}'

fake_bin="$temp_dir/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
: "${CURL_ARGS:?CURL_ARGS must name the captured argument file}"
: "${CURL_CONFIG:?CURL_CONFIG must name the captured config file}"
: "${CURL_PAYLOAD:?CURL_PAYLOAD must name the captured payload file}"
printf '%s\n' "$@" > "$CURL_ARGS"
cat > "$CURL_CONFIG"
payload_file="$(awk -F@ '$0 == "--data-binary" { getline; sub(/^@/, ""); print; exit }' "$CURL_ARGS")"
cat "$payload_file" > "$CURL_PAYLOAD"
if [[ -n "${FAKE_CURL_FAIL:-}" ]]; then
  exit 22
fi
printf '%s' "${FAKE_HTTP_STATUS:-204}"
EOF
chmod +x "$fake_bin/curl"

fail_test() {
  echo "$1" >&2
  exit 1
}

expect_line() {
  grep -Fqx -- "$2" "$1" || fail_test "missing line: $2"
}

expect_text() {
  grep -Fq -- "$2" "$1" || fail_test "missing text: $2"
}

expect_no_text() {
  ! grep -Fq -- "$2" "$1" || fail_test "unexpected text: $2"
}

run_notifier() {
  local case_name="$1"
  shift
  env -u GITHUB_STEP_SUMMARY PATH="$fake_bin:$PATH" \
    CURL_ARGS="$temp_dir/curl-args-$case_name" \
    CURL_CONFIG="$temp_dir/curl-config-$case_name" \
    CURL_PAYLOAD="$temp_dir/curl-payload-$case_name" \
    "$@" bash "$NOTIFIER" 2> "$temp_dir/stderr-$case_name"
}

valid_env=(PEPPY_PLATFORM_DISPATCH_TOKEN="$token" PUBLIC_PR=42)
run_notifier valid "${valid_env[@]}" GITHUB_STEP_SUMMARY="$temp_dir/summary-valid.md"
expect_line "$temp_dir/curl-args-valid" "$dispatch_url"
expect_line "$temp_dir/curl-args-valid" '--config'
expect_line "$temp_dir/curl-args-valid" '-'
expect_line "$temp_dir/curl-args-valid" '--data-binary'
expect_line "$temp_dir/curl-args-valid" '--fail'
expect_no_text "$temp_dir/curl-args-valid" "$token"
expect_text "$temp_dir/curl-config-valid" "Authorization: Bearer $token"
expect_line "$temp_dir/curl-payload-valid" "$expected_payload"
expect_text "$temp_dir/summary-valid.md" 'Queued private platform companion workflow dispatch for public PR #42.'

newline_token="${token}"$'\n'
run_notifier newline-token PEPPY_PLATFORM_DISPATCH_TOKEN="$newline_token" PUBLIC_PR=42
[[ ! -s "$temp_dir/stderr-newline-token" ]] || fail_test 'newline-terminated token dispatch wrote to stderr'
expect_line "$temp_dir/curl-config-newline-token" "header = \"Authorization: Bearer $token\""
expect_line "$temp_dir/curl-payload-newline-token" "$expected_payload"

for invalid_pr in 0 01 42x '42;id' ''; do
  case_name="invalid-${invalid_pr//[^[:alnum:]]/_}"
  if run_notifier "$case_name" PEPPY_PLATFORM_DISPATCH_TOKEN="$token" PUBLIC_PR="$invalid_pr"; then
    fail_test "expected invalid public PR to fail: $invalid_pr"
  fi
  expect_line "$temp_dir/stderr-$case_name" 'Platform companion dispatch failed: invalid public_pr'
  [[ ! -e "$temp_dir/curl-args-$case_name" ]] || fail_test "curl was invoked for invalid public PR: $invalid_pr"
done

if run_notifier missing-token PEPPY_PLATFORM_DISPATCH_TOKEN= PUBLIC_PR=42; then
  fail_test 'expected missing token to fail'
fi
expect_line "$temp_dir/stderr-missing-token" 'Platform companion dispatch failed: PEPPY_PLATFORM_DISPATCH_TOKEN is unavailable'

if run_notifier non-204 "${valid_env[@]}" FAKE_HTTP_STATUS=202; then
  fail_test 'expected non-204 response to fail'
fi
expect_line "$temp_dir/stderr-non-204" 'Platform companion dispatch failed: private workflow dispatch did not queue the private workflow'

if run_notifier curl-failure "${valid_env[@]}" FAKE_CURL_FAIL=1; then
  fail_test 'expected curl failure to fail'
fi
expect_line "$temp_dir/stderr-curl-failure" 'Platform companion dispatch failed: private workflow dispatch request failed'

expect_text "$WORKFLOW" 'pull_request_target:'
expect_text "$WORKFLOW" 'opened, synchronize, reopened, closed, edited, labeled, unlabeled'
expect_text "$WORKFLOW" 'branches: [production]'
expect_text "$WORKFLOW" '# zizmor: ignore[dangerous-triggers]'
expect_text "$WORKFLOW" 'permissions: {}'
expect_text "$WORKFLOW" 'runs-on: ubuntu-24.04'
expect_text "$WORKFLOW" 'timeout-minutes: 5'
expect_text "$WORKFLOW" 'environment: production'
expect_text "$WORKFLOW" 'persist-credentials: false'
expect_no_text "$WORKFLOW" 'ref:'
expect_text "$WORKFLOW" 'PEPPY_PLATFORM_DISPATCH_TOKEN: ${{ secrets.PEPPY_PLATFORM_DISPATCH_TOKEN }}'
expect_text "$WORKFLOW" 'PUBLIC_PR: ${{ github.event.pull_request.number }}'
expect_no_text "$WORKFLOW" 'run: ${{'
[[ "$(grep -Fc '${{' "$WORKFLOW")" -eq 2 ]] || fail_test 'workflow must use only the dispatch secret and numeric PR expression'

echo 'Platform companion notifier tests passed'
