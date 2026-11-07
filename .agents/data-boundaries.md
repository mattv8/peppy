# Data, trust, and recovery boundaries

Agent reference for security-sensitive behavior. Preserve these boundaries when changing protocol, storage, client, or gateway code.

## Vault and enrollment trust

- Each vault has a generated shared secret. Clients derive vault keys locally; the server never receives the shared secret or plaintext transferred keys. Enrollment uses an approved encrypted QR handoff or manual secret proof. Do not claim forward secrecy.
- Public verification material permits offline guessing. Retain the generated random multiword secret.
- Every passphrase holder has equal cryptographic authority. Revoking a device credential does not revoke the shared secret. Rotation requires activating a new epoch on participating clients and retaining history keys. A gateway rejects commands from a retired epoch.
- Setup code authorizes first claim, administrator password authorizes management, and the vault secret authorizes vault data and admission. Keep those authorities distinct. Gateways record carrier attempts durably and never resend an attempt with an unknown carrier outcome.
- Copying a client database back behind the application is not detectable. Device bearer credentials authorize storage writes and retention operations. Encryption does not protect ciphertext availability from a compromised authorized credential; revocation and operator backups remain separate controls.

## Attachments and public copies

- Private attachment ciphertext and explicitly published PNG, JPEG, or WebP derivatives use backend-managed S3 SSE-S3 encryption. Operators do not configure a separate Peppy storage-encryption key.
- SeaweedFS Compose deployments persist filer key material and metadata in `seaweed-filer-data`, and volume data in `seaweed-volume-data`. Back up and restore both volumes together. A host or container administrator can access that key material, so disk or object protection does not protect a compromised host or filer state.
- Keep `-filer.encryptVolumeData`. Do not add `-s3.encryptVolumeData` with SSE-S3 on the pinned SeaweedFS image: it corrupts reads.
- A published derivative is client-supplied plaintext, not server-decrypted or re-encoded private media. It has a separate expiry and revocation lifecycle and can outlive private-object deletion. Its token and expiry controls do not encrypt it.
- Startup rejects a custom S3 backend that does not confirm SSE-S3. The first multichunk probe is bounded to 360 seconds; recurring readiness uses the configured shorter timeout. Versioned backends delete every object version and require ListObjectVersions and DeleteObjectVersion permissions. Backblaze B2 needs `listFiles` and `deleteFiles`; changing versioning requires restart. Lifecycle cleanup for crash-orphaned probes must scope to `peppy-storage-probe-` and `peppy-readiness-check-`.
- SSE response metadata and ciphertext framing are backend-format assertions, not independent proof of encrypted stored bytes. Hosted-provider support and image-pin rollout remain unverified here.

## Drafts, quotas, and MMS staging

- A new draft branch and each server-accepted authored edit expire 30 days after acceptance. Accepted non-authored updates keep the branch's current expiry. A draft can have at most three live branches; the server returns `draft_branch_limit` beyond that.
- Expiry closes the branch with reason `expired` and clears the sealed record bytes of its revisions. It deletes only private draft-only attachments. Attachments still referenced by a message record, held by sent-message history or another unreleased use, or backing a ready public copy stay in place. Public copies have their own lifecycle.
- The vault attachment quota is `VAULT_ATTACHMENT_QUOTA_BYTES`. Compose defaults it to `536870912` bytes. Quota use counts finalized attachments, unexpired upload reservations, unpurged removals, and unpurged public copies.
- Migration 0026 adds five draft tables: `draft_incarnations`, `draft_branches`, `draft_revisions`, `draft_terminals`, and `draft_attachment_holds`. Each has row-level security scoped to the vault. The server registers all five in the runtime-role grant and RLS validation lists in `services/server/src/rls.rs`.
- Migration 0026 downgrade refuses while any draft rows, draft-linked attachments or reservations, or pending `draft_expired` storage deletions exist. It leaves the database unchanged in that case.
- Android MMS with gateway content version 4 can resize images and split a message into ordered groups. Carrier delivery order is best effort; the planner does not promise that split parts arrive in order. Content version 3 and older get no split plan; an oversize message is refused (`TooLarge`).
- Android writes fitted MMS images as plaintext under `noBackupFilesDir/mms-fit`. The app disables Android backup (`allowBackup="false"`, `fullBackupContent="false"`). Fitted output is removed when its command settles, when the fit ends impossible or transient, and when stale entries are pruned.
- Attempted draft uploads: core introduced `draft_push_attempts` in schema 48 for a pending revision through `mark_draft_push_attempt`, which returns false for a stale revision. Hosts must call it before each draft PUT; until every host does, this evidence is not complete. A never-attempted draft remains sendable offline. A draft whose attempted PUT has unknown acceptance is blocked from local sending until the client reconnects and reconciles with the server.
- Client schema: the local SQLite schema is version 49 in this checkout (`SCHEMA_VERSION` in `crates/client-core/src/lib.rs`). Later checkouts may bump it. A binary that reads a database with a newer schema returns `UnsupportedSchema` and does not open it. No downgrade path exists.
- These statements describe repository behavior and tests. They do not establish production role permissions, hosted deployment state, or carrier delivery behavior. Verify those separately.

