# Contributing to Peppy

Peppy is an in-progress developer foundation. Keep simulator evidence, native build evidence, carrier evidence, and production or store readiness separate. Use synthetic credentials only. Do not commit `.env`, generated credentials, passphrases, SQLCipher keys, device tokens, signing material, local databases, or build outputs.

## First run

Use Bash, Docker with Compose (version ≥2.24.4), Python 3.11+, and `just`. Container-only server work does not require host Rust, Node, or pnpm.

From WSL, keep the **current checkout** on a drive-letter NTFS path. Native Windows desktop actions reject ext4 and UNC paths. Install native Windows Node, pnpm, Rust, MSVC with the Windows SDK, and native Perl with `IPC::Cmd`; do not use Git/MSYS Perl. The desktop helper changes no global PATH or PowerShell profile.

```sh
bash infra/dev/dev.sh dev-setup
just dev-up
just dev-build
just dev-test
just dev-down
```

`dev-setup` creates a mode-`0600` `.env` with random synthetic local credentials only when `.env` is absent. It refuses a symlinked `.env` and preserves an existing file. It also installs or merges the tracked OpenChamber action template into ignored `.openchamber/project.json`, preserving existing local configuration. Run `just dev-actions` (or `bash infra/dev/dev.sh dev-actions`) to refresh actions without creating or changing `.env`. OpenChamber still asks you to trust shared commands; after refreshing, reopen or reselect the project if the actions do not appear. The action template and installer are `infra/dev/openchamber-project.json` and `infra/dev/install-actions.py`; use VS Code tasks from `.vscode/tasks.json`.

`just dev-up` stops and removes obsolete `api` and `migrate` containers from the former development topology (without removing volumes), then merges the development overlay with Compose, builds the tooling image, starts PostgreSQL and SeaweedFS, and runs the `dev` service. The `dev` service synchronizes the source, builds `cargo build --locked -p peppy-server`, runs pending migrations against PostgreSQL, and serves the API published to `127.0.0.1:7000` (internal binding `0.0.0.0:8080`). It waits for PostgreSQL and SeaweedFS to be healthy before starting, with a cold-startup timeout of 1800 seconds; cold-build compilation may take several minutes. Build or migration failure exits nonzero and never serves. `just dev-down` stops and removes the `dev`, PostgreSQL, and SeaweedFS containers and cleans up obsolete `api` and `migrate` containers without removing any database or cache volumes. Run it before base-stack checks such as `just smoke-infra` or `just storage-contract`, which share this project and host port. Production deployments retain a separate small release image with independent `api` and `migrate` services.

After first-run setup, OpenChamber and VS Code offer these five everyday shortcuts:

| Shortcut | What it does |
| --- | --- |
| Dev: Start development | Preserves `.env`, starts and waits for the backend, readies the emulator, and opens the latest desktop build; it does not build Android. |
| Desktop: Rebuild and open | Builds before opening a fresh desktop instance; it does not open a stale app after a build failure. |
| Android: Rebuild and open | Requires `PEPPY_ACCEPT_ANDROID_LICENSES=1` before any effect, then starts the backend, builds, readies the emulator, deploys, and opens the app. |
| iOS: Rebuild and open | macOS only: starts the backend, boots an iPhone simulator, rebuilds the Debug Rust library and iOS app, then installs and launches it. Requires full Xcode and an iOS 26+ simulator runtime. |
| Dev: Stop backend | Stops backend containers while preserving data and caches; native apps and the emulator remain running. |

The shortcuts require a running Docker daemon, installed native desktop tools, and a configured Android SDK/AVD where applicable. They do not reload a healthy backend; restart it to apply backend source changes. SDK license approval is always explicit. Granular `just` commands remain available, including `just dev-actions`, `just dev-setup`, `just dev-demo`, testing commands, and `just android-sms`; SMS is CLI-only. Retired editor actions are removed only when their original released command is unchanged, so customized actions are preserved. Reselect the project to review OpenChamber trust prompts after refreshing actions.

