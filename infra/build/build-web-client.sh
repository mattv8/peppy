#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
dist="$root/apps/web/dist"
core_output="$dist/core"

fail() {
    printf 'web-client: %s\n' "$*" >&2
    exit 1
}

[[ -f "$root/apps/web/package.json" ]] || fail 'Missing apps/web/package.json.'
command -v pnpm >/dev/null || fail 'Missing pnpm.'
[[ "$(pnpm --version)" == 12.8.1 ]] || fail 'Expected pnpm 12.8.1.'

cd "$root"
pnpm install --frozen-lockfile
pnpm --filter @peppy/web build
bash "$script_dir/build-browser-core.sh" "$core_output"

for asset in index.html worker.js core/peppy-browser-core.js core/peppy_browser_core.wasm; do
    [[ -s "$dist/$asset" ]] || fail "Missing built asset: $asset"
done
