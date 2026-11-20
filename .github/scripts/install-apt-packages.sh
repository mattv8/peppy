#!/usr/bin/env bash
#
# Installs apt packages on a Linux runner with bounded, retried index refresh and download
# phases. The final install is deliberately unbounded so dpkg is never killed mid-unpack.
# An attempt that grows its phase's apt directory made progress and is retried without a cap;
# the workflow step timeout is the only overall limit on progressing attempts.
#
# Usage: install-apt-packages.sh [--strict-index] <package>...

set -euo pipefail

apt_options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=120)
strict_index=false

usage() {
  echo "usage: $0 [--strict-index] <package>..." >&2
}

# Prints an integer tunable from the environment; exits 2 when it is not an integer of at least MINIMUM.
read_count() {
  local name=$1 default=$2 minimum=$3 value
  value="${!name:-$default}"
  if [[ ! "$value" =~ ^[0-9]+$ ]] || ((10#$value < minimum)); then
    echo "$name must be an integer of at least $minimum: $value" >&2
    exit 2
  fi
  printf '%s' "$((10#$value))"
}

# Prints the total bytes under an apt directory, partial downloads included; 0 when unmeasurable.
measure_progress() {
  local bytes
  bytes=$(sudo du -sb "$1" 2>/dev/null | cut -f1 || true)
  if [[ "$bytes" =~ ^[0-9]+$ ]]; then
    printf '%s' "$bytes"
  else
    printf '0'
  fi
}

# Runs one apt-get phase under a time bound. Attempts that grow PROGRESS_DIR are retried; $attempts
# consecutive attempts without growth fail the phase with the last attempt's exit status.
run_bounded_phase() {
  local phase=$1 seconds=$2 progress_dir=$3 attempt=0 stagnant=0 before after status
  shift 3
  while true; do
    attempt=$((attempt + 1))
    before=$(measure_progress "$progress_dir")
    status=0
    sudo timeout --kill-after=15s "${seconds}s" "$@" || status=$?
    if ((status == 0)); then
      return 0
    fi
    after=$(measure_progress "$progress_dir")
    if ((after > before)); then
      stagnant=0
      echo "::warning::apt-get $phase attempt $attempt ended with exit status $status after making progress ($((after - before)) bytes); continuing" >&2
    else
      stagnant=$((stagnant + 1))
      if ((stagnant >= attempts)); then
        echo "::error::apt-get $phase failed after $stagnant consecutive attempts without progress; last exit status $status" >&2
        exit "$status"
      fi
      echo "::warning::apt-get $phase attempt $attempt ended with exit status $status without progress; retrying ($stagnant of $attempts)" >&2
    fi
    sleep "$retry_pause_seconds"
  done
}

while [[ $# -gt 0 && "$1" == --* ]]; do
  case "$1" in
    --strict-index)
      strict_index=true
      ;;
    *)
      usage
      exit 2
      ;;
  esac
  shift
done

if [[ $# -eq 0 ]]; then
  usage
  exit 2
fi
packages=("$@")

update_seconds=$(read_count PEPPY_APT_UPDATE_SECONDS 120 1)
download_seconds=$(read_count PEPPY_APT_DOWNLOAD_SECONDS 600 1)
attempts=$(read_count PEPPY_APT_ATTEMPTS 3 1)
retry_pause_seconds=$(read_count PEPPY_APT_RETRY_PAUSE_SECONDS 5 0)

all_installed=true
for package in "${packages[@]}"; do
  if [[ "$(dpkg-query -W -f='${db:Status-Status}' "$package" 2>/dev/null || true)" != installed ]]; then
    all_installed=false
    break
  fi
done
if [[ "$all_installed" == true ]]; then
  echo "All requested packages are already installed; nothing to install."
  exit 0
fi

update_command=(apt-get "${apt_options[@]}" update)
if [[ "$strict_index" == true ]]; then
  update_command+=(--error-on=any)
fi
run_bounded_phase "index refresh" "$update_seconds" /var/lib/apt/lists "${update_command[@]}"

run_bounded_phase "package download" "$download_seconds" /var/cache/apt/archives \
  apt-get "${apt_options[@]}" install --yes --download-only "${packages[@]}"

sudo apt-get "${apt_options[@]}" install --yes "${packages[@]}"
