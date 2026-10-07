#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
target=wasm32-unknown-emscripten
expected_emscripten=6.0.11
expected_rust=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$root/rust-toolchain.toml")
persistent_target_cache=${CARGO_TARGET_DIR:-}

fail() {
    printf 'browser-core: %s\n' "$*" >&2
    exit 1
}

refresh_workspace_inputs() {
    local directory

    touch -- "$root/Cargo.toml" "$root/Cargo.lock" "$root/rust-toolchain.toml"
    for directory in "$root/crates" "$root/vendor" "$root/services"; do
        [[ -d "$directory" ]] || continue
        find "$directory" -type d \( -name target -o -name .git -o -name cache \) -prune -o -type f -exec touch -- {} +
    done
}

[[ $# -eq 1 ]] || fail 'Usage: bash infra/build/build-browser-core.sh <output-directory>'
output=$1
[[ "$output" = /* ]] || output="$PWD/$output"
[[ -n "${EMSDK:-}" ]] || fail 'Activate Emscripten 6.0.11 with emsdk_env.sh before building.'
for tool in emcc emar emranlib cargo rustc; do
    command -v "$tool" >/dev/null || fail "Missing required tool: $tool"
done
emcc_version=$(emcc --version)
[[ "${emcc_version%%$'\n'*}" == *" $expected_emscripten "* ]] || fail "Expected Emscripten $expected_emscripten."
[[ "$(rustc --version)" == "rustc $expected_rust "* ]] || fail "Expected Rust $expected_rust."

target_dir=${CARGO_TARGET_DIR:-"$root/target/browser-core"}
[[ "$target_dir" = /* ]] || target_dir="$PWD/$target_dir"
export CARGO_TARGET_DIR="$target_dir"
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER
CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER=$(command -v emcc)
export AR
AR=$(command -v emar)
export RANLIB
RANLIB=$(command -v emranlib)
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS
CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS="-C link-arg=-sFORCE_FILESYSTEM -C link-arg=-sMODULARIZE=1 -C link-arg=-sEXPORT_ES6=1 -C link-arg=-sEXPORT_NAME=createPeppyCore -C link-arg=-sENVIRONMENT=worker -C link-arg=-sEXIT_RUNTIME=0 -C link-arg=-sINITIAL_MEMORY=536870912 -C link-arg=-sALLOW_MEMORY_GROWTH=1 -C link-arg=-sEXPORTED_RUNTIME_METHODS=FS,UTF8ToString,HEAPU8 -C link-arg=-sEXPORTED_FUNCTIONS=_main,_peppy_browser_alloc,_peppy_browser_dispatch,_peppy_browser_free_request,_peppy_browser_free_response"

# BuildKit's persistent target mount can be newer than source mtimes preserved by COPY.
[[ -z "$persistent_target_cache" ]] || refresh_workspace_inputs
cargo build --locked --manifest-path "$root/Cargo.toml" --release \
    --package peppy-browser-bindings --bin peppy-browser-core --target "$target"

js="$target_dir/$target/release/peppy-browser-core.js"
wasm="$target_dir/$target/release/peppy_browser_core.wasm"
[[ -s "$js" && -s "$wasm" ]] || fail 'The build did not produce both browser module files.'
mkdir -p -- "$output"
cp -- "$js" "$output/peppy-browser-core.js"
# Rust normalizes the WASM sidecar name; the generated ES module references this name.
cp -- "$wasm" "$output/peppy_browser_core.wasm"
printf 'Browser core module: %s\n' "$output/peppy-browser-core.js"
