# Google Play billing protocol

This reference defines the public hosted API contract for Google Play billing.
The private platform implements purchase verification, account binding, token
ownership, Real-time developer notifications (RTDN), reconciliation, and
acknowledgement. Those implementation details are not part of this repository.

## Transport and authentication

Both routes use the signed-in hosted account's bearer authentication. They do
not use an enrollment-session or device credential. `POST` requests use
`Content-Type: application/json`.

| Method | Route | Success response |
| --- | --- | --- |
| `GET` | `/hosted/v1/play/billing/context` | HTTP 200 JSON context |
| `POST` | `/hosted/v1/play/billing/link` | HTTP 200 JSON link outcome |

HTTP 401 means that the hosted account session is missing or expired. The host
must require sign-in rather than silently retrying with a different account.
HTTP 403 maps to `HostedFailure.Kind.ENTITLEMENT`. Transport failures and HTTP
5xx responses are retryable, but never grant access. In particular, `pending`
is an HTTP 200 JSON outcome, not an empty HTTP 202 or HTTP 204 response.

The normative server success status is HTTP 200 for both routes. The Android
host currently accepts HTTP 200 or 201 before parsing a response, but servers
must return HTTP 200 for this protocol.

The Rust shared client rejects an empty response or one larger than 8 KiB of
JSON text. Before parsing, the Android host rejects an HTTP response body
larger than 64 KiB.

## Context route

`GET /hosted/v1/play/billing/context` returns the billing context bound to the
authenticated account:

```json
{
  "account_id": "00000000-0000-4000-8000-000000000001",
  "available": true,
  "product_id": "peppy_hosted",
  "base_plan_ids": ["monthly", "annual"],
  "account_binding": "example_opaque_binding",
  "current_provider": "none"
}
```

`account_id` identifies the authenticated hosted account. Clients reject a
response whose `account_id` does not equal the account for the current session.
Extra response fields may be ignored. Future `current_provider` values fail
closed as `unknown`.

`available` is only a new-purchase permission. A client may begin a new Play
purchase only when all of the following are true:

- `available` is `true`.
- `current_provider` is `none`.
- `product_id` is present and valid.
- `base_plan_ids` is nonempty and valid.
- `account_binding` is present and valid.

The shared client validator accepts product and base-plan IDs that are 1–128
bytes of ASCII letters, digits, `.`, `_`, or `-`; it accepts at most 16 base
plans. It accepts an account binding that is 1–64 bytes of ASCII letters,
digits, `_`, or `-`. Invalid metadata rejects the context rather than making it
purchasable.

`current_provider` describes current billing ownership; it does not prove that
access is active. `stripe`, `play`, and `unknown` make the context unavailable
for a new Play purchase. A context can remain unavailable while retaining valid
Play metadata for restore or management. Account and access APIs, not this
route or a purchase token, decide entitlement and access.

`product_id` and `account_binding` can be `null` for unsupported accounts.
They must remain valid when supplied. Product names and base-plan IDs in this
document are examples; the response must use the configured Play Console
products.

## Link route

`POST /hosted/v1/play/billing/link` associates a server-verified Play purchase
with the authenticated hosted account. Its request body is:

```json
{
  "operation_id": "00000000-0000-4000-8000-000000000002",
  "product_id": "peppy_hosted",
  "purchase_token": "example-provider-token"
}
```

`operation_id` is a UUID used only to deduplicate a request attempt. It is not
the idempotency identity. `product_id` follows the context product-ID rules.
`purchase_token` is credential material and must not be logged; the shared
client accepts a nonempty visible-ASCII token (bytes `0x21`–`0x7E`, with spaces
excluded) up to 4096 bytes.

The HTTP 200 response is always account-bound:

```json
{
  "account_id": "00000000-0000-4000-8000-000000000001",
  "outcome": "linked"
}
```

Known `outcome` values are `linked`, `pending`, `bound_to_other_account`, and
`not_entitling`. Unknown future values fail closed as `unknown`. `pending`,
`bound_to_other_account`, `not_entitling`, and `unknown` do not grant an
entitlement. Clients reject a response whose `account_id` does not match the
current session account.

## Private-platform invariants

The private platform is authoritative for access. It must verify Play state
before granting or changing entitlement; a callback, client request, or
purchase token alone never grants access.

- Persist a lifetime-stable opaque account binding. It must not be an account
  UUID or a value derived from account key material.
- On a new, unbound purchase-token assignment, require a nonempty Play
  `obfuscatedExternalAccountId` and verify that it equals the persisted opaque
  binding before assigning the token.
- Bind idempotency and ownership to the purchase token. A same-token relink for
  the same account after reinstall succeeds with a fresh `operation_id`; a
  recorded token cannot move to another account.
- Persist and reconcile replacement tokens safely. When a replacement succeeds,
  supersede its `linkedPurchaseToken` predecessor without allowing either token
  to transfer account ownership.
- Verify the Play entitlement and external account binding before granting
  access. Reconcile RTDN using authoritative Play API lookup, not notification
  contents alone.
- Handle a pending payment that completes while the app is closed. RTDN and
  authoritative lookup must resolve the persisted binding, reconcile a
  legitimate entitlement, and acknowledge within Play's required deadline
  without requiring the user to reopen the app.
- Acknowledge only server-verified purchases. Do not acknowledge pending or
  unverified purchases. On missing, malformed, mismatched, unknown, or
  unverifiable data, fail closed and grant no access.

License-tester coverage must exercise a pending payment that completes with the
app closed, including authoritative reconciliation and acknowledgement timing.
