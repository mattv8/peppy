package dev.peppy.mobile

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject
import uniffi.peppy_mobile_bindings.NativeAccountAccessResult
import uniffi.peppy_mobile_bindings.NativeAccountNotice
import uniffi.peppy_mobile_bindings.NativeAccountNoticeKind
import uniffi.peppy_mobile_bindings.NativeNoticeSeverity
import uniffi.peppy_mobile_bindings.accountAccessFromResponse
import uniffi.peppy_mobile_bindings.accountAccessRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionResponseReadLimitBytes

internal enum class AccountRefreshTrigger { LAUNCH_OR_FOREGROUND, GATEWAY_PASS, POLICY_DENIAL }

/**
 * Process-wide account access cache. Its functions do blocking prefs and network work, so callers
 * run them on [kotlinx.coroutines.Dispatchers.IO] (or the gateway worker thread), never on main.
 * It needs only the device credential, so it also works while vault keys are locked.
 */
internal object AccountAccessState {
    private const val PREFS = "peppy-account-access"
    private const val K_CACHE = "cache.v1"
    private const val K_SNOOZES = "snoozes.v1"
    private const val K_LAST_ATTEMPT = "last-attempt-ms"
    private const val K_LAST_FORCED = "last-forced-ms"
    private const val K_EPISODE_ACTIVE = "episode-active"
    private const val K_EPISODE_KIND = "episode-kind"

    private val lock = Any()
    private var loaded = false
    private val accessFlow = MutableStateFlow<CachedAccountAccess?>(null)
    private val snoozeFlow = MutableStateFlow<Map<String, Long>>(emptyMap())
    private val readOnlyFlow = MutableStateFlow(false)

    /** Last good server answer, or null before the first success. */
    val access: StateFlow<CachedAccountAccess?> = accessFlow.asStateFlow()
    /** Snooze expiry per `snoozeKey`, in epoch millis. */
    val snoozes: StateFlow<Map<String, Long>> = snoozeFlow.asStateFlow()
    /** True only while the last successful answer says read-only. */
    val readOnly: StateFlow<Boolean> = readOnlyFlow.asStateFlow()

    fun ensureLoaded(context: Context) = synchronized(lock) {
        if (loaded) return
        val prefs = prefs(context)
        accessFlow.value = prefs.getString(K_CACHE, null)?.let(::decodeAccess)
        snoozeFlow.value = prefs.getString(K_SNOOZES, null)?.let(::decodeSnoozes).orEmpty()
        readOnlyFlow.value = accessFlow.value?.notice?.readOnly == true
        loaded = true
    }

    /** Foreground, launch, gateway-hourly and post-denial refreshes all funnel through here. */
    fun refresh(context: Context, trigger: AccountRefreshTrigger, nowMs: Long = System.currentTimeMillis()) {
        val app = context.applicationContext
        ensureLoaded(app)
        val binding = NativeGateway.reachabilityBinding(app) ?: return
        val key = listOf(binding.origin, binding.vaultId, binding.deviceId, binding.generation).joinToString("\n")
        prefs(app).edit().putLong(K_LAST_ATTEMPT, nowMs).apply()
        if (trigger == AccountRefreshTrigger.POLICY_DENIAL) prefs(app).edit().putLong(K_LAST_FORCED, nowMs).apply()
        // A cache (and its notification) from another binding must not survive, even if the request below fails.
        if (accessFlow.value?.let { it.bindingKey != key } == true) {
            publish(app, null)
            notifyEpisode(app, null, nowMs)
        }

        val answer = try {
            val response = GatewayHttp(binding.origin, binding.bearerToken)
                .retentionRequest(accountAccessRequest(), attachmentRetentionResponseReadLimitBytes())
            accountAccessFromResponse(
                response.code.toUShort(),
                response.body.orEmpty(),
                nowMs / 1000,
                listOf(binding.origin, HostedAccountClient.ORIGIN),
            )
        } catch (_: Exception) {
            // Transport failure: keep the last good answer, and never show or clear on error.
            return
        }
        val next = when (answer) {
            is NativeAccountAccessResult.Notice -> CachedAccountAccess(key, answer.notice.toCached(), nowMs)
            NativeAccountAccessResult.None, NativeAccountAccessResult.Unsupported -> CachedAccountAccess(key, null, nowMs)
            NativeAccountAccessResult.TemporarilyUnavailable -> accessFlow.value
        }
        if (next != accessFlow.value) publish(app, next)
        if (answer !is NativeAccountAccessResult.TemporarilyUnavailable) {
            notifyEpisode(app, next?.notice, nowMs)
        }
    }

