# Peppy

[![CI](https://github.com/mattv8/peppy/actions/workflows/ci.yml/badge.svg?branch=production)](https://github.com/mattv8/peppy/actions/workflows/ci.yml)

Peppy is a self-hosted messaging foundation for developer and operator evaluation. It includes an encrypted-envelope server, encrypted attachment storage, snapshots, explicit public image copies, a simulated gateway, native desktop client, Android SMS companion, and capability-gated Swift client.

## Status

Development remains in progress. The simulator exercises synthetic messages, not a carrier. CI builds development artifacts on pushes to `staging`; a production push rebuilds trusted artifacts only when stable signing is ready. Staging publishes prerelease artifacts automatically, and each green production push with ready signing automatically publishes a stable release. The "Release" workflow remains available from `production` for a selected version bump, explicit version override, or a `dry_run` test. Download prerelease and CI artifacts from [GitHub Releases](https://github.com/mattv8/peppy/releases) and [CI runs](https://github.com/mattv8/peppy/actions/workflows/ci.yml). All artifacts provide build evidence only:

- Android development artifacts contain only an automatically debug-signed APK;
  they are not release artifacts.
- macOS bundles and DMGs, Windows installers, and Linux packages are development outputs. macOS bundles are development bundles, not Developer ID signed or notarized; macOS may apply ad-hoc linker signing without a TeamIdentifier or sealed resources.
- The iOS simulator app and unsigned device archive do not establish signing, store, or device eligibility. The archive is not an installable IPA.

Official desktop update releases are separate protected artifacts. Stable releases
run only for `production` dispatches or green `production` pushes; staging updates run only from pushes to
`staging`. They use protected signing environments, version-bound updater
signatures, signed checksums, and an explicit per-platform signing policy.
Development artifacts and direct artifact dispatches are never update feeds and
must not be treated as signed installers. Provisioning, key rotation, rescue
bootstrap, Linux package version mapping, and repository-key trust requirements
are documented in [`infra/release/README-updates.md`](infra/release/README-updates.md).

The desktop provides unread/tray updates, native notification banners and an in-app feed for mirrored Android notifications. Android emulators can receive synthetic SMS. iOS has no carrier executor or third-party notification listener in this build. Native banner delivery depends on OS permission, system notification settings and platform installation requirements; browser previews cannot verify it.

## Breaking rename boundary

Peppy is a clean-install boundary with no compatibility or migration path for previous identities, encrypted data, or backups. Existing databases fail the altered migration checksum checks. App IDs install separately, and the Peppy Compose project creates separate volumes. Production requires an explicit `PEPPY_ENV=production`; an old environment variable is ignored and the default remains development. The production image is `peppy-server`.

## Components

- **Server:** device authentication, ordered encrypted envelopes, encrypted attachments, snapshots, and public image-copy endpoints.
- **Browser client:** the shared messaging UI with Rust/WASM, encrypted browser storage, and a single SharedWorker session across tabs. The server image bundles its assets and Community serves it at the public root by default. See the [browser build and hosting guide](apps/web/README.md).
- **Gateway simulator:** synthetic SMS and carrier-effect exercise for local development.
- **Native clients:** Tauri desktop, Android SMS companion with opt-in experimental MMS, and capability-gated Swift client.

Read the [Android](apps/android/README.md), [iOS](apps/ios/README.md), [mobile bindings](crates/mobile-bindings/README.md), and [desktop](apps/desktop/README.md) guides for component limits and native details.

## Contacts

Android and iOS can publish their address books to the encrypted vault. Enable
contact sync on each phone; each phone owns its book and applies edit requests
from other devices. The desktop Contacts view browses and edits those books.
New-contact destination and Auto / Confirm / Off edit policy are configured on
the owning phone. Large deletions require approval there.

Contacts include structured names, nickname, labeled phones/emails,
organization/title, postal addresses, birthday, Android notes and normalized
avatars. Photos are private encrypted 256×256 JPEG attachments, at most 64 KiB.
Deleted contacts retain restore data for 90 days; restoring requests a new OS
contact on the owning phone. Shared numbers remain distinct contacts, and
ambiguous name matches fall back to the address.

Contact writes require a request from the phone's active encryption epoch.
Rotating keys rejects older requests that have not received a write permit;
already-issued uncertain attempts remain reconcilable and are never reissued.

Phone permissions, locked keys and OS scheduling affect freshness. Android uses
bounded WorkManager passes; iOS also uses discretionary background refresh and
processing. Neither promises immediate remote edits. See the platform guides for
field restrictions and verification limits.

## Notification mirroring

Upgrade all participating clients before enabling mirroring on Android. Older builds quarantine unfamiliar notification records and do not retry them automatically after upgrade.

Mirroring requires Android notification access and an enabled mirroring switch. Apps are allowed by default; per-phone app filters can be changed on the companion or desktop. Peppy's own notifications, the default SMS app's duplicate notifications, group summaries, ongoing/progress notifications and empty notifications are excluded. Locked sync does not collect a plaintext notification backlog.

The desktop's Notifications view provides the feed, app mute controls and dismissal. Phone dismissals propagate through sync. Desktop dismissal requests reach the phone at its next sync; Android background scheduling can delay this by 15 minutes or longer. This is not an immediate remote-control channel. Group summaries may remain on the phone after their children are dismissed.

The feed shows up to 1,000 active notifications, newest first; bulk dismissal handles up to 100 per action. The Android companion's **Refresh app list** reloads observed apps and synchronized filter choices.

Desktop Settings controls message banners, mirrored-notification banners, and full or hidden banner previews. These preferences are local to each desktop. Use the in-app feed for navigation and dismissal; native banner activation and notification-center interactions vary by OS.

### Storage and trust boundaries

Notification titles, text and app metadata travel inside encrypted envelopes. The server's replay log defaults to 30 days. Compatible clients can explicitly supersede notification state for compaction after it leaves replay retention; dismissal does not immediately erase previously synced ciphertext. Muting cannot recall an upload already in flight. Filters changed remotely take effect on the phone after it syncs.

Notification state shares the existing 100,000-record snapshot import limit. Coalescing and compaction reduce superseded state, but legacy records and immutable message history still consume storage and bootstrap capacity. Compaction does not remove this vault-wide limit.

Every passphrase holder retains the same authority within the vault, including the ability to request a phone notification's dismissal. Notification mirroring does not change carrier SMS/MMS encryption or add forward secrecy.

### Compaction and recovery

The server can compact a vault only after every non-revoked device declares
support for generation-fenced snapshots. Upgrade participating clients together;
an older device blocks compaction. Declaring support is a build-level promise:
downgrading a declared client can make that client unable to restore a compacted
snapshot.

Contact producers require a compatible server and the active epoch's compaction
key. An upgraded device with an older cached-key format must unlock once with
the shared passphrase. Existing message decryption can still work while contact
sync waits for that unlock. Local history is indexed in bounded passes before
contact production resumes. Missing historical epoch keys also pause contact
production: unlocking the current epoch cannot recover lost older keys. Keep
historical key material needed to read retained history. A device that has not declared snapshot support
pauses vault-wide cleanup even when the other devices can sync contacts.

Compaction follows explicit references to superseded records, rather than treating
the last upload as the latest state. A change to the retained snapshot set advances
its generation, causing an affected import to restart. Carrier commands are not
compaction targets. Unmarked legacy records are retained unless an eligible
replacement explicitly supersedes them.

Compaction exposes opaque grouping keys, supersession relationships and deletion
timing to the server. Reference-tracked private attachments additionally expose
their association with opaque envelope identities so the server can check whether
reclamation is safe. These metadata do not contain contact fields, photo plaintext
or encryption keys. Public image copies remain separate plaintext objects with
their own lifecycle.

Device bearer credentials authorize storage writes and retention operations.
Encryption protects contents; it does not protect ciphertext availability from
a compromised authorized credential. Revocation and operator backups remain
separate controls. Superseded ciphertext is not removed immediately, and retained
duplicate-detection identities continue to consume server storage after compaction.

## Security boundaries

Each vault has a generated shared secret. Clients derive vault keys locally. A new phone can receive keys through an explicitly approved, encrypted QR pairing exchange or enter the shared secret manually. The server API never receives the shared secret or plaintext transferred keys. The protocol does not claim forward secrecy. Public verification material permits offline guessing, so retain the generated random multiword secret.

Every passphrase holder has the same cryptographic authority. Credential revocation does not remove access from a passphrase holder. Rotate the passphrase, activate the new epoch on each participating client, and keep historical epoch material needed to read history. A gateway blocks commands from a retired epoch. The composed protocol has not received an external security audit.

SMS and MMS move in plaintext outside Peppy's encryption boundary. A gateway records carrier attempts durably and does not resend an attempt with an unknown carrier outcome. Snapshot history cannot execute carrier work. Copying a client database back behind the application is not detectable. Public attachment copies are separately supplied PNG, JPEG, or WebP plaintext derivatives. Their share tokens and expiry/revocation controls do not encrypt them; never upload private originals as public copies.

### Attachment storage encryption

Private attachment ciphertext and explicitly published plaintext image derivatives are encrypted at rest. Peppy requests automatic, backend-managed S3 SSE-S3 encryption; operators do not create, enter, or configure a separate Peppy storage-encryption key. SeaweedFS Compose deployments generate a hidden machine key once in the `seaweed-filer-data` volume and retain filer metadata and volume data in `seaweed-volume-data`. S3 volume chunks store SSE ciphertext, and `-filer.encryptVolumeData` protects filer-originated writes. Do not enable `-s3.encryptVolumeData` with SSE-S3 on the pinned SeaweedFS image: that interaction corrupts reads. Back up and restore both volumes together. A container or host administrator can read the key from the process environment or persisted filer state, and filer backups contain it. This protects stored data from disk or object access without that state; it does not protect against compromise of the complete host or filer state.

Published derivatives remain plaintext when delivered through their existing `public/` prefix and retain their expiry and revocation behavior. This does not make those derivatives end-to-end encrypted and does not add forward secrecy.

SSE response metadata and ciphertext framing are backend and format assertions, not independent proof that stored bytes are encrypted. Peppy fails startup when a custom S3 backend does not confirm the required SSE-S3 behavior; its initial multichunk storage probe is bounded at 360 seconds, while recurring readiness uses the configured shorter timeout. Hosted-provider support and its image-pin rollout are unverified in this repository and require follow-up in the private deployment repository.

## Local server and TLS experiments

### First-run setup and device connections

Open the server's HTTPS address in the web app, or connect a desktop or phone to
that address. The setup screen identifies whether the unclaimed community server
is **open** or **token-protected**. On an open server, the first device that
completes setup becomes the administrator and creates the first vault. On a
token-protected server, that device must also enter the operator setup code.
Opening a page or fetching discovery data does not claim the server.

Set `PEPPY_SETUP_TOKEN` to a nonempty operator code of at least 32 UTF-8 bytes
to require that code for the first claim. Configure the same token on every
replica. Leave it absent or empty for open first claim; the server does not
generate, log, or provide a fallback token. An existing unclaimed deployment
upgraded without `PEPPY_SETUP_TOKEN` becomes open. Set the token before upgrading
when token-protected first claim is required.

Use a current client for open first claim. A current client treats an older
generated-code server that does not advertise this policy conservatively and
requests a setup code. A legacy client may unnecessarily show or require a
setup-code field for an open server; that prompt does not mean the server is
token-protected.

The setup code authorizes only the initial claim. The owner username and password
authorize management of that owner's vault; the administrator owner can also
manage the server. The generated vault shared secret protects vault data and
admission to that vault. Keep the username/password and the vault secret
separate, and save the generated secret when prompted. None of these values can
be recovered by looking in server logs.

#### Add family vaults

After the first claim, the administrator uses **Manage server** to create a
one-time invite link. Send that link to the intended requester. The requester
opens the link, provides a display name, and waits for the administrator to
approve or deny the request in the live **Manage server** view. After approval,
the requester chooses a distinct owner username and password, creates a new vault
shared secret, and finishes setup as that vault's owner. The owner can then
manage that vault's devices and storage; they do not gain administrator access or
access to other vaults.

Do not publish invite links. An invite token authorizes one vault request, not
access to an existing vault or its shared secret. Vaults are not listed on public
setup screens.

Existing installations that migrate to this model name their existing
administrator owner `admin`. Sign-in accepts an empty username for that
administrator, but entering `admin` explicitly is equivalent. Each additional
owner signs in with their own username and password.

On the browser, a pending invite request exists only in the current page's
memory. Reloading that page loses the request checkpoint while leaving the invite
consumed. The administrator must deny the orphaned request or let it expire, then
issue a new invite.

Live administrator updates use server-sent events (SSE). The server permits at
most four SSE streams for an administrator session and 32 streams in one server
process. These limits are per process, so running replicas weakens the effective
deployment-wide limit.

### Recover from an unwanted first claim

If a stranger completes the first claim, there is no password-reset or automatic
reclaim path. For an affected self-hosted deployment, stop it and remove only its
Peppy database and attachment state, then start it again and complete setup
privately. This permanently destroys the server's vault data and attachment
ciphertext, along with all other state in those volumes; it is not general
password recovery.

The root Compose file defines the three state volumes as `postgres-data`,
`seaweed-volume-data`, and `seaweed-filer-data`. Docker prefixes those names with
the Compose project name. Before removal, inspect the exact project-prefixed
names and Compose labels. Set the project name and every `--env-file` and `-f`
option to the actual affected installation, including its community-image and TLS
overlays where used. Do not use `docker compose down -v`, and never delete volumes
from another project or shared storage.

```sh
# Set these to the affected deployment only. Do not use the persistent dev stack.
: "${PROJECT:?Set PROJECT to the affected Compose project after inspection}"
COMPOSE_ARGS=(
  --project-name "$PROJECT"
  --env-file /absolute/path/to/the/affected/.env
  -f /absolute/path/to/docker-compose.yml
  # Add every overlay used by this deployment, for example compose.community.yml
  # and compose.caddy.yml. Do not omit its image or TLS overlay.
)
compose() { docker compose "${COMPOSE_ARGS[@]}" "$@"; }

# Inspect the affected project's labeled volumes, then verify these three names.
docker volume ls --filter "label=com.docker.compose.project=$PROJECT"
VOLUMES=(
  "${PROJECT}_postgres-data"
  "${PROJECT}_seaweed-volume-data"
  "${PROJECT}_seaweed-filer-data"
)
docker volume inspect "${VOLUMES[@]}"

# Stop and remove only this deployment's service containers; this does not remove volumes.
compose stop api migrate postgres seaweedfs
compose rm -sf api migrate postgres seaweedfs

# Destructive: removes only the inspected vault-data and attachment-ciphertext volumes.
docker volume rm "${VOLUMES[@]}"

# Restart with the same Compose files and environment.
compose up -d
```

For an external PostgreSQL database or object store, clear only the dedicated
Peppy database and dedicated Peppy attachment state for the affected deployment.
Do not delete a shared database service, bucket, or object store wholesale.

#### Downgrade warning

The multi-vault database down migration is lossy for secondary web logins. It
preserves each secondary vault and its data, but removes the secondary owner's
server login. Downgrade only with a recovery plan for the affected owners; it is
not a way to remove their vault data.

The web app's **Manage server** page lists devices, gateway status and storage
usage, and supports device revocation. Quota and retention values are read-only.
It is not a database backup/restore interface.

Use **Add phone** in the web or desktop workbench. There is one QR format:
**Connect your phone**. Compare the verification codes and approve the connection.
Choose encrypted key transfer or manual shared-secret entry. Alternatively,
enter the server address and shared secret on the new device without using a QR.
Hosted enrollment additionally requires the account's available Apple/Google
sign-in and entitlement checks. No QR appears in Settings, and there is no
credential-file onboarding or reversed **Add a computer** flow.

After enrollment, grant the supported mirroring permissions on the phone.
Connecting a device cannot grant OS permissions or enable unsupported RCS access.
Desktop application locking is optional and disabled by default; its local
password is separate from the server password and vault secret.

This enrollment redesign is a breaking foundation change, not an upgrade path
for older pairing deployments. The application does not erase existing databases
or client profiles automatically.

The local API publishes on `127.0.0.1:7000`; PostgreSQL and SeaweedFS have no host ports. `/healthz` reports whether the process can serve requests. `/readyz` also checks PostgreSQL, migrations, and, when configured, a bounded attachment-store probe. `just smoke-infra` starts the stack, checks both routes, and runs the storage contract check.

To use another local API port, keep all three override values aligned:

If your `.env` predates the 7000 default, update `PUBLIC_API_URL` and `PUBLIC_ATTACHMENT_URL` (and `API_HOST_PORT`, if set) to the same port, or set `API_HOST_PORT=8080` to keep the old port.

```sh
export API_HOST_PORT=18080
export PUBLIC_API_URL=http://127.0.0.1:18080
export PUBLIC_ATTACHMENT_URL=http://127.0.0.1:18080
docker compose --env-file .env -f docker-compose.yml up
```

For a public TLS experiment, set `PUBLIC_HOST` and add `-f infra/compose/compose.caddy.yml` to the Compose command. Community serves the browser UI and API together at `https://PUBLIC_HOST/`; set `WEB_UI_ENABLED=false` to opt out of the UI. Keep `PUBLIC_API_URL` and `PUBLIC_ATTACHMENT_URL` as external HTTPS origins, and keep `S3_INTERNAL_ENDPOINT` internal. Set `TRUSTED_PROXY_CIDRS` to only the direct reverse proxy's IP addresses or narrow CIDRs and keep it aligned with the proxy topology. If it is empty, forwarded addresses are ignored and clients behind one proxy share that proxy's rate-limit bucket. The Caddy overlay is the only supplied configuration that publishes ports 80 and 443. It is not production-readiness evidence.

## Develop and contribute

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for container, Android, and native desktop workflows.

Operators must read [backup and restore guidance](infra/compose/README.md) before recovery. A restore rewinds revocations and cursors and requires reconciliation and client resynchronization before service access resumes.

## Release boundary

Releases are git tags; checked-in manifests keep development placeholder versions and CI stamps the computed version into each build.

- **Staging prereleases:** when CI passes for a push to `staging`, it publishes the GitHub prerelease `vX.Y.Z-staging.N`, where `X.Y.Z` is the next version predicted from Conventional Commits and `N` counts commits since the last stable tag. The Android and WiX version codes retain the monotonic prerelease sequence. The newest 10 prereleases are kept. The server and relay images are pushed only after the same SHA's `gate` job succeeds, as `hub.docker.visnovsky.us/library/peppy-server:staging` and `hub.docker.visnovsky.us/library/peppy-push-relay:staging`, plus immutable `sha-<commit>` tags.
- **Production edge and stable:** after CI's gate and both production `:edge` image publishers succeed for a `production` push, the same SHA automatically creates the next stable release and publishes both Harbor images as `X.Y.Z`, `X.Y`, `latest`, and `X` from `1.0.0`; the immutable SHA tags are promoted rather than rebuilt. Conventional Commits choose the automatic major/minor/patch bump; pushes with no releasable commits use a patch fallback. A stale or duplicate automatic release no-ops. When all stable signing inputs are unconfigured, an automatic release visibly skips with a warning until they are provisioned; partial configuration fails, and manual releases remain fail-closed. CI and stable-release concurrency each retain only one pending run, so a burst can cancel an intermediate whole CI run before it gets a gate or edge image; a pending manual or automatic stable release can likewise replace the other and is shown as cancelled. The **Release** workflow remains available from `production` with inputs for `bump` (`auto`, `patch`, `minor`, `major`), an optional explicit `version` (`X.Y.Z`, for example `1.0.0`), and `dry_run` (build without publishing); these manual controls preserve their existing behavior. Every stable release requires a successful CI push run for its exact production commit.
- **Assets:** certificate-verified Android `peppy-<version>-android.apk` and
  `.aab`; Linux AppImage, deb, and rpm (stable only); macOS DMG and app archive;
  Windows MSI and NSIS installer; `SHA256SUMS`; generated release notes. iOS
  artifacts stay in CI runs and are not published. The Android APK is suitable
  for direct sideloading; the AAB is store tooling input and is not installable.
- On Windows, uninstall a prerelease MSI before installing the stable MSI of the same `X.Y.Z`.

Trusted desktop releases always include Tauri updater signatures, GPG-signed
checksums, and GPG-signed RPM packages. Their protected environments select
macOS `developer-id` (the strict default) or explicit free `adhoc`, and Windows
`authenticode` (the strict default) or explicit free `none`; strict modes fail
closed rather than silently downgrading. Ad-hoc macOS builds are not notarized
or Gatekeeper-approved, and Windows `none` installers have no Authenticode
publisher signature. Android assets use a persistent protected-environment
release certificate and fail closed without its keystore, access inputs, and
pinned SHA-256 certificate fingerprint; the iOS archive remains unsigned.
These are GitHub Release artifacts, not store submissions. Keep signing
credentials only in protected CI environment secrets; never generate or commit
them in this repository. See
[`infra/release/README-updates.md`](infra/release/README-updates.md) for Android
keystore backup, key-continuity, and OAuth certificate-registration requirements.

Preview the next version with `just version --channel prerelease`. Run `just release-test` after changing `infra/release/` or the release workflows; it needs git-cliff 2.14.2 on `PATH`, which `infra/release/install-git-cliff.sh <dir>` installs; it also needs `JAVA_HOME` set to a JDK 17 and `ANDROID_HOME` or `ANDROID_SDK_ROOT` pointing to an Android SDK with build-tools `35.0.0` and `platforms/android-35` installed. Harbor publication requires protected `staging` and `production` environments with `HARBOR_USERNAME`, `HARBOR_PASSWORD`, `COSIGN_PRIVATE_KEY`, and `COSIGN_PASSWORD`; no billing capability is included in these public images.
