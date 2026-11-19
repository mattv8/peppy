#!/usr/bin/env bash

set -euo pipefail

maximum_partition_number=2147483647

usage() {
    echo "usage: $0 <workspace|desktop> <index> <count>" >&2
    exit 2
}

is_canonical_positive_integer() {
    [[ $1 =~ ^[1-9][0-9]*$ ]]
}

is_valid_partition_number() {
    local value=$1
    local maximum=$maximum_partition_number

    if ! is_canonical_positive_integer "$value"; then
        return 1
    fi
    if ((${#value} < ${#maximum})); then
        return 0
    fi
    if ((${#value} > ${#maximum})); then
        return 1
    fi
    # shellcheck disable=SC2071 # Same-length canonical decimals require lexical comparison.
    [[ $value < $maximum || $value == "$maximum" ]]
}

if (($# != 3)); then
    usage
fi

lane=$1
index=$2
count=$3
if [[ $lane != workspace && $lane != desktop ]] || ! is_valid_partition_number "$index" || ! is_valid_partition_number "$count"; then
    usage
fi
if ((index > count)); then
    usage
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repository_root="$(cd "$script_dir/../.." && pwd)"
cd "$repository_root"
nextest_arguments=(
    nextest run
    --locked
    --profile ci
    --config-file "$repository_root/.config/nextest.toml"
    --partition "hash:$index/$count"
)

if [[ $lane == workspace ]]; then
    target_dir=${CARGO_TARGET_DIR:-"$repository_root/target"}
else
    target_dir=${CARGO_TARGET_DIR:-"$repository_root/apps/desktop/src-tauri/target"}
fi

junit_path="$target_dir/nextest/ci/junit.xml"
report_safe=true
rm -f "$junit_path" || { echo "::warning::Could not remove stale JUnit report"; report_safe=false; }
status=0
if [[ -n ${PEPPY_NEXTEST_ARCHIVE:-} ]]; then
    if [[ $(basename "$target_dir") != target ]]; then
        echo "archive mode requires CARGO_TARGET_DIR to end in /target" >&2
        exit 2
    fi
    cargo nextest run --archive-file "$PEPPY_NEXTEST_ARCHIVE" \
        --extract-to "$(dirname "$target_dir")" --extract-overwrite \
        --profile ci --config-file "$repository_root/.config/nextest.toml" \
        --partition "hash:$index/$count" || status=$?
elif [[ $lane == workspace ]]; then
    cargo "${nextest_arguments[@]:0:2}" --workspace "${nextest_arguments[@]:2}" || status=$?
else
    cargo "${nextest_arguments[@]:0:2}" --manifest-path "$repository_root/apps/desktop/src-tauri/Cargo.toml" "${nextest_arguments[@]:2}" || status=$?
fi

if [[ -n ${GITHUB_STEP_SUMMARY:-} && $report_safe == true ]]; then
    if python3 -c '' >/dev/null 2>&1; then
        python=python3
    elif python -c '' >/dev/null 2>&1; then
        python=python
    else
        python=
        echo "::notice::Python unavailable; skipping timing report"
    fi
    if [[ -n $python ]]; then
        "$python" "$script_dir/ci-timing-report.py" junit --title "$lane shard $index/$count" "$junit_path" || echo "::warning::Timing report failed"
    fi
fi
exit "$status"
