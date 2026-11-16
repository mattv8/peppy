# Google Play distribution

Operational reference for the Android Play build (`dev.peppy.mobile`, `-PpeppyDistribution=play`). It covers the policy basis for the permissions and the Google Messages link, the declaration demo, the Play Console setup, the platform configuration, and the Play coverage limits. The sideload APK and its release process are in [release updates](release-updates.md) and [Android app guidance](../apps/android.md).

Items marked `[verify]` are not confirmed against current Google policy or Play Console screens. Check them before submission.

## Listing and policy basis

### SMS and MMS permissions

The Play build declares `READ_SMS`, `RECEIVE_SMS`, `RECEIVE_MMS`, and `SEND_SMS` (`apps/android/app/src/main/AndroidManifest.xml`). Peppy is not the default SMS app and does not request the default-SMS role.

- The Permissions Declaration exception used is "Cross-device synchronization or transfer of SMS or calls": [Google Play policy, SMS and call log permissions](https://support.google.com/googleplay/android-developer/answer/10208820). Checked 2026-11-16.
- Precedent: Pushbullet (`com.pushbullet.android`) is listed on Play for cross-device SMS sync and is not the default SMS app. Checked 2026-11-16.
- Do not declare `WRITE_SMS` in the manifest. The sideload-only AppOp path in [Android app guidance](../apps/android.md) is not used by the Play build.

### Google Messages link

The link runs libgm (`apps/android/googlemessages`) in the native host on the phone. The Play AAB includes it (`-PpeppyGoogleMessagesLink=true`, verified by `verifyReleaseGoogleMessagesLink`).

- Precedent: Beeper is listed on Play and connects Google Messages through libgm. Beeper runs libgm in its cloud bridge. Peppy runs libgm on the user's phone. This difference must be stated in the declaration and in reviewer notes.
- Neither precedent guarantees approval. Google can reject either the SMS or the libgm use on its own review.

### Foreground services and listeners

- `RemoteMessagingService` runs as a `remoteMessaging` foreground service. The link service (`src/gmlink`) also declares `foregroundServiceType="remoteMessaging"`. Justify both as keeping the user's own messages syncing while the app is in use or the link is active.
- The notification listener (`NotificationListenerService`) mirrors notifications the user has allowed. Justify it with the user's explicit grant in system settings and the in-app disclosure.

### Contacts

`READ_CONTACTS` and `WRITE_CONTACTS` sync the phone's address book with the user's vault. Contact writes need a durable one-use permit; see the Contacts section of [domain guidance](../AGENTS.md).

### Restricted settings

The Play build never instructs the user to run adb and shows no on-phone Advanced features setup. For `READ_RESTRICTED_MESSAGES`, the Play build shows **Link Google Messages** instead of the copied adb command. Sideload and debug builds keep the copied command.

### Other declared permissions

These are in the merged Play manifest and each needs a justification before submission:

- `READ_PHONE_NUMBERS`: reads only this SIM's own number, for group MMS self-identification. It is requested after an in-app disclosure in the own-number settings. It is never used for route identity (`telephony/SimRoutes.kt`).
- `CAMERA`: scans the enrollment QR code. Optional; requested only when the user starts scanning.
- `POST_NOTIFICATIONS` (Android 13 and later): shows the Google Messages link service's status notifications.
- Also declared, not yet justified here `[verify]`: `USE_BIOMETRIC`, `RECEIVE_BOOT_COMPLETED`.

## Declaration demo video

Record one take per shot, with the on-device disclosure visible. Keep each shot short.

1. Install Peppy from the Play Store on an Android phone.
2. Open Peppy and sign in to the hosted account.
3. Show the SMS permission request preceded by the in-app prominent disclosure. Grant `READ_SMS`, `RECEIVE_SMS`, `RECEIVE_MMS`, and `SEND_SMS` as the flow requests them.
4. Send a text to the phone from another number. Show it arriving on the phone.
5. Show the same message in the Peppy web or desktop client.
6. Reply from the desktop client. Show the reply sent from the phone's SMS app.
7. Open **Link Google Messages** and link the phone. Show an RCS conversation from the Google Messages link.

Do not show real third-party contacts or numbers. Blur or use test numbers.

## Play Console checklist

1. Developer account: complete identity verification. For a new personal account, Google requires 12 testers opted in for 14 continuous days in a closed test before production access.
2. Create the app with package name `dev.peppy.mobile`.
3. Play App Signing: upload the existing Android release key as the app-signing key, using PEPK (Play Encrypted Private Key tool). Reuse the existing key rather than generating a new one, so the SHA-256 pin (`PEPPY_ANDROID_SIGNING_CERT_SHA256`) and the Google sign-in OAuth client registration stay valid. See [release updates](release-updates.md#android-signing).
4. Monetization: create the subscription product matching `PEPPY_HOSTED_PLAY_PRODUCT_ID`, and base plans matching `PEPPY_HOSTED_PLAY_BASE_PLAN_IDS`. Create no offers, trials, or intro offers in v1.
5. Service account: create a Google Cloud service account for the Developer API. Grant it the Play Console financial and orders permissions needed for subscription reads and acknowledgement. Use it for `PEPPY_HOSTED_PLAY_SERVICE_ACCOUNT_JSON` (or `_FILE`).
6. Pub/Sub: create a topic and a push subscription. Use OIDC authentication with the push service account. Set the endpoint to `https://peppy.pro/hosted/v1/play/billing/notifications` and the audience to `PEPPY_HOSTED_PLAY_RTDN_AUDIENCE`. The push identity email is `PEPPY_HOSTED_PLAY_RTDN_SERVICE_ACCOUNT`.
7. Monetization setup: connect the Pub/Sub topic, and enable real-time developer notifications and voided purchase notifications.
8. License testers: add test accounts under license testing. Set `PEPPY_HOSTED_PLAY_ALLOW_TEST_PURCHASES=true` only on a staging platform. Keep it `false` in production; test purchases are then rejected as `not_entitling`.
9. Data safety: declare the data types collected and shared, including SMS and contacts, and the encryption in transit and at rest. Keep it consistent with the privacy policy.
10. Privacy policy URL: `https://peppy.pro/privacy`.
11. Account deletion URL: `https://peppy.pro/account/delete`. Google requires an in-app and web deletion path for apps with accounts. `[verify]` current form fields.
12. Content rating questionnaire and target audience.
13. Store listing: use the assets in `apps/android/fastlane/metadata/android/en-US` (`title.txt`, `short_description.txt`, `full_description.txt`, `images/`).
14. Release uploads: follow [Play release upload](#play-release-upload) below. Upload the first release by hand; the workflow is for later releases.

## Play release upload

`.github/workflows/play-publish.yml` uploads the GitHub release AAB (`peppy-<version>-android.aab`) of a published stable `android-v<semver>` release to Google Play. It runs only on `workflow_dispatch`, with inputs `release_tag` and `track` (`internal`, the default, or `alpha`). `internal` sends the release as completed; `alpha` sends it as a draft, so roll it out in Play Console.

Owner checklist:

1. Create the GitHub environment `play-store` with these protection rules:
   - Deployment branches and tags: `production` only.
   - Required reviewers, with no administrator bypass. The same policy applies as for `updates-stable`.
2. Add the secret `PLAY_SERVICE_ACCOUNT_JSON` to the `play-store` environment only, not as a repository secret. It holds the full JSON key of a release-only service account.
3. Create the release-only service account in Google Cloud. In Play Console, under Users and permissions, invite its email with **Release manager** on the Peppy app only. Do not grant account-level or financial permissions. Do not reuse the billing service account (`PEPPY_HOSTED_PLAY_SERVICE_ACCOUNT_JSON`). Rotate the key if it ever appears in a log, a chat, or a file.
4. Dispatch the workflow only from `production`. The first job fails for any other ref.
5. Before upload, the workflow checks that the release is published and not a prerelease. It then checks that `SHA256SUMS` is signed by the stable checksum key committed at `infra/release/keys/stable-checksums.asc`. The workflow pins that key's fingerprint, `22973A816783D4DEF91C64BBBDEFEAB01080A42D`, which matches [release keys](release-keys.md). Verify the committed fingerprint against the protected-environment key through an independent channel, as [release updates](release-updates.md) requires. The workflow also checks the AAB digest against the signed manifest and runs `gh attestation verify` for `refs/heads/production`.
6. The first upload to a new Play app is manual. The Developer API cannot create the app record or complete the declarations. Before the first upload, complete the store listing, Data safety, content rating, and the SMS and other permission declarations. Then upload the first AAB in Play Console as a draft, and only use the workflow after that.

## Platform environment variables

Set these in the private platform (`peppy-platform`). Partial configuration is a startup error. Absent configuration disables Play, and the billing context then returns `available:false`.

| Variable | Purpose |
| --- | --- |
| `PEPPY_HOSTED_PLAY_PACKAGE_NAME` | Application ID, `dev.peppy.mobile`. |
| `PEPPY_HOSTED_PLAY_PRODUCT_ID` | Subscription product ID. |
| `PEPPY_HOSTED_PLAY_BASE_PLAN_IDS` | Comma-separated base plan IDs. |
| `PEPPY_HOSTED_PLAY_SERVICE_ACCOUNT_JSON` | Full service-account JSON (secret). |
| `PEPPY_HOSTED_PLAY_SERVICE_ACCOUNT_JSON_FILE` | Path to the service-account JSON file, as an alternative. |
| `PEPPY_HOSTED_PLAY_RTDN_AUDIENCE` | OIDC audience for the Pub/Sub push token. |
| `PEPPY_HOSTED_PLAY_RTDN_SERVICE_ACCOUNT` | Email of the push-authentication service account. |
| `PEPPY_HOSTED_PLAY_ALLOW_TEST_PURCHASES` | Boolean, default `false`. Allows license-tester purchases. |
| `PEPPY_HOSTED_PLAY_BASE_ATTACHMENT_ALLOWANCE` | Optional. Default matches the Apple default. |

Secrets are zeroized and never logged or printed in `Debug`. Purchase tokens never appear in logs, errors, or tracing fields. The endpoint contract is in [PLAY_BILLING.md](../../crates/hosted-api/PLAY_BILLING.md).

## Play coverage limits

These limits apply to users who have not granted `READ_RESTRICTED_MESSAGES` (the AppOp):

- Users get RCS through the link only for conversations matched to Google Messages.
- Unmatched conversations stay SMS.
- Outages longer than 15 minutes are not back-dated. Messages older than that can be missed.
- History before linking is not backfilled.
- Restricted media rows are not captured. Duplicates can appear for them on devices without the optional provider columns.
- Blocklist sync needs `WRITE_SMS`, so it is not covered.

Do not describe these limits as fixed in user-facing copy. Use the same wording as the [Google Messages link limits](../apps/android.md#google-messages-link-github-release-apk).

## Billing

- Hosted billing on Play uses the server endpoints in [PLAY_BILLING.md](../../crates/hosted-api/PLAY_BILLING.md) and the client in `apps/android/app/src/main/java/dev/peppy/mobile/billing/`.
- Account deletion with an active Play subscription does not revoke or refund through Google. The account is fenced, and the user is told to cancel in Google Play.
- The product sells no storage packs through Play in v1.
