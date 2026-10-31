# Vault-bound tester cryptography

This crate supplies a small worker-local cryptographic boundary for hosted tester
administration. It does not open a local database, enroll a device, grant
administrator authority, or retrieve account credentials. The hosted service must
authorize every request and bind the supplied public vault metadata to the
authenticated administrator's existing vault.

## Unlock and envelopes

`unlock` derives the vault root using the existing `peppy-crypto` Argon2id and
normalization rules, verifies its `VaultCheckHeader`, and derives purpose ID 6,
`TesterRegistry`. The root is not retained as session state. A successful unlock
returns an opaque worker-local handle; there is no key-export command.

`encrypt` and `decrypt` use XChaCha20-Poly1305 with purpose-separated shared crypto
envelopes. Their authenticated context is the byte sequence:

```text
"peppy-tester-vault-v2\0"
account UUID (16 bytes)
registry UUID (16 bytes)
entry UUID (16 bytes)
encryption epoch keyId UUID (16 bytes)
public profile fingerprint (32 bytes)
vault key epoch (big-endian u32)
```

The encryption epoch key ID is independent of the registry UUID. Generate a fresh
one for initialization, migration, or re-encryption. The server preserves key-ID
history and rejects reuse. Canonical wire types and route names live in
`peppy-hosted-api::tester_admin_v2`, including the retained vault header used for
explicit old-profile recovery after rotation.

`close` invalidates a handle and clears its stored purpose key before removal.
Request secret fields and FFI request/response allocations are cleared on their
documented lifecycle. These measures are best effort: compiler temporaries and
JavaScript/browser copies can remain. Terminate the worker on lock, page exit,
account/session change, or permission loss; do not promise complete memory
erasure. Never persist phrases, handles, keys, or decrypted tester lists.

The worker bounds request size to 64 KiB, plaintext size to 4 KiB, and concurrent
sessions to eight. It returns structured failures for malformed requests, failed
unlock, missing handles, and envelope authentication failures.

## Build and checks

Use the repository's pinned Rust and Emscripten toolchains. After activating
Emscripten 6.0.11, the maintained build helper emits `peppy-tester-crypto.js` and
`peppy_tester_crypto.wasm` into the specified output directory:

```sh
cargo test --locked -p peppy-tester-crypto
bash infra/build/build-tester-crypto.sh <output-directory>
```

`cargo run --locked -p peppy-tester-crypto --example smoke_fixture` prints a
synthetic vault fixture for compiled-module smoke tests. Its phrase and UUIDs are
test data, not production credentials. The private account-site build consumes
the exact public source pin and tests this fixture against the compiled module.
Browser verification must additionally exercise worker asset routing, MIME types,
and the worker entry response's CSP; a Node-hosted WASM test cannot prove those.
