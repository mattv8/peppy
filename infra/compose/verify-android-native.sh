#!/usr/bin/env bash
set -euo pipefail

: "${ANDROID_NDK_HOME:?ANDROID_NDK_HOME must name the installed Android NDK}"
caller_dir=$PWD
normalize_caller_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s/%s\n' "$caller_dir" "$1" ;;
    esac
}

ndk_home=$(normalize_caller_path "$ANDROID_NDK_HOME")
target_dir=$(normalize_caller_path "${CARGO_TARGET_DIR:-target}")
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"
export CARGO_TARGET_DIR="$target_dir"
native_profile="${PEPPY_ANDROID_NATIVE_PROFILE:-debug}"
case "$native_profile" in
  debug|release) ;;
  *) echo "PEPPY_ANDROID_NATIVE_PROFILE must be 'debug' or 'release', got '$native_profile'" >&2; exit 1 ;;
esac
ndk_prebuilt_root="$ndk_home/toolchains/llvm/prebuilt"
prebuilt_hosts=()
for candidate in "$ndk_prebuilt_root"/*; do
    test -d "$candidate" && prebuilt_hosts+=("$candidate")
done
test "${#prebuilt_hosts[@]}" -gt 0 || { echo "Android NDK LLVM toolchain is missing" >&2; exit 1; }

if test "${#prebuilt_hosts[@]}" -eq 1; then
    ndk_host=${prebuilt_hosts[0]}
else
    case "$(uname -s)" in
        Darwin) host_prefix=darwin- ;;
        Linux) host_prefix=linux- ;;
        *) host_prefix= ;;
    esac
    test -n "$host_prefix" || {
        echo "Android NDK has multiple LLVM prebuilts for an unsupported host" >&2
        exit 1
    }
    ndk_host=
    for candidate in "${prebuilt_hosts[@]}"; do
        if [[ $(basename "$candidate") == "$host_prefix"* ]]; then
            ndk_host=$candidate
            break
        fi
    done
    test -n "$ndk_host" || {
        echo "Android NDK has multiple LLVM prebuilts but no usable $host_prefix host tools" >&2
        exit 1
    }
fi
ndk_bin="$ndk_host/bin"
llvm_ar="$ndk_bin/llvm-ar"
llvm_ranlib="$ndk_bin/llvm-ranlib"
llvm_readelf="$ndk_bin/llvm-readelf"
for tool in "$llvm_ar" "$llvm_ranlib" "$llvm_readelf"; do
    test -x "$tool" || { echo "Android NDK tool is missing: $tool" >&2; exit 1; }
done

# Generate Kotlin from the host library before target-scoped NDK tools are exported.
SODIUM_USE_PKG_CONFIG=1 cargo build --locked -p peppy-mobile-bindings
host_library=$(find "$target_dir/debug" -maxdepth 1 -type f \( -name 'libpeppy_mobile_bindings.so' -o -name 'libpeppy_mobile_bindings.dylib' \) -print -quit)
test -n "$host_library" || { echo "Host mobile-bindings library is missing" >&2; exit 1; }
SODIUM_USE_PKG_CONFIG=1 cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library "$host_library" --language kotlin --out-dir apps/android/app/src/main/java

build_target() {
    local target=$1 abi=$2 clang="$ndk_bin/$3"
    local cargo_target upper_target cargo_args
    cargo_target=$(printf '%s' "$target" | tr '-' '_')
    upper_target=$(printf '%s' "$cargo_target" | tr '[:lower:]' '[:upper:]')
    test -x "$clang" || { echo "Android NDK compiler is missing: $clang" >&2; exit 1; }
    (
        unset SODIUM_USE_PKG_CONFIG
        export PATH="$ndk_bin:$PATH" CC="$clang" AR="$llvm_ar" RANLIB="$llvm_ranlib"
        export "CC_$cargo_target=$clang" "AR_$cargo_target=$llvm_ar" "RANLIB_$cargo_target=$llvm_ranlib"
        export "CARGO_TARGET_${upper_target}_LINKER=$clang" "CARGO_TARGET_${upper_target}_AR=$llvm_ar"
        export "CARGO_TARGET_${upper_target}_RUSTFLAGS=-C link-arg=-Wl,--no-undefined"
        cargo clean --target "$target" -p libsodium-sys-stable
        cargo_args=("--locked" "-p" "peppy-mobile-bindings" "--lib" "--target" "$target")
        if [[ "$native_profile" == "release" ]]; then
            cargo_args+=("--release")
        fi
        env "CC_$target=$clang" "AR_$target=$llvm_ar" "RANLIB_$target=$llvm_ranlib" \
            cargo build "${cargo_args[@]}"
    )
    local library_dir="$target_dir/$target/$native_profile"
    local library="$library_dir/libpeppy_mobile_bindings.so"
    test -s "$library" || { echo "Android mobile-bindings library is missing: $library" >&2; exit 1; }
    local unresolved
    unresolved=$("$llvm_readelf" --dyn-syms --wide "$library" | awk '$7 == "UND" { sub(/@.*/, "", $8); print $8 }' | grep -E '^(sodium_|randombytes_|crypto_)' || true)
    test -z "$unresolved" || { echo "Android $target library retains unresolved crypto symbols:" >&2; echo "$unresolved" >&2; exit 1; }
    mkdir -p "apps/android/app/src/main/jniLibs/$abi"
    cp "$library" "apps/android/app/src/main/jniLibs/$abi/"
}

build_target aarch64-linux-android arm64-v8a aarch64-linux-android26-clang
build_target x86_64-linux-android x86_64 x86_64-linux-android26-clang
