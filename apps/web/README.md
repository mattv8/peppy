# Browser messaging UI

React desktop UI bundled for browser with the Rust client core, SharedWorker runtime,
and browser-specific host integration.

## Build

The complete build is orchestrated by a single maintained script from the repository
root:

```sh
bash infra/build/build-web-client.sh
```

This produces `apps/web/dist/` with the bundled UI and WASM core.

## Requirements

- **Emscripten** 6.0.11
- **Rust** from the repository's pinned toolchain with `wasm32-unknown-emscripten` target
- **Node** 24.21.0 (pinned in `.node-version`)
- **pnpm** 12.8.1

Build sources include:
- Browser core WASM compiled by `build-browser-core.sh` using Emscripten
- React UI bundled by Vite with two entry points: `index.html` (main) and `shared-worker.ts` (worker)
- Runtime coordination from `@peppy/browser-runtime`

## Deployment

Build artifacts are served from `PEPPY_WEB_CLIENT_DIR`. Community enables
`PEPPY_WEB_CLIENT_ROOT=true` by default, so the same server serves the UI at `/`
and provides `/v1/*` APIs on that origin using `PUBLIC_API_URL` as the canonical
reference.

**Hostname separation (hosted only):** Account and billing remain on a separate origin configured
by `PEPPY_WEB_ASSET_DIR` and `PEPPY_WEB_ORIGIN`. The messaging origin (`PEPPY_WEB_CLIENT_HOST`)
is independently configurable and uses same-origin fetch with HTTPS required.

Community has no hosted account or billing site. Its Compose configuration maps
`WEB_UI_ENABLED` to root mode; set `WEB_UI_ENABLED=false` to disable browser
messaging. `WEB_CLIENT_HOST` is retired: migrate users to `https://PUBLIC_HOST/`
and remove the separate hostname and DNS record. Configurable external account
navigation remains available through `WEB_CLIENT_ACCOUNT_URL`.

## Capabilities and constraints

- **Required** browser capabilities: SharedWorker, WebAssembly, IndexedDB, Web Locks, same-origin HTTPS
- **Memory allocation** starts at 512 MiB for Emscripten; insufficient allocation triggers startup failure
- **IndexedDB quota** holds encrypted checkpoints and notification preferences; browser storage limits apply
- **QR code origin** in pairing protocol is canonical and fetched from `/web/config.json` on the app host
  without credentials; all phone enrollment fields use the canonical API origin, never hostname aliases

Desktop browsers with these capabilities are supported. Constrained or mobile browsers
may display an unsupported message and must gracefully handle missing features.

## Origin and security

The app operates in same-origin context with HTTPS enforcement. Passphrase input bypasses
React state and plaintext logs through imperative browser dialogs. Credential import through
the file picker uses an imperative browser control that reads the selected credentials
and sends them directly to the worker, outside React state. The worker never returns
device tokens or encryption keys in snapshots or renderer RPC results. Rust owns
domain state; the worker persists encrypted checkpoints.

API errors are never rendered as HTML and do not cause fallback to SPA routes. Static
requests using non-GET/HEAD methods receive error responses without index.html fallback.

## Not claimed

This build does not prove production deployment, real-device carrier execution,
native integration, or phone enrollment. Testing requires:
- Real browser verification against the actual generated WASM
- Phone enrollment on the canonical API origin with maintained simulator or real server
- Carrier outcome reconciliation separate from app startup

Deployment, DNS, TLS, reverse-proxy configuration, and release integration are
operator responsibilities. See the [hosting guide](../../infra/compose/README.md).

## Checks

```sh
pnpm --filter @peppy/web typecheck
pnpm --filter @peppy/web test
pnpm --filter @peppy/web build
```

Desktop regression tests and SharedWorker/WASM loader checks are required alongside
unit tests.
