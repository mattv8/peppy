# Peppy domain guidance

This reference records non-obvious Peppy domain constraints. Related references:

- [Infrastructure and release boundaries](infra.md): public/private images,
  source pins, Harbor signing, deployment proof, and native build pitfalls.
- [Desktop frontend](frontend-style-guide.md) and
  [native mobile](mobile-style-guide.md): shared design APIs and platform rules.

## Ownership boundaries

- Rust core is the sole owner of local SQLite/SQLCipher and durable
  domain/protocol state. Native hosts own transport, OS scheduling, secure
  storage, cancellation, and carrier effects. Web UI receives sanitized DTOs,
  never passphrases, device credentials, database keys, or encryption keys.
  See [client core](../crates/client-core/src/lib.rs) and
  [desktop sessions](../apps/desktop/src-tauri/src/session.rs).
- Device sync uses a manually shared passphrase. Do not substitute automatic
  key distribution or claim forward secrecy. Carrier SMS/MMS is outside the
  application's encryption boundary.
- Cached purpose keys can restore a previously verified, unlocked epoch but do
  not pin a profile or change the active epoch; they can make an already-active
  epoch usable. Only manual passphrase/header unlock records cache integrity,
  and hosts explicitly cut over to a newer verified epoch after that unlock.
  See [key lifecycle](../crates/client-core/src/lib.rs) and [desktop unlock](../apps/desktop/src-tauri/src/lib.rs).
- A published attachment is a separately supplied plaintext derivative, not a
  publication of the encrypted original. See
  [attachment API](../services/server/src/api/attachments.rs).

## Commands, delivery, and recovery

- Keep local durable receipt, transport acknowledgement, application state,
  carrier submission, and delivery evidence distinct. `SendState` defines
  which states may retry transport or carrier work; `OutcomeUnknown` requires
  reconciliation and must not be automatically resent. See
  [send states](../crates/domain/src/command.rs) and
  [retry policy](../crates/sync-core/src/retry.rs).
- A gateway receives a durable, one-use carrier permit only for a command in
  its received-command ledger. An unfinished attempt reopens as
  `OutcomeUnknown`, never with a second permit. See
  [client-core contract](../crates/client-core/src/lib.rs).
- The receive cursor is the highest contiguous journaled cursor; quarantine
  malformed records rather than wedging the stream. Snapshot import is staged
  and monotonic. Snapshot commands are historical and never authorize carrier
  work. A restore guard persists before import validation, so do not claim a
  rejected restore leaves all state unchanged.
- Server replay is retained transport history. Snapshot ciphertext rows are not
  updated in place; eligible superseded records can be pruned through compaction.
  A snapshot generation fences that changing retained set. Never equate a replay
  cursor with a snapshot generation. See [sync API](../services/server/src/api/sync.rs).

## Protocol contracts

- Rust is the source of shared wire contracts. Generate bindings and schemas
  from it rather than maintaining equivalent hand-written platform models.
  [Envelope generation](../crates/protocol/src/generate.rs) covers envelope
  and pairing schemas, not every REST adapter.
- Preserve distinct identity roles: envelope, command, device, vault, event,
  message, and source sequence are not interchangeable. See
  [domain IDs](../crates/domain/src/ids.rs) and
  [envelope headers](../crates/protocol/src/envelope.rs).
- `Cursor` and `SourceSequence` serialize as canonical unsigned decimal
  strings, not JSON numbers or padded strings. Envelope header/ciphertext
  validation is a contract boundary.

## Hosted enrollment and pairing

- A hosted account session and an enrolled device credential are separate
  authorities. Expiring or signing out of the account session must not erase
  device credentials or stop device-authenticated sync. Native sign-in does not
  create a browser cookie; website billing can require another sign-in to the
  same account. See the native account clients for
  [Android](../apps/android/app/src/main/java/dev/peppy/mobile/HostedAccountClient.kt)
  and [iOS](../apps/ios/PeppyNative/HostedAccountClient.swift).
- Hosted billing uses Stripe through Peppy's website. Subscription lapse means
  indefinite read-only retention, not scheduled account deletion. Read-only
  policy still permits reads, sync, export, revocation, and attachment deletion;
  explicit account deletion owns billing and vault cleanup. See
  `peppy-platform/src/policy.rs` and `src/deletion.rs` in the private repository.
