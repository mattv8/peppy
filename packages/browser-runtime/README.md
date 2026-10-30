# Browser runtime primitives

Worker-side adapters for the Rust core, bundled with SharedWorker coordination, port/session routing, enrollment orchestration, and sync scheduling.

## Worker ownership

`acquireWorkerOwner()` holds an exclusive Web Lock for the runtime's lifetime.
It fails immediately if another worker holds that lock; it never steals from
an older build or suspended worker. Startup cancellation does not publish a
late-starting owner. Release completes only after teardown and lock release.

If teardown fails, the lease rejects release but retains the lock. Terminate
that worker before attempting to create another sender. The SharedWorker host
connects this lease to its ports and session lifecycle.

## Core

`BrowserCore` loads one Emscripten module and owns request/response buffer
cleanup for the fixed Rust ABI. `CoreRejectedError` means Rust returned a
handled rejection with a well-formed error code. Malformed replies, traps, and
`core-poisoned` mean the module cannot safely continue. The renderer boundary
maps unknown valid rejection codes to a generic error and preserves canonical
revision metadata.

## Checkpoints

`IndexedDbCheckpointStore` stores one generation containing:

- `client.db`, produced by SQLCipher;
- `client.db.media/cipher/<uuid>.ppss` ciphertext files;
- opaque, authenticated wrapped credentials produced by Rust.

It rejects plaintext SQLite headers and non-allowlisted paths. These checks do
not authenticate ciphertext; Rust must verify the identity and media when
opening them.

`commit()` uses a strict-durability IndexedDB transaction with a generation
compare-and-swap. It resolves on transaction completion. Browser durability
and origin-storage retention still depend on the browser platform.

Provide `retainedCipherPaths` for committed media that has not been loaded into
the worker. Such paths must already belong to the previous generation; the
store retains their bytes without loading them. Omitted old paths are deleted
in the same transaction. The database is copied in full for each checkpoint;
batch domain operations before committing large imports.

`load()` returns the encrypted database and a generation-bound lazy media
reader. After a successful new commit, use `readCipher(path, generation)` with
the newly committed generation. Missing or malformed data in an existing
checkpoint is an error, not permission to reset the vault.

## Mutation fence

`SerializedRuntime.mutate()` serializes the core operation, file capture, and
checkpoint commit. Its result contains the committed generation. Capture must
run while core filesystem mutation is quiescent and must exclude journals,
plaintext, staging inputs, and temporary exports.

A handled core rejection is returned only after checkpointing its state.
Traps and unexpected failures terminate the owner without a checkpoint.
Checkpoint failures also terminate the owner and prevent queued operations
from running. The caller must construct a new worker from the last committed
generation; no automatic mutation retry occurs.

## Transport

`OriginTransport` is worker-only, origin-bound HTTP with header authentication,
redirect refusal, bounded response streams, and cancellation. It does not own
device-token persistence or decide which messages may be retried. Do not expose
it or its token-provider closure to renderer ports.

## SharedWorker host and RPC

`BrowserWorkerHost` coordinates port attachment, session lifecycle, and RPC dispatch
in the SharedWorker. It enforces port message allowlists and validates command origins,
rejecting raw core operations like unlock/open when invoked through renderer ports.

`BrowserSession` manages the core instance, checkpoint lifecycle, and state
serialization. Mutations serialize core operations, file capture, and checkpoint
commits atomically; checkpoint failures terminate the session without automatic retry.

## Remembered browser unlock

After a manual phrase unlock, the worker can remember an encrypted recovery record
for the current checkpoint and a valid hosted app session. Its session check uses
the actual worker/app origin (where hosted cookies belong), while the wrapped
identity retains the canonical API context. The record is bound to the origin,
account, and wrapped identity. It stores no plaintext passphrase, root
or database key, and no OAuth token; OAuth does not cryptographically encrypt the
vault. Recovery is best effort, so unavailable or evicted browser storage falls
back to the ordinary locked state.

This is a browser-profile trust choice, not cryptographic enforcement of app
session policy. Anyone who can run same-origin code in, or access, that browser
profile can bypass the app-session gate and decrypt the remembered record. Explicit
Lock attempts to forget recovery before stopping the worker; if storage cleanup
fails, the worker still locks and reports that forgetting was not durable. Sign-out
still reaches the server when cleanup fails, then reports that cleanup failure.
Worker-side validation runs nonblocking after enrollment status, coalesced and no
more than once per minute; a rejected record clears only remembered recovery. These
actions do not erase the independent checkpoint or device token, and active device
sync continues. Computer handoff has no phrase to cache, so a later manual unlock
is required once before that browser can use remembered recovery.

`BrowserScheduler` coordinates network activity, enrollment, and polling with
backoff. Device tokens and network state remain worker-private.

Phone-approval status is locally validated by Rust as an owner view. The worker
pins the first displayed claim digest for an invitation and requires that same
digest again before approval. A changed claim returns the existing restartable
invitation state; stopping a worker aborts local requests and does not cancel
an invitation remotely.

Nonsecret notification display preferences use a separate `IndexedDbNotificationSettings`
record and are persisted to encrypted IndexedDB before success is reported; encrypted
message/domain state remains Rust-owned.

## Failure modes and recovery

Worker poisoning occurs on WASM traps, malformed responses, or `core-poisoned`
status. These failures terminate the owner without a checkpoint and require
restoring from the last committed generation and creating a new worker.

Handled core rejections checkpoint state before returning rejection; integrators
must commit those changes before acknowledging mutations.

Worker terminal protocol broadcasts `{event:'stopped',reason:<code>}` before closing
ports. All tabs clear plaintext presentation and reject pending requests. Drain
checkpoint work before releasing ownership.

## Checks

```sh
pnpm --filter @peppy/browser-runtime test
pnpm --filter @peppy/browser-runtime typecheck
```

Tests use `fake-indexeddb` as a development dependency. Real browser checks are
also required for worker loading, IndexedDB durability behavior, and reloads.
