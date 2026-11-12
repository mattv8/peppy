# Google Messages link (RCS) field notes

Notes for debugging RCS through the Google Messages link on an Android gateway phone: a message that will not send, a message that appears twice, or one that never arrives. They record what is not visible from the code alone. For the build layout and credential rules see [Android app guidance](../apps/android.md#google-messages-link-github-release-apk); for carrier SMS/MMS failures and how to reach the phone see [SMS/MMS send-failure triage](sms-mms-send-triage.md). The ground rules there apply here too: never resend, never project message bodies, keep pulled logs in `.opencode/sessions/<task>/`.

## Two sources deliver every RCS message

Google Messages copies each RCS message, incoming and outgoing, into the phone's MMS provider. On a linked phone Peppy can therefore learn of one RCS message twice:

- from the link (libgm), as a record with Google's message ID, stored with transport `Rcs`;
- from the provider capture, as an MMS-shaped row, stored with transport `Mms`.

Most RCS bugs in this area come from the two sources disagreeing or from code treating the provider copy as carrier history.

### What the provider rows look like

Observed with `content query` on the evidence device. Query IDs, dates, boxes, and address types only.

| Row | `restricted` | `text_only` | `tr_id` | Addresses |
| --- | --- | --- | --- | --- |
| Mirrored RCS text | 1 | 1 | `proto:<base64>`; protobuf path 1 → {1: `com.google.android.apps.messaging.`, 2 → 2 → 5: 24-character ID starting `Mx`} | Incoming: participant twice (types 130 and 137) and the phone's own E.164 number (151). Outgoing: own number (137), participant (151). |
| Genuine carrier MMS | 0 | 0 | Also `proto:<base64>` with the same package string, but the ID sits at path 1 → 2 → 3 → 1 | As sent by the carrier |
| Peppy carrier MMS | not observed | not observed | `peppy-<command UUID>` according to `mms.rs`; no such row was seen in the provider | not observed |

- A `proto:` prefix and the Google package string do not identify a mirrored RCS row. Only the inner path does. `rcs_mirror::mirrored_message_id` reads the RCS path and returns `None` for the carrier shape; keep it that way.
- A thread on the phone holds SMS rows, real MMS rows, and mirrored RCS rows for the same contact. One provider thread maps to one Peppy conversation (`mms_thread_conversations`), so no stored-data merge is needed to put them together.
- Reading restricted rows needs `READ_RESTRICTED_MESSAGES`. `SmsCaptureWork` never passes `restricted=true`, so SMS rows are always provider-owned.

## Rules that follow

- **Do not decide identity or routing from stored message history.** Mirrored rows are captured MMS that name the chat participant and the phone's own number. A history scan sees them as "an MMS involving this contact" and concludes the contact is uncertain or has two threads. Two shipped bugs had this cause: a send refused forever with `peppy_conversation_uncertain` (222), and every mirrored row captured as a duplicate. Use the number index (`addresses`, read through `rcs_mapping::indexed_conversation`), the proven mapping, and group rosters. A captured MMS never writes the number index, and a group never holds an index entry (`index_address`).
- **Link ownership of a chat is not delivery of a message.** The provider-row router (`route_rcs_provider_row_result`) returning `DetectorRecorded` makes the capture skip the row for good. Review found that nothing re-offers a skipped row and that detectors do not drive recovery. Skip a row only when Peppy already holds that message. Skipping on ownership alone turns a duplicate into a missing message whenever the link misses an event.
- **Match the two copies only through `rcs_mirror`.** IDs are scoped to one Peppy conversation and one source (`link` or `capture`), matched across sources only, and only for text. Each source records every ID it sees before any lookup, under its own identity for the message, so an ID reported for two messages becomes untrusted for both sources. Any attachment on either copy keeps both: the copies can hold different media. Removing any of these scopes reintroduces a wrong merge that review found.
- **The core cannot delete or retract a message.** A duplicate that is already stored stays, and a late second copy cannot be merged afterwards. Prevent duplicates at ingest; do not plan on cleanup.
- **A link identity with no message is deliberate.** When the capture already holds a text, the link records `rcs_message_identities.message_id = NULL`. Go reports a delivery status inside each record, and the binding applies it right after the record. A rejected status makes history recovery abort the page (`RcsReceiveCoordinator.ingestAborts`), and every retry repeats it. Any new "duplicate" outcome must leave its status applicable or explicitly ignored.
- **Stamp link-received one-to-one messages with the proven participant.** A reply takes its recipients from the newest message in the conversation, so an RCS message stored without the counterpart's address breaks the next reply.
- **Compare the phone's own number exactly when a match withholds a row.** `conversation_match::confirmed_own_address` also accepts a number that only ends like the own number. That is acceptable for reply recipients and wrong for proof matching: it let a group row pass as a one-to-one mirror.
- **Composed text is `Sms` for one recipient whatever the thread last carried** (`compose_transport`). The gateway picks RCS or carrier at send time. Before this rule, a text reply in a thread whose newest message was a mirrored row was queued as MMS and then fit neither the text nor the media send path.

## Sending while linked

- With the link on, a carrier send to an unmapped one-to-one conversation is blocked until a carrier-only proof exists for it (`rcs_send::carrier_admission_unmapped`). Only the native text send flow makes proofs, by classifying the number as having no RCS chat.
- A proof is valid for 30 seconds and only against the current link context. The next native attempt in the same batch advances the context and stales it. `RcsNativeSendWork` therefore ends the batch after a carrier-only result (`carrierHandoffRequested`) and the gateway pass sends one carrier message. Expect one carrier message per pass.
- A one-recipient message that is not plain text (attachment or subject) and has no live mapping is listed as a text candidate for the route decision only (`unattempted_route_probe_message`). It can never be sent as text: prepare and admission still use the strict text loader.
- `verifySendMapping` is called by both the text and the media coordinator. Its loader must accept any unattempted command (`pending_command_message`). A text-only loader there strands every picture to a mapped chat behind a live mapping, where the carrier is blocked as well.
- The send needs a complete conversation index. A full scan took about 45 seconds and 46 requests on the evidence device, and prepares are refused while it runs (124). The Go link reuses a complete index for up to ten minutes (`conversationIndexMaxAge`); group creation forces a rescan. Do not add callers that rescan on every readiness pass.
- A subject-only MMS to a number that has an RCS chat has no send path. It is neither text nor media, and the live mapping blocks the carrier.

## Reading the link's diagnostics

Diagnostics are off unless enabled in **Settings → Diagnostics**. When on, each link event also goes to logcat as `PeppyRcsDiagnostics: reason=<n>`, with `n` from the append-only table in [GoogleMessagesLinkDiagnostics.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/diagnostics/GoogleMessagesLinkDiagnostics.kt). Other diagnostic events, including provider-capture reasons from `SyncDiagnosticReason`, appear only in the exported file.

| Pattern | Meaning |
| --- | --- |
| 204 with 206, 207, or 208 on every pass | A command is waiting that no send path lists (`pending_without_candidates` plus its shape). This is a routing gap, not a carrier failure. 209 is a plain text and should become a candidate. |
| 135, 140, 211, then 79 | A normal RCS text send. 211 (`google_conversation_ambiguous`) at the route step is expected. |
| 135, 140, 211, then 80 | The number has no RCS chat. A carrier-only proof was written; the carrier send follows on the next gateway pass. |
| 84, 94, 97 before a 79 | Transient refusals (begin, facts, target binding). `BlockedSendRetry` retries with backoff. Persistent repeats are a bug. |
| 93 with a code from 210–224 | The core refused to bind the Google chat to the Peppy conversation; the second code is the reason. For a send, 217 means the number index has no entry for the recipient, 218 that the entry is another conversation, and 224 that the command's recipient is not the chat's participant. 222 comes from the receive-side thread guard (the indexed thread also carries someone else). |
| 124 | The conversation index is incomplete or being rescanned. |
| 226 | A mirrored text row was skipped because the link already delivered it. |
| 231 on every mirrored text | The link did not deliver, or its IDs do not match the row's ID. See the evidence boundary. |
| 230 | A mirrored row is waiting one capture pass for the link. |
| 233 | A mirrored row with an attachment; always captured, so the picture appears twice. |
| 227, 228 | The row predates the receive claim (claim time minus 15 minutes) or falls in the 120-second guard band after it; the capture takes it. |
| 190 in bulk | Bootstrap could not bind a chat for receive, for example no recorded own number for the SIM or a thread that also holds a group. Those chats arrive only as provider copies. |
| 44, 45 | History recovery aborted or incomplete for a chat. |

## Phone access traps

- After a reboot the Pixel's logcat buffers were 256 KiB and held about 40 seconds of history. Raise them before a test with `adb logcat -G 16M`; the setting does not survive a reboot. Evidence older than the buffer is gone, so capture at the time of the event.
- Capture with `logcat -d -v time --pid=<peppy pid>` into the session folder and filter locally. The pid changes on every install.
- Over Wireless debugging, run `adb -P <port> connect <ip>:<port>` and use `<ip>:<port>` as the serial. When USB debugging was turned off, the phone disappeared from all three `adb` servers and Wireless debugging still worked.
- Each MMS capture pass, except a manual recheck, starts 15 seconds after it is requested, so the link's live event normally arrives before the capture reads the mirrored row. The duplicate rules still have to hold in both orders.
- `adb install -r` over an installed release needs an APK signed with the same certificate and a version code no lower than the installed one. `assembleSideload` produces an unsigned APK.

## Build and test traps

- Several RCS suites are separate test targets, not modules of `integration`: `rcs_receive`, `rcs_receive_regressions`, `rcs_status`, `rcs_group_receive`, `rcs_group_create`, `rcs_group_e2e`, `rcs_media_replication`, `rcs_target`, `rcs_carrier_regressions`. A filter such as `--test integration -- rcs_` skips all of them. The full `integration` target takes more than an hour locally; filter it.
- After changing a core signature or a public struct, run `cargo check --workspace --tests --locked`. `services/mcp`, `browser-bindings`, and `desktop-api` construct core records in their tests and are easy to miss.
- `cargo test -p peppy-mcp` includes `server_mcp`, which needs `TEST_DATABASE_URL`. Use `--lib` locally.
- Gradle's file watcher missed a source edit in this checkout and ran stale classes. Pass `--no-watch-fs`.
- CI runs `gofmt -l .` in `apps/android/googlemessages` after `prepare-libgm.sh`, so it checks the patched libgm tree under `build/third_party`. Code added by `upstream-safety.patch` must be gofmt-clean, and every patch edit needs `shasum -a 256 upstream-safety.patch > upstream-safety.patch.sha256`.
- A Go change to `gmlink` needs `./build-aar.sh` before the Android build picks it up. A change to a binding type needs the Kotlin and Swift bindings regenerated with `--no-format`, as in [CONTRIBUTING.md](../../CONTRIBUTING.md); CI fails on any diff.
- Go JSON read by the core is parsed with `deny_unknown_fields`. Add the Rust field in the same change as the Go field. The AAR and the native library ship in one APK, so no mixed deployment exists.
- Router and claim tests use the wall clock: a claim sets its proof time to now minus 15 minutes, and rows inside the 120-second guard band are never skipped. Fixed timestamps such as `1_000` route as pre-proof.
- To land a mirrored MMS acquisition in an existing conversation in a core test, record the SIM's own number (`set_mms_own_address`) and tie the provider thread to the conversation first, for example with an SMS acquisition that carries the same `provider_thread_id`. Otherwise the acquisition opens a new conversation and cross-source matching never applies.
- A gateway test fixture holds an attachment only after `install_downloaded_attachment`; until then the media send path does not materialize the command.
- A local table that needs no data migration can be created by an initializer called at open, without a schema version bump (`rcs_mirror::initialize`, like `mms_backfill::initialize`).

## Evidence boundary

Observed on 2026-11-11 on one Pixel 7 on Verizon with Google Messages as the default app, on builds `0.30.1-rcs-debug.13` through `.16`.

Established on that device: the provider row shapes above, RCS text sends submitted through the link (79), the carrier handoff after a carrier-only proof (80), and index reuse.

Not established:

- Which libgm field equals the ID in a mirrored row: `messageID`, `anotherMessageID.messageID`, or `anotherMessageID.anotherID`. Go reports all three and the core matches any. If none matches, texts in a link-owned chat are still captured twice (231, never 226). Once a device shows which field matches, report only that one.
- The route probe and the media verification fix on a device. Both are covered by tests only.
- Group RCS on a device.
- Row shapes on other devices, Android versions, carriers, or Google Messages versions.
- Why pictures that had been waiting for hours left the queue after a reboot with no outgoing row in the provider. The log for that period was lost.
