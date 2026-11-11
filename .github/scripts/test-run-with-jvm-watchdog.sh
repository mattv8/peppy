#!/usr/bin/env bash

set -Eeuo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
watchdog="$script_dir/run-with-jvm-watchdog.sh"
started_seconds=$SECONDS
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

mkdir "$temp_dir/bin"
cat > "$temp_dir/bin/jcmd" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$temp_dir/bin/jcmd"
# The real GNU timeout is required: the stall path depends on it forwarding TERM to the command.
timeout --version | grep -Fq 'GNU coreutils'

run_case() {
  local expected_status=$1
  local output=$2
  shift 2

  if PATH="$temp_dir/bin:$PATH" WATCHDOG_POLL_SECONDS=1 "$watchdog" "$@" > "$output" 2>&1; then
    status=0
  else
    status=$?
  fi
  if ((status != expected_status)); then
    cat "$output" >&2
    echo "expected status $expected_status, got $status" >&2
    exit 1
  fi
}

run_case 0 "$temp_dir/success" 2 8 -- bash -c 'printf "success output\\n"'
grep -Fqx 'success output' "$temp_dir/success"

run_case 3 "$temp_dir/failure" 2 8 -- bash -c 'exit 3'

# shellcheck disable=SC2016 # Expanded by the child shell.
run_case 124 "$temp_dir/stalled" 2 8 -- bash -c 'echo "$$" > "$1"; sleep 10' _ "$temp_dir/stalled.pid"
grep -Fq '::error::JVM output stalled for 2 seconds' "$temp_dir/stalled"
if kill -0 "$(cat "$temp_dir/stalled.pid")" 2>/dev/null; then
  echo 'stalled command outlived the watchdog' >&2
  exit 1
fi

run_case 124 "$temp_dir/hard-cap" 3 5 -- bash -c 'while :; do echo progress; sleep 0.5; done'
grep -Fq 'progress' "$temp_dir/hard-cap"
if grep -Fq 'JVM output stalled' "$temp_dir/hard-cap"; then
  echo 'progressing command was stopped by the stall detector' >&2
  exit 1
fi

# A leaked descendant holding stdout must not keep the watchdog waiting beyond its grace period.
leaked_started=$SECONDS
# shellcheck disable=SC2016 # Expanded by the child shell.
run_case 0 "$temp_dir/leaked" 2 8 -- bash -c '(sleep 30 & echo "$!" > "$1"); echo finished' _ "$temp_dir/leaked.pid"
kill "$(cat "$temp_dir/leaked.pid")" 2>/dev/null || true
grep -Fqx 'finished' "$temp_dir/leaked"
if ((SECONDS - leaked_started > 15)); then
  echo 'leaked stdout holder delayed the watchdog beyond its grace period' >&2
  exit 1
fi

expect_bad_arguments() {
  if PATH="$temp_dir/bin:$PATH" "$watchdog" "$@" > "$temp_dir/bad-arguments" 2>&1; then
    echo "bad arguments unexpectedly succeeded: $*" >&2
    exit 1
  else
    status=$?
  fi
  test "$status" -eq 2
}

expect_bad_arguments 0 8 -- true
expect_bad_arguments 2 8 true
expect_bad_arguments 2 8 --

if ((SECONDS - started_seconds >= 45)); then
  echo 'watchdog tests did not finish promptly' >&2
  exit 1
fi

echo 'JVM watchdog tests passed'
