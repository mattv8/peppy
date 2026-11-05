package dev.peppy.mobile.restrictedsetup

import android.content.Context
import android.provider.Settings
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal class WirelessDebuggingSettingReadback(context: Context) : RestrictedSetupWirelessReadback {
    private val resolver = context.contentResolver

    override suspend fun isOn(): Boolean? = withContext(Dispatchers.IO) {
        try {
            when (Settings.Global.getString(resolver, WIRELESS_DEBUGGING_SETTING)) {
                "0" -> false
                "1" -> true
                else -> null
            }
        } catch (_: Exception) {
            null
        }
    }

    private companion object {
        const val WIRELESS_DEBUGGING_SETTING = "adb_wifi_enabled"
    }
}
