package dev.peppy.mobile.gateway

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performSemanticsAction
import dev.peppy.mobile.BuildConfig
import dev.peppy.mobile.LoopbackHttpServer
import dev.peppy.mobile.attachments.AttachmentStorageCleanup
import dev.peppy.mobile.attachments.AttachmentStorageLoad
import dev.peppy.mobile.attachments.AttachmentStorageSnapshot
import dev.peppy.mobile.attachments.AttachmentStorageUnavailableReason
import dev.peppy.mobile.security.CredentialParser
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayAccountSettingsShellTest : GatewayTestBase() {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun ownerHostedRootShowsDevicesPlanAndAppGroups() {
        renderRoot(origin = HOSTED_ORIGIN, account = ownerAccount(), canAddPhone = true, canConnectComputer = true)

        compose.waitUntil { compose.onAllNodesWithTag("account-devices-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-section").fetchSemanticsNode()
        compose.onNodeWithTag("add-phone-row").fetchSemanticsNode()
        compose.onNodeWithTag("connect-computer-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-plan-group").fetchSemanticsNode()
        val planRow = if (BuildConfig.PEPPY_DISTRIBUTION == "play") "account-subscription-row" else "account-hosted-row"
        compose.onNodeWithTag(planRow).fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-app-group").fetchSemanticsNode()
        compose.onNodeWithTag("account-settings-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-guides-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-about-row").fetchSemanticsNode()
        compose.onNodeWithTag("account-privacy-policy-row").assertDoesNotExist()
    }

    @Test fun readOnlyHostedRootShowsPlanAttentionAndNoRootCallout() {
        val readOnly = MutableStateFlow(true)
        val planRow = hostedPlanRowTag()
        renderRoot(origin = HOSTED_ORIGIN, account = ownerAccount(), canAddPhone = false, canConnectComputer = false, readOnlyState = readOnly)

        compose.waitUntil { compose.onAllNodesWithTag(planRow).fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("$planRow-attention", useUnmergedTree = true).fetchSemanticsNode()
        compose.onAllNodesWithText(ACTION_NEEDED, useUnmergedTree = true).fetchSemanticsNodes().let { assertEquals(1, it.size) }
        assertEquals(0, compose.onAllNodesWithTag("account-read-only").fetchSemanticsNodes().size)
        assertEquals(0, compose.onAllNodesWithText(READ_ONLY_CALLOUT).fetchSemanticsNodes().size)

        compose.runOnIdle { readOnly.value = false }
        compose.waitUntil { compose.onAllNodesWithText(ACTION_NEEDED, useUnmergedTree = true).fetchSemanticsNodes().isEmpty() }
        assertEquals(0, compose.onAllNodesWithTag("$planRow-attention", useUnmergedTree = true).fetchSemanticsNodes().size)
    }

    @Test fun gatewaySelfHostedRootHasNoOwnerRows() {
        renderRoot(origin = TEST_ORIGIN, account = gatewayAccount(), canAddPhone = true, canConnectComputer = false)

        compose.waitUntil { compose.onAllNodesWithTag("account-devices-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("add-phone-row").assertDoesNotExist()
        compose.onNodeWithTag("connect-computer-row").assertDoesNotExist()
        compose.onNodeWithTag("account-plan-group").assertDoesNotExist()
        compose.onNodeWithTag("account-app-group").fetchSemanticsNode()
    }

    @Test fun devicesRouteOpensItsPageBackReturnsToRootAndMakesNoRequests() {
        val requests = AtomicInteger()
        LoopbackHttpServer {
            requests.incrementAndGet()
            LoopbackHttpServer.Response(500)
        }.use { server ->
            enrollLoopback(server.origin)
            var phase by mutableIntStateOf(0)
            compose.setContent {
                MaterialTheme {
                    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                        key(phase) {
                            val account = if (phase == 1) null else shellAccount()
                            GatewayAccountSettings(
                                onChanged = {},
                                attachmentStorageOriginSupported = { true },
                                accountLoader = { account },
                                gatewayStatusLoader = { shellStatus(server.origin) },
                            )
                        }
                    }
                }
            }

            compose.waitUntil { compose.onAllNodesWithTag("account-devices-row").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithTag("account-devices-row").performScrollTo().performClick()
            compose.waitUntil { compose.onAllNodesWithTag("devices-body").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithTag("devices-back").performScrollTo().performClick()
            compose.waitUntil { compose.onAllNodesWithTag("account-body").fetchSemanticsNodes().isNotEmpty() }
            compose.runOnIdle { phase = 1 }
            compose.waitUntil { compose.onAllNodesWithTag("devices-loading").fetchSemanticsNodes().isNotEmpty() }
            assertEquals(0, requests.get())
        }
    }

    @Test fun aboutRouteOpensItsPageAndBackReturnsToRoot() {
        renderRoot(origin = TEST_ORIGIN, account = gatewayAccount(), canAddPhone = false, canConnectComputer = false)

        compose.waitUntil { compose.onAllNodesWithTag("account-about-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("account-about-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("about-body").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("about-back").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("account-body").fetchSemanticsNodes().isNotEmpty() }
    }

    @Test fun openingHostedStorageUsesOnlyTheStatusRequest() {
        val statusRequests = AtomicInteger()
        val unexpectedRequest = AtomicReference<String?>(null)
        LoopbackHttpServer { request ->
            when (request.target) {
                "/v1/attachments/storage/status" -> {
                    statusRequests.incrementAndGet()
                    LoopbackHttpServer.Response(200, statusBody())
                }
                else -> {
                    unexpectedRequest.set("${request.method} ${request.target}")
                    LoopbackHttpServer.Response(500)
                }
            }
        }.use { server ->
            enrollLoopback(server.origin)
            renderShell(server.origin)

            compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithTag("attachment-storage-row").performScrollTo().performClick()
            compose.waitUntil(10_000) { statusRequests.get() == 1 }

            assertEquals(null, unexpectedRequest.get())
            assertEquals(1, statusRequests.get())
        }
    }

    @Test fun blockedNearFullLearnMoreOpensInfoWithoutAnotherStorageRequest() {
        val statusRequests = AtomicInteger()
        val unexpectedRequest = AtomicReference<String?>(null)
        LoopbackHttpServer { request ->
            when (request.target) {
                "/v1/attachments/storage/status" -> {
                    statusRequests.incrementAndGet()
                    LoopbackHttpServer.Response(200, nearFullBlockedStatusBody())
                }
                else -> {
                    unexpectedRequest.set("${request.method} ${request.target}")
                    LoopbackHttpServer.Response(500)
                }
            }
        }.use { server ->
            enrollLoopback(server.origin)
            renderShell(server.origin)

            compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithTag("attachment-storage-row").performScrollTo().performClick()
            compose.waitUntil(10_000) { statusRequests.get() == 1 }
            compose.onNodeWithTag("attachment-storage-capacity-notice").fetchSemanticsNode()
            compose.onNodeWithTag("attachment-storage-cause").fetchSemanticsNode()
            compose.onNodeWithTag("attachment-storage-capacity-notice-action").performScrollTo().performClick()
            compose.onNodeWithTag("attachment-storage-info-dialog").fetchSemanticsNode()

            assertEquals(null, unexpectedRequest.get())
            assertEquals(1, statusRequests.get())
        }
    }

    @Test fun sameScopeFailedRefreshKeepsStaleValues() {
        val loadRequests = AtomicInteger()
        val storageScope = enrolledScope()
        renderShell(
            TEST_ORIGIN,
            attachmentStorageScopeLoader = { storageScope },
            attachmentStorageLoader = { _, _ ->
                if (loadRequests.incrementAndGet() == 1) availableLoad()
                else AttachmentStorageLoad.TemporarilyUnavailable
            },
        )
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("attachment-storage-row").performScrollTo().performClick()
        compose.waitUntil(10_000) { loadRequests.get() == 1 }
        compose.onNodeWithTag("attachment-storage-values").fetchSemanticsNode()
        compose.runOnIdle {}

        compose.onNodeWithTag("attachment-storage-refresh").assertIsEnabled().performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil(10_000) { loadRequests.get() == 2 }
        compose.onNodeWithTag("attachment-storage-stale-label").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-stale-values").fetchSemanticsNode()
    }

    @Test fun refreshWithNoCurrentStorageScopeShowsRecoverableUnavailableStateWithoutHttp() {
        val loadRequests = AtomicInteger()
        renderShell(
            TEST_ORIGIN,
            attachmentStorageScopeLoader = { null },
            attachmentStorageLoader = { _, _ -> loadRequests.incrementAndGet(); AttachmentStorageLoad.TemporarilyUnavailable },
        )
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }

        compose.onNodeWithTag("attachment-storage-row").performScrollTo().performClick()
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-unavailable").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("attachment-storage-refresh").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("attachment-storage-unavailable").fetchSemanticsNode()
        assertEquals(0, loadRequests.get())
    }

    private fun hostedPlanRowTag() = if (BuildConfig.PEPPY_DISTRIBUTION == "play") "account-subscription-row" else "account-hosted-row"

    private fun renderRoot(
        origin: String,
        account: GatewayAccountState,
        canAddPhone: Boolean,
        canConnectComputer: Boolean,
        readOnlyState: StateFlow<Boolean> = MutableStateFlow(false),
    ) {
        compose.setContent {
            MaterialTheme {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    GatewayAccountSettings(
                        onChanged = {},
                        canAddPhone = canAddPhone,
                        canConnectComputer = canConnectComputer,
                        accountLoader = { account },
                        gatewayStatusLoader = { shellStatus(origin) },
                        readOnlyState = readOnlyState,
                    )
                }
            }
        }
    }

    private fun renderShell(
        origin: String,
        attachmentStorageScopeLoader: (android.content.Context) -> GatewayAccountHost.AttachmentStorageScope? = GatewayAccountHost::attachmentStorageScope,
        attachmentStorageLoader: (android.content.Context, GatewayAccountHost.AttachmentStorageScope) -> AttachmentStorageLoad? = GatewayAccountHost::loadAttachmentStorage,
    ) {
        compose.setContent {
            MaterialTheme {
                Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
                    GatewayAccountSettings(
                        onChanged = {},
                        attachmentStorageOriginSupported = { true },
                        accountLoader = { shellAccount() },
                        gatewayStatusLoader = { shellStatus(origin) },
                        attachmentStorageScopeLoader = attachmentStorageScopeLoader,
                        attachmentStorageLoader = attachmentStorageLoader,
                    )
                }
            }
        }
    }

    private fun enrolledScope(): GatewayAccountHost.AttachmentStorageScope {
        enrollLoopback(TEST_ORIGIN)
        return checkNotNull(GatewayAccountHost.attachmentStorageScope(context))
    }

    private fun availableLoad() = AttachmentStorageLoad.Available(
        AttachmentStorageSnapshot(
            usedBytes = 12_345,
            limitBytes = 23_456,
            baseBytes = 10_000,
            cleanup = AttachmentStorageCleanup.BLOCKED,
            unavailableReason = AttachmentStorageUnavailableReason.DEVICE_NOT_READY,
        ),
    )

    private fun ownerAccount() = GatewayAccountState(GatewayAccountDevice(deviceId, "owner", false, 1), emptyList())

    private fun gatewayAccount() = GatewayAccountState(GatewayAccountDevice(deviceId, "gateway", false, 1), emptyList())

    private fun shellAccount() = gatewayAccount()

    private fun shellStatus(origin: String) = GatewayStatus(true, origin, false, false)

    private fun enrollLoopback(origin: String) {
        val credential = checkNotNull(CredentialParser.parse(credentialJson(origin, vaultId, deviceId).toByteArray(), allowDebugLoopback = true))
        assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credential, vaultMaterial()))
        assertNotNull(GatewayAccountHost.attachmentStorageScope(context))
    }

    private fun statusBody() = """{"used_bytes":12345,"limit_bytes":23456,"base_bytes":10000,"pack_bytes":2345,"protected_bytes":8765,"removed_bytes":1234,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready"}"""

    private fun nearFullBlockedStatusBody() = """{"used_bytes":90,"limit_bytes":100,"base_bytes":50,"pack_bytes":50,"protected_bytes":0,"removed_bytes":0,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready"}"""

    private companion object {
        const val HOSTED_ORIGIN = "https://peppy.pro"
        const val ACTION_NEEDED = "Action needed"
        const val READ_ONLY_CALLOUT = "Paused: plan is read-only"
    }
}
