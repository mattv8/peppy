package dev.peppy.mobile.contacts

import android.content.Context
import android.provider.Settings

/** Reinstall-stable Android identity supplied to the core without persisting a second copy. */
internal object AndroidDeviceHint {
    private const val KNOWN_BAD_EMULATOR_ID = "9774d56d682e549c"

    fun read(context: Context): ByteArray? = Settings.Secure
        .getString(context.contentResolver, Settings.Secure.ANDROID_ID)
        ?.takeIf { it.isNotBlank() && it != KNOWN_BAD_EMULATOR_ID }
        ?.toByteArray(Charsets.UTF_8)
}
