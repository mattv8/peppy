package dev.peppy.mobile

import android.Manifest
import android.content.ContextWrapper
import android.content.pm.PackageManager
import androidx.activity.ComponentActivity
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.remember
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.isEnabled
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.waitUntilExactlyOneExists
import androidx.compose.ui.res.stringResource
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.peppy.mobile.ui.ConsentSheet
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
@OptIn(androidx.compose.ui.test.ExperimentalTestApi::class)
class PhoneReadSettingsTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()

    @Test
    fun consentCanBeDismissedThenAppliedOnlyAfterANewExplicitEnable() {
        var applied = 0
        compose.setContent {
            var consent by remember { mutableStateOf(ConsentGatedToggle()) }
            PhoneReadSettingsContent(
                presentation = PhoneReadPresentation(PhoneReadPresentation.State.OFF_READY, true),
                checked = false,
                subtitle = "Ready",
                onCheckedChange = { consent = consent.requestEnable() },
                onOpenAccess = {},
                onRetry = {},
            )
            if (consent.phase == ConsentPhase.DISCLOSURE) ConsentSheet(
                title = stringResource(R.string.peppy_toggle_android_phone_read_consent_title),
                body = stringResource(R.string.peppy_phone_read_disclosure),
                tag = "phone-read-consent-sheet",
                disclosureTag = "phone-read-disclosure",
                confirmLabel = stringResource(R.string.peppy_toggle_android_agree),
                onDismiss = { consent = consent.cancel() },
                onConfirm = {
                    val transition = consent.agree(hasAccess = true)
                    consent = transition.state
                    if (transition.effect == ConsentEffect.APPLY) applied++
                },
            )
        }

        compose.onNodeWithTag("phone-read-row").performClick()
        compose.onNodeWithTag("phone-read-consent-sheet").assertIsDisplayed()
        compose.onNodeWithTag("phone-read-consent-sheet-dismiss").performClick()
        assertEquals(0, applied)
        compose.onNodeWithTag("phone-read-row").performClick()
        compose.onNodeWithTag("phone-read-consent-sheet-confirm").performClick()
        assertEquals(1, applied)
    }

    @Test
    fun agreeingWritesTheExistingSettingWithoutNotificationAccess() {
        val store = FakePhoneReadSettingsStore(enabled = false, smsSyncEnabled = true)
        val smsReadGrantedContext = object : ContextWrapper(compose.activity) {
            override fun checkPermission(permission: String, pid: Int, uid: Int): Int =
                if (permission == Manifest.permission.READ_SMS) PackageManager.PERMISSION_GRANTED
                else super.checkPermission(permission, pid, uid)
        }
        compose.setContent {
            PhoneReadSettings(
                context = smsReadGrantedContext,
                refresh = 0,
                onChanged = {},
                store = store,
                notificationAccessGranted = { false },
            )
        }

        compose.waitUntilExactlyOneExists(hasTestTag("phone-read-row") and isEnabled(), TIMEOUT_MS)
        compose.onNodeWithTag("phone-read-row").performClick()
        compose.waitUntilExactlyOneExists(hasTestTag("phone-read-consent-sheet-policy"), TIMEOUT_MS)
        compose.onNodeWithTag("phone-read-consent-sheet-policy").performScrollTo().assertIsDisplayed()
        compose.onNodeWithTag("phone-read-consent-sheet-confirm").performClick()
        compose.waitUntil { store.writes == listOf(true) }

        assertEquals(true, store.enabled)
    }

    @Test
    fun smsReadPrerequisiteDisablesTheRow() {
        compose.setContent {
            PhoneReadSettingsContent(
                presentation = PhoneReadPresentation(PhoneReadPresentation.State.SMS_READ_REQUIRED, false),
                checked = false,
                subtitle = "Allow SMS read access to use this.",
                onCheckedChange = {},
                onOpenAccess = {},
                onRetry = {},
            )
        }
        compose.onNodeWithTag("phone-read-row").assertIsNotEnabled()
    }

    @Test
    fun accessLimitedKeepsInboundToggleEnabledAndOffersOptionalNotificationAccess() {
        compose.setContent {
            PhoneReadSettingsContent(
                presentation = PhoneReadPresentation(PhoneReadPresentation.State.ACCESS_LIMITED, true, showOpenAccess = true),
                checked = true,
                subtitle = "Phone reads sync to Peppy.",
                onCheckedChange = {},
                onOpenAccess = {},
                onRetry = {},
            )
        }

        compose.onNodeWithTag("phone-read-row").assertIsEnabled()
        compose.onNodeWithTag("phone-read-open-access").assertIsDisplayed()
    }

    @Test
    fun outboundUnsupportedKeepsInboundToggleEnabledWithoutNotificationAccessAction() {
        compose.setContent {
            PhoneReadSettingsContent(
                presentation = PhoneReadPresentation(PhoneReadPresentation.State.OUTBOUND_UNSUPPORTED, true),
                checked = true,
                subtitle = "Phone reads sync to Peppy.",
                onCheckedChange = {},
                onOpenAccess = {},
                onRetry = {},
            )
        }

        compose.onNodeWithTag("phone-read-row").assertIsEnabled()
        compose.onAllNodesWithTag("phone-read-open-access").assertCountEquals(0)
    }

    @Test
    fun errorStateExposesAnActionableRetry() {
        var retries = 0
        compose.setContent {
            PhoneReadSettingsContent(
                presentation = PhoneReadPresentation(PhoneReadPresentation.State.ERROR, false),
                checked = false,
                subtitle = "Could not update this setting. Try again.",
                onCheckedChange = {},
                onOpenAccess = {},
                onRetry = { retries++ },
            )
        }
        compose.onNodeWithTag("phone-read-retry").performClick()
        assertEquals(1, retries)
    }

    private companion object {
        const val TIMEOUT_MS = 10_000L
    }

    private class FakePhoneReadSettingsStore(
        var enabled: Boolean?,
        private val smsSyncEnabled: Boolean,
    ) : PhoneReadSettingsStore {
        var reads = 0
        val writes = mutableListOf<Boolean>()

        override fun read() = (enabled to smsSyncEnabled).also { reads++ }
        override fun write(value: Boolean): Boolean? {
            writes += value
            enabled = value
            return enabled
        }
    }
}
