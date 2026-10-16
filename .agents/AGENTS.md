# Peppy domain guidance

This reference records non-obvious Peppy domain constraints. Related references:

- [Infrastructure and release boundaries](infra.md): public/private images,
  source pins, Harbor signing, and native/browser build pitfalls.
- [Desktop frontend](frontend-style-guide.md) and
  [native mobile](mobile-style-guide.md): shared design APIs and platform rules.

## Ownership boundaries

- Rust core is the sole owner of local SQLite/SQLCipher and durable
  domain/protocol state. Native hosts own transport, OS scheduling, secure
  storage, cancellation, and carrier effects. Shared React views receive
  sanitized DTOs, never passphrases, device credentials, database keys, or
  encryption keys. Browser secret forms and private worker RPC are separate
  from those DTOs, but remain accessible to same-origin JavaScript.
  See [client core](../crates/client-core/src/lib.rs) and
  [desktop sessions](../apps/desktop/src-tauri/src/session.rs).
- Enrollment can transfer a root only through a context-authenticated
  `VerifiedHandoff`, or admit a device that proves the existing vault shared
  secret. It does not make a QR code, server-held invitation value, or account
  session into a vault secret. Carrier SMS/MMS remains outside the application's
  encryption boundary. Do not claim forward secrecy. See
  [enrollment client](../crates/hosted-client/src/newenrollment.rs)
  and [handoff import](../crates/client-core/src/lib.rs).
- A protected native key cache may restore an already verified, unlocked epoch,
  but cannot pin a profile, change the active epoch, or establish enrollment
  bindings. Manual passphrase/header verification and `VerifiedHandoff` import
  are explicit verified key-installation paths; a handoff also binds the
  enrollment context and recipient identity before pinning its profile and
  epoch. Import initializes an absent active epoch; it is not an implicit
  cutover from an already-active epoch. See [key lifecycle](../crates/client-core/src/lib.rs).
- Phone transfer approval on desktop and browser uses the active Rust client's
  verified root handoff capability, without collecting an approval passphrase.
  Passphrase/header unlock and owner-role `VerifiedHandoff` import establish it;
  native purpose-key caches cannot recreate it. Desktop may recover an unavailable
  capability using only the matching protected binding secret; missing or
  unreadable stored secrets fail closed, without a prompt. Profile or
  cryptographic failures do not authorize that fallback. Seal only locally
  authenticated enrollment contexts, not raw renderer or server input. Mobile
  approval still uses the stored secret or a verified native fallback prompt.
  See [shared approval](../crates/hosted-client/src/newenrollment.rs),
  [desktop approval](../apps/desktop/src-tauri/src/enrollment.rs), and
  [browser approval](../crates/browser-bindings/src/newenrollment.rs).
- A published attachment is a separately supplied plaintext derivative, not a
  publication of the encrypted original. The client supplies the image bytes;
  the server does not decrypt or re-encode the original. Public copies have
  their own expiry and revocation lifecycle and can outlive private-object
  deletion. See
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
- Epoch activation is an explicit cutover, not a side effect of unlocking newer
  keys. Old epochs become decrypt-only; never-attempted older-epoch commands
  retire as `FailedBeforeSubmission`, while attempted commands keep their
  recorded outcomes. See [cutover tests](../crates/client-core/tests/c3_additions.rs).
- Core cannot detect a database rollback. If restore recovers an outbox envelope
  ID with conflicting bytes, quarantine the conflict rather than resealing or
  uploading it; preserve the local capture. See
  [restore conflict tests](../crates/client-core/tests/c5_snapshot.rs).
- The receive cursor is the highest contiguous journaled cursor; quarantine
  malformed records rather than wedging the stream. Snapshot import is staged
  and monotonic. Snapshot commands are historical and never authorize carrier
  work. A restore guard persists before import validation, so do not claim a
  rejected restore leaves all state unchanged.
- Server replay is retained transport history. Snapshot ciphertext rows are not
  updated in place; eligible superseded records can be pruned through compaction.
  A snapshot generation fences that changing retained set. Never equate a replay
  cursor with a snapshot generation. See [sync API](../services/server/src/api/sync.rs).
- Attachment release requires the current nonzero compaction generation, a
  cutoff no later than the replay floor, and proof that every registered
  reference was compacted. Empty reference sets, elapsed time, or private-object
  deletion alone do not establish release authority. See
  [release proof tests](../services/server/tests/attachments.rs).

## Protocol contracts

