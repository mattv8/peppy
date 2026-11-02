# Browser client reference

This reference covers browser-client implementation, deployment configuration, and checks. For repository-wide workflow, see [CONTRIBUTING.md](../../CONTRIBUTING.md). User requirements are in [Clients](../../README.md#clients).

The React desktop UI bundles with the Rust client core, SharedWorker runtime, and browser host integration. Build it from the root:

```sh
bash infra/build/build-web-client.sh
```

This writes `apps/web/dist/` with the UI and WASM core. It needs Emscripten 6.0.11, the pinned Rust toolchain with `wasm32-unknown-emscripten`, Node 26 selected by `.node-version`, and pnpm 12.8.1. Sources include Emscripten-built browser WASM, Vite entries `index.html` and `shared-worker.ts`, and `@peppy/browser-runtime` coordination.

## Deployment

Serve artifacts from `PEPPY_WEB_CLIENT_DIR`. Community sets `PEPPY_WEB_CLIENT_ROOT=true` by default and serves the UI at `/` with `/v1/*` APIs on that origin, using `PUBLIC_API_URL` as canonical reference. Hosted account and billing use the separate origin configured by `PEPPY_WEB_ASSET_DIR` and `PEPPY_WEB_ORIGIN`; the messaging origin `PEPPY_WEB_CLIENT_HOST` is separate and uses same-origin HTTPS fetches.

Community has no hosted account or billing site. Its Compose configuration maps `WEB_UI_ENABLED` to root mode; set `WEB_UI_ENABLED=false` to disable browser messaging. `WEB_CLIENT_HOST` is retired: move users to `https://PUBLIC_HOST/` and remove the separate hostname and DNS record. `WEB_CLIENT_ACCOUNT_URL` still configures external account navigation.

## Origin and security

Use same-origin HTTPS enforcement. Keep passphrase input out of React state and plaintext logs through imperative browser dialogs. Credential import uses an imperative file control and sends selected credentials directly to the worker, outside React state. The worker must not return device tokens or encryption keys in snapshots or renderer RPC results. Rust owns domain state; the worker stores encrypted checkpoints. API errors never render as HTML or fall back to SPA routes. Static non-GET/HEAD requests return errors without `index.html` fallback.

## Not claimed

This build does not prove production deployment, real-device carrier execution, native integration, or phone enrollment. Test it with a real browser against the generated WASM, phone enrollment on the canonical API origin with a maintained simulator or real server, and carrier-outcome reconciliation separate from app startup.

Deployment, DNS, TLS, reverse-proxy configuration, and release integration are operator work. See the [backup, restore and hosting runbook](../ops/backup-restore.md).

## Checks

```sh
pnpm --filter @peppy/web typecheck
pnpm --filter @peppy/web test
pnpm --filter @peppy/web build
```

Run desktop regression tests and SharedWorker/WASM loader checks with unit tests.
