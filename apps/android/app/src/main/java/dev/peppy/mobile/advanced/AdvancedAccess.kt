package dev.peppy.mobile.advanced

import android.app.AppOpsManager
import android.content.Context
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.mms.checkAppOp
import dev.peppy.mobile.mms.restrictedMessageAccess
import dev.peppy.mobile.mms.restrictedMessageAccessForCheck

internal const val WRITE_SMS_OP = "android:write_sms"
internal const val ADVANCED_FEATURES_PREFERENCES = "peppy-advanced-features"
internal const val ADVANCED_FEATURES_SEEN = "advanced_features_seen"

internal data class AdvancedFeatureState(
    val access: Pair<RestrictedMessageAccess, RestrictedMessageAccess>,
    val seen: Boolean,
)

internal fun blockedSendersAccess(
    context: Context,
    check: (AppOpsManager, String, Int, String) -> Int = ::checkAppOp,
): RestrictedMessageAccess {
    val appOps = context.getSystemService(AppOpsManager::class.java) ?: return RestrictedMessageAccess.UNKNOWN
    return restrictedMessageAccessForCheck {
        check(appOps, WRITE_SMS_OP, context.applicationInfo.uid, context.packageName)
    }
}

internal fun readAdvancedAccess(context: Context): Pair<RestrictedMessageAccess, RestrictedMessageAccess> =
    restrictedMessageAccess(context) to blockedSendersAccess(context)

internal fun readAdvancedFeatureState(context: Context): AdvancedFeatureState = AdvancedFeatureState(
    access = readAdvancedAccess(context),
    seen = context.getSharedPreferences(ADVANCED_FEATURES_PREFERENCES, Context.MODE_PRIVATE)
        .getBoolean(ADVANCED_FEATURES_SEEN, false),
)

internal fun markAdvancedFeaturesSeen(context: Context) {
    context.getSharedPreferences(ADVANCED_FEATURES_PREFERENCES, Context.MODE_PRIVATE)
        .edit().putBoolean(ADVANCED_FEATURES_SEEN, true).apply()
}

internal fun showNewFeatureHint(
    restricted: RestrictedMessageAccess,
    blocked: RestrictedMessageAccess,
    seen: Boolean,
): Boolean = restricted == RestrictedMessageAccess.ALLOWED && blocked == RestrictedMessageAccess.NOT_ALLOWED && !seen