## Notification storage and authority

- Notification title, text, and app metadata travel in encrypted envelopes. Replay defaults to 30 days. Compatible clients can supersede state after replay retention; dismissal does not erase previously synced ciphertext immediately.
- Notification imports share the 100,000-record snapshot limit. Coalescing and compaction reduce superseded state but do not remove legacy history or vault-wide bootstrap limits.
- While keys are locked, notification plaintext is dropped; there is no plaintext backlog. Remote app filters apply after the phone syncs, and muting cannot recall an upload in flight.
- Every vault-secret holder can request a phone notification dismissal. Mirroring does not encrypt carrier SMS/MMS or add forward secrecy. Push wakes carry opaque, content-free hints and are neither content nor delivery evidence.

## Contacts, epochs, and OS writes

- Each phone owns its OS address book. Other devices request edits; they do not replace another phone's book. Source checks preserve protocol consistency, not isolation between secret holders.
- Contact capture needs unlocked active-epoch keys and has no plaintext locked backlog. Native hosts own provider access, permission checks, scheduling, and OS effects; Rust owns durable contact state and ledgers.
- Contact writes require a request from the phone's active epoch. Rotation rejects older requests lacking a write permit. A durable one-use permit precedes each OS write; ambiguous or interrupted writes require reconciliation and never blind retry.
- Partial scans, permission loss, limited access, or unavailable providers never authorize deletion. Deletion needs authoritative complete scope at the current generation and the configured policy.
- Retain deleted-contact restore data and referenced photos for their promised lifetime. A timer alone does not prove replay and restore references have released them.

## Compaction and recovery

- Compact only after every non-revoked device declares generation-fenced snapshot support. A downgrade after declaring support can make a device unable to restore. Devices need the compatible server and active epoch compaction key; older cached key formats require one unlock with the shared passphrase. The client indexes local history in bounded passes before contact production resumes.
- Missing historical epoch keys pause contact production. Current-key unlock does not recover lost historical keys. A device that has not declared support pauses vault-wide cleanup.
- Compaction follows explicit supersession references, not latest upload or newest cursor. Changing retained snapshots advances their generation and forces affected imports to restart. Carrier commands are not compaction targets; unmarked legacy records remain.
- The server sees opaque grouping keys, supersession relationships, deletion timing, and attachment-to-envelope association needed for safe reclamation. It does not receive contact fields, photo plaintext, or encryption keys.
- Attachment release requires the current nonzero generation, a cutoff no later than the replay floor, and proof that every reference compacted. Private-object deletion, elapsed time, or empty references do not authorize release.
- Superseded ciphertext is not removed immediately. Retained duplicate-detection identities continue to consume server storage after compaction.
- Snapshot imports are staged and monotonic. Historical snapshot commands never execute carrier work. Restore guards persist before import validation, so a rejected restore is not a guarantee of unchanged local state.

## Clean-install and rename boundaries

- Peppy is a clean-install boundary without a migration path for previous identities, encrypted data, or backups. Altered migration checksums fail existing databases. App IDs and Compose volumes are separate.
- Production requires `PEPPY_ENV=production`; the default remains development and the production image is `peppy-server`.
- The multi-vault down migration keeps secondary vault data but removes secondary owner server logins. It is lossy for those logins and cannot remove vault data.
- Enrollment redesign does not erase existing databases or client profiles. It has no compatibility path for older pairing deployments.
