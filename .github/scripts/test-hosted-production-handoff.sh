#!/usr/bin/env bash
# shellcheck disable=SC2016 # Workflow ${{ }} expressions are literal text in this contract test.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTIFIER="$SCRIPT_DIR/notify-hosted-production.sh"
WORKFLOW="$SCRIPT_DIR/../workflows/ci.yml"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/build-publish.yml/dispatches'
readonly token='ghs_handoff_test_token'
readonly source_revision='0123456789abcdef0123456789abcdef01234567'
readonly server_digest='sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
readonly relay_digest='sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789'
readonly expected_payload='{"ref":"production","inputs":{"environment":"production","source_revision":"0123456789abcdef0123456789abcdef01234567","public_run_id":"123","public_run_attempt":"2","server_digest":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","relay_digest":"sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}}'

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
  echo 'curl: (22) The requested URL returned error: 404' >&2
  exit 22
fi
printf '%s' "${FAKE_HTTP_STATUS:-204}"
EOF
chmod +x "$fake_bin/curl"

valid_env=(
  PEPPY_PLATFORM_DISPATCH_TOKEN="$token"
  SOURCE_REVISION="$source_revision"
  RUN_ID=123
  RUN_ATTEMPT=2
  SERVER_DIGEST="$server_digest"
  RELAY_DIGEST="$relay_digest"
)

case_index=0

fail_test() {
  echo "$1" >&2
  exit 1
}

run_notifier() {
  local stderr_file="$1"
  local args_file="$2"
  local config_file="$3"
  local payload_file="$4"

  shift 4
  env -u GITHUB_STEP_SUMMARY PATH="$fake_bin:$PATH" CURL_ARGS="$args_file" CURL_CONFIG="$config_file" CURL_PAYLOAD="$payload_file" "$@" "$NOTIFIER" 2> "$stderr_file"
}

expect_rejected() {
  local diagnostic="$1"
  local args_file="$temp_dir/curl-args-$case_index"
  local config_file="$temp_dir/curl-config-$case_index"
  local payload_file="$temp_dir/curl-payload-$case_index"
  local stderr_file="$temp_dir/stderr-$case_index"

  shift
  case_index=$((case_index + 1))
  if run_notifier "$stderr_file" "$args_file" "$config_file" "$payload_file" "$@"; then
    fail_test "expected notifier to reject: $diagnostic"
  fi
  if [[ -e "$args_file" ]]; then
    fail_test "curl was invoked for rejected input: $diagnostic"
  fi
  grep -Fqx "Hosted production dispatch failed: $diagnostic" "$stderr_file" || fail_test "missing diagnostic: $diagnostic"
}

expect_line() {
  grep -Fqx -- "$2" "$1" || fail_test "missing line: $2"
}

expect_text() {
  grep -Fq -- "$2" "$1" || fail_test "missing text: $2"
}

expect_no_text() {
  if grep -Fq -- "$2" "$1"; then
    fail_test "unexpected text: $2"
  fi
}

expect_option_value() {
  local actual

  actual="$(awk -v option="$2" 'found { print; exit } $0 == option { found = 1 }' "$1")"
  [[ "$actual" == "$3" ]] || fail_test "expected $2 value '$3' (got '$actual')"
}

valid_args="$temp_dir/curl-args-valid"
valid_config="$temp_dir/curl-config-valid"
valid_payload="$temp_dir/curl-payload-valid"
valid_stderr="$temp_dir/stderr-valid"
valid_summary="$temp_dir/summary-valid.md"
run_notifier "$valid_stderr" "$valid_args" "$valid_config" "$valid_payload" "${valid_env[@]}" GITHUB_STEP_SUMMARY="$valid_summary"
[[ ! -s "$valid_stderr" ]] || fail_test 'successful dispatch wrote to stderr'
expect_line "$valid_args" "$dispatch_url"
expect_line "$valid_args" '--config'
expect_line "$valid_args" '-'
expect_line "$valid_args" '--data-binary'
expect_line "$valid_args" '--retry-connrefused'
expect_line "$valid_args" '--fail'
expect_option_value "$valid_args" --request POST
expect_no_text "$valid_args" '--retry-all-errors'
expect_option_value "$valid_args" --connect-timeout 10
expect_option_value "$valid_args" --max-time 30
expect_option_value "$valid_args" --retry 3
expect_option_value "$valid_args" --retry-delay 2
expect_text "$valid_config" "Authorization: Bearer $token"
expect_no_text "$valid_args" "$token"
expect_line "$valid_payload" "$expected_payload"
expect_text "$valid_summary" 'Queued private production workflow dispatch'
expect_no_text "$valid_summary" "$token"

