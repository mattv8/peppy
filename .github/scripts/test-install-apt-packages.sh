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
# Measurement calls (du) are not apt calls, so they are kept out of the sudo log.
cat > "$fake_bin/sudo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" != du ]]; then
  printf '%s\n' "$*" >> "$FAKE_SUDO_LOG"
fi
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
# Plan letters, one per attempt: P = grow the phase directory then fail, F = fail without growth,
# S = succeed. Attempts past the end of a plan succeed.
plan_step() {
  local plan=$1 index=$2
  if ((index <= ${#plan})); then
    printf '%s' "${plan:index-1:1}"
  else
    printf S
  fi
}
grow() {
  local file="$FAKE_STATE/bytes-$1" bytes=0
  if [[ -f "$file" ]]; then bytes=$(<"$file"); fi
  printf '%s' "$((bytes + 1000))" > "$file"
}
case "$1" in
  update)
    attempt=$(bump update)
    if [[ "${FAKE_UPDATE_HANG_FIRST:-0}" == 1 && "$attempt" == 1 ]]; then
      sleep 30
    fi
    if [[ -n "${FAKE_UPDATE_PLAN:-}" ]]; then
      case "$(plan_step "$FAKE_UPDATE_PLAN" "$attempt")" in
        P) grow lists; exit 100 ;;
        F) exit 100 ;;
        *) exit 0 ;;
      esac
    fi
    if [[ -n "${FAKE_UPDATE_EXIT:-}" ]]; then
      exit "$FAKE_UPDATE_EXIT"
    fi
    ;;
  install)
    if [[ " $* " == *" --download-only "* ]]; then
      attempt=$(bump download)
      if [[ -n "${FAKE_DOWNLOAD_PLAN:-}" ]]; then
        case "$(plan_step "$FAKE_DOWNLOAD_PLAN" "$attempt")" in
          P) grow archives; exit 124 ;;
          F) exit 124 ;;
          *) exit 0 ;;
        esac
      fi
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
# Reports the per-directory byte count the fake apt-get grows; FAKE_DU_MODE simulates du that prints nothing or fails.
cat > "$fake_bin/du" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
dir="${*: -1}"
case "${FAKE_DU_MODE:-normal}" in
  silent) exit 0 ;;
  fail) exit 1 ;;
esac
bytes=0
file="$FAKE_STATE/bytes-$(basename "$dir")"
if [[ -f "$file" ]]; then bytes=$(<"$file"); fi
printf '%s\t%s\n' "$bytes" "$dir"
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
chmod +x "$fake_bin/sudo" "$fake_bin/apt-get" "$fake_bin/du" "$fake_bin/dpkg-query"

apt_options='-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=120'
packages=(build-essential pkg-config libssl-dev)
package_list="${packages[*]}"
update_call="$apt_options update"
download_call="$apt_options install --yes --download-only $package_list"
install_call="$apt_options install --yes $package_list"

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

# Prints one download-only call per requested attempt.
downloads() {
  local count=$1 index
  for ((index = 0; index < count; index++)); do
    printf '%s\n' "$download_call"
  done
}

# Runs one case in a subshell so a failing case is reported without hiding the others.
failed_cases=0
run_case() {
  local name=$1
  shift
  if ("$@"); then
    echo "ok: $name"
  else
    echo "FAIL: $name"
    failed_cases=$((failed_cases + 1))
  fi
}

case_defaults_bound_each_phase() {
  local case_dir="$temp_dir/defaults"
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
  local status=$?
  set -e
  if ((status != 0)); then
    fail "the default-tunable path must succeed (exit $status): $(cat "$case_dir/err")"
  fi
  expect_text "$(cat "$case_dir/sudo.log")" "timeout --kill-after=15s 120s apt-get $update_call
timeout --kill-after=15s 600s apt-get $download_call
apt-get $install_call" 'the default bounds must be 120 s for the index refresh and 600 s for the download'
}

case_download_progress_survives_five_timeouts() {
  CASE_ENV=(FAKE_DOWNLOAD_PLAN=PPPPP)
  run_installer download-progress "${packages[@]}"
  expect_status 0 'a download that makes progress on every timed-out attempt must keep going until it succeeds'
  expect_text "$(cat "$case_dir/apt.log")" "$(printf '%s\n' "$update_call"; downloads 6; printf '%s\n' "$install_call")" 'five progressing download timeouts must be followed by six download-only calls and one final install'
  grep -Fq 'after making progress' "$case_dir/err" || fail 'a progressing attempt must emit a progress warning'
}

case_download_stagnant_attempts_fail() {
  CASE_ENV=(FAKE_DOWNLOAD_PLAN=FFF)
  run_installer download-stagnant "${packages[@]}"
  expect_status 124 'three consecutive download attempts without progress must exit with the last status'
  expect_text "$(cat "$case_dir/apt.log")" "$(printf '%s\n' "$update_call"; downloads 3)" 'three stagnant download attempts must make exactly three download-only calls and never install'
  grep -Fq '::error::apt-get package download failed after 3 consecutive attempts without progress; last exit status 124' "$case_dir/err" || fail 'a stagnant download must report the consecutive-stagnation error'
}