The API listens on `127.0.0.1:7000` by default; `API_HOST_PORT` changes that host port, while normal backend `BIND_ADDR` remains `0.0.0.0:8080` internally. PostgreSQL and SeaweedFS do not publish host ports. Run `bash infra/dev/dev.sh dev-demo` for an isolated synthetic gateway exercise. A successful run exercises private synthetic state, normal replay/sync of a new simulated message, and SQLCipher reopening. It is not carrier, keychain, password-dialog, store, or production evidence. The controller retains private synthetic credentials and logs under `.opencode/dev/artifacts/gateway-demo-*` for failure diagnosis; it does not retain the simulated vault passphrase on disk.

`just dev-up`, `just dev-build`, and `just dev-test` create the private artifact directory and the externally named, UID/GID-keyed Docker cache volumes before use. The volumes persist across container removal. After `just dev-up`, you can invoke the container PATH helper directly while the `dev` service is running:

```sh
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml exec dev run build server
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml exec dev run test rust
```

The running `dev` service runs a container-local copy of the API binary, independent of helper rebuilds. Helper `exec dev run build server` commands rebuild Cargo output but do not replace the serving binary; restart with `just dev-down` followed by `just dev-up` to apply source changes, which reruns pending migrations without deleting data. Source changes do not reload the running API without an explicit restart.

`just dev-build` and `just dev-test` use one-off `compose run --rm --no-deps dev run ...` commands and work with the `dev` service stopped. They use the last-built image (rebuild the image by running `just dev-up`) and create fresh workspaces with a shared Cargo cache. They do not publish a service port or start PostgreSQL and SeaweedFS. To override `PEPPY_WORKSPACE_LOCK_TIMEOUT` in a one-off command, pass it through Compose:

```sh
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml run --rm --no-deps -e PEPPY_WORKSPACE_LOCK_TIMEOUT=1800 dev run build server
```

Or with direct `exec`:

```sh
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml exec -e PEPPY_WORKSPACE_LOCK_TIMEOUT=1800 dev run build server
```

Queued container `run` commands, including demo and development tests, share a target-volume lock. `PEPPY_WORKSPACE_LOCK_TIMEOUT` defaults to `600` seconds and accepts at most `86400`. A cold build or stalled migration holds the lock through the 1800-second startup allowance; startup itself can fail its 600-second lock timeout if another helper holds the shared lock, and queued helpers may also time out before startup completes. A timeout does not clear or reset any cache or user data. If `--wait` times out, containers remain running; inspect logs and service status before assuming startup failed:

```sh
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml logs dev
DEV_UID=$(id -u) DEV_GID=$(id -g) docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml ps
```

Retry after startup completes, or override the timeout via `-e PEPPY_WORKSPACE_LOCK_TIMEOUT=<seconds>` in `run` or `exec` commands (see one-off build/test examples above).

## Choose a development surface

| Surface | Prerequisites | Commands |
| --- | --- | --- |
| Server Rust checks | Bash, Docker Compose, Python 3, just | `just dev-build`, `just dev-test` |
| Web PATH-helper checks | Running `dev` service | `docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml exec dev run build web`; `docker compose --env-file .env -f docker-compose.yml -f infra/compose/compose.dev.yml exec dev run test web` |
| Native macOS desktop | Node from `.node-version`, pnpm 12.8.1, Rust from `rust-toolchain.toml` | `just desktop-dev`, `just desktop-bundle`, `just desktop-run`, `just desktop-open` |
| Native Windows desktop from WSL | Current NTFS checkout plus native Windows Node, pnpm, Rust, MSVC/Windows SDK, WebView2, and native Perl | `just desktop-dev`, `just desktop-bundle`, `just desktop-run`, `just desktop-open` |
| Android builder | Docker Compose; explicit SDK license approval in shell environment or `.opencode/dev/android.env`; optional linux/amd64 image on Apple Silicon may run slowly under emulation | `just android-build` |
| Android emulator operations | Host Android SDK with `platform-tools`, an AVD, and emulator tools | `just android-emulator`, `just android-deploy`, `just android-smoke`, `just android-sms` |
| iOS host checks | macOS Command Line Tools, Swift, and generated mobile bindings | `just ios-test` |

On macOS, `desktop-dev`, `desktop-bundle`, and `desktop-run` use already-installed Homebrew `node@24` and `rustup` kegs when the current PATH is missing or mismatches the repository pins. They do not install tools or change global environment configuration; missing kegs leave the normal actionable pin check in place. For other commands, select the pinned kegs in the current terminal:

