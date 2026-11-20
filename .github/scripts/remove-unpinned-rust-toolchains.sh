#!/usr/bin/env bash
set -euo pipefail

if (($# != 1)) || [[ -z $1 ]]; then
    echo "usage: $0 <pinned-version>" >&2
    exit 2
fi

pinned=$1
toolchains=$(rustup toolchain list)
found=false
while read -r name _; do
    if [[ $name == "$pinned" || $name == "$pinned"-* ]]; then
        found=true
    fi
done <<< "$toolchains"
if [[ $found != true ]]; then
    echo "pinned Rust toolchain $pinned is not installed" >&2
    exit 1
fi

while read -r name _; do
    if [[ -n $name && $name != "$pinned" && $name != "$pinned"-* ]]; then
        rustup toolchain uninstall "$name"
    fi
done <<< "$toolchains"

remaining=$(rustup toolchain list)
printf '%s\n' "$remaining"
found=false
while read -r name _; do
    if [[ $name == "$pinned" || $name == "$pinned"-* ]]; then
        found=true
    fi
done <<< "$remaining"
if [[ $found != true ]]; then
    echo "pinned Rust toolchain $pinned is not installed" >&2
    exit 1
fi
