package dev.peppy.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performSemanticsAction
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayAccountSettingsShellTest : GatewayTestBase() {
    @get:Rule val compose = createComposeRule()

    @Test fun hiddenAccountModesAndDevicesDetailNeverRequestStorage() {
        val requests = AtomicInteger()
        LoopbackHttpServer {
            requests.incrementAndGet()
            LoopbackHttpServer.Response(500)
        }.use { server ->
            enrollLoopback(server.origin)
            var phase by mutableIntStateOf(0)
            compose.setContent {
                MaterialTheme {
                    key(phase) {
                        val account = if (phase == 1) null else shellAccount()
                        GatewayAccountSettings(
                            onChanged = {},
                            settingsMode = phase == 0,
                            attachmentStorageOriginSupported = { true },
                            accountLoader = { account },
                            gatewayStatusLoader = { shellStatus(server.origin) },
                        )
                    }
                }
            }

            compose.waitUntil { compose.onAllNodesWithTag("settings-account-group").fetchSemanticsNodes().isNotEmpty() }
            assertEquals(0, compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().size)
            compose.runOnIdle { phase = 1 }
            compose.waitUntil { compose.onAllNodesWithTag("devices-loading").fetchSemanticsNodes().isNotEmpty() }
            assertEquals(0, compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().size)
            compose.runOnIdle { phase = 2 }
            compose.waitUntil { compose.onAllNodesWithTag("account-devices-row").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithTag("account-devices-row").performClick()
            compose.onNodeWithTag("devices-screen").fetchSemanticsNode()
            assertEquals(0, requests.get())
        }
    }

    @Test fun openingHostedStorageUsesProductionGetAndAcknowledgementOnce() {
        val statusRequests = AtomicInteger()
        val acknowledgementRequests = AtomicInteger()
        val unexpectedRequest = AtomicReference<String?>(null)
        LoopbackHttpServer { request ->
            when (request.target) {
                "/v1/attachments/storage/status" -> {
                    statusRequests.incrementAndGet()
                    LoopbackHttpServer.Response(200, notPresentedStatusBody())
                }
                "/v1/attachments/storage/notice/acknowledgement" -> {
                    acknowledgementRequests.incrementAndGet()
                    LoopbackHttpServer.Response(204)
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
            compose.onNodeWithTag("attachment-storage-row").performClick()
            compose.waitUntil(10_000) { statusRequests.get() >= 2 && acknowledgementRequests.get() == 1 }

            assertEquals(null, unexpectedRequest.get())
            assertEquals(2, statusRequests.get())
            assertEquals(1, acknowledgementRequests.get())
        }
    }

    @Test fun sameScopeFailedRefreshKeepsStaleValues() {
        val loadRequests = AtomicInteger()
        val storageScope = enrolledScope()
        renderShell(
            TEST_ORIGIN,
            attachmentStorageScopeLoader = { storageScope },
            attachmentStorageLoader = { _, _ ->
                if (loadRequests.incrementAndGet() == 1) availableLoad(AttachmentStorageNoticeState.ELIGIBLE)
                else AttachmentStorageLoad.TemporarilyUnavailable
            },
        )
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("attachment-storage-row").performClick()
        compose.waitUntil(10_000) { loadRequests.get() == 1 }
        compose.onNodeWithTag("attachment-storage-values").fetchSemanticsNode()
        compose.runOnIdle {}

        compose.onNodeWithTag("attachment-storage-refresh").assertIsEnabled().performSemanticsAction(SemanticsActions.OnClick)
        compose.waitUntil(10_000) { loadRequests.get() == 2 }
        compose.onNodeWithTag("attachment-storage-stale-label").fetchSemanticsNode()
        compose.onNodeWithTag("attachment-storage-stale-values").fetchSemanticsNode()
    }

    @Test fun acknowledgementFinishingAfterNavigationHasNoUiOrRefreshEffects() {
        val loadRequests = AtomicInteger()
        val acknowledgementRequests = AtomicInteger()
        val acknowledgementStarted = CountDownLatch(1)
        val releaseAcknowledgement = CountDownLatch(1)
        val acknowledgementCompleted = AtomicBoolean(false)
        val storageScope = enrolledScope()
        renderShell(
            TEST_ORIGIN,
            attachmentStorageScopeLoader = { storageScope },
            attachmentStorageLoader = { _, _ ->
                if (loadRequests.incrementAndGet() == 1) availableLoad(AttachmentStorageNoticeState.NOT_PRESENTED)
                else availableLoad(AttachmentStorageNoticeState.ELIGIBLE)
            },
            attachmentStorageAcknowledger = { _, _ ->
                acknowledgementRequests.incrementAndGet()
                acknowledgementStarted.countDown()
                assertTrue(releaseAcknowledgement.await(10, TimeUnit.SECONDS))
                acknowledgementCompleted.set(true)
                true
            },
        )
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("attachment-storage-row").performClick()
        compose.waitUntil(10_000) { acknowledgementStarted.count == 0L }
        compose.onNodeWithTag("attachment-storage-refresh").assertIsNotEnabled()

        compose.onNodeWithTag("attachment-storage-back").performClick()
        releaseAcknowledgement.countDown()
        compose.waitUntil(10_000) { acknowledgementCompleted.get() }
        compose.runOnIdle {}
        assertEquals(1, loadRequests.get())
        assertEquals(1, acknowledgementRequests.get())

        compose.onNodeWithTag("attachment-storage-row").performClick()
        compose.waitUntil(10_000) { loadRequests.get() == 2 }
        assertEquals(1, acknowledgementRequests.get())
    }

    @Test fun refreshWithNoCurrentStorageScopeShowsRecoverableUnavailableStateWithoutHttp() {
        val loadRequests = AtomicInteger()
        renderShell(
            TEST_ORIGIN,
            attachmentStorageScopeLoader = { null },
            attachmentStorageLoader = { _, _ -> loadRequests.incrementAndGet(); AttachmentStorageLoad.TemporarilyUnavailable },
        )
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-row").fetchSemanticsNodes().isNotEmpty() }

        compose.onNodeWithTag("attachment-storage-row").performClick()
        compose.waitUntil { compose.onAllNodesWithTag("attachment-storage-unavailable").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithTag("attachment-storage-refresh").performSemanticsAction(SemanticsActions.OnClick)
        compose.onNodeWithTag("attachment-storage-unavailable").fetchSemanticsNode()
        assertEquals(0, loadRequests.get())
    }

    private fun renderShell(
        origin: String,
        attachmentStorageScopeLoader: (android.content.Context) -> GatewayAccountHost.AttachmentStorageScope? = GatewayAccountHost::attachmentStorageScope,
        attachmentStorageLoader: (android.content.Context, GatewayAccountHost.AttachmentStorageScope) -> AttachmentStorageLoad? = GatewayAccountHost::loadAttachmentStorage,
        attachmentStorageAcknowledger: (android.content.Context, GatewayAccountHost.AttachmentStorageScope) -> Boolean = GatewayAccountHost::acknowledgeAttachmentStorageNotice,
    ) {
        compose.setContent {
            MaterialTheme {
                GatewayAccountSettings(
                    onChanged = {},
                    attachmentStorageOriginSupported = { true },
                    accountLoader = { shellAccount() },
                    gatewayStatusLoader = { shellStatus(origin) },
                    attachmentStorageScopeLoader = attachmentStorageScopeLoader,
                    attachmentStorageLoader = attachmentStorageLoader,
                    attachmentStorageAcknowledger = attachmentStorageAcknowledger,
                )
            }
        }
    }

    private fun enrolledScope(): GatewayAccountHost.AttachmentStorageScope {
        enrollLoopback(TEST_ORIGIN)
        return checkNotNull(GatewayAccountHost.attachmentStorageScope(context))
    }

    private fun availableLoad(noticeState: AttachmentStorageNoticeState) = AttachmentStorageLoad.Available(
        AttachmentStorageSnapshot(
            usedBytes = 12_345,
            limitBytes = 23_456,
            baseBytes = 10_000,
            cleanup = AttachmentStorageCleanup.BLOCKED,
            unavailableReason = AttachmentStorageUnavailableReason.DEVICE_NOT_READY,
            notice = AttachmentStorageNotice("hosted_attachment_retention_v1", noticeState, 1_702_592_000),
            noticeCopySupported = true,
        ),
    )

    private fun shellAccount() = GatewayAccountState(GatewayAccountDevice(deviceId, "gateway", false, 1), emptyList())

    private fun shellStatus(origin: String) = GatewayStatus(true, origin, false, false)

    private fun enrollLoopback(origin: String) {
        val credential = checkNotNull(CredentialParser.parse(credentialJson(origin, vaultId, deviceId).toByteArray(), allowDebugLoopback = true))
        assertEquals(ImportResult.IMPORTED, NativeGateway.persistVerified(context, credential, vaultMaterial()))
        assertNotNull(GatewayAccountHost.attachmentStorageScope(context))
    }

    private fun notPresentedStatusBody() = statusBody("not_presented")


    private fun statusBody(noticeState: String) = """{"used_bytes":12345,"limit_bytes":23456,"base_bytes":10000,"pack_bytes":2345,"protected_bytes":8765,"removed_bytes":1234,"cleanup_state":"blocked","cleanup_unavailable_reason":"device_not_ready","notice":{"state":"$noticeState","policy_version":"hosted_attachment_retention_v1","presented_at":null,"acknowledged_at":null,"activation_at":1702592000}}"""
}
