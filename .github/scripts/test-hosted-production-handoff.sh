#!/usr/bin/env bash
# shellcheck disable=SC2016 # Workflow ${{ }} expressions are literal text in this contract test.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTIFIER="$SCRIPT_DIR/notify-hosted-production.sh"
WORKFLOW="$SCRIPT_DIR/../workflows/ci.yml"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

readonly dispatch_url='https://api.github.com/repos/mattv8/peppy-platform/actions/workflows/promote-public-production.yml/dispatches'
readonly token='ghs_handoff_test_token'
readonly source_revision='0123456789abcdef0123456789abcdef01234567'
readonly server_digest='sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
readonly relay_digest='sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789'
readonly expected_payload='{"ref":"production","inputs":{"schema_version":"1","source_revision":"0123456789abcdef0123456789abcdef01234567","run_id":"123","run_attempt":"2","server_digest":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","relay_digest":"sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}}'

fake_bin="$temp_dir/bin"
mkdir -p "$fake_bin"
cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
: "${CURL_ARGS:?CURL_ARGS must name the captured argument file}"
printf '%s\n' "$@" > "$CURL_ARGS"
if [[ -n "${FAKE_CURL_FAIL:-}" ]]; then
  echo 'curl: (22) The requested URL returned error: 404' >&2
  exit 22
fi
EOF
chmod +x "$fake_bin/curl"

valid_env=(
  HOSTED_DISPATCH_TOKEN="$token"
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
  # Usage: run_notifier <stderr-file> <curl-args-file> <env-assignment>...
  local stderr_file="$1"
  local args_file="$2"

  shift 2
  env -u GITHUB_STEP_SUMMARY PATH="$fake_bin:$PATH" CURL_ARGS="$args_file" "$@" "$NOTIFIER" 2> "$stderr_file"
}

expect_rejected() {
  # Usage: expect_rejected <diagnostic> <env-assignment>...
  local diagnostic="$1"
  local args_file="$temp_dir/curl-args-$case_index"
  local stderr_file="$temp_dir/stderr-$case_index"

  shift
  case_index=$((case_index + 1))
  if run_notifier "$stderr_file" "$args_file" "$@"; then
    fail_test "expected notifier to reject: $diagnostic"
  fi
  if [[ -e "$args_file" ]]; then
    fail_test "curl was invoked for rejected input: $diagnostic"
  fi
  if ! grep -Fqx "Hosted production handoff failed: $diagnostic" "$stderr_file"; then
    cat "$stderr_file" >&2
    fail_test "missing diagnostic: $diagnostic"
  fi
}

expect_line() {
  local file="$1"
  local expected="$2"

  grep -Fqx -- "$expected" "$file" || fail_test "missing line: $expected"
}

expect_no_line() {
  local file="$1"
  local unexpected="$2"

  if grep -Fqx -- "$unexpected" "$file"; then
    fail_test "unexpected line: $unexpected"
  fi
}

expect_option_value() {
  local file="$1"
  local option="$2"
  local expected="$3"
  local actual

  actual="$(awk -v option="$option" 'found { print; exit } $0 == option { found = 1 }' "$file")"
  [[ "$actual" == "$expected" ]] || fail_test "expected $option value '$expected' (got '$actual')"
}

expect_text() {
  local file="$1"
  local needle="$2"

  grep -Fq -- "$needle" "$file" || fail_test "missing text: $needle"
}

expect_no_text() {
  local file="$1"
  local needle="$2"

  if grep -Fq -- "$needle" "$file"; then
    fail_test "unexpected text: $needle"
  fi
}

# Valid dispatch: endpoint, auth, bounded transient retries, and exact payload.
valid_args="$temp_dir/curl-args-valid"
valid_stderr="$temp_dir/stderr-valid"
valid_summary="$temp_dir/summary-valid.md"
run_notifier "$valid_stderr" "$valid_args" "${valid_env[@]}" GITHUB_STEP_SUMMARY="$valid_summary"
[[ ! -s "$valid_stderr" ]] || fail_test 'successful dispatch wrote to stderr'
expect_line "$valid_args" "$dispatch_url"
expect_line "$valid_args" 'Authorization: Bearer '"$token"
expect_line "$valid_args" '--fail'
expect_line "$valid_args" '--silent'
expect_line "$valid_args" '--show-error'
expect_line "$valid_args" '--retry-connrefused'
expect_no_line "$valid_args" '--retry-all-errors'
expect_option_value "$valid_args" --request POST
expect_option_value "$valid_args" --connect-timeout 10
expect_option_value "$valid_args" --max-time 30
expect_option_value "$valid_args" --retry 3
expect_option_value "$valid_args" --retry-delay 2
expect_option_value "$valid_args" --data "$expected_payload"

