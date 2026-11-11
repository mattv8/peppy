package dev.peppy.mobile.about

import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import dev.peppy.mobile.gateway.GatewayAccountDevice
import dev.peppy.mobile.gateway.GatewayAccountSettings
import dev.peppy.mobile.gateway.GatewayAccountState
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.testing.ComposeTestActivityRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AboutComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private lateinit var activity: ComponentActivity

    @Test fun accountAboutRowIsLastOnlineAndAvailableOffline() {
        renderAccount(account = account(), notices = null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        val rows = compose.onAllNodes(
            hasTestTag("account-privacy-policy-row").or(hasTestTag("account-about-row")),
        ).fetchSemanticsNodes().map { it.config[SemanticsProperties.TestTag] }
        assertEquals(listOf("account-privacy-policy-row", "account-about-row"), rows)
    }

    @Test fun offlineAccountAlsoShowsAboutLast() {
        renderAccount(account = null, notices = null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-privacy-policy-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-about-row").fetchSemanticsNode()
    }

    @Test fun navigatesThroughLicensesAndBackControlsReturnToAccount() {
        renderAccount(account(), FakeNotices())
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-screen").fetchSemanticsNodes().isNotEmpty() }
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-licenses-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-row-BoringSSL-LICENSE").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-row-BoringSSL-LICENSE").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-screen").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-back-button").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("licenses-screen").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("licenses-back-button").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-screen").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-back-button").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("account-tab-screen").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun systemBackNavigatesFromLicenseToAccount() {
        renderAccount(account(), FakeNotices())
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-licenses-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-row-BoringSSL-LICENSE").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-row-BoringSSL-LICENSE").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-screen").fetchSemanticsNodes().isNotEmpty() }

        repeat(3) { compose.runOnIdle { activity.onBackPressedDispatcher.onBackPressed() } }

        compose.waitUntil { compose.onAllNodesWithTag("account-tab-screen").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun licensesRowIsAbsentWhenLoaderReturnsNull() {
        renderAccount(account(), null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-screen").fetchSemanticsNodes().isNotEmpty() }
        assertTrue(compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isEmpty())
    }

    @Test fun licensesRowIsPresentWhenLoaderReturnsNotices() {
        renderAccount(account(), FakeNotices())
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun licensesRowAndListShowLinkOnlyNoticesWithoutHeader() {
        renderAccount(account(), LinkOnlyNotices())
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-licenses-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-row-NOTICE-google-messages").fetchSemanticsNodes().isNotEmpty() }
        assertTrue(compose.onAllNodesWithTag("licenses-notice").fetchSemanticsNodes().isEmpty())
    }

    @Test fun offlineFailureSurvivesOpeningAndClosingAbout() {
        renderAccount(account = null, notices = null)
        compose.waitUntil { compose.onAllNodesWithTag("devices-loading").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-screen").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-back-button").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("account-offline-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("devices-loading").assert(SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf(androidx.compose.ui.text.AnnotatedString("Could not load devices. Unlock or check the server connection."))))
    }

    @Test fun aboutRendersBuildInfoAndExternalLinksOpenExactUris() {
        var opened = ""
        compose.setContent {
            MaterialTheme {
                activity = requireNotNull(LocalActivity.current) as ComponentActivity
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    AboutScreen(AboutBuildInfo("1.2.3", 42, "a".repeat(40), "true"), {}, { opened = it }, {}, false)
                }
            }
        }
        compose.onNodeWithTag("about-version-name").fetchSemanticsNode()
        compose.onNodeWithTag("about-version-code").fetchSemanticsNode()
        compose.onNodeWithTag("about-source-commit").fetchSemanticsNode()
        compose.onNodeWithTag("about-source-modified").fetchSemanticsNode()
        compose.onNodeWithTag("about-title").assert(SemanticsMatcher.expectValue(SemanticsProperties.Heading, Unit))
        compose.onNodeWithTag("about-privacy-row").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(PRIVACY_URL, opened) }
        compose.onNodeWithTag("about-source-row").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(SOURCE_URL, opened) }
    }

    @Test fun licenseListShowsEveryFileAndLicenseTextHandlesContentAndFailure() {
        val notices = FakeNotices()
        var phase by mutableIntStateOf(0)
        compose.setContent { MaterialTheme { when (phase) {
            0 -> LicensesScreen(notices, {}, {})
            1 -> LicenseTextScreen(notices, notices.entries().first(), {})
            else -> LicenseTextScreen(FailingNotices, notices.entries().first(), {})
        } } }
        compose.waitUntil { compose.onAllNodesWithTag("license-row-BoringSSL-LICENSE").fetchSemanticsNodes().isNotEmpty() }
        notices.entries().forEach { entry -> compose.onNodeWithTag("license-row-${entry.fileName.substringBeforeLast('.')}").fetchSemanticsNode() }

        compose.runOnIdle { phase = 1 }
        compose.waitUntil { compose.onAllNodesWithTag("license-text").fetchSemanticsNodes().isNotEmpty() }
        compose.runOnIdle { phase = 2 }
        compose.waitUntil { compose.onAllNodesWithTag("license-error").fetchSemanticsNodes().isNotEmpty() }
    }

    private fun renderAccount(account: GatewayAccountState?, notices: ThirdPartyNotices?) {
        compose.setContent {
            MaterialTheme {
                activity = requireNotNull(LocalActivity.current) as ComponentActivity
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    GatewayAccountSettings(
                        onChanged = {},
                        accountLoader = { account },
                        gatewayStatusLoader = { GatewayStatus(true, "https://vault.example", false, false) },
                        thirdPartyNoticesLoader = { notices },
                    )
                }
            }
        }
    }

    private fun account() = GatewayAccountState(GatewayAccountDevice("device", "gateway", false, 1), emptyList())
}

private class FakeNotices : ThirdPartyNotices {
    private val files = listOf("NOTICE.txt", "LGPL-3.0.txt", "LGPL-2.1.txt", "GPL-3.0.txt", "Apache-2.0.txt", "BSD-3-Clause.txt", "MIT.txt", "BouncyCastle-LICENSE.txt", "Conscrypt-NOTICE.txt", "BoringSSL-LICENSE.txt")
    override fun notice() = "notices"
    override fun entries() = files.filterNot { it == "NOTICE.txt" }.map { ThirdPartyNoticeEntry(it, null) }
    override fun read(fileName: String) = "text for $fileName"
}

private class LinkOnlyNotices : ThirdPartyNotices {
    private val files = listOf("AGPL-3.0.txt", "LICENSE-exceptions-google-messages.txt", "NOTICE-google-messages.txt", "MPL-2.0.txt")
    override fun notice() = ""
    override fun entries() = files.map { ThirdPartyNoticeEntry(it, null) }
    override fun read(fileName: String) = "text for $fileName"
}

private object FailingNotices : ThirdPartyNotices {
    override fun notice(): String = error("unavailable")
    override fun entries(): List<ThirdPartyNoticeEntry> = emptyList()
    override fun read(fileName: String): String = error("unavailable")
}
