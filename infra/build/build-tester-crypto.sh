#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
root=$(CDPATH='' cd -- "$script_dir/../.." && pwd -P)
target=wasm32-unknown-emscripten
expected_emscripten=6.0.11
expected_rust=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$root/rust-toolchain.toml")

fail() { printf 'tester-crypto: %s\n' "$*" >&2; exit 1; }
[[ $# -eq 1 ]] || fail 'Usage: bash infra/build/build-tester-crypto.sh <output-directory>'
output=$1; [[ "$output" = /* ]] || output="$PWD/$output"
[[ -n "${EMSDK:-}" ]] || fail 'Activate Emscripten 6.0.11 with emsdk_env.sh before building.'
for tool in emcc emar emranlib cargo rustc; do command -v "$tool" >/dev/null || fail "Missing required tool: $tool"; done
[[ "$(emcc --version)" == *" $expected_emscripten "* ]] || fail "Expected Emscripten $expected_emscripten."
[[ "$(rustc --version)" == "rustc $expected_rust "* ]] || fail "Expected Rust $expected_rust."
target_dir=${CARGO_TARGET_DIR:-"$root/target/tester-crypto"}; export CARGO_TARGET_DIR="$target_dir"
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_LINKER="$(command -v emcc)" AR="$(command -v emar)" RANLIB="$(command -v emranlib)"
export CARGO_TARGET_WASM32_UNKNOWN_EMSCRIPTEN_RUSTFLAGS='-C link-arg=-sMODULARIZE=1 -C link-arg=-sEXPORT_ES6=1 -C link-arg=-sEXPORT_NAME=createPeppyTesterCrypto -C link-arg=-sENVIRONMENT=worker -C link-arg=-sEXIT_RUNTIME=0 -C link-arg=-sINITIAL_MEMORY=536870912 -C link-arg=-sALLOW_MEMORY_GROWTH=1 -C link-arg=-sEXPORTED_RUNTIME_METHODS=UTF8ToString,HEAPU8 -C link-arg=-sEXPORTED_FUNCTIONS=_main,_peppy_tester_crypto_alloc,_peppy_tester_crypto_dispatch,_peppy_tester_crypto_free_request,_peppy_tester_crypto_free_response'
cargo build --locked --manifest-path "$root/Cargo.toml" --release --package peppy-tester-crypto --bin peppy-tester-crypto --target "$target"
js="$target_dir/$target/release/peppy-tester-crypto.js"; wasm="$target_dir/$target/release/peppy_tester_crypto.wasm"
[[ -s "$js" && -s "$wasm" ]] || fail 'The build did not produce both worker module files.'
mkdir -p -- "$output"; cp -- "$js" "$output/peppy-tester-crypto.js"; cp -- "$wasm" "$output/peppy_tester_crypto.wasm"
printf 'Tester crypto module: %s\n' "$output/peppy-tester-crypto.js"