- Provisioning retries reuse the protected checkpoint's operation, vault,
  device, encryption material, and grant. Cancellation or a lost completion
  response can occur after the server commits. Keep the checkpoint through
  session expiry and sign-out; acknowledge it only after importing the matching
  device identity. The account response's `operation_id` does not identify a
  unique pending operation: the hosted query can select among several. A
  differing value alone is not grounds to discard the local checkpoint. See
  [mobile provisioning](../crates/mobile-bindings/src/hosted.rs); the account
  selection query is in `peppy-platform/src/service.rs`.
- Before a checkpoint exists, the chosen passphrase is volatile presentation
  state. Preserve it across password-manager hops and configuration changes,
  with sensitive content concealed. Do not serialize it into Android saved
  state or silently generate a replacement on return. After process death,
  recover prepared material using the user's saved phrase; a checkpoint never
  contains the plaintext passphrase.
- A phone that creates the first vault is an `owner`. Capability registration
  lets it execute work addressed to itself without changing its role to
  `gateway`. Adding execution capability grants no owner authority; server
  filtering must still prevent consuming another gateway's work. See
  [server sync](../services/server/src/api/sync.rs) and
  [desktop gateway selection](../apps/desktop/src-tauri/src/gateways.rs).
- The two QR flows have different directions. A new phone scans an enrolled
  owner's intent QR. In **Add a computer**, the owner phone scans the new
  computer's join-request QR and offers an encrypted intent through the server.
  That offer is not approval: both flows still require the locally computed
  verification code and explicit owner confirmation. Computer claimants use
  role `device`, not `gateway`. See
  [pairing contracts](../crates/protocol/src/pairing.rs) and
  [join cryptography](../crates/hosted-client/src/lib.rs).
- Server-advertised providers and platform configuration both gate native
  sign-in. Mobile Google SDKs use the server's nonce; browser OAuth clients and
  native token audiences stay separate. Configuring an Apple audience alone
  does not make native Apple usable: its authorization-code exchange and
  revocation path must exist before it is advertised. Provider setup belongs
  to the private platform; public self-hosting must remain usable without it.

## Notifications

- While vault keys are locked, notification plaintext is dropped: there is no
  plaintext locked queue. SMS capture has different durable locked handling;
  do not generalize notification behavior to it. See
  [notification capture](../crates/client-core/src/notifications.rs) and
  [client-core contract](../crates/client-core/src/lib.rs).
- Notification identity includes source device, notification key, and lifetime;
  a current OS instance tracks updates/removals. Android guards removal with
  the same key and `postTime` instance, not the key alone.

## Android carrier companion

- Android is a companion to the default messaging app: do not request the
  default-SMS role or `WRITE_SMS`, use hidden APIs, or imply general RCS
  support. Default-app status alone does not grant RCS access. See
  [Android limits](../apps/android/README.md).
- MMS capture reconciles content already downloaded by the default messaging
  app's provider; it does not make Peppy the carrier/default app. Provider
  acquisition is offline reconciliation, separate from gateway upload. See
  [MMS capture](../apps/android/app/src/main/java/dev/peppy/mobile/MmsCaptureWork.kt).
- A SIM route is the exact current default-SMS subscription route, not a
  physical-SIM identity. Commands for an old or missing route remain pending;
  never redirect them to another SIM. See
  [SIM routes](../apps/android/app/src/main/java/dev/peppy/mobile/SimRoutes.kt).

## iOS limits

- This build has no carrier executor or notification listener and reports carrier
  messaging unavailable; it never calls `begin_send_attempt` on iOS. Foreground sync is bounded and
  cancelled on leaving foreground. Contacts may also use explicitly scheduled,
  bounded background passes with expiration cancellation and durable progress;
  do not introduce a long-lived background WebSocket. Background execution is
  discretionary and must not promise immediate remote edits. See
  [iOS behavior](../apps/ios/README.md) and
  [telephony eligibility](../apps/ios/PeppyNative/TelephonyEligibility.swift).
- A host Swift package build is not evidence of an iOS SDK build, carrier
  eligibility, or carrier capability.
