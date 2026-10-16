package dev.peppy.mobile

import android.Manifest
import android.content.Context
import android.database.ContentObserver
import android.os.Handler
import android.os.Looper
import android.provider.Telephony
import androidx.core.content.ContextCompat

internal fun smsReadGranted(context: Context): Boolean =
    ContextCompat.checkSelfPermission(context, Manifest.permission.READ_SMS) == android.content.pm.PackageManager.PERMISSION_GRANTED

internal fun shouldObserveSmsProvider(readGranted: Boolean): Boolean = readGranted

internal fun needsSmsReadSettingsRecovery(hasRequested: Boolean, granted: Boolean, shouldShowRationale: Boolean): Boolean =
    hasRequested && !granted && !shouldShowRationale

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
    private var observer: ContentObserver? = null

    fun ensureObserver(context: Context) {
        val app = context.applicationContext
        // Policy is core-backed and is checked by the IO worker. Registration itself is harmless
        // and must not open SQLCipher on application/observer main-thread callbacks.
        val wanted = shouldObserveSmsProvider(smsReadGranted(app))
        synchronized(lock) {
            if (wanted && observer == null) {
                observer = observer(app).also { app.contentResolver.registerContentObserver(SMS_URI, true, it) }
            } else if (!wanted && observer != null) {
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

    private fun observer(context: Context): ContentObserver {
        val debounceHandler = Handler(Looper.getMainLooper())
        return object : ContentObserver(debounceHandler) {
            private val enqueue = Runnable { onPermissionOrForeground(context) }

            override fun onChange(selfChange: Boolean) {
                debounceHandler.removeCallbacks(enqueue)
                debounceHandler.postDelayed(enqueue, DEBOUNCE_MS)
            }
        }
    }

    private const val DEBOUNCE_MS = 15_000L
    private val SMS_URI = Telephony.Sms.CONTENT_URI
}
