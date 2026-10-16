package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.database.ContentObserver
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Telephony
import androidx.core.content.ContextCompat

internal fun smsReadGranted(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) == android.content.pm.PackageManager.PERMISSION_GRANTED

internal fun shouldObserveSmsProvider(readGranted: Boolean): Boolean = readGranted

internal fun needsSmsReadSettingsRecovery(hasRequested: Boolean, granted: Boolean, shouldShowRationale: Boolean): Boolean =
    hasRequested && !granted && !shouldShowRationale

internal data class SmsCaptureDebouncePolicy(val quietMs: Long, val maximumWaitMs: Long? = null)

internal fun smsCaptureDebouncePolicy(fastMode: Boolean): SmsCaptureDebouncePolicy =
    if (fastMode) SmsCaptureDebouncePolicy(FAST_QUIET_MS, FAST_MAX_WAIT_MS) else SmsCaptureDebouncePolicy(BASELINE_QUIET_MS)

/** Tracks a provider-notification burst using Handler's uptime clock. */
internal class SmsCaptureDebouncer(private val policy: SmsCaptureDebouncePolicy) {
    private var firstNotificationUptimeMs: Long? = null

    fun nextDelayMs(nowUptimeMs: Long): Long {
        val first = firstNotificationUptimeMs ?: nowUptimeMs.also { firstNotificationUptimeMs = it }
        val quietDeadline = nowUptimeMs + policy.quietMs
        val deadline = policy.maximumWaitMs?.let { minOf(quietDeadline, first + it) } ?: quietDeadline
        return (deadline - nowUptimeMs).coerceAtLeast(0)
    }

    fun reset() {
        firstNotificationUptimeMs = null
    }
}

internal object SmsReadPermissionRecovery {
    private const val NAME = "peppy-sms-permission"
    private const val REQUESTED = "read-sms-requested"

    fun recordResult(context: Context, granted: Boolean) {
        context.applicationContext.getSharedPreferences(NAME, Context.MODE_PRIVATE).edit()
            .putBoolean(REQUESTED, !granted)
            .apply()
    }

    fun needsSettingsRecovery(context: Context, shouldShowRationale: Boolean): Boolean = needsSmsReadSettingsRecovery(
        context.applicationContext.getSharedPreferences(NAME, Context.MODE_PRIVATE).getBoolean(REQUESTED, false),
        smsReadGranted(context),
        shouldShowRationale,
    )
}

/** Reads aggregate, vault-scoped history state only from a background dispatcher. */
internal fun readSmsHistoryState(context: Context, fallback: SmsHistoryState): SmsHistoryState {
    if (fallback != SmsHistoryState.WAITING) return fallback
    val client = NativeGateway.open(context) ?: return SmsHistoryState.WAITING_FOR_VAULT
    return try {
        val status = client.smsHistoryStatus(MmsPreferences(context).sourceGeneration, SmsCapture.PROVIDER_SCOPE)
        smsHistoryState(
            SmsHistoryCoreStatus(
                status.baselineInitialized,
                status.scanning,
                status.historyComplete,
                status.paused,
                status.providerError,
                status.pendingCount > 0uL || status.ambiguousCount > 0uL,
            ),
        )
    } catch (_: Exception) {
        // The vault database could not answer; this is not an Android provider outage.
        SmsHistoryState.WAITING_FOR_VAULT
    }
}

/** Schedules scanner work after provider changes; it never reads provider rows on the main thread. */
internal object SmsCaptureLifecycle {
    private val lock = Any()
    private var observer: SmsProviderObserver? = null

    fun ensureObserver(context: Context) {
        val app = context.applicationContext
        // Policy is core-backed and is checked by the IO worker. Registration itself is harmless
        // and must not open SQLCipher on application/observer main-thread callbacks.
        val wanted = shouldObserveSmsProvider(smsReadGranted(app))
        synchronized(lock) {
            if (wanted && observer == null) {
                observer = observer(app).also { app.contentResolver.registerContentObserver(SMS_URI, true, it) }
            } else if (!wanted && observer != null) {
                observer!!.cancel()
                app.contentResolver.unregisterContentObserver(observer!!)
                observer = null
            }
        }
    }

    fun onPermissionOrForeground(context: Context) {
        ensureObserver(context)
        if (smsReadGranted(context)) {
            SmsCaptureWork.enqueue(context.applicationContext)
            SmsCaptureWork.ensurePeriodic(context.applicationContext)
        }
    }

