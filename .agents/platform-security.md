# Hosted platform security utilities

The private sibling `peppy-platform` owns hosted identity, tester authorization
and billing policy. Public community hosting does not acquire those authorities.
See its `README.md` for the separately authorized maintenance cutover procedure.

## Encryption and lookup reference

- Private `web/src/security/encryption.ts` exports `createKdf`,
  `derivePurposeKey`, `ciphertextAad`, `encryptText`, `decryptText`, and the
  structural `KdfV1` / `CiphertextV1` types. Canonical wire DTOs originate in
  `crates/hosted-api/src/tester_admin.rs`; `keyId` is camel-case inside crypto
  metadata, while outer request fields are snake_case.
- Version 1 uses PBKDF2-SHA256 at 600000 iterations, a fresh 16-byte salt, then
  purpose-separated HKDF-SHA256 and nonextractable AES-256-GCM keys. Encryption
  generates a fresh 12-byte nonce. Encodings are canonical unpadded base64url;
  encryption key IDs and record UUIDs are lowercase hyphenated UUIDs. Never
  reuse test salts/nonces in application code or normalize/trim the phrase.
- Tester display-email purpose is `peppy.platform.tester-emails.v1`.
  `ciphertextAad(purpose, keyId, recordId)` authenticates namespace, version,
  purpose, encryption key ID and record identity; use literal `registry-check`
  only for the registry check record. Never move ciphertext between entries or
  purpose domains. Validate the check locally before showing decrypted entries.
- Private Rust `security` exports `validate_kdf`, `validate_ciphertext`,
  `ciphertext_aad`, `normalized_email`, `LookupKey`, `TESTER_EMAIL_PURPOSE`,
  and `TESTER_EMAIL_LOOKUP_PURPOSE`. Rust validates envelopes but never derives
  the owner's encryption key or decrypts the list. Consumers also enforce the
  registry/envelope key-ID match and request/entry bounds.
- `LookupKey::from_bytes(key_id, bytes)`, `key_id()`, `token(purpose,
  canonical_value)` and `lookup_check()` implement independent server-only HMAC
  lookup. Normalize with `normalized_email` first: bounded ASCII mailbox,
  lowercase, surrounding ASCII whitespace removed, no alias folding. Lookup
  IDs are opaque nonempty text, at most 128 UTF-8 bytes, not encryption UUIDs.
  The fixed independent lookup secret stays outside the database and source;
  startup checks its persisted ID/check under the registry lock before tokens.

Illustrative browser use inside a private page-session scope (record ID supplied
by the caller; never persist the phrase, key or decrypted text):

```ts
import {
  createKdf, derivePurposeKey, ciphertextAad, encryptText, decryptText,
} from './security/encryption'

const purpose = 'peppy.platform.tester-emails.v1'
const kdf = createKdf()
const key = await derivePurposeKey(phrase, kdf, purpose)
const aad = ciphertextAad(purpose, kdf.keyId, entryId)
const envelope = await encryptText(key, kdf.keyId, email, aad)
const displayEmail = await decryptText(key, envelope, aad)
```

## Lifecycle and authorization decisions

- The owner supplies the actual vault keyphrase locally. No phrase/derived-key
  transport, Web Storage, IndexedDB, URLs, analytics or shared app-state storage.
  Lock on reload, page exit, sign-out, account/session change or permission loss.
  Same-origin JavaScript can access browser secret inputs; this is not the native
  secure-control boundary. Temporary buffer clearing is best effort.
- Database-only disclosure is the primary threat model. Verified and
  admin-submitted addresses may reach the server transiently over TLS for HMAC;
  never persist plaintext addresses or log bodies/emails. Local plaintext after
  unlock is intended; do not show ciphertext as an editable email.
- Administration is pinned to one existing verified account UUID by a privileged
  operator, never an email allowlist or source UUID. Any deletion intent disables
  it permanently for that account. Tester membership controls Stripe mode only;
  membership never grants admin. Nonowners get no admin UI or list requests.
- Initialization explicitly reconciles legacy sandbox UUIDs. Phrase rekey uses
  both phrases and preserves the complete entry set/membership. Lost-phrase
  reset revokes old memberships and requires recent real OAuth authentication.
  Neither changes the lookup secret; recover that original secret if lost.
- Encryption epoch IDs are never reusable. Migration 0020 records observed and
  subsequent IDs in `tester_registry.encryption_key_history` with an append-only
  trigger; keep this history through empty lists, rekey and reset. Backfill cannot
  reconstruct IDs erased before the migration. This is distinct from the fixed
  server lookup-key identity and does not store phrases or encryption keys.
- Removal preserves vault/device/session access under indefinite read-only
  policy and requires explicit fresh live billing; never charge automatically.
  Immutable billing bindings fence all writes/replays. Website billing mutations
  send `X-Peppy-Billing-Binding`; headerless callers conflict after rotation.
  Preserve historical operations and unknown-ID cleanup until authoritative
  provider resolution; stale responses belong only to their original binding.
- Private `billing_requests::{reserve, claim, record_success, record_failure,
  mark_cleanup_resolved}` journals exact dispatch inputs on immutable bindings.
  Hold account→binding locks for short SQL transactions; commit the claim before
  provider IO. Preserve stored body/version/idempotency keys on retry, including
  legacy keys. `FailureClass::Ambiguous` cannot resolve cleanup;
  `ResolutionProof` requires actual authoritative evidence, not HTTP status,
  lease expiry, timeout, an empty listing or UI retirement. Never dispatch a
  `legacy_unknown` tombstone or persist raw provider/client secrets. Record late
  receipts on their original binding and use `reconcile_generation` CAS before
  canonical writes. Migration 0019 and its guards require a stopped-writer
  cutover, never mixed old/new writers; backfill cannot discover pre-upgrade
  provider effects whose entire local transaction rolled back.
- Creation flows persist immutable version1 `creation_intent` inputs before any
  customer-creation dependency (migration 0021). Freeze ordered fields/API/key;
  materialize the exact dispatch body only after the customer receipt is known.
  NULL legacy intent is not permission to reconstruct current inputs. No
  customer placeholders, intent rewrites or journal dispatch-spec rewrites.
- Terminal cleanup requires retirement or explicit deletion intent, an
  account-locked parent gate before reservation/claim, pinned ownership/mode and
  safe owned children. Binding-owned successful comp coupons can be deleted;
  deleting a coupon never removes applied discounts. Known successful customer
  PM-default/portal effects close only after exact owned customer deletion.
  Unknown stages, incomplete legacy pipelines, shared coupons and foreign
  references remain blockers. Resume uses validated durable DELETE evidence,
  not arbitrary 404/tombstones; tester removal preserves vault and history.
  Additive migration 0023 replaces the validated journal path guard, with no
  new runtime grants or public routes.