case_progress_resets_stagnation_counter() {
  CASE_ENV=(FAKE_DOWNLOAD_PLAN=FFPFFS)
  run_installer download-reset "${packages[@]}"
  expect_status 0 'a progressing attempt must reset the stagnation counter so the download can succeed'
  expect_text "$(cat "$case_dir/apt.log")" "$(printf '%s\n' "$update_call"; downloads 6; printf '%s\n' "$install_call")" 'stagnant, progressing, stagnant, then success must make six download-only calls and one final install'
}

case_index_progress_uses_lists_directory() {
  CASE_ENV=(FAKE_UPDATE_PLAN=PFFF)
  run_installer index-progress "${packages[@]}"
  expect_status 100 'three consecutive index refreshes without progress must exit with the last status'
  expect_text "$(cat "$case_dir/apt.log")" "$(printf '%s\n' "$update_call" "$update_call" "$update_call" "$update_call")" 'a progressing index refresh must not count, so four update calls and no download or install'
  grep -Fq '::error::apt-get index refresh failed after 3 consecutive attempts without progress; last exit status 100' "$case_dir/err" || fail 'a stagnant index refresh must report the consecutive-stagnation error'
}

case_unmeasurable_progress_counts_as_stagnant() {
  local mode
  for mode in silent fail; do
    CASE_ENV=("FAKE_DU_MODE=$mode" FAKE_DOWNLOAD_PLAN=FFF)
    run_installer "du-$mode" "${packages[@]}"
    expect_status 124 "a du that prints nothing or fails ($mode) must be treated as zero bytes"
    expect_text "$(cat "$case_dir/apt.log")" "$(printf '%s\n' "$update_call"; downloads 3)" "a du that prints nothing or fails ($mode) must not stop the stagnation rule"
    grep -Fq '3 consecutive attempts without progress' "$case_dir/err" || fail "a du that is $mode must report the stagnation error"
  done
}

run_case 'defaults bound each phase at 120 s and 600 s' case_defaults_bound_each_phase
run_case 'download that progresses past five timeouts still succeeds' case_download_progress_survives_five_timeouts
run_case 'three stagnant download attempts fail' case_download_stagnant_attempts_fail
run_case 'progress resets the stagnation counter' case_progress_resets_stagnation_counter
run_case 'index refresh uses the lists directory for progress' case_index_progress_uses_lists_directory
run_case 'unmeasurable progress is treated as zero bytes' case_unmeasurable_progress_counts_as_stagnant

CASE_ENV=(FAKE_ALL_INSTALLED=1)
run_installer already-installed "${packages[@]}"
expect_status 0 'an installed package set must succeed without apt'
expect_text "$(cat "$case_dir/sudo.log")" '' 'an installed package set must not call sudo'
expect_text "$(cat "$case_dir/apt.log")" '' 'an installed package set must not call apt-get'

run_installer healthy "${packages[@]}"
expect_status 0 'the healthy path must succeed'
expect_text "$(cat "$case_dir/apt.log")" "$update_call
$download_call
$install_call" 'the healthy path must refresh, download, then install with the exact apt arguments'
expect_text "$(cat "$case_dir/sudo.log")" "timeout --kill-after=15s 1s apt-get $update_call
timeout --kill-after=15s 2s apt-get $download_call
apt-get $install_call" 'every apt-get call must go through sudo, with timeout wrapping apt-get for the bounded phases only'

run_installer strict --strict-index "${packages[@]}"
expect_status 0 'the strict healthy path must succeed'
expect_text "$(cat "$case_dir/apt.log")" "$apt_options update --error-on=any
$download_call
$install_call" '--strict-index must add --error-on=any to the index refresh only'

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
expect_text "$(cat "$case_dir/apt.log")" "$update_call
$update_call
$download_call
$install_call" 'a timed-out index refresh must be retried before downloading'
if ((elapsed >= 20)); then
  fail "a hung index refresh must be killed by its bound, not wait for the fake sleep (took ${elapsed}s)"
fi

CASE_ENV=(FAKE_DOWNLOAD_FAIL_TIMES=1)
run_installer download-fails-once "${packages[@]}"
expect_status 0 'a download that fails once must be retried and succeed'
expect_text "$(cat "$case_dir/apt.log")" "$update_call
$download_call
$download_call
$install_call" 'a retried download must be followed by exactly one final install'

CASE_ENV=(FAKE_DOWNLOAD_FAIL_TIMES=99)
run_installer download-always-fails "${packages[@]}"
expect_status 1 'a download that always fails must propagate its exit status'
expect_text "$(cat "$case_dir/apt.log")" "$update_call
$download_call
$download_call
$download_call" 'a download that always fails must never reach the final install'

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

if ((failed_cases > 0)); then
  echo "$failed_cases install-apt-packages case(s) failed" >&2
  exit 1
fi
echo 'install-apt-packages tests passed'
