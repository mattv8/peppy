package dev.peppy.mobile

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.unit.Density
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PermissionDisclosureControlsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun productionPermissionControlsKeepDisclosuresBeforeActionsAtNormalFont() {
        assertDisclosuresPrecedeActions(fontScale = 1f)
    }

    @Test
    fun productionPermissionControlsKeepDisclosuresBeforeActionsAtLargeFont() {
        assertDisclosuresPrecedeActions(fontScale = 1.3f)
    }

    private fun assertDisclosuresPrecedeActions(fontScale: Float) {
        var smsRequests = 0
        var mmsRequests = 0
        var notificationAccessRequests = 0

        compose.setContent {
            val density = LocalDensity.current
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    SmsPermissionsSection { smsRequests += 1 }
                    MmsCaptureControls(
                        enabled = false,
                        receiveGranted = false,
                        onEnabledChanged = {},
                        onRequestPermissions = { mmsRequests += 1 },
                    )
                    NotificationMirroringAccessControls(
                        accessGranted = false,
                        enabled = false,
                        onRequestAccess = { notificationAccessRequests += 1 },
                        onEnabledChanged = {},
                    )
                }
            }
        }

        val smsDisclosure = compose.onNodeWithTag("sms-permissions-disclosure")
        val mmsDisclosure = compose.onNodeWithTag("mms-capture-disclosure")
        val notificationDisclosure = compose.onNodeWithTag("notification-mirroring-disclosure")
        smsDisclosure.performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(compose.activity.getString(R.string.peppy_permissions_sms_disclosure)).assertIsDisplayed()
        mmsDisclosure.performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(compose.activity.getString(R.string.peppy_mms_capture_disclosure)).assertIsDisplayed()
        notificationDisclosure.performScrollTo().assertIsDisplayed()
        compose.onNodeWithText(compose.activity.getString(R.string.peppy_notification_mirroring_disclosure)).assertIsDisplayed()

        val smsAction = compose.onNodeWithTag("permissions-button")
        val mmsAction = compose.onNodeWithTag("mms-permission-button")
        val mmsEnable = compose.onNodeWithTag("mms-enable-switch")
        val notificationAction = compose.onNodeWithTag("notification-access-button")
        val notificationEnable = compose.onNodeWithTag("notification-mirroring-master-switch")
        smsDisclosure.performScrollTo()
        smsAction.performScrollTo()
        assertTrue(smsDisclosure.fetchSemanticsNode().boundsInRoot.top < smsAction.fetchSemanticsNode().boundsInRoot.top)
        mmsDisclosure.performScrollTo()
        mmsAction.performScrollTo()
        assertTrue(mmsDisclosure.fetchSemanticsNode().boundsInRoot.top < mmsAction.fetchSemanticsNode().boundsInRoot.top)
        mmsDisclosure.performScrollTo()
        mmsEnable.performScrollTo()
        assertTrue(mmsDisclosure.fetchSemanticsNode().boundsInRoot.top < mmsEnable.fetchSemanticsNode().boundsInRoot.top)
        notificationDisclosure.performScrollTo()
        notificationAction.performScrollTo()
        assertTrue(notificationDisclosure.fetchSemanticsNode().boundsInRoot.top < notificationAction.fetchSemanticsNode().boundsInRoot.top)
        notificationDisclosure.performScrollTo()
        notificationEnable.performScrollTo()
        assertTrue(notificationDisclosure.fetchSemanticsNode().boundsInRoot.top < notificationEnable.fetchSemanticsNode().boundsInRoot.top)

        smsAction.performScrollTo()
        smsAction.performClick()
        mmsAction.performScrollTo()
        mmsAction.performClick()
        notificationAction.performScrollTo()
        notificationAction.performClick()
        assertEquals(1, smsRequests)
        assertEquals(1, mmsRequests)
        assertEquals(1, notificationAccessRequests)
    }
}