```sh
export PATH="$(brew --prefix rustup)/bin:$(brew --prefix node@24)/bin:$PATH"
```

The Android builder needs explicit SDK license approval. After reviewing the Android SDK licenses, set `PEPPY_ACCEPT_ANDROID_LICENSES=1` in the current shell or once in ignored `.opencode/dev/android.env`; a shell value, including `0` or empty, takes precedence over that file. It installs API 36, build-tools 35.0.0, and NDK 27.2.12479018. The Linux Android NDK prebuilts require the linux/amd64 builder image, including on Apple Silicon.

## Android emulator workflow

Set `ANDROID_SDK_ROOT` or `ANDROID_HOME`. Create normal host AVDs with Android Studio or `avdmanager`: `~/.android/avd` on macOS/Linux, or Windows `%USERPROFILE%\.android\avd` when using WSL. Without overrides, `just android-emulator` reuses one running emulator, or starts the sole configured AVD when none runs; it refuses zero or ambiguous choices. Set `PEPPY_ANDROID_AVD` in the shell or ignored `.opencode/dev/android.env` to choose an existing AVD, and `PEPPY_ANDROID_SERIAL` to choose a running `emulator-*` serial; shell values take precedence. Physical devices are rejected. The command returns after the emulator is ready and leaves it running.

Build output defaults to `.opencode/dev/artifacts/android/`: `app-debug.apk` and `app-debug-androidTest.apk`. Override the location with `PEPPY_ANDROID_ARTIFACTS`. `PEPPY_ANDROID_BOOT_TIMEOUT` defaults to `180`; `PEPPY_DEBUG_SERVER` defaults to `http://127.0.0.1:7000`. In WSL, the helper uses Windows SDK `adb.exe` and `emulator.exe`, obtains the SDK from `ANDROID_SDK_ROOT`, `ANDROID_HOME`, or Windows `LOCALAPPDATA`, and checks `PEPPY_DEBUG_SERVER/healthz` from Windows before `adb reverse`. It supports SDK and APK paths with spaces.

```sh
PEPPY_ACCEPT_ANDROID_LICENSES=1 just android-build
just android-emulator
just android-deploy
just android-smoke
just android-sms +15555550123 "synthetic test message"
```

`android-smoke` installs the debug and instrumentation APKs and accepts only an instrumentation result with `OK` for at least one test and `INSTRUMENTATION_CODE: -1`. It does not wipe or uninstall an app when signatures conflict. SMS remains configurable through `just android-sms <number> <message>`. On WSL, `android-emulator` uses the tracked Windows helper to start or stop only its owned process.

## iOS Simulator workflow

On macOS, `just ios-run` requires full Xcode with an iOS Simulator SDK and an iOS 26+ runtime installed. It checks Xcode, Rust, Python, and an available compatible iPhone simulator, starts the backend, boots and opens the simulator frontend, then builds, installs, and launches the Debug app. It builds the current Rust library and verifies the generated Swift bindings before the Xcode build; failed builds never deploy a stale app. The command respects `DEVELOPER_DIR`. Xcode 26 uses Simulator.app; Xcode 27+ uses Device Hub. When Command Line Tools are selected, it automatically uses `/Applications/Xcode.app` without changing `xcode-select`.

Without overrides, `just ios-run` reuses one booted compatible iPhone simulator, or selects a deterministic iPhone from the newest available compatible runtime when none is booted; multiple booted compatible iPhones require an explicit selection. Set `PEPPY_IOS_SIMULATOR` in the shell to an exact available device name or UDID to choose a specific device; ambiguous names are rejected. Physical devices are never targeted. The command does not download runtimes, accept licenses, create, or erase devices; missing runtime errors point to Xcode Settings > Components.

Build output and derived data default to `.opencode/dev/artifacts/ios/`. Override the location with `PEPPY_IOS_ARTIFACTS`; Rust output respects `CARGO_TARGET_DIR`. Temporary binding generation and compiler wrappers live in `.opencode/sessions/ios-simulator-actions/` (override with `PEPPY_IOS_SCRATCH`) and are removed after each build. The app bundle ID is `dev.peppy.mobile`. Simulator checks are not evidence of real-device carrier capabilities.

If the build reports Swift binding drift, regenerate only the Swift bindings from the repository root, then rerun `just ios-run`:

