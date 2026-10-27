package dev.peppy.mobile

import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ContactSyncPreferencesTest {
    @Test fun osReadAccessNeverPromotesUnsetConsent() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val preferences = ContactSyncPreferences(context)

        assertEquals(ContactSyncIntent.UNSET, preferences.intent)
        assertEquals(ContactSyncIntent.UNSET, preferences.intent)

        preferences.enabled = false
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        preferences.readPermissionRequested = true
        assertEquals(true, preferences.readPermissionRequested)
        preferences.writePermissionRequested = true
        assertEquals(true, preferences.writePermissionRequested)
        assertEquals(false, preferences.readAccessGranted)
        preferences.readAccessGranted = true
        assertEquals(true, preferences.readAccessGranted)
        preferences.enabled = true
        assertEquals(ContactSyncIntent.ON, preferences.intent)
        preferences.retirePending = true
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
    }

    @Test fun explicitOnRemainsDurableAcrossPermissionReconciliation() {
        val preferences = freshPreferences()
        preferences.enabled = true
        assertEquals(ContactSyncIntent.ON, preferences.intent)
    }

    @Test fun clearsLegacyDeferredStartWithoutEnabling() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE)
            .edit().clear().putBoolean("start-after-read-grant", true).commit()

        val preferences = ContactSyncPreferences(context)

        assertEquals(ContactSyncIntent.UNSET, preferences.intent)
        assertEquals(false, context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE)
            .contains("start-after-read-grant"))
    }

    @Test fun passiveStoppedGrantDoesNotPromote() {
        val preferences = freshPreferences()
        preferences.enabled = false

        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
    }

    @Test fun retirePendingStaysStopped() {
        val preferences = freshPreferences()
        preferences.enabled = false
        preferences.retirePending = true

        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
    }

    private fun freshPreferences(): ContactSyncPreferences {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        return ContactSyncPreferences(context)
    }
}
