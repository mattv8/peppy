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
    @Test fun migratesLegacyValuesAndOnlyPromotesUnsetAfterReadAccess() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val preferences = ContactSyncPreferences(context)

        assertEquals(ContactSyncIntent.UNSET, preferences.intent)
        assertEquals(false, preferences.promoteGrantedUnset(readGranted = false))
        assertEquals(true, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(ContactSyncIntent.ON, preferences.intent)

        preferences.enabled = false
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        assertEquals(false, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        preferences.readPermissionRequested = true
        assertEquals(true, preferences.readPermissionRequested)
        preferences.startAfterReadGrant = true
        assertEquals(true, preferences.startAfterReadGrant)
        assertEquals(false, preferences.readAccessGranted)
        preferences.readAccessGranted = true
        assertEquals(true, preferences.readAccessGranted)
        preferences.enabled = true
        assertEquals(ContactSyncIntent.ON, preferences.intent)
        preferences.retirePending = true
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
    }

    @Test fun explicitStoppedStartDeniedThenSettingsGrantTurnsOnExactlyOnce() {
        val preferences = freshPreferences()
        preferences.enabled = false
        preferences.startAfterReadGrant = true

        assertEquals(false, preferences.promoteGrantedUnset(readGranted = false))
        assertEquals(true, preferences.startAfterReadGrant)
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)

        assertEquals(true, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(false, preferences.startAfterReadGrant)
        assertEquals(ContactSyncIntent.ON, preferences.intent)

        assertEquals(false, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(ContactSyncIntent.ON, preferences.intent)
    }

    @Test fun passiveStoppedGrantDoesNotPromote() {
        val preferences = freshPreferences()
        preferences.enabled = false

        assertEquals(false, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        assertEquals(false, preferences.startAfterReadGrant)
    }

    @Test fun retirePendingNeverResumesEvenWithStartFlag() {
        val preferences = freshPreferences()
        preferences.enabled = false
        preferences.startAfterReadGrant = true
        preferences.retirePending = true

        assertEquals(false, preferences.promoteGrantedUnset(readGranted = true))
        assertEquals(ContactSyncIntent.STOPPED, preferences.intent)
        assertEquals(true, preferences.startAfterReadGrant)
    }

    private fun freshPreferences(): ContactSyncPreferences {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-contact-sync", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        return ContactSyncPreferences(context)
    }
}
