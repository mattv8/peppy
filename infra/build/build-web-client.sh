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
command -v node >/dev/null || fail 'Missing node.'
[[ "$(pnpm --version)" == 12.8.1 ]] || fail 'Expected pnpm 12.8.1.'

cd "$root"
pnpm install --frozen-lockfile
pnpm --filter @peppy/web build
bash "$script_dir/build-browser-core.sh" "$core_output"

for asset in index.html worker.js core/peppy-browser-core.js core/peppy_browser_core.wasm; do
    [[ -s "$dist/$asset" ]] || fail "Missing built asset: $asset"
done
[[ -d "$dist/assets" ]] || fail 'Missing built asset directory: assets'

node - "$dist" <<'NODE'
const { readFileSync, readdirSync, renameSync, unlinkSync, writeFileSync } = require("node:fs");
const { join } = require("node:path");
const { brotliCompressSync, constants } = require("node:zlib");

const dist = process.argv[2];
const assets = [
  "core/peppy_browser_core.wasm",
  "core/peppy-browser-core.js",
  "worker.js",
  ...readdirSync(join(dist, "assets"), { withFileTypes: true })
    .filter((asset) => asset.isFile() && (asset.name.endsWith(".js") || asset.name.endsWith(".css")))
    .map(({ name }) => `assets/${name}`),
];

for (const asset of assets) {
  const source = join(dist, asset);
  const input = readFileSync(source);
  const compressed = brotliCompressSync(input, {
    params: {
      [constants.BROTLI_PARAM_QUALITY]: 11,
      [constants.BROTLI_PARAM_SIZE_HINT]: input.length,
    },
  });
  const sibling = `${source}.br`;
  if (compressed.length >= input.length) {
    try {
      unlinkSync(sibling);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    continue;
  }
  const temporary = `${sibling}.${process.pid}.tmp`;
  writeFileSync(temporary, compressed);
  renameSync(temporary, sibling);
}
NODE
