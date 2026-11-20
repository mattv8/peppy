#!/usr/bin/env bash
#
# Installs apt packages on a Linux runner with bounded, retried index refresh and download
# phases. The final install is deliberately unbounded so dpkg is never killed mid-unpack.
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

# Runs one apt-get phase under a time bound, retrying failed or timed-out attempts.
run_bounded_phase() {
  local phase=$1 seconds=$2 attempt status
  shift 2
  for ((attempt = 1; attempt <= attempts; attempt++)); do
    status=0
    sudo timeout --kill-after=15s "${seconds}s" "$@" || status=$?
    if ((status == 0)); then
      return 0
    fi
    if ((attempt == attempts)); then
      echo "::error::apt-get $phase failed after $attempts attempts; last exit status $status" >&2
      exit "$status"
    fi
    echo "::warning::apt-get $phase attempt $attempt of $attempts failed with exit status $status; retrying" >&2
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

update_seconds=$(read_count PEPPY_APT_UPDATE_SECONDS 60 1)
download_seconds=$(read_count PEPPY_APT_DOWNLOAD_SECONDS 180 1)
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
run_bounded_phase "index refresh" "$update_seconds" "${update_command[@]}"

run_bounded_phase "package download" "$download_seconds" \
  apt-get "${apt_options[@]}" install --yes --download-only "${packages[@]}"

sudo apt-get "${apt_options[@]}" install --yes "${packages[@]}"