expect_line "$valid_summary" 'Requested promotion of source '"$source_revision"' (run 123, attempt 2) with server digest '"$server_digest"' and relay digest '"$relay_digest"'.'
expect_no_text "$valid_summary" 'verified'
expect_no_text "$valid_summary" "$token"

# Rejected inputs: curl must never be invoked, and the exact diagnostic must be emitted.
expect_rejected 'HOSTED_DISPATCH_TOKEN is unavailable' "${valid_env[@]}" HOSTED_DISPATCH_TOKEN=
expect_rejected 'invalid source_revision' "${valid_env[@]}" SOURCE_REVISION=0123456789abcdef0123456789abcdef0123456
expect_rejected 'invalid source_revision' "${valid_env[@]}" SOURCE_REVISION=0123456789ABCDEF0123456789abcdef01234567
expect_rejected 'invalid run_id' "${valid_env[@]}" RUN_ID=0
expect_rejected 'invalid run_attempt' "${valid_env[@]}" RUN_ATTEMPT=0
expect_rejected 'invalid server_digest' "${valid_env[@]}" SERVER_DIGEST=sha256:not-a-digest
expect_rejected 'invalid relay_digest' "${valid_env[@]}" RELAY_DIGEST=sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789

# Curl failure: the dispatch was attempted, the failure is exact, and the summary never carries the token.
failure_args="$temp_dir/curl-args-failure"
failure_stderr="$temp_dir/stderr-failure"
failure_summary="$temp_dir/summary-failure.md"
if run_notifier "$failure_stderr" "$failure_args" "${valid_env[@]}" FAKE_CURL_FAIL=1 GITHUB_STEP_SUMMARY="$failure_summary"; then
  fail_test 'expected curl failure to fail the notifier'
fi
[[ -e "$failure_args" ]] || fail_test 'curl was not invoked for dispatch failure'
expect_line "$failure_stderr" 'Hosted production handoff failed: private workflow dispatch request failed'
expect_no_text "$failure_stderr" "$token"
expect_line "$failure_summary" '**Failed:** private workflow dispatch request failed'
expect_no_text "$failure_summary" "$token"
expect_no_text "$failure_summary" 'Requested promotion'

notifier_job="$temp_dir/notifier-job.yml"
awk '/^  notify-hosted-production:/{inside=1} /^  stable-release:/{inside=0} inside' "$WORKFLOW" > "$notifier_job"
expect_line "$notifier_job" '  notify-hosted-production:'

# Workflow contract, scoped to the notifier job.
expect_line "$notifier_job" "    if: \"!cancelled() && github.event_name == 'push' && github.ref == 'refs/heads/production' && needs.gate.result == 'success' && needs.publish-production-server.result == 'success' && needs.publish-production-relay.result == 'success'\""
expect_line "$notifier_job" '    needs: [gate, publish-production-server, publish-production-relay]'
expect_line "$notifier_job" '    environment: production'
expect_line "$notifier_job" '      contents: read'
expect_line "$notifier_job" '        uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683'
expect_line "$notifier_job" '          ref: ${{ github.sha }}'
expect_line "$notifier_job" '          persist-credentials: false'
expect_line "$notifier_job" '        uses: actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349 # v2.2.2'
expect_line "$notifier_job" '          app-id: ${{ vars.HOSTED_DISPATCH_APP_ID }}'
expect_line "$notifier_job" '          private-key: ${{ secrets.HOSTED_DISPATCH_APP_PRIVATE_KEY }}'
expect_line "$notifier_job" '          owner: mattv8'
expect_line "$notifier_job" '          repositories: peppy-platform'
expect_line "$notifier_job" '          permission-actions: write'
expect_line "$notifier_job" '          HOSTED_DISPATCH_TOKEN: ${{ steps.dispatch-token.outputs.token }}'
expect_line "$notifier_job" '          SOURCE_REVISION: ${{ github.sha }}'
expect_line "$notifier_job" '          RUN_ID: ${{ github.run_id }}'
expect_line "$notifier_job" '          RUN_ATTEMPT: ${{ github.run_attempt }}'
expect_line "$notifier_job" '          SERVER_DIGEST: ${{ needs.publish-production-server.outputs.digest }}'
expect_line "$notifier_job" '          RELAY_DIGEST: ${{ needs.publish-production-relay.outputs.digest }}'
expect_line "$notifier_job" '        run: bash .github/scripts/notify-hosted-production.sh'

checkout_line=$(grep -n -m1 'actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683' "$notifier_job" | cut -d: -f1)
token_line=$(grep -n -m1 'actions/create-github-app-token@fee1f7d63c2ff003460e3d139729b119787bc349' "$notifier_job" | cut -d: -f1)
[[ "$checkout_line" -lt "$token_line" ]] || fail_test 'checkout must precede GitHub App token minting'

echo 'Hosted production handoff tests passed'