    /** Gateway pass hook: refresh at most once an hour. */
    fun refreshIfDue(context: Context, nowMs: Long = System.currentTimeMillis()) {
        ensureLoaded(context.applicationContext)
        val last = prefs(context.applicationContext).getLong(K_LAST_ATTEMPT, 0L)
        if (gatewayRefreshDue(last, nowMs)) refresh(context, AccountRefreshTrigger.GATEWAY_PASS, nowMs)
    }

    /** A coded 403 policy pause: one forced, debounced refresh unless the cache already reads read-only. */
    fun onPolicyDenial(context: Context, nowMs: Long = System.currentTimeMillis()) {
        ensureLoaded(context.applicationContext)
        val lastForced = prefs(context.applicationContext).getLong(K_LAST_FORCED, 0L)
        if (policyDenialRefreshDue(accessFlow.value?.notice, lastForced, nowMs)) {
            refresh(context, AccountRefreshTrigger.POLICY_DENIAL, nowMs)
        }
    }

    /** Persists a 24h snooze for a dismissible warning only; read-only and near-deadline notices ignore it. */
    fun snooze(context: Context, notice: CachedAccountNotice, nowMs: Long = System.currentTimeMillis()) {
        ensureLoaded(context.applicationContext)
        synchronized(lock) {
            if (!notice.dismissible) return
            val next = snoozeUntil(snoozeFlow.value, notice, nowMs)
            prefs(context.applicationContext).edit().putString(K_SNOOZES, encodeSnoozes(next)).apply()
            snoozeFlow.value = next
        }
    }

    private fun publish(context: Context, next: CachedAccountAccess?) = synchronized(lock) {
        val editor = prefs(context).edit()
        if (next == null) editor.remove(K_CACHE) else editor.putString(K_CACHE, encodeAccess(next))
        editor.apply()
        accessFlow.value = next
        readOnlyFlow.value = next?.notice?.readOnly == true
    }

    private fun notifyEpisode(context: Context, notice: CachedAccountNotice?, nowMs: Long) {
        val prefs = prefs(context)
        val episode = AccountEpisode(
            active = prefs.getBoolean(K_EPISODE_ACTIVE, false),
            notifiedKind = prefs.getString(K_EPISODE_KIND, null)?.let { runCatching { NativeAccountNoticeKind.valueOf(it) }.getOrNull() },
        )
        val live = notice?.takeUnless { it.isStale(nowMs) }
        val (next, action) = accountNotificationStep(episode, live)
        if (next != episode) {
            prefs.edit().putBoolean(K_EPISODE_ACTIVE, next.active).putString(K_EPISODE_KIND, next.notifiedKind?.name).apply()
        }
        AccountStatusNotifier.apply(context, action, live)
    }

    private fun prefs(context: Context) = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun NativeAccountNotice.toCached() = CachedAccountNotice(kind, severity, deadline, manageUrl, dismissible, snoozeKey)

