package dev.peppy.mobile.mms

import android.app.AppOpsManager
import android.content.Context
import android.os.Build
import kotlinx.coroutines.CancellationException

internal enum class RestrictedMessageAccess { ALLOWED, NOT_ALLOWED, UNSUPPORTED, UNKNOWN }

internal const val READ_RESTRICTED_MESSAGES_OP = "android:read_restricted_messages"

internal fun restrictedMessageAccess(
    context: Context,
    check: (AppOpsManager, String, Int, String) -> Int = ::checkAppOp,
): RestrictedMessageAccess {
    val appOps = context.getSystemService(AppOpsManager::class.java) ?: return RestrictedMessageAccess.UNKNOWN
    return restrictedMessageAccessForCheck {
        check(appOps, READ_RESTRICTED_MESSAGES_OP, context.applicationInfo.uid, context.packageName)
    }
}

internal fun restrictedMessageAccessForCheck(check: () -> Int): RestrictedMessageAccess = try {
    if (check() == AppOpsManager.MODE_ALLOWED) {
        RestrictedMessageAccess.ALLOWED
    } else {
        RestrictedMessageAccess.NOT_ALLOWED
    }
} catch (_: IllegalArgumentException) {
    RestrictedMessageAccess.UNSUPPORTED
} catch (_: UnsupportedOperationException) {
    RestrictedMessageAccess.UNSUPPORTED
} catch (_: Exception) {
    RestrictedMessageAccess.UNKNOWN
}

internal fun checkAppOp(appOps: AppOpsManager, operation: String, uid: Int, packageName: String): Int =
    if (Build.VERSION.SDK_INT >= 36) appOps.checkOpNoThrow(operation, uid, packageName)
    else if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) appOps.unsafeCheckOpNoThrow(operation, uid, packageName)
    else throw UnsupportedOperationException("AppOps check unavailable")

internal fun restrictedMessageAppOpCommand(packageName: String): String =
    "adb shell appops set $packageName $READ_RESTRICTED_MESSAGES_OP allow"

internal enum class ProviderRescanRequestOutcome { QUEUED, ALREADY_QUEUED, UNAVAILABLE, FAILED }

internal fun canRequestProviderRescan(
    mmsEnabled: Boolean,
    receiveGranted: Boolean,
    databaseOpen: Boolean,
    access: RestrictedMessageAccess,
): Boolean = mmsEnabled && receiveGranted && databaseOpen && access == RestrictedMessageAccess.ALLOWED

internal fun requestProviderRescan(
    access: RestrictedMessageAccess,
    observedMaxId: () -> Long,
    request: (String) -> Boolean,
    enqueue: () -> Unit,
): ProviderRescanRequestOutcome {
    if (access != RestrictedMessageAccess.ALLOWED) return ProviderRescanRequestOutcome.UNAVAILABLE
    return try {
        val started = request(observedMaxId().toString())
        enqueue()
        if (started) ProviderRescanRequestOutcome.QUEUED else ProviderRescanRequestOutcome.ALREADY_QUEUED
    } catch (_: SecurityException) {
        ProviderRescanRequestOutcome.UNAVAILABLE
    } catch (error: CancellationException) {
        throw error
    } catch (_: Exception) {
        ProviderRescanRequestOutcome.FAILED
    }
}
