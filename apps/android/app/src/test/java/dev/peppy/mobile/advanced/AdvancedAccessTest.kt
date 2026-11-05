package dev.peppy.mobile.advanced

import android.app.AppOpsManager
import dev.peppy.mobile.mms.RestrictedMessageAccess
import dev.peppy.mobile.mms.restrictedMessageAccessForCheck
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AdvancedAccessTest {
    @Test fun `blocked senders uses write sms app op mapping`() {
        assertEquals("android:write_sms", WRITE_SMS_OP)
        assertEquals(RestrictedMessageAccess.ALLOWED, restrictedMessageAccessForCheck { AppOpsManager.MODE_ALLOWED })
    }

    @Test fun `new feature hint requires unseen blocked-senders access`() {
        assertTrue(showNewFeatureHint(RestrictedMessageAccess.ALLOWED, RestrictedMessageAccess.NOT_ALLOWED, false))
        assertFalse(showNewFeatureHint(RestrictedMessageAccess.ALLOWED, RestrictedMessageAccess.NOT_ALLOWED, true))
        assertFalse(showNewFeatureHint(RestrictedMessageAccess.NOT_ALLOWED, RestrictedMessageAccess.NOT_ALLOWED, false))
    }
}