- Rust is the source of shared wire contracts. Generate bindings and schemas
  from it rather than maintaining equivalent hand-written platform models.
  [Protocol generation](../crates/protocol/src/generate.rs) covers envelope
  contracts; enrollment REST DTOs are canonical in
  [hosted-api enrollment](../crates/hosted-api/src/enrollment.rs), not an old
  pairing schema or platform copy.
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
- Generated vault shared secrets require either an explicit save
  acknowledgement (browser) or protected native secure storage bound to the
  vault and device with a native-only reveal path (desktop, Android, iOS).
  Copying a secret never counts as acknowledgement. Native hosts write the
  pending secret to secure storage before the owner or vault-request
  checkpoint; a failed pending write aborts creation, and an existing non-empty
  pending secret is reused rather than replaced. Desktop and Android resume an
  interrupted owner claim or vault creation from their protected checkpoint,
  read the pending secret first, and prompt only when it is missing. iOS resumes
  vault creation the same way but has no owner-claim checkpoint: an interrupted
  iOS claim can only be retried through shared-secret admission, which uses the
  stored pending secret for that origin when none is entered. After verified
  activation hosts store the secret actually used under the device binding,
  including a user-entered secret from shared-secret admission or a
  mobile phone-approval fallback prompt that passed the vault-header check. A
  failed write there does not undo enrollment, and a non-empty stored value is never
  overwritten. After a successful binding write, remove the matching pending
  secret so a later vault on the same origin cannot reuse it. A storage read
  error fails closed rather than prompting. Mobile reveal is gated by device authentication
  in the UI; the stored item is not bound to user authentication because
  approval reads it silently. iOS uses the shared Keychain store
  (`AfterFirstUnlockThisDeviceOnly`, not synchronized); Android uses the
  Keystore-wrapped store excluded from backup and device transfer.
  Stored binding secrets are not yet removed on revocation or vault teardown.
  Preserve an in-progress secret in transient private state across same-page
  retries; do not put it in logs, process arguments, Tauri IPC, Android saved
  state, or browser local/session storage. Recover protected prepared material
  after a restart using the saved secret, not a silently regenerated phrase.
- Enrollment has one `connect_phone` QR: version, purpose, canonical origin,
  invitation secret, and transfer public key. The invitation secret stays on
  the two clients: the claimant supplies an HMAC-bound claim, the owner checks
  it locally, both compare the derived SAS, and the owner explicitly approves.
  The approved result is a `VerifiedHandoff` for transfer or a local shared-
  secret proof for manual admission. See [QR DTO](../crates/hosted-api/src/enrollment.rs),
  [claim authenticator](../crates/crypto/src/enrollment.rs), and
  [enrollment state machine](../crates/hosted-client/src/newenrollment.rs).
- The recipient must pin the owner's transfer public key to the scanned QR
  before accepting any server claim or transcript. Matching a server-supplied
  key to a server-supplied transcript is not authentication. A short SAS does
  not replace either endpoint's out-of-band key/claim verification.
- Owner-instance claim retries and phone-invitation claim retries differ. An
  owner claim can replay the same protected operation and request after password
  verification; repeating a phone invitation claim cannot recover a lost claim
  secret (`claim_secret_not_replayable`). Consumed phone confirmation can replay,
  but still checks the original proofs/transcript, current vault epoch and
  fingerprint, and non-revoked owner. See
  [instance claims](../services/server/src/api.rs) and
  [phone invitations](../services/server/src/enrollment/invitations.rs).
- On an unclaimed community instance, the setup code authorizes claiming; the
  admin password authorizes server management; and the generated or existing
  vault shared secret authorizes vault admission. Keep all three distinct.
  Claim retry is protected and idempotent. Browser, desktop, or phone can be the
  first owner. A first phone is an `owner`; a later
  phone normally has `gateway` role, while execution capability remains a
  separate registration. Execution capability grants no owner authority and
  must not permit consuming another gateway's work. See
  [claim API](../crates/hosted-api/src/enrollment.rs),
  [instance routes](../services/server/src/api.rs), and
  [gateway work filtering](../services/server/src/api/sync.rs).
- Community setup policy is server-advertised: absent or empty
  `PEPPY_SETUP_TOKEN` permits open first claim, while a nonempty token requires
  at least 32 UTF-8 bytes and protects first claim. UI must visibly distinguish
  these modes and request a code only when the server requires one. No token is
  generated or logged as a fallback. Configure the same token on every replica.
  Existing unclaimed installations upgraded without the setting become open;
  operators who require protection must configure it before upgrade.