```sh
cargo run --locked -p peppy-mobile-bindings --features cli --bin uniffi-bindgen -- generate --library "${CARGO_TARGET_DIR:-target}/debug/libpeppy_mobile_bindings.dylib" --language swift --out-dir apps/ios/Generated
```

## Native desktop workflow

On macOS, `desktop-dev` runs Tauri's hot-reload development server using the `tauri.dev.conf.json` overlay with dev identity `org.peppy.desktop.dev`. `desktop-bundle` and `desktop-run` build release bundles with the same dev overlay; they generate only the `.app` bundle at `apps/desktop/src-tauri/target/release/bundle/macos/Peppy_dev.app` with no DMG installer created locally. `desktop-bundle` builds without opening; `desktop-run` builds then opens the app in place. `desktop-open` opens an existing `Peppy_dev.app` in place without installing it to `/Applications`. Set `CARGO_TARGET_DIR` to choose a different target directory. `desktop-dev` and `desktop-bundle` run `pnpm install --frozen-lockfile`.

Development builds are not Developer ID signed or notarized; macOS may apply ad-hoc linker signing without a TeamIdentifier or sealed resources. Development uses a separate Keychain namespace. The first dev launch creates an empty profile. Any prior app data and Keychain records stored under the production identity `org.peppy.desktop` remain unchanged; there is no automatic migration. Legacy Keychain records copy only within the current namespace, never from production into dev.

Each ad-hoc rebuild has a new code identity, so macOS asks again before the dev app can read its Keychain record. To stop the prompts across rebuilds, set `PEPPY_MACOS_SIGNING_IDENTITY` to the exact name or SHA-1 of an Apple Development identity (Xcode, signed in with your Apple ID) or a Developer ID identity, then choose **Always Allow** once. This applies when building the bundle; `desktop-open` only opens the existing bundle, and `desktop-dev` does not use it. Switching identities asks once more, and Apple Development certificates expire after a year.

