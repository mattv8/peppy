#!/usr/bin/env bash

platform_dispatch_failure_summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '## %s\n\n**Failed:** %s\n' "$1" "$2" >> "$GITHUB_STEP_SUMMARY"
  fi
}

platform_dispatch_fail() {
  platform_dispatch_failure_summary "$1" "$2"
  echo "$1 failed: $2" >&2
  exit 1
}

platform_dispatch_require_match() {
  [[ "$3" =~ $4 ]] || platform_dispatch_fail "$1" "invalid $2"
}

platform_dispatch_write_curl_config() {
  local token_variable="$1"
  local dispatch_token

  IFS= read -r dispatch_token <<< "${!token_variable:-}"

  printf '%s\n' \
    'header = "Accept: application/vnd.github+json"' \
    "header = \"Authorization: Bearer $dispatch_token\"" \
    'header = "X-GitHub-Api-Version: 2022-11-28"' \
    'header = "Content-Type: application/json"'
}

platform_dispatch_post() {
  local title="$1"
  local token_variable="$2"
  local url="$3"
  local payload_file="$4"
  local request_failure="$5"
  local response_failure="$6"
  local http_status

  if ! http_status="$(platform_dispatch_write_curl_config "$token_variable" | curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
    --retry 3 --retry-delay 2 --retry-connrefused \
    --config - --request POST "$url" --data-binary "@$payload_file" --output /dev/null --write-out '%{http_code}')"; then
    platform_dispatch_fail "$title" "$request_failure"
  fi

  [[ "$http_status" == '204' ]] || platform_dispatch_fail "$title" "$response_failure"
}

platform_dispatch_success_summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '## %s\n\n%s\n' "$1" "$2" >> "$GITHUB_STEP_SUMMARY"
  fi
}
