#!/usr/bin/env bash
set -euo pipefail

ROOT=${PEPPY_REPOSITORY_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
ROOT=$(cd "$ROOT" && pwd)
SCRATCH=${PEPPY_CI_SCRATCH:-"$ROOT/.opencode/sessions/ci-tests"}
TARGET_DIR=${CARGO_TARGET_DIR:-"$ROOT/target"}
[[ $TARGET_DIR == /* ]] || TARGET_DIR="$ROOT/$TARGET_DIR"
cd "$ROOT"

die() { echo "ci-test: $*" >&2; exit 1; }
suite() { printf '\n== %s ==\n' "$1"; }
require_command() { command -v "$1" >/dev/null || die "$1 is required for full local CI coverage"; }

preflight() {
    [[ $(uname) == Darwin ]] || die "full local CI coverage requires macOS and Xcode"
    xcode-select -p >/dev/null 2>&1 || die "full Xcode is required"
    xcrun --sdk iphonesimulator --show-sdk-path >/dev/null 2>&1 || die "the iOS Simulator SDK is required"
    [[ ${PEPPY_ACCEPT_ANDROID_LICENSES:-} == 1 ]] || die "PEPPY_ACCEPT_ANDROID_LICENSES=1 is required after reviewing Android SDK licenses"
    [[ -f .env ]] || die ".env is required; run bash infra/dev/dev.sh dev-setup first"
    local command
    for command in cargo docker git-cliff gitleaks just node pnpm python3 shellcheck swift; do
        require_command "$command"
    done
    python3 -c 'import sys; raise SystemExit(sys.version_info < (3, 11))' || die "Python 3.11 or newer is required for full local CI coverage"
    docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is required for full local CI coverage"
    cargo deny --version >/dev/null 2>&1 || die "cargo-deny is required; install version 0.20.2"
    cargo audit --version >/dev/null 2>&1 || die "cargo-audit is required; install version 0.22.2"
    git-cliff --version >/dev/null 2>&1 || die "git-cliff is required for release tooling"
}

run_integration_tests() {
    suite "Disposable integration tests"
    (
        # shellcheck disable=SC1091
        source infra/compose/test-env.sh
        # shellcheck disable=SC2329
        cleanup_integration() {
            local status=$?
            peppy_test_compose logs --no-color || true
            peppy_test_infra_down || true
            exit "$status"
        }
        trap cleanup_integration EXIT
        peppy_test_infra_up
        cargo test --workspace --locked
        cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml -- --ignored
        peppy_test_infra_down
        trap - EXIT
    )
}

check_generated_bindings() {
    suite "Mobile bindings and Swift"
    local bindings_scratch kotlin_dir swift_dir
    bindings_scratch=$(mktemp -d "$SCRATCH/bindings.XXXXXX")
    kotlin_dir="$bindings_scratch/kotlin"
    swift_dir="$bindings_scratch/swift"
    mkdir -p "$kotlin_dir" "$swift_dir"
    cargo build -p peppy-mobile-bindings --locked
    cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library "$TARGET_DIR/debug/libpeppy_mobile_bindings.dylib" --language kotlin --out-dir "$kotlin_dir"
    cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library "$TARGET_DIR/debug/libpeppy_mobile_bindings.dylib" --language swift --out-dir "$swift_dir"
    diff -ru apps/android/app/src/main/java/uniffi "$kotlin_dir/uniffi"
    diff -ru --exclude=module.modulemap apps/ios/Generated "$swift_dir"
    cargo test -p peppy-mobile-bindings --locked
    (cd apps/ios && DYLD_LIBRARY_PATH="$TARGET_DIR/debug${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" swift test -Xlinker -L -Xlinker "$TARGET_DIR/debug" --no-parallel)
    (cd apps/ios && DYLD_LIBRARY_PATH="$TARGET_DIR/debug${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" swift run -Xlinker -L -Xlinker "$TARGET_DIR/debug" PeppyMobileSmoke)
}

main() {
    suite "Preflight"
    preflight
    mkdir -p "$SCRATCH"
    chmod 0700 "$SCRATCH"

    suite "Release tooling and source audit"
    shellcheck infra/release/*.sh
    infra/release/test-compute-version.sh
    infra/release/test-release-scripts.sh
    just audit-secrets

    suite "Development tooling"
    node packages/mobile-design/scripts/generate.mjs --check
    node --test packages/mobile-design/test/*.test.mjs
    python3 -m unittest discover -s tests/dev -p 'test_*.py'
    bash -n infra/dev/dev.sh infra/dev/container-run.sh infra/dev/android.sh infra/dev/android-container-run.sh infra/dev/ci-test.sh infra/dev/desktop.sh infra/compose/verify-android-native.sh
    shellcheck infra/dev/dev.sh infra/dev/container-run.sh infra/dev/android.sh infra/dev/android-container-run.sh infra/dev/ci-test.sh infra/dev/desktop.sh infra/compose/verify-android-native.sh .github/scripts/*.sh
    bash .github/scripts/test-ci-scripts.sh

    suite "Rust workspace"
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    cargo test --workspace --lib --exclude peppy-push-relay --locked
    cargo run --locked -p peppy-protocol --bin generate-contracts -- --check
    cargo deny check licenses bans sources
    cargo deny check advisories
    python3 infra/audit/check-vendored.py

    suite "Frontend and desktop"
    pnpm install --frozen-lockfile
    pnpm --filter @peppy/browser-runtime typecheck
    pnpm --filter @peppy/browser-runtime test
    pnpm --filter @peppy/desktop-ui test
    pnpm --filter @peppy/desktop test
    pnpm --filter @peppy/desktop build
    pnpm --filter @peppy/web typecheck
    pnpm --filter @peppy/web test
    pnpm --filter @peppy/web build

    suite "Tauri"
    cargo check --locked --manifest-path apps/desktop/src-tauri/Cargo.toml
    cargo test --locked --manifest-path apps/desktop/src-tauri/Cargo.toml
    cargo deny --manifest-path apps/desktop/src-tauri/Cargo.toml --config deny.toml check licenses bans sources advisories

    run_integration_tests

    suite "Android"
    PEPPY_ANDROID_BUILD_BACKEND=docker bash infra/dev/android.sh test

    check_generated_bindings
}

main "$@"