- Xcode 27 replaces `$DEVELOPER_DIR/Applications/Simulator.app` with
  `$DEVELOPER_DIR/../Applications/DeviceHub.app`; missing Simulator.app alone
  does not mean Xcode is incomplete. See [iOS helper](../infra/dev/ios.sh).
- CoreSimulator can list an obsolete MobileAsset image as unusable with
  `Cryptex Mount Preferred` while a separate Cryptex image is ready and booted.
  A failed verification of the obsolete UUID does not diagnose the active runtime;
  a displayed 128-byte Cryptex size can describe metadata, not the runtime payload.
- Rust's Intel iOS Simulator target is `x86_64-apple-ios` (`target_env="sim"`);
  only the ARM64 simulator target uses the `-sim` suffix: `aarch64-apple-ios-sim`.

## Mobile gateway parity

- Android and iOS own native enrollment and pairing UI; hosted billing opens
  Peppy's website. Passphrase entry and generation stay native. Follow the
  [mobile style guide](mobile-style-guide.md) for shared generated tokens and copy.
- Rust owns shared mobile domain and policy. Generate bindings, tokens, and copy
  from their shared sources rather than duplicating platform models or catalogs.
- When an affected mobile feature changes, update and test both native hosts and
  shared contracts, stating explicit capability gates for exceptions. Native hosts
  own OS facts and effects; do not make meaningless edits to unrelated surfaces.

## Contacts

- Each phone owns its OS address book. Other vault devices request changes;
  they do not directly replace another phone's book. Source identity checks
  enforce protocol consistency, not cryptographic isolation between passphrase
  holders. The vault's equal-authority trust model still applies.
- Rust owns contact state, scan checkpoints, revisions, source mappings, edit
  ledgers and photo references. Native hosts own provider access, scheduling,
  permission checks and OS effects. Keep raw contact data out of logs.
- Snapshot or historical edit requests never authorize OS writes. A durable
  one-use permit precedes each write; interrupted writes require reconciliation,
  never blind retry. An ambiguous create must not create a duplicate.
- Partial scans, limited contact access, permission revocation and unavailable
  providers never imply contact deletion. Require authoritative complete scope
  and apply the deletion policy before publishing removals.
- Contact photo objects are private encrypted attachments. Retain restore data
  and referenced photos for their promised lifetime; a timer alone does not
  establish that every replay or restore reference has been released.
- Compaction must preserve semantic state under delayed and multiwriter updates.
  A grouping key or newest server cursor is not sufficient deletion authority.
  Changing the retained snapshot set must force an affected import to restart;
  it must not silently merge stale non-owner contact caches into a restored book.

## Desktop native boundary

- A missing protected database key must fail closed when the local database
  already exists; never reset or replace that database. Database keys are bound
  to a vault/device identity and generated only for an absent database. See
  [credential handling](../apps/desktop/src-tauri/src/credentials.rs).
- Desktop secure storage fails closed when unavailable. The bundled macOS
  secure-store record preserves compatible legacy entries and rejects malformed
  records or conflicting database keys rather than replacing them. See
  [secure store](../apps/desktop/src-tauri/src/secure_store.rs).
- Keep passphrases in reviewed native secure controls, outside the webview,
  Tauri IPC, and process argv. The desktop dialog implementation covers macOS,
  Windows, and Linux native paths. See
  [native dialogs](../apps/desktop/src-tauri/src/dialogs.rs).
- Windows uses a nonpersisting native CredUI prompt. Linux tries `zenity`, then
  `kdialog`, through `Command` without a shell; absent helpers fail closed with
  no webview fallback.
- Native floating-head click-through needs real input-region/focus evidence;
  CSS rounding or webview hit testing does not establish it. Preserve a usable
  main-window/composer fallback.

## Workspace and evidence boundaries

- The root Rust workspace excludes `apps/desktop/src-tauri`; treat its build
  and dependency graph as separate. See [root Cargo workspace](../Cargo.toml).
- Contract coverage and integration fixtures are documented in
  [CONTRIBUTING.md](../CONTRIBUTING.md): generated contracts do not cover every
  REST adapter, and database/S3 integration fixtures are fail-fast and distinct
  from persistent development volumes.
- Keep simulator, native-build, real-carrier, store-distributable, and
  production-readiness evidence separate. Android carrier and iOS capability
  claims require the applicable evidence, not a fixture or host build.