    private fun encodeAccess(access: CachedAccountAccess): String = JSONObject()
        .put("bindingKey", access.bindingKey)
        .put("fetchedAtMs", access.fetchedAtMs)
        .apply {
            access.notice?.let { notice ->
                put("notice", JSONObject()
                    .put("kind", notice.kind.name)
                    .put("severity", notice.severity.name)
                    .put("deadline", notice.deadline ?: JSONObject.NULL)
                    .put("manageUrl", notice.manageUrl ?: JSONObject.NULL)
                    .put("dismissible", notice.dismissible)
                    .put("snoozeKey", notice.snoozeKey))
            }
        }
        .toString()

    private fun decodeAccess(text: String): CachedAccountAccess? = runCatching {
        val json = JSONObject(text)
        val notice = json.optJSONObject("notice")?.let { n ->
            CachedAccountNotice(
                kind = NativeAccountNoticeKind.valueOf(n.getString("kind")),
                severity = NativeNoticeSeverity.valueOf(n.getString("severity")),
                deadline = if (n.isNull("deadline")) null else n.getLong("deadline"),
                manageUrl = if (n.isNull("manageUrl")) null else n.getString("manageUrl"),
                dismissible = n.getBoolean("dismissible"),
                snoozeKey = n.getString("snoozeKey"),
            )
        }
        CachedAccountAccess(json.getString("bindingKey"), notice, json.getLong("fetchedAtMs"))
    }.getOrNull()

    private fun encodeSnoozes(snoozes: Map<String, Long>): String =
        JSONObject().apply { snoozes.forEach { (key, until) -> put(key, until) } }.toString()

    private fun decodeSnoozes(text: String): Map<String, Long> = runCatching {
        val json = JSONObject(text)
        json.keys().asSequence().associateWith { json.getLong(it) }
    }.getOrDefault(emptyMap())
}

/** Posts or clears the one account-status notification. Fixed id, updated in place, never re-requests permission. */
internal object AccountStatusNotifier {
    const val CHANNEL_ID = "account_status"
    const val NOTIFICATION_ID = 4107
    /** Set on the intent that opens the Account tab from the notification. */
    const val EXTRA_OPEN_ACCOUNT = "dev.peppy.mobile.OPEN_ACCOUNT"

    fun apply(context: Context, action: AccountNotificationAction, notice: CachedAccountNotice?) {
        when (action) {
            AccountNotificationAction.None -> Unit
            AccountNotificationAction.Clear -> NotificationManagerCompat.from(context).cancel(NOTIFICATION_ID)
            is AccountNotificationAction.Post, is AccountNotificationAction.Update -> post(context, notice)
        }
    }

    @SuppressLint("MissingPermission")
    private fun post(context: Context, notice: CachedAccountNotice?) {
        if (notice == null) return
        // Never request POST_NOTIFICATIONS here; without a grant the ribbon is the fallback.
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) return
        val manager = context.getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, context.getString(R.string.peppy_account_notif_channel_name), NotificationManager.IMPORTANCE_DEFAULT),
            )
        }
        val intent = Intent(context, MainActivity::class.java)
            .putExtra(EXTRA_OPEN_ACCOUNT, true)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val pending = PendingIntent.getActivity(context, NOTIFICATION_ID, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val body = context.getString(R.string.peppy_account_notif_body, accountRibbonTitle(context, notice))
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher_monochrome)
            .setContentTitle(context.getString(R.string.peppy_account_notif_title))
            .setContentText(body)
            .setStyle(NotificationCompat.BigTextStyle().bigText(body))
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setPriority(NotificationCompat.PRIORITY_DEFAULT)
            .build()
        try {
            NotificationManagerCompat.from(context).notify(NOTIFICATION_ID, notification)
        } catch (_: SecurityException) {
            // The permission was revoked between the check and the post; the ribbon remains.
        }
    }
}

/** One-shot request to show the Account tab, consumed by the main screen. */
internal object AccountTabRequests {
    private val pending = MutableStateFlow(false)
    val state: StateFlow<Boolean> = pending.asStateFlow()

    fun request() { pending.value = true }
    fun consume() { pending.value = false }
}
