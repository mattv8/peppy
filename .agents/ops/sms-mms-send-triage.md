# SMS/MMS send-failure triage

Runbook for a report that an SMS or MMS "did not send" from an Android gateway phone. The goal is to decide quickly whether the failure is in Peppy or in the carrier path, using evidence from the phone before reading code.

Use it with a phone connected over USB debugging. Every step is read-only.

## Ground rules

- Do not resend or retry a message. A send with an unknown carrier outcome must never be repeated; see [commands, delivery, and recovery](../AGENTS.md#commands-delivery-and-recovery).
- Do not force-stop apps, toggle settings, clear data, or install builds without the owner's approval. Never touch `dev.peppy.gmtest`.
- Do not project message bodies. Query recipient addresses only to identify the one row under investigation.
- Keep pulled logs, screenshots, and probe scripts in `.opencode/sessions/<task>/`. Never commit them.
- Do not start from a suspected pull request. Identify the failing layer first.

## Decision summary

| Evidence | Conclusion |
| --- | --- |
| The platform MMS log shows requests for the message, and each fails at connect, HTTP, or network acquisition | Carrier or network path. Peppy is not involved. |
| The platform MMS log shows requests only for another app's message while that message is stuck | Not Peppy. Peppy had nothing in the platform queue. |
| The platform MMS log shows `processResult: success` but the recipient has nothing | Carrier or recipient side, after handoff. |
| The message is a Peppy command and the platform log shows no request for it | Peppy side: the command is waiting, was refused before the carrier, or never reached the phone. Continue with [the Peppy side](#6-check-the-peppy-side). |
| A Peppy `MMS_CALLBACK` or `SMS_CALLBACK` broadcast carries a non-OK result code | Platform or carrier rejected a Peppy send. Read the code and the platform log at that time. |

## 1. Reach the phone

`adb` is not on `PATH` on the development Mac. Use `~/Library/Android/sdk/platform-tools/adb`.

Several `adb` servers can run at once on ports 5037, 5038, and 5039. Only one holds the USB device at a time, and the device moves between them. Symptoms are an empty `adb devices` list, `device not found`, and `error: closed` in the middle of a command. Check each port:

```sh
adb -P 5037 devices -l
adb -P 5038 devices -l
adb -P 5039 devices -l
```

Do not kill the other servers; they may belong to other sessions. Run each check as one short `adb -P <port> -s <serial> shell '<command>'` call, filter on the device with `grep`, and retry on whichever port currently lists the phone. In zsh, write `"${SERIAL}[[:space:]]"`, not `"$SERIAL[[:space:]]"`, which zsh parses as a subscript.

## 2. Identify the message and which app created it

Ask the user which app they sent from and at what time. Then confirm it on the phone.

```sh
adb shell cmd role get-role-holders android.app.role.SMS
adb shell 'content query --uri content://mms --projection _id:thread_id:date:date_sent:msg_box:resp_st:sub_id:m_size:creator:tr_id --where "date > <epoch-seconds>" --sort "_id DESC"'
adb shell 'content query --uri content://mms/<id>/addr --projection address:type'
adb shell 'content query --uri content://sms --projection _id:date:date_sent:type:status:error_code:creator:sub_id --where "thread_id=<thread> AND date > <epoch-millis>" --sort "_id DESC"'
```

- Always pass a `--where` bound. An unbounded MMS query returns the whole history.
- MMS `date` is in seconds. SMS `date` is in milliseconds. Convert on macOS with `date -r <epoch-seconds>`.
- `creator` names the package that wrote the row. Peppy-sent SMS rows were observed with `creator=dev.peppy.mobile`. Whether Peppy-sent MMS appear in `content://mms` was not established.
- MMS address `type` 151 is a recipient and 137 is the sender. `msg_box` 1 is inbox and 2 is sent. SMS `type` 1 is inbox, 2 sent, 4 outbox, 5 failed, 6 queued.
- A row in the sent box is not proof of sending. Google Messages wrote `msg_box=2` for an MMS that its own UI still showed as pending, and that the carrier never accepted. Trust the platform log in the next step, not the provider row.

If the user has a screenshot, pull the newest one and read it:

```sh
adb shell 'ls -lt /sdcard/Pictures/Screenshots | head -5'
adb pull /sdcard/Pictures/Screenshots/<name>.png .opencode/sessions/<task>/
```

## 3. Read the platform MMS log

This step usually settles the question. On the Pixel 7 the platform `MmsService` tag is in the `radio` log buffer. The `main` and `system` buffers returned no `MmsService` lines, so a default `logcat` capture looks empty.

Bound the read with `-T` and filter on the device. Unfiltered dumps of every buffer are large and tend to drop the connection.

```sh
adb shell 'logcat -b radio -d -v threadtime -T "MM-DD HH:MM:SS.000" -s MmsService | grep -E "sendMessage \{|downloadMessage:|processResult|java.net.[A-Za-z]+Exception|MMS failed with|HTTP: [0-9]{3} |HTTP: (POST|GET)|Wifi signal" | cut -c1-250'
```

Read it as follows:

- `sendMessage {x-message-id:<n>}` starts one request. Count the distinct message ids. Each id is one message, whichever app sent it.
- `HTTP: POST <mmsc>, PDU size=<bytes>` is the upload attempt. A request makes several attempts before it gives up.
- `HTTP: 200 OK` followed by `processResult: success` means the carrier's MMS server accepted the message.
- `processResult: failure(<n>)` and `MMS failed with error: <n> httpStatus:<code>` report the failure. An exception line before it names the cause, for example a connect timeout to the MMS server.

The failure number is an Android `SmsManager` result code. Only 4 was observed in this investigation; the rest are listed from the Android API.

| Code | Constant |
| --- | --- |
| 1 | `MMS_ERROR_UNSPECIFIED` |
| 2 | `MMS_ERROR_INVALID_APN` |
| 3 | `MMS_ERROR_UNABLE_CONNECT_MMS` |
| 4 | `MMS_ERROR_HTTP_FAILURE` |
| 5 | `MMS_ERROR_IO_ERROR` |
| 6 | `MMS_ERROR_RETRY` |
| 7 | `MMS_ERROR_CONFIGURATION_ERROR` |
| 8 | `MMS_ERROR_NO_DATA_NETWORK` |

Platform log tags for SMS sends were not captured in this investigation. For an SMS, start from the `content://sms` row and the Peppy callback broadcasts in step 4.

## 4. Attribute each request to an app

The platform log does not print the calling package. Tie a message id to an app through the result broadcast the platform sends back:

```sh
adb shell 'dumpsys activity broadcasts | grep -E -A 26 "BroadcastRecord\{[^ ]+ [^ ]*MMS_SENT" | grep -E "BroadcastRecord|resultCode|enqueueClockTime|extras:" | cut -c1-520'
adb shell 'dumpsys activity broadcasts | grep -E "act=dev\.peppy|cmp=dev\.peppy\.mobile" | cut -c1-260 | sort | uniq -c'
```

- Google Messages receives `...SendStatusReceiver.MMS_SENT` with `dat=content://mms/<id>`. Its extras carry `messageId=<n>`, which equals the `x-message-id` in the platform log. `resultCode` is the code from the table above.
- Peppy receives `dev.peppy.mobile.MMS_CALLBACK` with data `peppy-mms://callback/<commandId>`, and `dev.peppy.mobile.SMS_CALLBACK` for SMS. See [MmsCallbacks.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/MmsCallbacks.kt) and [SmsReceivers.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/SmsReceivers.kt).
- Broadcast history covers only recent minutes. An empty Peppy result means no callback in that window, not that Peppy never sent.

If every message id in the window belongs to another app, Peppy had no send in the platform queue.

## 5. Check the radio and data path

```sh
adb shell 'dumpsys telephony.registry | grep -E "mServiceState=|mSignalStrength=|mDataConnectionState=|mPreciseDataConnectionStates" | cut -c1-900'
adb shell 'dumpsys connectivity | grep -E "NetworkAgentInfo\{" | grep -E "MMS|IMS" | cut -c1-420'
```

Record the operator, the LTE or NR `level` with `rsrp` and `rssnr`, whether data is on `IWLAN` (Wi-Fi calling), and whether the MMS APN is connected and on which transport. MMS used the cellular MMS APN even while the phone was on Wi-Fi calling.

These values describe the moment of the query, not the moment of the send. State that when reporting them.

## 6. Check the Peppy side

Do this when the message is a Peppy command, or to confirm Peppy was idle.

```sh
adb shell 'dumpsys package dev.peppy.mobile | grep -E "versionCode=|versionName=|lastUpdateTime="'
adb shell 'dumpsys activity services dev.peppy.mobile | grep -E "ServiceRecord|isForeground|createTime|lastActivity" | cut -c1-260'
```

Ask the user to export diagnostics from **Settings → Diagnostics → Share log**, then pull the newest file:

```sh
adb shell 'ls -lt /sdcard/Download | head -5'
adb pull /sdcard/Download/diagnostics-<uuid>.log.txt .opencode/sessions/<task>/
```

The first line of the export records `version=`, `source=<commit>`, and `dirty=`. Compare that commit with the checkout before reading code:

```sh
git diff --stat <source-commit> HEAD -- apps/android crates/client-core crates/mobile-bindings
```

Event and field numbers are defined in [SyncDiagnostics.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/diagnostics/SyncDiagnostics.kt); gateway phase numbers are in [GatewayWork.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/GatewayWork.kt). The ones this triage uses:

| Record | Meaning |
| --- | --- |
| `event=15` with `20=17` | One carrier dispatch phase (MMS, then SMS). `6` is wall time, `7` is thread CPU, `8` is success. |
| `event=16` | One HTTP request to the Peppy server. `23` is the status; `-1` is a transport failure. |
| `event=22` | Socket state. `29=7` is connected, `29=8` is reconnecting. |
| `event=14` | Gateway pass end. `28` is 1 success, 2 retry, 3 failure, 4 throw. |
| `event=27` | Google Messages link event. `29` is a reason from [GoogleMessagesLinkDiagnostics.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/diagnostics/GoogleMessagesLinkDiagnostics.kt). |

Timestamps are elapsed time since boot, not wall time. Anchor them to the export file's modification time: the last record is close to the export.

Bucket the export to see when Peppy could reach its server and whether dispatch did real work:

```sh
awk -v bucket_min=5 'BEGIN { bucket = bucket_min * 60000 }
/^event=/ { delete k; for (i = 1; i <= NF; i++) { split($i, a, "="); k[a[1]] = a[2] }
  t = k["timestamp_ms"] + 0; if (t == 0) next; if (t > last) last = t; b = int(t / bucket); seen[b] = 1
  if ($1 == "event=16") { if (k["23"] == "-1") http_fail[b]++; else http_ok[b]++ }
  if ($1 == "event=22") { if (k["29"] == "7") sock_up[b]++; else sock_retry[b]++ }
  if ($1 == "event=15" && k["20"] == "17") { dispatch[b]++; if (k["7"] + 0 > cpu[b]) cpu[b] = k["7"] + 0; if (k["6"] + 0 > wall[b]) wall[b] = k["6"] + 0 } }
END { for (b in seen) printf "%6.0f min before export: http_ok=%d http_fail=%d socket_connected=%d socket_reconnecting=%d dispatch=%d dispatch_max_cpu_ms=%d dispatch_max_wall_ms=%d\n", (last - b * bucket) / 60000, http_ok[b], http_fail[b], sock_up[b], sock_retry[b], dispatch[b], cpu[b], wall[b] }' <export> | sort -n
```

Interpret it with these limits in mind:

- A run of buckets with only `http_fail` means Peppy could not reach its server. A send requested from another device cannot reach the phone during that time. It also shows the phone's connectivity was poor, which is useful corroboration when a carrier send failed in the same period.
- A dispatch phase with long wall time and a few milliseconds of CPU was waiting on the core client lock, usually behind SMS capture. It was not composing or sending.
- The export does not record how many commands were submitted, refused, or left waiting. `GatewayWork` discards those counts. Never report them as zero.
- A successful dispatch phase means the phase returned. It is not evidence that anything was sent.

Limits of inspection on a release build:

- The app is not debuggable, so `run-as` cannot read the callback store (`peppy-mms-callbacks`, `peppy-sms-callbacks`) or `files/mms-pdu`.
- `MmsDispatcher` catches composition and carrier-call exceptions without logging them, and a non-OK callback keeps only a numeric code. A Peppy-side wait or refusal therefore has to be inferred from the absence of a platform request plus the command's state on another device.

## 7. Rule out interference by reading code

Only after the phone evidence, and only if it leaves a question open. As of commit `c5f0fe39`, these were checked and hold:

- [AndroidManifest.xml](../../apps/android/app/src/main/AndroidManifest.xml) registers `SMS_RECEIVED` and `WAP_PUSH_RECEIVED` receivers with no priority. Peppy does not hold the default SMS role and does not declare `WRITE_SMS`. Both receivers handle incoming messages only.
- Peppy does not write to the SMS or MMS provider, abort or alter broadcasts, request or bind a network, or change SIM, APN, or default-app settings. Recheck with:

  ```sh
  rg -n 'abortBroadcast|setResultCode|requestNetwork|bindProcessToNetwork|RoleManager|resolver\.(insert|update|delete)|applyBatch|cancelNotification' apps/android/app/src --glob '*.kt'
  ```

  The expected matches are contact-provider writes and the notification-mirror dismissal.
- The only send paths are [MmsDispatcher.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/mms/MmsDispatcher.kt) and [SmsDispatcher.kt](../../apps/android/app/src/main/java/dev/peppy/mobile/sms/SmsDispatcher.kt), which call the system `SmsManager` for commands in Peppy's own pending queue.
- Peppy can share the platform's per-subscription MMS queue with the default app. Step 4 shows whether it did.
- The notification listener can invoke the default app's mark-as-read action and dismiss mirrored notifications. Neither affects an outgoing send.
- The sideload Google Messages link has native send and receive disabled. Step 6 shows whether `GoogleMessagesLinkService` is running and whether the export has link events.

## Report

State which layer failed and the evidence for it: the message id, the number of requests and attempts, the failure code and exception, and the time range. Then state what was not established, such as whether a connect timeout came from weak signal or a carrier-side fault. Say plainly that nothing was resent or changed.

## Evidence boundary

These steps were run on 2026-11-09 against a Pixel 7 on Verizon with Google Messages as the default SMS app and Peppy 0.27.3. In that case every request for the stuck MMS timed out connecting to the carrier's MMS server, and Peppy had no request in the platform queue. Log buffers, tags, broadcast names, and provider conventions on other devices, Android versions, carriers, and messaging apps are unestablished.