    internal fun observer(context: Context, policy: SmsCaptureDebouncePolicy = smsCaptureDebouncePolicy(BuildConfig.PEPPY_SMS_FAST_CAPTURE), onDispatch: (() -> Unit)? = null): SmsProviderObserver {
        val debounceHandler = Handler(Looper.getMainLooper())
        val debouncer = SmsCaptureDebouncer(policy)
        return object : SmsProviderObserver(debounceHandler) {
            private val enqueue = Runnable {
                if (!isActive()) return@Runnable
                debouncer.reset()
                val hintCount = pendingHints
                pendingHints = 0
                if (onDispatch == null && !smsReadGranted(context)) {
                    SmsCaptureLifecycle.ensureObserver(context)
                    return@Runnable
                }
                if (onDispatch != null) onDispatch() else {
                    if (SyncDiagnostics.enabled) {
                        SyncDiagnostics.withTrace(SyncDiagnostics.newTraceId()) {
                            SyncDiagnostics.record(SyncTraceEvent.OBSERVER_DISPATCH, SyncTraceField.COUNT to hintCount.toLong(), SyncTraceField.FAST_MODE to if (BuildConfig.PEPPY_SMS_FAST_CAPTURE) 1 else 0)
                            SmsCaptureWork.enqueueObserver(context)
                            SmsCaptureWork.ensurePeriodic(context)
                        }
                    } else {
                        SmsCaptureWork.enqueueObserver(context)
                        SmsCaptureWork.ensurePeriodic(context)
                    }
                }
            }

            private var pendingHints = 0

            override fun onChange(selfChange: Boolean) {
                if (!isActive()) return
                if (pendingHints == 0 && SyncDiagnostics.enabled) SyncDiagnostics.record(SyncTraceEvent.OBSERVER_HINT)
                pendingHints++
                debounceHandler.removeCallbacks(enqueue)
                debounceHandler.postDelayed(enqueue, debouncer.nextDelayMs(SystemClock.uptimeMillis()))
            }

            override fun cancelPending() {
                debounceHandler.removeCallbacks(enqueue)
                debouncer.reset()
                pendingHints = 0
            }
        }
    }

    private val SMS_URI = Telephony.Sms.CONTENT_URI
}

internal abstract class SmsProviderObserver(handler: Handler) : ContentObserver(handler) {
    private var active = true

    fun isActive(): Boolean = active

    fun cancel() {
        active = false
        cancelPending()
    }

    protected abstract fun cancelPending()
}

/**
 * Observes MMS-provider changes as hints only. The worker rechecks provider rows off the main
 * thread; registration is fenced by both the user's MMS setting and current permissions.
 */
internal object MmsCaptureLifecycle {
    private val lock = Any()
    private var observer: MmsProviderObserver? = null

    fun ensureObserver(context: Context) {
        val app = context.applicationContext
        val wanted = MmsPreferences(app).enabled && mmsReceiveGranted(app)
        synchronized(lock) {
            if (wanted && observer == null) {
                observer = observer(app).also { app.contentResolver.registerContentObserver(MMS_URI, true, it) }
            } else if (!wanted && observer != null) {
                observer!!.cancel()
                app.contentResolver.unregisterContentObserver(observer!!)
                observer = null
            }
        }
    }

    internal fun observer(
        context: Context,
        policy: SmsCaptureDebouncePolicy = SmsCaptureDebouncePolicy(BASELINE_QUIET_MS),
        onDispatch: (() -> Unit)? = null,
    ): MmsProviderObserver {
        val handler = Handler(Looper.getMainLooper())
        val debouncer = SmsCaptureDebouncer(policy)
        return object : MmsProviderObserver(handler) {
            private val enqueue = Runnable {
                if (!isActive()) return@Runnable
                debouncer.reset()
                if (onDispatch != null) onDispatch()
                else if (MmsPreferences(context).enabled && mmsReceiveGranted(context)) {
                    MmsCaptureWork.enqueue(context.applicationContext)
                    MmsCaptureWork.ensurePeriodic(context.applicationContext)
                }
            }

            override fun onChange(selfChange: Boolean) {
                if (!isActive()) return
                handler.removeCallbacks(enqueue)
                handler.postDelayed(enqueue, debouncer.nextDelayMs(SystemClock.uptimeMillis()))
            }

            override fun cancelPending() {
                handler.removeCallbacks(enqueue)
                debouncer.reset()
            }
        }
    }

    private val MMS_URI = android.net.Uri.parse("content://mms")
}

internal abstract class MmsProviderObserver(handler: Handler) : ContentObserver(handler) {
    private var active = true

    fun isActive(): Boolean = active

    fun cancel() {
        active = false
        cancelPending()
    }

    protected abstract fun cancelPending()
}

private const val BASELINE_QUIET_MS = 15_000L
private const val FAST_QUIET_MS = 1_000L
private const val FAST_MAX_WAIT_MS = 3_000L