expect_rejected 'PEPPY_PLATFORM_DISPATCH_TOKEN is unavailable' "${valid_env[@]}" PEPPY_PLATFORM_DISPATCH_TOKEN=
expect_rejected 'invalid source_revision' "${valid_env[@]}" SOURCE_REVISION='0123456789abcdef0123456789abcdef01234567;id'
expect_rejected 'invalid public_run_id' "${valid_env[@]}" RUN_ID='123;id'
expect_rejected 'invalid public_run_attempt' "${valid_env[@]}" RUN_ATTEMPT='2;id'
expect_rejected 'invalid server_digest' "${valid_env[@]}" SERVER_DIGEST='sha256:bad;id'
expect_rejected 'invalid relay_digest' "${valid_env[@]}" RELAY_DIGEST='sha256:bad;id'

failure_args="$temp_dir/curl-args-failure"
failure_config="$temp_dir/curl-config-failure"
failure_payload="$temp_dir/curl-payload-failure"
failure_stderr="$temp_dir/stderr-failure"
failure_summary="$temp_dir/summary-failure.md"
if run_notifier "$failure_stderr" "$failure_args" "$failure_config" "$failure_payload" "${valid_env[@]}" FAKE_CURL_FAIL=1 GITHUB_STEP_SUMMARY="$failure_summary"; then
  fail_test 'expected curl failure to fail the notifier'
fi
expect_line "$failure_stderr" 'Hosted production dispatch failed: private workflow dispatch request failed'
expect_no_text "$failure_stderr" "$token"
expect_text "$failure_summary" '**Failed:** private workflow dispatch request failed'
expect_no_text "$failure_summary" "$token"

not_queued_args="$temp_dir/curl-args-not-queued"
not_queued_config="$temp_dir/curl-config-not-queued"
not_queued_payload="$temp_dir/curl-payload-not-queued"
not_queued_stderr="$temp_dir/stderr-not-queued"
if run_notifier "$not_queued_stderr" "$not_queued_args" "$not_queued_config" "$not_queued_payload" "${valid_env[@]}" FAKE_HTTP_STATUS=302; then
  fail_test 'expected non-204 response to fail the notifier'
fi
expect_line "$not_queued_stderr" 'Hosted production dispatch failed: private workflow dispatch did not queue the private workflow'

notifier_job="$temp_dir/notifier-job.yml"
awk '/^  notify-hosted-production:/{inside=1} /^  stable-release:/{inside=0} inside' "$WORKFLOW" > "$notifier_job"
expect_line "$notifier_job" '  notify-hosted-production:'
expect_line "$notifier_job" '    needs: [gate, publish-production-server, publish-production-relay]'
expect_line "$notifier_job" '    environment: production'
expect_line "$notifier_job" '      contents: read'
expect_line "$notifier_job" '        uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683'
expect_line "$notifier_job" '          ref: ${{ github.sha }}'
expect_line "$notifier_job" '          persist-credentials: false'
expect_line "$notifier_job" '          PEPPY_PLATFORM_DISPATCH_TOKEN: ${{ secrets.PEPPY_PLATFORM_DISPATCH_TOKEN }}'
expect_line "$notifier_job" '        run: bash .github/scripts/notify-hosted-production.sh'
expect_no_text "$notifier_job" 'create-github-app-token'
expect_no_text "$notifier_job" 'HOSTED_DISPATCH_APP_'
checkout_line=$(grep -n -m1 'actions/checkout@' "$notifier_job" | cut -d: -f1)
dispatch_line=$(grep -n -m1 'notify-hosted-production.sh' "$notifier_job" | cut -d: -f1)
[[ "$checkout_line" -lt "$dispatch_line" ]] || fail_test 'pinned checkout must precede the dispatch script'

echo 'Hosted production handoff tests passed'
