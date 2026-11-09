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
    cargo "${nextest_arguments[@]:0:2}" --workspace "${nextest_arguments[@]:2}"
else
    cargo "${nextest_arguments[@]:0:2}" --manifest-path "$repository_root/apps/desktop/src-tauri/Cargo.toml" "${nextest_arguments[@]:2}"
fi
