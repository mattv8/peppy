#!/usr/bin/env bash

set -Eeuo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
installer="$script_dir/install-apt-packages.sh"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

# The real GNU timeout must be reachable: the bounded phases depend on it killing a hung apt-get.
if ! command -v timeout >/dev/null 2>&1; then
  echo "test-install-apt-packages.sh requires GNU timeout on PATH" >&2
  exit 1
fi
if ! timeout --version 2>/dev/null | grep -Fq 'GNU coreutils'; then
  echo "test-install-apt-packages.sh requires GNU coreutils timeout on PATH" >&2
  exit 1
fi
real_path=$PATH

fake_bin="$temp_dir/bin"
mkdir "$fake_bin"
cat > "$fake_bin/sudo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_SUDO_LOG"
exec "$@"
SH
cat > "$fake_bin/apt-get" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_APT_LOG"
while [[ "${1:-}" == -o ]]; do shift 2; done
bump() {
  local file="$FAKE_STATE/$1" count=0
  [[ -f "$file" ]] && count=$(<"$file")
  count=$((count + 1))
  printf '%s' "$count" > "$file"
  printf '%s' "$count"
}
case "$1" in
  update)
    attempt=$(bump update)
    if [[ "${FAKE_UPDATE_HANG_FIRST:-0}" == 1 && "$attempt" == 1 ]]; then
      sleep 30
    fi
    if [[ -n "${FAKE_UPDATE_EXIT:-}" ]]; then
      exit "$FAKE_UPDATE_EXIT"
    fi
    ;;
  install)
    if [[ " $* " == *" --download-only "* ]]; then
      attempt=$(bump download)
      if [[ -n "${FAKE_DOWNLOAD_FAIL_TIMES:-}" && "$attempt" -le "$FAKE_DOWNLOAD_FAIL_TIMES" ]]; then
        exit 1
      fi
    elif [[ -n "${FAKE_INSTALL_EXIT:-}" ]]; then
      exit "$FAKE_INSTALL_EXIT"
    fi
    ;;
  *)
    exit 97
    ;;
esac
SH
cat > "$fake_bin/dpkg-query" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${FAKE_ALL_INSTALLED:-0}" == 1 ]]; then
  printf 'installed'
else
  printf 'not-installed'
fi
SH
chmod +x "$fake_bin/sudo" "$fake_bin/apt-get" "$fake_bin/dpkg-query"

apt_options='-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=120'
packages=(build-essential pkg-config libssl-dev)
package_list="${packages[*]}"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

expect_text() {
  local actual=$1 expected=$2 message=$3
  if [[ "$actual" != "$expected" ]]; then
    printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
    fail "$message"
  fi
}

expect_status() {
  local expected=$1 message=$2
  if [[ "$last_status" != "$expected" ]]; then
    fail "$message (exit $last_status, expected $expected): $(cat "$case_dir/err")"
  fi
}

# Runs the installer with a fresh fake state. Extra environment assignments are passed in
# the caller's CASE_ENV array; installer arguments follow the case name.
run_installer() {
  local name=$1
  shift
  case_dir="$temp_dir/$name"
  mkdir -p "$case_dir/state"
  : > "$case_dir/sudo.log"
  : > "$case_dir/apt.log"
  local start=$SECONDS
  set +e
  env PATH="$fake_bin:$real_path" \
    FAKE_SUDO_LOG="$case_dir/sudo.log" \
    FAKE_APT_LOG="$case_dir/apt.log" \
    FAKE_STATE="$case_dir/state" \
    PEPPY_APT_RETRY_PAUSE_SECONDS=0 \
    PEPPY_APT_UPDATE_SECONDS=1 \
    PEPPY_APT_DOWNLOAD_SECONDS=2 \
    ${CASE_ENV[@]+"${CASE_ENV[@]}"} \
    bash "$installer" "$@" >"$case_dir/out" 2>"$case_dir/err"
  last_status=$?
  set -e
  elapsed=$((SECONDS - start))
  CASE_ENV=()
}

CASE_ENV=(FAKE_ALL_INSTALLED=1)
run_installer already-installed "${packages[@]}"
expect_status 0 'an installed package set must succeed without apt'
expect_text "$(cat "$case_dir/sudo.log")" '' 'an installed package set must not call sudo'
expect_text "$(cat "$case_dir/apt.log")" '' 'an installed package set must not call apt-get'

run_installer healthy "${packages[@]}"
expect_status 0 'the healthy path must succeed'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update
$apt_options install --yes --download-only $package_list
$apt_options install --yes $package_list" 'the healthy path must refresh, download, then install with the exact apt arguments'
expect_text "$(cat "$case_dir/sudo.log")" "timeout --kill-after=15s 1s apt-get $apt_options update
timeout --kill-after=15s 2s apt-get $apt_options install --yes --download-only $package_list
apt-get $apt_options install --yes $package_list" 'every apt-get call must go through sudo, with timeout wrapping apt-get for the bounded phases only'

