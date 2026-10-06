# Infrastructure and release boundaries

This is an agent reference for Peppy-specific deployment and artifact constraints.
Paths prefixed with `peppy-platform/` refer to the separate private repository,
not a directory in this checkout. Live addresses, inventory, credentials, and
operator procedures belong in that repository's `docs/operator-runbook.md`
and its external inventory/env files.

## Public and hosted lanes

- `mattv8/peppy` owns shared server/contracts and native clients. Its Harbor
  images live under `hub.docker.visnovsky.us/library/`. Private identity,
  billing, policy, and hosted web/deployment code belong to
  `mattv8/peppy-platform`; the hosted image uses `private-library/peppy-hosted`.
  Public images remain independently self-hostable without hosted credentials.
- Public `production` pushes advance `:edge` and immutable `sha-<commit>` tags;
  a stable version requires the separate Release workflow. A production branch
  push alone is not a stable release. See [release channels](../README.md) and
  [public image publication](../infra/release/publish-image.sh).
- Hosted publication binds **two** commits: the private release SHA and the
  exact public revision in `peppy-platform/docker/source-pins.json`. A matching
  branch name or locally edited sibling checkout is insufficient. The workflow
  checks publication/ancestry, uses the pinned public tree as a named build
  context, and checks both OCI revision labels when reusing a hosted digest.
- The private pipeline consumes the signed public relay at its pinned public
  revision; it does not rebuild or sign the relay with the hosted key.
  Public signing uses `COSIGN_PRIVATE_KEY`; the hosted publisher uses
  `COSIGN_KEY`. Public-repository `COSIGN_PUBLIC_KEY` verifies public images;
  private-repository `COSIGN_PUBLIC_KEY` verifies hosted images. The private
  pipeline verifies the public relay with `PUBLIC_COSIGN_KEY`, and Community
  staging uses `vars.COMMUNITY_COSIGN_PUBLIC_KEY`. Same-named secrets across
  these repositories do not imply the same signing authority.

Source: `peppy-platform/.github/workflows/build-publish.yml`,
`docker/scripts/source_compatibility.py`, and `docs/operator-runbook.md`.

## Harbor's unsigned-image read trap

- Harbor's Cosign-required project policy can reject **manifest reads** with
  HTTP 412 after an image push has already succeeded. That failure does not
  establish that the tag is absent or that Docker failed to build it.
- The hosted publisher uses Buildx `--metadata-file` and reads the flat JSON
  key `containerimage.digest`, not `containerimage.config.digest` or a nested
  object. It signs that build-produced digest, verifies it with the expected
  public key, then pulls and checks labels. Reading the unsigned manifest to
  discover the digest before signing recreates the failure.
- Existing unsigned or uncertain tags fail closed. Rerunning the same source
  pair is not an automatic repair path. Signature policy must not be disabled
  to turn a failed inspection into permission to overwrite an immutable tag.
- For an inspected reference, the private publisher treats only the entire
  `<requested-reference>: not found` response (optionally prefixed `ERROR: `)
  as permission to build a missing tag. Other inspection errors, including
  generic `manifest unknown`, fail closed. A generic `not found` can describe
  a missing tool or resolver rather than a missing image.

Source: `peppy-platform/docker/scripts/publish_hosted_image.py` and its tests.
The public publisher is a separate implementation; do not assume it has the
private metadata/classification behavior. A successful live sign/verify cycle
is still required to establish compatibility with the configured registry.

## Deployment authority and proof

- Build and rehearsal jobs use the private `buildserver` runners. Production
  deployment uses `[self-hosted, peppy-deploy, peppy1]`. Peppy1 is the sole
  controller because its release journal and `flock` are host-local.
  Peppy2 is a deployment target whose runner
  is standby-only; two controllers would not share that lock or journal.
- A hosted production deployment requires the **same build run's** manifest
  and `full-web-sandbox` proof. The proof binds the manifest hash, image digest,
  private/public revisions, mode, and run ID. Mock/preflight results and a
  successful dirty local build are not substitute release proofs. The automatic
  deploy job also rejects a workflow SHA that is no longer the production head.
  Manual `production-deploy.yml` instead uses the named `build_run_id`'s
  manifest and proof; it does not impose that head check.
- The sandbox driver's seeded browser sessions do not prove Google or Apple
  login. Stripe test-object billing, Stripe-originated webhook delivery, Test
  Clock renewal/lapse, and real-device enrollment are separate evidence.
- `push_relay_enabled=false` is a supported release state. The controller skips
  relay startup/migration/readiness, sets the API relay URL empty, and serves an
  empty-body 503 for the relay hostname. Do not infer relay availability from
  the API's health or remove this per-release state during reconciliation.

Source: `peppy-platform/docker/scripts/{release_controller,hosted_rehearsal,rehearsal_proof}.py`
and the private operator runbook. Community staging is a separate public-image
path; it does not consume hosted env files or run private migrations.

## Database, storage, and billing boundaries

- The hosted `migrate` command runs the public core `MIGRATOR` before private
  migrations. In particular, core `0013_pairing_join_requests.sql` must exist
  before private `0008_pairing_join_requests_rls.sql`. Advance the public source
  pin before releasing private code that depends on that table.
- API runtime, migration, and maintenance connections have distinct privileges;
  relay connections are separate again. Runtime uses RLS, while maintenance
  has deliberately bounded bypass access. A successful migration-role query
  does not establish that a runtime-role request is authorized. See
  `peppy-platform/src/{main,rls}.rs` and private migration tests.
- Hosted production uses B2; disposable rehearsal storage uses SeaweedFS.
  Credentials and endpoint/DSN checks apply to the **resolved Compose config**,
  including env files. A reachable S3 endpoint alone does not prove that the
  runtime can perform the required signed bucket/object operations.
- Stripe's API pin is `STRIPE_API_VERSION` in private `src/billing.rs`; its
  `dahlia_contract_tests` document the expected shapes. Invoice subscription
  binding uses `parent.subscription_details.subscription`, and payment handling
  uses invoice payment/confirmation-secret shapes. Old top-level invoice
  examples are not compatible fixtures.

## Native artifact traps

- Regenerating UniFFI bindings does not update packaged JNI/static libraries.
  Missing new Rust symbols at iOS link time can mean stale libraries rather
  than an invalid Swift API. Rebuild the matching target libraries with the
  maintained [Android](../infra/compose/verify-android-native.sh) and
  [iOS](../infra/build/build-ios-artifacts.sh) scripts.
- Android's script defaults to `PEPPY_ANDROID_NATIVE_PROFILE=debug`; the
  [artifact workflow](../.github/workflows/build-artifacts.yml) explicitly uses
  `release`. The host library used for bindgen remains debug in either case.
  Gradle `assembleRelease` alone does not optimize prebuilt JNI libraries.
- The iOS `.xcarchive` is intentionally unsigned, not an installable IPA.
  Host Swift tests, simulator builds, and signed physical-device installation
  establish different things; see [iOS guidance](AGENTS.md#ios-limits).
- Google client IDs/callback schemes are public build configuration, not OAuth
  client secrets. Changing them requires rebuilding the mobile app; changing
  server audience allowlists does not rewrite its bundled configuration. See
  [Android setup](../apps/android/README.md) and [iOS setup](../apps/ios/README.md).
