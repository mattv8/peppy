package dev.peppy.mobile

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsEnableGuardTest {
    @Test
    fun freshReadGrantDisablesAndReadsBackPolicyBeforeLaunchingPermissionRequest() = runBlocking {
        val effects = mutableListOf<String>()
        val action = SmsCapturePolicyAction(
            writeAndRead = { enabled -> effects += "policy:$enabled"; !enabled },
            onActivated = { effects += "capture" },
        )

        assertTrue(action.prepareFreshReadGrant(readGranted = false) { effects += "request" })

        assertEquals(listOf("policy:false", "request"), effects)
    }

    @Test
    fun failedPolicyReadbackAbortsPermissionRequestAndLeavesPolicyOff() = runBlocking {
        var launched = false
        val effects = mutableListOf<String>()
        val action = SmsCapturePolicyAction(
            writeAndRead = { enabled -> effects += "policy:$enabled"; false },
            onActivated = {},
        )

        assertFalse(action.prepareFreshReadGrant(readGranted = false) { launched = true })

        assertFalse(launched)
        assertEquals(listOf("policy:false"), effects)
    }

    @Test
    fun confirmedGrantStartsCaptureOnlyAfterEnabledPolicyReadback() = runBlocking {
        val effects = mutableListOf<String>()
        val action = SmsCapturePolicyAction(
            writeAndRead = { enabled -> effects += "policy:$enabled"; enabled },
            onActivated = { effects += "capture-and-gateway" },
        )

        assertTrue(action.enableAfterGrant())

        assertEquals(listOf("policy:true", "capture-and-gateway"), effects)
    }

    @Test
    fun dismissedDisclosurePerformsNoPolicyOrPermissionEffect() {
        val dismissed = ConsentGatedToggle().requestEnable().cancel()

        assertEquals(ConsentPhase.IDLE, dismissed.phase)
    }
}
