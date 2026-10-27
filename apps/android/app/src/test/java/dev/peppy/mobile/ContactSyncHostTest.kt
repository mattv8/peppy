package dev.peppy.mobile

import android.Manifest
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ContactSyncHostTest {
    @Test fun grantedReadForegroundCallbackDoesNotPromoteUnsetConsent() {
        val context = ApplicationProvider.getApplicationContext<android.app.Application>()
        context.getSharedPreferences("peppy-contact-sync", Context.MODE_PRIVATE).edit().clear().commit()
        shadowOf(context).grantPermissions(Manifest.permission.READ_CONTACTS)

        val resumed = ContactSyncHost.onPermissionOrForeground(context)

        assertFalse(resumed)
        assertEquals(ContactSyncIntent.UNSET, ContactSyncPreferences(context).intent)
        shadowOf(context).denyPermissions(Manifest.permission.READ_CONTACTS)
    }
}
