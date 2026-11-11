#!/usr/bin/env bash

set -Eeuo pipefail

usage() {
  echo "usage: $0 <stall-seconds> <max-seconds> -- <command...>" >&2
  exit 2
}

is_positive_integer() {
  [[ $1 =~ ^[1-9][0-9]*$ ]]
}

if (($# < 4)) || ! is_positive_integer "$1" || ! is_positive_integer "$2" || [[ $3 != -- ]]; then
  usage
fi

stall_seconds=$1
max_seconds=$2
shift 3
if (($# == 0)); then
  usage
fi

poll_seconds=${WATCHDOG_POLL_SECONDS:-5}
if ! is_positive_integer "$poll_seconds"; then
  echo 'WATCHDOG_POLL_SECONDS must be a positive integer' >&2
  exit 2
fi

readonly TEE_GRACE_SECONDS=10

temp_dir=$(mktemp -d)
log_file="$temp_dir/output.log"
output_pipe="$temp_dir/output.pipe"
watchdog_pid=
timeout_pid=
tee_pid=

# Attaching to a wedged JVM can hang, so every jcmd call is bounded.
dump_threads() {
  timeout 20 jcmd -l || true
  timeout 20 jcmd -l 2>/dev/null | awk '$2 !~ /JCmd/ { print $1 }' | while read -r pid; do
    timeout 20 jcmd "$pid" Thread.print || true
  done || true
}

# shellcheck disable=SC2329 # Invoked by the EXIT trap below.
cleanup() {
  if [[ -n $watchdog_pid ]]; then
    kill "$watchdog_pid" 2>/dev/null || true
    wait "$watchdog_pid" 2>/dev/null || true
  fi
  rm -rf "$temp_dir"
}
trap cleanup EXIT

mkfifo "$output_pipe"
tee "$log_file" < "$output_pipe" &
tee_pid=$!

# Hard cap: 124 when the command honours TERM, 137 when --kill-after had to send KILL.
timeout --signal=TERM --kill-after=30s "${max_seconds}s" "$@" > "$output_pipe" 2>&1 &
timeout_pid=$!

(
  last_size=0
  last_progress=$SECONDS
  cap_dumped=false

  while kill -0 "$timeout_pid" 2>/dev/null; do
    sleep "$poll_seconds"
    current_size=$(wc -c < "$log_file")
    now=$SECONDS
    if ((current_size > last_size)); then
      last_size=$current_size
      last_progress=$now
    elif ((now - last_progress >= stall_seconds)); then
      echo "::error::JVM output stalled for ${stall_seconds} seconds; dumping JVM threads" >&2
      dump_threads
      : > "$temp_dir/stalled"
      # Signaling timeout lets it forward TERM to the Gradle process group.
      kill -TERM "$timeout_pid" 2>/dev/null || true
      exit
    fi

    if [[ $cap_dumped == false ]] && ((max_seconds > 60 && now >= max_seconds - 60)); then
      echo "::error::JVM hard cap of ${max_seconds} seconds is approaching; dumping JVM threads" >&2
      dump_threads
      cap_dumped=true
    fi
  done
) &
watchdog_pid=$!

if wait "$timeout_pid"; then
  command_status=0
else
  command_status=$?
fi
# A leaked descendant that inherited stdout keeps the FIFO open, so tee gets a bounded grace period.
for _ in $(seq 1 "$TEE_GRACE_SECONDS"); do
  kill -0 "$tee_pid" 2>/dev/null || break
  sleep 1
done
kill "$tee_pid" 2>/dev/null || true
wait "$tee_pid" || true
kill "$watchdog_pid" 2>/dev/null || true
wait "$watchdog_pid" 2>/dev/null || true
watchdog_pid=

if ((command_status != 0)) && [[ -e $temp_dir/stalled ]]; then
  exit 124
fi
exit "$command_status"
