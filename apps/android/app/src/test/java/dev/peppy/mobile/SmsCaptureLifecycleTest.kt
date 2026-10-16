package dev.peppy.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsCaptureLifecycleTest {
    @Test
    fun observerRegistrationRequiresOnlyReadPermissionNotACorePolicyLookup() {
        assertFalse(shouldObserveSmsProvider(false))
        assertTrue(shouldObserveSmsProvider(true))
    }

    @Test
    fun onlyDurableCapturesScheduleGatewayUpload() {
        assertFalse(shouldScheduleGatewayUpload(SmsCaptureSummary()))
        assertFalse(shouldScheduleGatewayUpload(SmsCaptureSummary(more = true, providerError = true)))
        assertTrue(shouldScheduleGatewayUpload(SmsCaptureSummary(captured = 1)))
    }
}
