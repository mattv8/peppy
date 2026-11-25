package dev.peppy.mobile.about

import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.compose.LocalActivity
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import dev.peppy.mobile.R
import dev.peppy.mobile.gateway.GatewayAccountDevice
import dev.peppy.mobile.gateway.GatewayAccountSettings
import dev.peppy.mobile.gateway.GatewayAccountState
import dev.peppy.mobile.gateway.GatewayStatus
import dev.peppy.mobile.testing.ComposeTestActivityRule
import dev.peppy.mobile.ui.LocalPageChromeHost
import dev.peppy.mobile.ui.PageChromeHost
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
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
    // Queue IO completions instead of resuming recomposition inline on the worker.
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)
    private lateinit var activity: ComponentActivity

    @Test fun accountAboutRowIsLastOnlineAndAvailableOffline() {
        renderAccount(account = account(), notices = null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        val rows = compose.onAllNodes(
            hasTestTag("account-settings-row")
                .or(hasTestTag("account-guides-row"))
                .or(hasTestTag("account-about-row")),
        ).fetchSemanticsNodes().map { it.config[SemanticsProperties.TestTag] }
        assertEquals(listOf("account-settings-row", "account-guides-row", "account-about-row"), rows)
    }

    @Test fun offlineAccountAlsoShowsAboutLast() {
        renderAccount(account = null, notices = null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-settings-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-guides-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-about-row").fetchSemanticsNode()
    }

    @Test fun navigatesThroughLicensesAndBackControlsReturnToAccount() {
        renderAccount(account(), FakeNotices())
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-licenses-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-row-BoringSSL-LICENSE").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-row-BoringSSL-LICENSE").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-back").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("licenses-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("licenses-back").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-back").performScrollTo().performClick()
        assertAccountRootShown()
    }

    @Test fun systemBackNavigatesFromLicenseToAccount() {
        val host = PageChromeHost()
        renderAccount(account(), FakeNotices(), chromeHost = host)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-licenses-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-row-BoringSSL-LICENSE").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-row-BoringSSL-LICENSE").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("license-body").fetchSemanticsNodes().isNotEmpty() }

        compose.runOnIdle { activity.onBackPressedDispatcher.onBackPressed() }
        compose.waitUntil { compose.onAllNodesWithTag("licenses-body").fetchSemanticsNodes().isNotEmpty() }
        compose.runOnIdle { activity.onBackPressedDispatcher.onBackPressed() }
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        compose.runOnIdle { activity.onBackPressedDispatcher.onBackPressed() }
        assertAccountRootShown(host)
    }

    private fun assertAccountRootShown(host: PageChromeHost? = null) {
        compose.waitUntil { compose.onAllNodesWithTag("account-body").fetchSemanticsNodes().isNotEmpty() }
        assertTrue(compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isEmpty())
        assertTrue(compose.onAllNodesWithTag("licenses-body").fetchSemanticsNodes().isEmpty())
        assertTrue(compose.onAllNodesWithTag("license-body").fetchSemanticsNodes().isEmpty())
        host?.let { assertNull(it.current) }
    }

    @Test fun licensesRowIsAbsentWhenLoaderReturnsNull() {
        renderAccount(account(), null)
        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        assertTrue(compose.onAllNodesWithTag("about-licenses-row").fetchSemanticsNodes().isEmpty())
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
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-back").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("account-offline-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("devices-loading").assert(SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf(androidx.compose.ui.text.AnnotatedString(activity.getString(R.string.peppy_devices_load_failed)))))
    }

    @Test fun aboutRendersBuildInfoAndExternalLinksOpenExactUris() {
        var opened = ""
        renderAbout(buildInfo = AboutBuildInfo("1.2.3", 42, "a".repeat(40), "true"), onOpenLink = { opened = it })
        compose.onNodeWithTag("about-version-name").fetchSemanticsNode()
        compose.onNodeWithTag("about-version-code").fetchSemanticsNode()
        compose.onNodeWithTag("about-source-commit").fetchSemanticsNode()
        compose.onNodeWithTag("about-source-modified").fetchSemanticsNode()
        compose.onNodeWithTag("about-title").assert(SemanticsMatcher.expectValue(SemanticsProperties.Heading, Unit))
        compose.onNodeWithTag("about-privacy-row").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(PRIVACY_URL, opened) }
        compose.onNodeWithTag("about-source-row").performScrollTo().performClick()
        compose.runOnIdle { assertEquals(SOURCE_URL, opened) }
        assertTrue(compose.onAllNodesWithTag("about-guides-row").fetchSemanticsNodes().isEmpty())
    }

    @Test fun privacyInfoButtonShowsScopeWithoutOpeningPolicy() {
        var opened = ""
        renderAbout(licensesAvailable = true, onOpenLink = { opened = it })
        compose.onNodeWithTag("info-about-privacy").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("info-about-privacy-dialog").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithText(activity.getString(R.string.peppy_privacy_policy_scope)).fetchSemanticsNode()
        compose.runOnIdle { assertEquals("", opened) }
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

    @Test fun backgroundLicenseCompletionResumesOnMainLooper() {
        // Gate both the real screen loader and a continuation probe in the same effect context.
        val entered = CountDownLatch(2)
        val release = CountDownLatch(1)
        val resumed = CountDownLatch(1)
        val resumedLooper = AtomicReference<Looper?>()
        val recomposedLooper = AtomicReference<Looper?>()
        val completed = mutableStateOf(false)
        val notices = object : ThirdPartyNotices by FakeNotices() {
            override fun notice(): String {
                entered.countDown()
                check(release.await(5, TimeUnit.SECONDS))
                return "notices"
            }

            override fun entries(): List<ThirdPartyNoticeEntry> {
                // LicensesScreen asks for entries only after its IO-backed notice has loaded.
                recomposedLooper.set(Looper.myLooper())
                return listOf(ThirdPartyNoticeEntry("MIT.txt", null))
            }
        }
        try {
            compose.setContent {
                LaunchedEffect(Unit) {
                    withContext(Dispatchers.IO) { notices.notice() }
                    resumedLooper.set(Looper.myLooper())
                    completed.value = true
                    resumed.countDown()
                }
                if (completed.value) {
                    assertSame("recomposition looper", Looper.getMainLooper(), Looper.myLooper())
                }
                MaterialTheme { LicensesScreen(notices, {}, {}) }
            }
            compose.waitUntil(5_000) { entered.count == 0L }
            compose.onNodeWithTag("licenses-loading").fetchSemanticsNode()
            compose.mainClock.autoAdvance = false
            release.countDown()
            compose.waitUntil(5_000) {
                compose.mainClock.advanceTimeByFrame()
                resumed.count == 0L
            }
            assertSame("effect continuation looper", Looper.getMainLooper(), resumedLooper.get())
            compose.mainClock.autoAdvance = true
            compose.waitUntil {
                compose.onAllNodesWithTag("license-row-MIT").fetchSemanticsNodes().isNotEmpty()
            }
            assertSame("license loader recomposition looper", Looper.getMainLooper(), recomposedLooper.get())
            compose.onNodeWithTag("licenses-notice").assert(
                SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf(androidx.compose.ui.text.AnnotatedString("notices"))),
            )
        } finally {
            release.countDown()
            compose.mainClock.autoAdvance = true
        }
    }

    @Test fun licenseTextRepeatsTitleAsHeading() {
        val notices = FakeNotices()
        compose.setContent { MaterialTheme { LicenseTextScreen(notices, notices.entries().first(), {}) } }
        compose.waitUntil { compose.onAllNodesWithTag("license-text").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("license-name").assert(SemanticsMatcher.expectValue(SemanticsProperties.Heading, Unit))
    }

    private fun renderAbout(
        buildInfo: AboutBuildInfo = AboutBuildInfo("1.2.3", 42, "a".repeat(40), "false"),
        licensesAvailable: Boolean = false,
        onOpenLink: (String) -> Unit = {},
    ) {
        compose.setContent {
            MaterialTheme {
                activity = requireNotNull(LocalActivity.current) as ComponentActivity
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    AboutScreen(buildInfo, {}, onOpenLink, {}, licensesAvailable)
                }
            }
        }
    }

    private fun renderAccount(account: GatewayAccountState?, notices: ThirdPartyNotices?, chromeHost: PageChromeHost? = null) {
        compose.setContent {
            MaterialTheme {
                activity = requireNotNull(LocalActivity.current) as ComponentActivity
                CompositionLocalProvider(LocalPageChromeHost provides chromeHost) {
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