- A stranger's completed first claim has no automatic reclaim or password
  recovery path. Recovery requires stopping the affected deployment and removing
  only its dedicated PostgreSQL and attachment state. For root Compose, inspect
  the project-prefixed `postgres-data`, `seaweed-volume-data`, and
  `seaweed-filer-data` volumes before destructive removal. Never blanket-delete
  another project, shared database, bucket, or object store. See the root
  [recovery procedure](../README.md#recover-from-an-unwanted-first-claim).
- Hosted browser authorization uses a bounded PKCE code handoff with separate
  account and app-session audiences. Initialize enrollment identity with the
  canonical API origin, but fetch through the same-app-origin proxy; browser
  origin is transport authorization, not enrollment context. See
  [browser sign-in client](../apps/web/src/newhosted-app-auth.ts) and
  [app-host routing](../services/server/src/web_client.rs). Private
  `peppy-platform/src/web_auth.rs` and `src/enrollment_policy.rs` own session
  audiences and hosted account admission. Instance management is available
  only for self-hosted community servers, not the hosted service.
- Account authentication and owner-device authentication can both be required
  for hosted enrollment. Native `Peppy-Account-Session` is not the device's
  Authorization bearer. Browser app cookies do not authorize account billing.
  Authenticate the account and bind its vault before scoped database reads;
  a valid device bearer alone must not bypass hosted enrollment policy.
- First setup uses `/v1/instance/*`; device admission uses `/v1/enrollment/*`.
  The old `/v1/pairing` routes, reversed computer-join protocol, and
  `create-owner` CLI are removed. Keep the single QR in onboarding or the main
  Add phone flow, never Settings; do not add a Save QR action. Owner phones
  (desktop, Android, iOS) share the Rust owner view in
  [hosted-client enrollment](../crates/hosted-client/src/newenrollment.rs)
  (`owner_phone_view`): it validates status against the active invitation,
  derives the SAS locally, and returns a transcript-pinned claim digest. Hosts
  compare invitation id and that digest before and after any awaited secret
  prompt and require the owner's explicit codes-match confirmation. Rotate a
  waiting invitation only near expiry or when the requested method changes;
  **Enter shared secret on phone** switches a waiting invitation to the
  shared-secret method before revealing the secret. Once the invitation is
  approved, stop treating it as rotatable: the server can report an approved
  invitation as expired before the phone confirms, and that must not offer
  **Start again** or create a replacement.
- Device credentials may be serialized only for protected native secure storage.
  Do not add credential-file onboarding or export UI.
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
- Push relay wakes carry content-free opaque hints. Jobs coalesce and delivery
  is best-effort; a wake is neither message content nor delivery evidence. See
  [relay](../services/push-relay/src/lib.rs).

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
  permission checks and OS effects. Provider IDs and provenance stay local and
  do not enter shared contact projections. Capture requires unlocked active-epoch
  keys; contacts have no plaintext locked backlog, so providers retry after unlock.
  See [contact core](../crates/client-core/src/contacts.rs).
- Snapshot or historical edit requests never authorize OS writes. A durable
  one-use permit precedes each write; interrupted writes require reconciliation,
  never blind retry. An ambiguous create must not create a duplicate.
- Partial scans, limited contact access, permission revocation and unavailable
  providers never imply contact deletion. Require authoritative complete scope
  at the current book generation, fence revisions changed since scan start,
  and apply the deletion policy before publishing removals. Expired or cancelled
  background scans preserve progress without authorizing deletion.
- Contact photo objects are private encrypted attachments. Retain restore data
  and referenced photos for their promised lifetime; a timer alone does not
  establish that every replay or restore reference has been released.
- Compaction must preserve semantic state under delayed and multiwriter updates.
  A grouping key or newest server cursor is not sufficient deletion authority.
  Changing the retained snapshot set must force an affected import to restart;
  it must not silently merge stale non-owner contact caches into a restored book.
- Authoritative compaction imports retain the visible view until authenticated
  promotion. Partial import, reopen, or authentication failure must not clear it;
  legacy snapshots without a compaction generation merge rather than remove
  contacts. See [projection tests](../crates/client-core/tests/snapshot_projection.rs).

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

## Browser runtime boundary

- The browser host requires SharedWorker, WebAssembly, IndexedDB, Web Locks,
  and same-origin HTTPS. It is not a native capability fallback. See
  [web host](../apps/web/README.md).
- Worker ownership uses an exclusive, non-stealable Web Lock. An older or
  suspended worker retains ownership; uncertain teardown keeps the lock until
  worker termination. Do not start a competing sender to recover. See
  [ownership](../packages/browser-runtime/src/owner.ts).
- Mutation acknowledgement requires an encrypted IndexedDB checkpoint commit,
  including handled Rust rejections that changed state. Checkpoint/core failures
  poison the session and block queued work; recover a fresh worker from the last
  committed generation without automatically retrying the mutation. Missing or
  malformed data in an existing checkpoint does not authorize resetting the
  vault. Browser storage retention remains platform-dependent. See
  [checkpoint contract](../packages/browser-runtime/README.md#checkpoints).
- Checkpoints contain SQLCipher state, referenced attachment ciphertext and
  opaque wrapped identity, not plaintext SQLite or live journals. Rust
  authenticates restored data; capture checks are not authentication. Lazy
  attachment hydration must use the matching committed generation. See
  [filesystem adapter](../packages/browser-runtime/src/filesystem.ts).
- Renderer RPC exposes an explicit DTO command allowlist, not generic Rust
  dispatch or worker transport credentials. Approved secret inputs can cross
  private worker RPC; same-origin JavaScript can access browser secret forms.
  Do not describe this as the native secure-control boundary. See
  [worker host](../packages/browser-runtime/src/host.ts).

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
