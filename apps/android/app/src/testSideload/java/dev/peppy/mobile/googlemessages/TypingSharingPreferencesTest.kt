package dev.peppy.mobile.googlemessages

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import dev.peppy.mobile.googlemessages.live.LiveCore
import dev.peppy.mobile.googlemessages.live.LiveMembershipSnapshot
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeLiveTypingException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class TypingSharingPreferencesTest {
    @Test fun `off while disabled is reapplied before local sealing without suppressing contact display`() {
        val preferences = TypingSharingPreferences(ApplicationProvider.getApplicationContext())
        var enabled = false
        var sharingEnabled = true
        val apply: (Boolean) -> Unit = { value ->
            if (!enabled) throw NativeLiveTypingException.Disabled()
            sharingEnabled = value
        }

        assertTrue(preferences.update(false, apply))
        enabled = true
        val delegate = object : LiveCore {
            override fun enable(snapshot: LiveMembershipSnapshot) = Unit
            override fun disable() = Unit
        }
        TypingSharingLiveCore(delegate, preferences, apply).enable(
            LiveMembershipSnapshot("synthetic-connection", 1, emptyList()),
        )

        val localFrame = if (sharingEnabled) "sealed-local-frame" else null
        val contactDisplay = "google-contact-typing"
        assertNull(localFrame)
        assertTrue(contactDisplay.isNotEmpty())
    }

    @Test fun `missing client does not claim to persist a sharing change`() {
        val preferences = TypingSharingPreferences(ApplicationProvider.getApplicationContext())

        assertFalse(preferences.update(false, null))
        assertTrue(preferences.enabled())
    }
}