# Without tunables the index refresh is bounded to 60 s and the download to 180 s.
case_dir="$temp_dir/defaults"
mkdir -p "$case_dir/state"
: > "$case_dir/sudo.log"
: > "$case_dir/apt.log"
set +e
env -u PEPPY_APT_UPDATE_SECONDS -u PEPPY_APT_DOWNLOAD_SECONDS -u PEPPY_APT_ATTEMPTS -u PEPPY_APT_RETRY_PAUSE_SECONDS \
  PATH="$fake_bin:$real_path" \
  FAKE_SUDO_LOG="$case_dir/sudo.log" \
  FAKE_APT_LOG="$case_dir/apt.log" \
  FAKE_STATE="$case_dir/state" \
  bash "$installer" "${packages[@]}" >"$case_dir/out" 2>"$case_dir/err"
last_status=$?
set -e
expect_status 0 'the default-tunable path must succeed'
expect_text "$(cat "$case_dir/sudo.log")" "timeout --kill-after=15s 60s apt-get $apt_options update
timeout --kill-after=15s 180s apt-get $apt_options install --yes --download-only $package_list
apt-get $apt_options install --yes $package_list" 'the default bounds must be 60 s for the index refresh and 180 s for the download'

run_installer strict --strict-index "${packages[@]}"
expect_status 0 'the strict healthy path must succeed'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update --error-on=any
$apt_options install --yes --download-only $package_list
$apt_options install --yes $package_list" '--strict-index must add --error-on=any to the index refresh only'

CASE_ENV=(FAKE_UPDATE_EXIT=100)
run_installer update-always-fails --strict-index "${packages[@]}"
expect_status 100 'a repeatedly failing index refresh must exit with the last attempt status'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update --error-on=any
$apt_options update --error-on=any
$apt_options update --error-on=any" 'a failing index refresh must make exactly three attempts and never install'
grep -Fq '::error::' "$case_dir/err" || fail 'a failed index refresh must emit an error annotation'

CASE_ENV=(FAKE_UPDATE_HANG_FIRST=1)
run_installer update-hangs "${packages[@]}"
expect_status 0 'a hung index refresh must be retried and succeed'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update
$apt_options update
$apt_options install --yes --download-only $package_list
$apt_options install --yes $package_list" 'a timed-out index refresh must be retried before downloading'
if ((elapsed >= 20)); then
  fail "a hung index refresh must be killed by its bound, not wait for the fake sleep (took ${elapsed}s)"
fi

CASE_ENV=(FAKE_DOWNLOAD_FAIL_TIMES=1)
run_installer download-fails-once "${packages[@]}"
expect_status 0 'a download that fails once must be retried and succeed'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update
$apt_options install --yes --download-only $package_list
$apt_options install --yes --download-only $package_list
$apt_options install --yes $package_list" 'a retried download must be followed by exactly one final install'

CASE_ENV=(FAKE_DOWNLOAD_FAIL_TIMES=99)
run_installer download-always-fails "${packages[@]}"
expect_status 1 'a download that always fails must propagate its exit status'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update
$apt_options install --yes --download-only $package_list
$apt_options install --yes --download-only $package_list
$apt_options install --yes --download-only $package_list" 'a download that always fails must never reach the final install'

CASE_ENV=(FAKE_INSTALL_EXIT=42)
run_installer install-fails "${packages[@]}"
expect_status 42 'a failing final install must propagate its exit status'

run_installer no-packages
expect_status 2 'no packages must be a usage error'
grep -Fq 'usage:' "$case_dir/err" || fail 'a usage error must print usage'
expect_text "$(cat "$case_dir/apt.log")" '' 'a usage error must not call apt-get'

run_installer unknown-option --bogus "${packages[@]}"
expect_status 2 'an unknown option must be a usage error'
expect_text "$(cat "$case_dir/apt.log")" '' 'an unknown option must not call apt-get'

CASE_ENV=(PEPPY_APT_UPDATE_SECONDS=abc)
run_installer invalid-update-seconds "${packages[@]}"
expect_status 2 'a non-integer update bound must be rejected'
grep -Fq 'PEPPY_APT_UPDATE_SECONDS' "$case_dir/err" || fail 'an invalid update bound must name its variable'
expect_text "$(cat "$case_dir/apt.log")" '' 'an invalid tunable must not call apt-get'

CASE_ENV=(PEPPY_APT_ATTEMPTS=0)
run_installer zero-attempts "${packages[@]}"
expect_status 2 'a zero attempt count must be rejected'
expect_text "$(cat "$case_dir/apt.log")" '' 'an invalid attempt count must not call apt-get'

echo 'install-apt-packages tests passed'
