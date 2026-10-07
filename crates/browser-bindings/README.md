# Browser core bindings

This crate adapts `peppy-client-core` to a browser worker. Rust retains ownership
of SQLCipher, encrypted envelopes, drafts, contact edits, and replay state.
The shared `peppy-desktop-api` crate supplies display projections.

This is a runtime component, not a complete browser application. Enrollment,
the worker's message-port permissions, UI integration, and server packaging
must be supplied by the browser host.

## Build

Activate Emscripten **6.0.11**, install the `wasm32-unknown-emscripten` target for
the repository's pinned Rust toolchain, then run from the repository root:

```sh
bash infra/build/build-browser-core.sh target/browser-assets
```

Keep the generated `peppy-browser-core.js` and `peppy_browser_core.wasm` together.
The ES module exports an asynchronous factory intended for a worker, not the
renderer. The build retains the existing 256 MiB Argon2id policy and starts
with 512 MiB of WASM memory. Browser memory locking is unavailable.

## ABI

| Export | Ownership |
|---|---|
| `peppy_browser_alloc(length)` | Allocates a request buffer, at most 8 MiB |
| `peppy_browser_dispatch(pointer, length)` | Returns an owned NUL-terminated UTF-8 response |
| `peppy_browser_free_request(pointer, length)` | Clears and frees the request allocation |
| `peppy_browser_free_response(pointer)` | Frees the returned response |

Requests have shape `{ "command": "…", "args": { … } }`. Responses are
`{ "ok": true, "value": … }` or `{ "ok": false, "error": { "code": "…", "message": "…" } }`.
Draft conflicts can also carry `currentRevision` as a decimal string.

## Host dispatcher boundary

The dispatcher is **worker-internal**, not a UI permission boundary. Raw open,
unlock, transport, and file-transfer operations are test-only and refused in
production when invoked through renderer ports. They are not forwarding candidates.

The production host (see `packages/browser-runtime/src/host.ts`) enforces an explicit
RPC allowlist and routes only safe commands through renderer ports. Sensitive commands
like unlock receive passphrase through imperative browser APIs outside React state,
and authentication relies on authenticated local-identity opening bound to the worker's
origin context.

See `apps/web/src/browser-bridge.ts` for the actual host integration point.

## Persistence and failure ordering

A completed Rust call proves a local core transaction, not durable browser
storage. Before acknowledging a mutation or uploading a newly sealed outbox
record, the host must commit the encrypted database and related encrypted
files through its checkpoint store.

A handled rejection may still change core state: restore-import guards are
one example. Checkpoint those changes before returning the rejection. A WASM
trap, malformed ABI response, or `core-poisoned` response instead requires
terminating the worker and restoring the last committed checkpoint. Never
replace database files underneath a live SQLCipher connection.

Persist only the database, verified ciphertext media, and the authenticated
local-identity envelope. Exclude media plaintext, staging inputs, temporary
exports, passphrases, and native key-cache exports. File promotion inside
Emscripten is exclusive copy; it relies on the single worker and checkpoint
transaction for browser-level publication.

`local_identity` seals the database key and device token using the existing
`LocalWrap` crypto purpose. It does not write storage, activate epochs, or
authorize UI access. Integrators must follow its documented rewrap ordering
and compare the envelope's epoch with the stored core epoch.

## Checks

```sh
cargo test -p peppy-browser-bindings --lib -- --test-threads=2
cargo clippy -p peppy-browser-bindings --all-targets -- -D warnings
```

Keep native-core tests, real browser storage checks, phone delivery evidence,
and production deployment verification separate.