Production installers use the `tauri.conf.json` configuration (`productName: Peppy`, identifier: `org.peppy.desktop`) and are downloaded from [GitHub releases](https://github.com/mattv8/peppy/releases).

From WSL, the same commands invoke PowerShell against the current Windows checkout. Windows builds write NSIS/MSI bundles below `$CARGO_TARGET_DIR/release/bundle/{nsis,msi}`; `desktop-open` starts `$CARGO_TARGET_DIR/release/peppy-desktop.exe`. The helper uses process-scoped `-ExecutionPolicy Bypass` with `-NoProfile` and does not change machine or user policy. Neither platform path proves a signed, notarized, or production-distributable artifact. See [apps/desktop/README.md](apps/desktop/README.md) for details.

Windows native pnpm installs win32 dependencies into the shared NTFS checkout's `node_modules`. Reinstall dependencies before returning to Linux-side pnpm work in that checkout.

## Checks and audits

Run the smallest relevant check before relying on a change:

```sh
just cargo-fmt
just lint
just server-test
just contracts-check
just desktop-test
just android-test
just ios-test
just ffi-smoke
just audit-dependencies
just audit-secrets
```

Use `just integration-test` for Compose-backed persistence coverage. `just audit-dependencies` runs `cargo deny check licenses bans sources`, `cargo deny check advisories`, and validates upstream audit coverage for vendored crates via `infra/audit/check-vendored.py`. The vendored audit script invokes `cargo audit --deny warnings --file <lock>`, which fails on vulnerabilities, unmaintained packages, unsound packages, and yanked packages. Ensure `cargo-audit` version 0.22.2 is installed (`cargo install cargo-audit --version 0.22.2 --locked`); `just audit-dependencies` requires it. `just audit-secrets` uses `gitleaks` against current source, including untracked source while excluding ignored local credentials. Generated contracts cover envelope/domain schemas and UniFFI bindings, not every REST adapter. Route changes need matching client changes and real-server integration checks.

Run final checks with `--locked`. Do not hand-edit `Cargo.lock`; only Cargo may resolve it. The root Cargo manifest and lockfile are shared integration files, so do not regenerate the root lockfile concurrently with another package change. Document verification precisely: name the command and platform, and mark unrun hardware, carrier, simulator, or native click-through steps. Keep reusable instructions in tracked documentation rather than session scratch files.

## Dependency management

Declare shared JavaScript versions in the `catalog` in `pnpm-workspace.yaml` and use `catalog:` in package dependency declarations. Keep package-specific dependencies in their owning manifest and retain the root `pnpm-lock.yaml`. Catalog updates to React and react-dom change peer contracts; verify that `packages/desktop-ui/package.json` peer ranges remain compatible after catalog updates.

Declare shared Rust versions and paths in the root `[workspace.dependencies]` and inherit them with `workspace = true`. Keep package and target-specific feature flags local. The excluded Tauri workspace in `apps/desktop/src-tauri/` retains its separate manifest and `Cargo.lock`; Android and iOS retain their native dependency files. Shared Rust pins affect both the public-workspace consumers and the separate Tauri workspace and private peppy-platform repository; verify and refresh their lockfiles when core workspace dependency versions change.

## Security and recovery limits

The manually shared vault passphrase stays on clients. React receives sanitized view models only, never passphrases, database keys, device credentials, or encryption keys. SMS/MMS carrier transport is plaintext outside the application boundary. Keep transport acknowledgement, local durable receipt, application state, carrier submission, and delivery evidence separate. Do not automatically resend a carrier attempt with an uncertain outcome. Public attachment copies are explicit plaintext derivatives, separate from encrypted originals.

Native clients require manual credential import and manual passphrase unlock. Snapshot history cannot execute carrier work. The Android shell remains companion-first: do not request the default-SMS role, `WRITE_SMS`, hidden APIs, or unverified RCS access. Do not claim store or carrier readiness from simulator or native-build results. Health routes must not disclose configuration, credentials, or dependency diagnostics. See [infra/compose/README.md](infra/compose/README.md) before backup, restore, key rotation, revocation, or recovery work. Do not use `docker compose down -v` as a recovery shortcut.

## Branches and pull requests

Branch from `production`, use Conventional Commit messages on each commit, and open a pull request to `production` (or use `staging` for prerelease integration). For contributors without bypass permission, the `gate` check must pass and the branch must be up to date; use **Update branch** when needed. Rebase is the default merge method (`gh pr merge --auto --rebase`); merge commits are also allowed (`--merge`), while squash merges are disabled. Branches auto-delete after merging. PR CI runs only jobs affected by changed paths; pushes to `staging` publish prereleases and trusted Harbor staging images after `gate`; production pushes advance only the community edge image.

Repository Admin and Maintain roles can bypass pull-request requirements, required CI checks, and force-push restrictions on `production`. A separate ruleset blocks deletion of the default branch and grants no bypass permissions.

## Commit messages and versioning

Use Conventional Commits (`type(scope): message`) to drive automatic version bumps. While major version is 0, the rules are: `feat` and breaking changes bump minor; `fix` and `perf` bump patch; `docs`, `ci`, `build`, `refactor`, `test`, `chore`, and `style` do not bump. Mark breaking changes with `type!:` (for example `feat!:`) or a `BREAKING CHANGE:` footer; while the version is `0.x` they bump minor. Merge commits are ignored; unconventional non-merge subjects bump patch.

Moving to `1.0.0` is explicit: set the **Release** workflow's `version` input. After changing `infra/release/` or the release workflows, run `just release-test`. Before the branch migration, an administrator must create protected `production` and `staging` branches, configure their Harbor/cosign environment secrets, require the `gate` job in branch rulesets, and run `infra/release/migrate-branches.sh` (add `--apply` only after its validation succeeds). The script sets the remote default branch but intentionally does not weaken or rewrite rulesets.

## Troubleshooting

Run `just doctor` only when you need the full host-native prerequisite check; container-only server work can use the recipes above. If a command reports a missing output, build that surface first. If emulator selection is ambiguous, set `PEPPY_ANDROID_SERIAL`; if no AVD exists, create one explicitly in Android Studio or with SDK tooling. An isolated demo fails closed on credential or bootstrap validation and retains private per-run logs and `credential-metadata.json` under `.opencode/dev/artifacts/gateway-demo-*`; inspect those diagnostics privately. Do not dump, reuse, or edit authentication files or passphrases, and do not retry automatically. Do not reset volumes, credentials, or client stores automatically to recover from a failure.
