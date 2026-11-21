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
- The send needs a complete conversation index, and prepares are refused while a scan runs (124). A full scan took about 45 seconds and 46 requests on the evidence device on 2026-11-11, and about 70 seconds and 24 requests on 2026-11-14. Do not add callers that rescan on every readiness pass. See [Conversation index revalidation](#conversation-index-revalidation).
- A carrier-only proof is decided from the index alone. Only an RCS send fetches its target conversation again before committing (`BindNativeRcsTarget`, `CommitNativeRcs`). An index that is wrong about a chat being absent or unsupported therefore produces an SMS, not a refusal.
- A subject-only MMS to a number that has an RCS chat has no send path. It is neither text nor media, and the live mapping blocks the carrier.

## Reactions

Native RCS reactions are always on. The phone advertises support (`publishReactionCapability`) while the link is linked and running, and runs reaction jobs whenever native RCS is enabled. The phone publishes on every status change, and core skips an unchanged value per key epoch, so a new epoch is advertised again. A desktop shows "Update Peppy on your phone to react in this chat." for an eligible RCS chat whose phone has no capability record for the active epoch, which means a build older than the reactions feature. A phone on the earlier dark-launch build published `supported: false`, so its chats show no control until it updates. Without an active epoch, desktops report no reaction support rather than the update prompt.

- `SendReaction` is the one libgm call added to `allowedLibgmCalls` for this feature (`gmlink/source_test.go`). Do not allowlist another call to work around a reaction problem.
- The send mirrors the text send: `PrepareReaction` makes no request; `CommitPreparedReaction` consumes the ticket, re-validates it after the conversation lookup and again at transport admission, then calls `SendReaction` once. A transport error, a nil response or `success: false` is `outcome_unknown` and is never retried. The SIM payload comes from the conversation's `DefaultOutgoingID`, as for text.
- Core grants one attempt per request (`begin_reaction_attempt`); an attempt left unfinished becomes unknown. Requests learned from a snapshot or restore, from a retired key epoch, or older than ten minutes plus skew are never attempted. Requests are sealed only for a phone that advertised support, so an older phone sees them only as unknown events.
- The Google message ID comes from `rcs_message_identities` by Peppy message ID. A message with no identity row, or with any `rcs_mirror_ids` entry marked conflicted, is refused as `message_unavailable`.
- Received reactions ride on the projected record's `reactions` field. A set that breaks the limits (32 reactions, 100 actors, 32-byte emoji, the shared emoji contract in `packages/contracts/fixtures/reaction-emoji.json`) is omitted whole, with diagnostic 287; the rest of the record, including group sender facts, is unchanged. Google sends most emoji, including 🙏 and anything from the picker, as `CUSTOM` with Unicode set; those are kept.
- Reaction diagnostics are 287 to 306 (`reaction/*`); none carries an emoji or message text.

What is not established: whether a reaction change on an existing message reaches the link as an update event, whether the receive sweep recovers one missed during a reopen (a reaction does not move a chat's last activity, so the memo can skip it, like a status change), and whether Google accepts the message ID for add, switch and remove on both received and sent messages. Start here when a reaction is missing, stuck as sending, or refused as `message_unavailable`.

## Conversation index revalidation

libgm reopens its long-poll stream about every 16 minutes on the evidence device, and the safety patch retires the stream on every reopen, including a clean one. The link used to discard the index on each reopen and at ten minutes of age (`conversationIndexMaxAge`), then list every conversation again. It now keeps a copy of a complete index, the probe basis, and checks it against one page.

- `EnsureConversationIndex` fetches the first inbox page (25 conversations) and adopts the basis when every returned conversation matches it in the fields the index serves to the core (`conversationCandidateRecordFor`). The fresh records replace the kept ones for the returned IDs. Adoption emits 282; a difference or a request error emits 283 and runs the full scan.
- The basis is captured before the index is invalidated by a clean `StreamRetired`, by `ActiveSessionSet`, by settings that carry SIM cards, and by the ten-minute age limit. `ActiveSessionSet` and the settings follow every reopen as the answer to the link's own session request (`sessionEvidenceRequestLocked`), so they keep an existing basis. Dropping it there would stop every probe.
- Everything else drops the basis: `ContinuityLost`, a reopen more than 15 seconds after the retire (`conversationProbeMaxGap`), any conversation event, `PhoneNotResponding`, a temporary listen error, `NoDataReceived`, `StreamGap`, old or SIM-less settings, a failed health check, a new run, revoke, and the start of any full scan.
- `RefreshConversationIndex` always scans. Group creation uses it to see the new group.
- A full scan runs at least every two hours (`conversationFullScanMaxAge`). An adopted index does not reset that clock.
- A probe whose page matched but whose result could not be published, because the stream epoch moved during the request, returns the stale code and keeps the basis. The settings event that lands about two seconds after a reopen does this. The event's own `index/stale` starts the next readiness pass, which probes again.

What the probe cannot see: a change to a conversation outside the first inbox page that happened during a reopen and that Google did not redeliver. Such a record can be stale for up to two hours. While it is, a message to a chat that only just became RCS-capable can go out as SMS, outbound typing can use the chat's previous SIM (`typing.go` reads the cached `DefaultOutgoingID`), and group reconcile and eligibility for that chat can be late. The owner accepted this on 2026-11-15. A fresh fetch before a carrier-only decision would remove the SMS case and is not built.

## Receive sweep

After every readiness pass (so after every stream reopen, about every 16 minutes on the evidence device) `RcsReceiveCoordinator.resume` recovers history for the chats the link owns. It used to walk the same chats again each time: 1,607 message list requests in 3 hours 41 minutes on 2026-11-16, in bursts of 86 to 131 after each reopen. Two causes, both fixed:

- A walk ended only on the last page or on a page whose projected records were all below the floor (`watermark - 15 min`). Go reports `has_more` for every non-empty page and drops messages it does not project, so a chat with no projectable record near the top (carrier messages in a chat that Google also offers over RCS) was walked to the end of its history, or to the 32-page cap, where it wrote no state and repeated. A walk now also ends at the first page whose newest message of any kind is below the floor (`newest_ms` in the page JSON, computed over every message on the page).
- A completed recovery sets the watermark to the newest projected record, so a chat with any message since then is never idle. The coordinator now keeps an in-memory memo of each chat's index last-activity at its last completed sweep and skips a chat whose activity is unchanged.

Rules for the memo:

- It is skipped only when the stored activity equals the current one, the entry is under two hours old (`SWEEP_MEMO_MAX_AGE_MS`), and the chat has no resume hint and is not incomplete. Activity of zero or unknown is always swept.
- Only a completed walk that started at the newest page stores an entry. A walk resumed from a hint covers the older tail only and stores nothing.
- A full index scan clears it: `GoogleMessagesLinkService.conversationIndexReady` calls `invalidateSweepMemo` for every successful index result that is not `"index":"reused"` or `"index":"adopted"`, including a result with no `index` field. A sweep that was running across the clear stores nothing (`memoGeneration`).
- A targeted recovery after a claim ignores the memo and removes that chat's entry. The live receive path never reads it.
- It is never persisted. The coordinator is created per opened link handle, so a new handle or a process restart starts empty.

What the memo cannot see: a delivery or read status that changed during a reopen and that Google did not redeliver. Status does not move a chat's last activity, so such a mark can lag until the next full scan or the two-hour age, whichever is first. A missed message either moves the first inbox page (the probe misses, a full scan runs, the memo is cleared) or sits in a chat outside that page, which is the exposure described under index revalidation. The owner accepted this on 2026-11-16.

Not fixed: the watermark advances only to a projected record. A chat whose new traffic is never projectable keeps its watermark, so each new message there costs one walk from the newest page down to it, and the walk grows. Past 3,200 such messages it would reach the page cap on every sweep again. Advancing the watermark after a walk with no projected record would change what recovery may skip for messages that become projectable later (`status_unproven`, `message_unproven`), so it needs a core decision first.

A page entirely below the floor is still offered to `core.invalidate` for protocol-switch signals before the walk ends; deeper pages no longer are. The 1,081 signals the measured walks offered were all older than 24 hours and all skipped.

## Typing indicators

A contact's typing arrives as a Google typing record. The link projects it ([typing.go](../../apps/android/googlemessages/gmlink/typing.go)), the core seals it to the vault, and the phone sends it on its `/v1/live` socket ([TypingBridge.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/TypingBridge.kt)). Typing from a computer arrives on that socket. The core opens and admits it, and the host forwards each admitted start to Google Messages ([LiveTypingAdapter.kt](../../apps/android/app/src/gmlink/java/dev/peppy/mobile/googlemessages/LiveTypingAdapter.kt)).

A receiver shows a peer's typing only from a session it has confirmed. It challenges the peer, and the peer's SessionAck confirms the session. Frames from an unconfirmed session trigger a challenge and are dropped. To recover the first frames, the device that answers a challenge re-sends its still-active typing starts as ordinary frames after the ack. The bounds are two re-sends per ack, no re-send of the same start within 3 seconds, no re-send of a start with under 1 second left, and six re-sends per minute per device ([ephemeral_v2.rs](../../crates/client-core/src/ephemeral_v2.rs), [MAX_REPLAYS_PER_MINUTE](../../crates/client-core/src/ephemeral_v2_control.rs)). A re-send keeps the original expiry, so it never extends typing. Contact starts re-send to any challenger. A vault-device start re-sends only to its gateway, and sharing off suppresses those.

Still lost:

- A start whose Challenge or SessionAck is lost. After three unanswered challenges to one peer, the next challenge waits 60 seconds ([ephemeral_v2_session.rs](../../crates/client-core/src/ephemeral_v2_session.rs)).
- Typing that happens while the phone's `/v1/live` socket is closed. The phone closes it 60 seconds after no other non-Android device of the vault is connected.
- A contact burst longer than 15 seconds when Google sends only one start. The dots drop at 15 seconds.

Recovery:

- After a protocol fuse (`PROTOCOL_ERROR`), the bridge restarts the live host on the next service update once a cooldown has passed. The cooldown starts at 5 minutes and doubles at each restart, up to 60 minutes. A stop or a new gateway session resets it. Reason 334 marks each restart.
- Authentication failure and membership revocation are terminal. The host does not restart until the gateway session changes or the bridge is stopped and started.
- A failed core enable is retried on a membership renewal, at most once per 60 seconds (335).

Google Messages has a setting that shows typing indicators. When the link sees its value change, it reports 312 (on) or 311 (off). The link only reports this value. It does not gate typing, and what Google does with the setting is not established.

Reading a capture, by the first boundary reached:

- Contact typing: expect 309, then 56, then 310 and 56 at the stop. 307 or 308 with no 309: the link did not project the record. 313 to 320 name the Kotlin gate that dropped it. 321: the socket refused the frame.
- Contact typing and a computer challenge: when a computer challenges the phone, expect 338, then 325. The re-sent frames record no 56 of their own. When a contact is typing, a 325 with no 338 means nothing was re-sent: no active start, under 1 second left, re-sent within 3 seconds, or the six-per-minute budget was spent. A 325 with no 338 is normal when the computer is the one typing.
- Typing from a computer: expect 325 (session confirmation), then 57. 336 on every frame: the core dropped each frame. 59: the adapter did not admit the intent. 280: no link-owned Google conversation. 328 to 333: the effect was refused, by cause.
- Live host: 49 or 50, then 53 or 55, with no later 52 or 54: the live host stopped (authentication or protocol). 334 shows a later restart. 52, then 322 to 324: the typing enable failed. 335 shows the retries. 54: the typing enable succeeded.

## Reading the link's diagnostics

Diagnostics are off unless enabled in **Settings → Advanced → Diagnostics**. When on, each link event also goes to logcat as `PeppyRcsDiagnostics: reason=<n>`, with `n` from the append-only table in [GoogleMessagesLinkDiagnostics.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/diagnostics/GoogleMessagesLinkDiagnostics.kt). Other diagnostic events, including provider-capture reasons from `SyncDiagnosticReason`, appear only in the exported file.

| Pattern | Meaning |
| --- | --- |
| 204 with 206, 207, or 208 on every pass | A command is waiting that no send path lists (`pending_without_candidates` plus its shape). This is a routing gap, not a carrier failure. 209 is a plain text and should become a candidate. |
| 135, 140, 211, then 79 | A normal RCS text send. 211 (`google_conversation_ambiguous`) at the route step is expected. |
| 135, 140, 211, then 80 | The number has no RCS chat. A carrier-only proof was written; the carrier send follows on the next gateway pass. |
| 84, 94, 97 before a 79 | Transient refusals (begin, facts, target binding). `BlockedSendRetry` retries with backoff. Persistent repeats are a bug. |
| 93 with a code from 210–224 | The core refused to bind the Google chat to the Peppy conversation; the second code is the reason. For a send, 217 means the number index has no entry for the recipient, 218 that the entry is another conversation, and 224 that the command's recipient is not the chat's participant. 222 comes from the receive-side thread guard (the indexed thread also carries someone else). |
| 124 | The conversation index is incomplete or being rescanned. |
| 282 after 26, 48, 199, 33 | A stream reopen followed by a one-page probe that adopted the kept index. Expect one or two list requests (178) per reopen. |
| 283, then many 168 and 171 and a final 169 to 173 | The probe found a difference or failed, and a full scan ran (every inbox page, then archive and spam). A probe on its own shows one 168 and one 171. On every reopen it means the first inbox page never matches the kept index. See the evidence boundary. |
| 161 between a probe's list request and a later 282 | The probe matched but the stream epoch moved during the request. The next readiness pass probes again. |
| 45 with 284 on every sweep | A recovery reached the 32-page cap (`recovery_incomplete/page_cap`). The chat has more unprojectable history above its watermark than one walk covers. See [Receive sweep](#receive-sweep). |
| 45 with 285 or 286 | A history page failed to load (285), or its cursor was missing or repeated (286). The hint is cleared and the next sweep starts at the newest page. |
| Event 28 with `PAGE_QUERIES` near zero after a 282 | The expected idle sweep: the probe adopted the index and unchanged chats were skipped. A sweep that fetches after a 283 is expected too, because the full scan cleared the memo. |
| 226 | A mirrored text row was skipped because the link already delivered it. |
| 231 on every mirrored text | The link did not deliver, or its IDs do not match the row's ID. See the evidence boundary. |
| 230 | A mirrored row is waiting one capture pass for the link. |
| 233 | A mirrored row with an attachment; always captured, so the picture appears twice. |
| 227, 228 | The row predates the receive claim (claim time minus 15 minutes) or falls in the 120-second guard band after it; the capture takes it. |
| 190 in bulk | Bootstrap could not bind a chat for receive, for example no recorded own number for the SIM or a thread that also holds a group. Those chats arrive only as provider copies. |
| 44, 45 | History recovery aborted or incomplete for a chat. |
| 309 and 310 | Google delivered a typing start or stop for a burst, and the link projected it. This is not host acceptance: 56 records that. |
| 307 and 308 | 307: the record was ignored (unknown typing type, no sender number, or a sender who is not a visible participant). 308: the conversation is unknown or its ID is empty. With no 307, 308, or 309, the link dropped the event silently (closed, stale stream, or no receive verification). |
| 311 and 312 | Google Messages' typing indicators setting went off (311) or on (312). Reported when the value changes. The link only reports this value. It does not gate typing, and what Google does with the setting is not established. |
| 313 to 317 | Contact typing dropped before it was sealed. 313: LinkRuntime rejected the callback (handle, generation, revision, or stream epoch). 314: no typing sink. 315: native client or local device ID missing. 316: record unparsable, no Google ID, or unknown state. 317: no receive context, or the conversation is not link-owned. |
| 318 to 320 | Contact typing sealed but not sent. 318: the core refused the seal (not eligible, participant not in roster, or budget). 319: the seal threw. 320: the sealed frame was not sent because generation, incarnation, or publication moved. |
| 56 | The host accepted a contact's sealed typing frame for sending. |
| 321 | The host could not hand a frame to the socket: no socket, oversize, stale publication, or the socket refused the enqueue. |
| 325 on a desktop start, with no 57 | Expected during session confirmation: the phone answered with only a handshake reply or session update. |
| 338 | The phone answered a peer's session challenge and sent at least one re-sent typing start after the ack. Recorded once per frame, before that frame's 325. A refused or stale send in the same batch also records 321 or 327. No 338 follows a refused or stale reply. It shows a re-send went out, not that the peer displayed it. |
| 336 on every frame | The core dropped each peer frame: not addressed to this device, not eligible, over budget, or malformed. Ordinary on the vault fan-out. Check eligibility and addressing for a computer that types. A frame with a future timestamp can also produce this. |
| 59 | The adapter did not admit a typing intent: wrong session, sequence not newer than the last one, not a start, or past its deadline. Also fires when an admitted intent is no longer current, or is past its deadline, when it is published. Expected at every stop from a computer. 59 on a start with no 57 is the failure signal. |
| 58 | A start was not forwarded because the last forwarded start for that conversation is still valid. |
| 57 | A desktop typing start was forwarded to Google Messages. |
| 280 | The intent's conversation has no link-owned Google conversation ID, or the lookup threw. Nothing was forwarded. |
| 328 to 333 | The typing effect was refused or failed before or at the Google call, by cause. 328: unavailable. 329: stale. 330: expired or deadline. 331: failed or cancelled. 332: invalid or stopped. 333: no usable conversation (unknown, not one-to-one, no SIM payload, or a group chat without group verification). The Google call itself can fail as 331 (error or cancelled) or as a deadline in 330. 331 can also be a native-call exception in LinkRuntime. 328, 329, 332, and 333 are refusals before the call. |
| 281 | An unrecognized setTyping result. Known results use 328 to 333. |
| 322 to 324 | Typing enable failed. 322: native client unavailable. 323: core enable threw. 324: sharing reapply failed. |
| 335 | A failed enable was retried on membership renewal, at most once per 60 seconds. |
| 334 | The bridge restarted the live host after a protocol-fuse cooldown. |
| 326, 327, 337 | 326: core open threw, or client missing. 327: a reply or re-sent frame was not sent because authority moved. 337: an opened outcome belonged to an older generation or incarnation. |
| 49, 50, 52, 53 | Live host state. 49: authentication refused. 50: protocol fatal. 52: live host enabled. 53: live host disabled. |
| 54, 55 | Typing state. 54: typing enabled. 55: typing disabled. |

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
- How often the one-page probe adopts on a device (282 against 283). It depends on Google Messages returning the same served fields for an unchanged conversation in two list calls. Tests cover the logic only.
- Whether Google redelivers a conversation event sent during a reopen of a few seconds.
- Row shapes on other devices, Android versions, carriers, or Google Messages versions.
- Why pictures that had been waiting for hours left the queue after a reboot with no outgoing row in the provider. The log for that period was lost.
- Typing, end to end. Nothing in the typing path was observed on a phone, in a browser, or in a running desktop floating head. The Windows head drawing has not been compiled or run. Unit and integration tests cover the logic only.
- Whether a re-send after a challenge recovers the first typing frame on a device. 338 records the send, not the display.
- Whether Google sends one STARTED per typing burst or repeats it. Reasons 309 and 310 exist to measure it.
- Whether the confirm-then-resend bounds, the 60-second peer close, and the fuse cooldowns behave as written on a device.
- Whether Google Messages reports the typing indicators setting (311 and 312) on a device.
