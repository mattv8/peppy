package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.LoopbackHttpServer
import dev.peppy.mobile.attachments.*
import dev.peppy.mobile.gateway.GatewayAccountHost
import dev.peppy.mobile.gateway.GatewayAccountIdentity
import dev.peppy.mobile.gateway.GatewayHttp
import java.io.File
import java.lang.reflect.Proxy
import java.nio.file.Files
import java.util.ArrayDeque
import java.util.concurrent.Executor
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.*

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class RcsMediaReceiveCoordinatorTest {
    @Test fun `held download imports uploads finalizes and drains retention before waking`() = Harness().use { h ->
        h.run()
        assertEquals(listOf("download", "import", "reserve", "put", "hosted_finalize", "uploaded", "finalize", "retention", "wake"), h.events)
        assertEquals(1, h.downloads)
        assertFalse(h.destination!!.exists())
        val reserve = JSONObject(h.requests.single { it.target.endsWith("/reserve") }.body)
        assertEquals("message_media", reserve.getString("retention_class"))
        assertTrue(reserve.getBoolean("reference_tracking"))
    }

    @Test fun `confirmed quota never downloads and wakes the placeholder update`() = Harness().use { h ->
        h.storage = h.capacity(0)
        h.run()
        assertEquals(listOf("quota_full"), h.states)
        assertEquals(0, h.downloads)
        assertEquals(listOf("wake"), h.events)
        assertTrue(h.requests.isEmpty())
    }

    @Test fun `unavailable capacity coalesces without claiming quota or downloading`() = Harness().use { h ->
        h.storage = AttachmentStorageLoad.TemporarilyUnavailable
        h.run(); h.run()
        assertEquals(1, h.capacityReads)
        assertEquals(0, h.downloads)
        assertTrue(h.states.isEmpty())
    }

    @Test fun `preuploaded finalizes with empty pending uploads and unavailable capacity`() = Harness().use { h ->
        h.prepared = true
        h.attachmentState = NativeAttachmentState.UPLOADED
        h.storage = AttachmentStorageLoad.TemporarilyUnavailable
        h.run()
        assertEquals(listOf("finalize", "retention", "wake"), h.events)
        assertEquals(0, h.downloads)
        assertEquals(0, h.capacityReads)
        assertFalse(h.requests.any { it.method == "PUT" })
    }

    @Test fun `hosted quota retains encrypted id and retry never downloads again`() = Harness().use { h ->
        h.hostedCode = 413
        h.run()
        assertEquals(listOf("quota_full"), h.states)
        assertTrue(h.cipher.isFile)
        h.hostedCode = 200
        h.run()
        assertEquals(1, h.downloads)
        assertEquals(1, h.events.count { it == "import" })
        assertEquals(1, h.events.count { it == "finalize" })
        assertTrue(h.requests.filter { it.target.endsWith("/reserve") }.all { JSONObject(it.body).getString("attachment_id") == h.id })
    }

    @Test fun `partial download failure deletes plaintext and publishes failed`() = Harness().use { h ->
        h.downloadCode = "failed"
        h.run()
        assertEquals(listOf("failed"), h.states)
        assertFalse(h.destination!!.exists())
        assertFalse(h.events.contains("import"))
        assertEquals("wake", h.events.last())
    }

    @Test fun `retirement during download cannot import publish or upload`() = Harness().use { h ->
        h.retireDuringDownload = true
        h.run()
        assertEquals(listOf("download"), h.events)
        assertFalse(h.destination!!.exists())
        assertTrue(h.states.isEmpty())
    }

    @Test fun `retirement during hosted response cannot publish stored`() = Harness().use { h ->
        h.retireOnReserve = true
        h.run()
        assertEquals(listOf("/v1/attachments/reserve"), h.requests.map { it.target })
        assertEquals(NativeAttachmentState.PENDING_UPLOAD, h.attachmentState)
        assertTrue(h.states.isEmpty())
        assertFalse(h.events.contains("finalize"))
        assertFalse(h.events.contains("wake"))
    }

    @Test fun `retirement after PUT denies hosted finalize and does not publish stored or quota`() = Harness().use { h ->
        h.retireOnPut = true
        h.run()
        assertEquals(listOf("/v1/attachments/reserve", "/v1/attachments/${h.id}/upload"), h.requests.map { it.target })
        assertEquals(NativeAttachmentState.PENDING_UPLOAD, h.attachmentState)
        assertTrue(h.states.isEmpty())
        assertFalse(h.events.contains("finalize"))
        assertFalse(h.events.contains("wake"))
    }

    @Test fun `auth denial defers later tasks in same captured scope`() = Harness().use { h ->
        h.hostedCode = 401
        h.run(); h.run()
        assertEquals(1, h.requests.count { it.target.endsWith("/reserve") })
        assertEquals(1, h.downloads)
        assertTrue(h.states.isEmpty())
    }

    @Test fun `only Google not found or expired evidence maps to expired`() {
        for (code in listOf("not_found", "expired", "failed", "unavailable")) Harness().use { h ->
            h.downloadCode = code
            h.run()
            assertEquals(listOf(if (code == "not_found" || code == "expired") "expired" else "failed"), h.states)
            assertFalse(h.destination!!.exists())
        }
    }

    @Test fun `capacity cache expires and does not poll while unavailable`() = Harness().use { h ->
        h.storage = AttachmentStorageLoad.Unsupported
        h.run(); h.run()
        assertEquals(1, h.capacityReads)
        h.clockMs += 5_001
        h.storage = h.capacity(100_000_000)
        h.run()
        assertEquals(2, h.capacityReads)
        assertEquals(1, h.downloads)
    }

    @Test fun `new account generation cannot borrow cached headroom`() = Harness().use { h ->
        h.storage = AttachmentStorageLoad.TemporarilyUnavailable
        h.run()
        h.scope = h.scope.copy(identity = h.scope.identity.copy(generation = 2))
        h.storage = h.capacity(100_000_000)
        h.run()
        assertEquals(2, h.capacityReads)
        assertEquals(1, h.downloads)
    }

    @Test fun `capacity authorization failure defers without Google or hosted IO`() = Harness().use { h ->
        h.storage = AttachmentStorageLoad.AuthorizationFailure
        h.run(); h.run()
        assertEquals(1, h.capacityReads)
        assertEquals(0, h.downloads)
        assertTrue(h.requests.isEmpty())
        assertTrue(h.states.isEmpty())
    }

    @Test fun `throwing downloader cleans its partial file and publishes failure`() = Harness().use { h ->
        h.throwDuringDownload = true
        h.run()
        assertEquals(listOf("failed"), h.states)
        assertFalse(h.destination!!.exists())
        assertEquals("wake", h.events.last())
    }

    private class Harness : AutoCloseable, RcsMediaReceiveIo {
        val id = "11111111-1111-1111-1111-111111111111"
        val events = mutableListOf<String>()
        val states = mutableListOf<String>()
        val requests = mutableListOf<LoopbackHttpServer.Request>()
        val cipher = Files.createTempFile("rcs-receive-cipher", ".bin").toFile().apply { writeText("cipher") }
        val files = RcsMediaPrivateFiles(ApplicationProvider.getApplicationContext<Context>()).also { it.sweep() }
        val executor = QueueExecutor()
        var storage: AttachmentStorageLoad = capacity(100_000_000)
        var capacityReads = 0
        var downloads = 0
        var prepared = false
        var attachmentState = NativeAttachmentState.PENDING_UPLOAD
        var hostedCode = 200
        var downloadCode = "ok"
        @Volatile var retired = false
        var clockMs = 0L
        var throwDuringDownload = false
        var retireDuringDownload = false
        var retireOnReserve = false
        var retireOnPut = false
        var destination: File? = null
        var retentionPending = false
        val server = LoopbackHttpServer { request ->
            requests += request
            when {
                request.target.endsWith("/reserve") -> {
                    events += "reserve"
                    if (retireOnReserve) retired = true
                    LoopbackHttpServer.Response(hostedCode, if (hostedCode == 200) """{"attachment_id":"$id"}""" else """{"code":"${if (hostedCode == 401) "unauthorized" else "vault_quota_exceeded"}"}""")
                }
                request.target.endsWith("/upload") -> {
                    events += "put"
                    if (retireOnPut) retired = true
                    LoopbackHttpServer.Response(204)
                }
                request.target.endsWith("/finalize") -> { events += "hosted_finalize"; LoopbackHttpServer.Response(200, """{"attachment_id":"$id"}""") }
                else -> { events += "retention"; LoopbackHttpServer.Response(200, """{"use_id":"22222222-2222-2222-2222-222222222222","attachment_id":"$id","device_id":"33333333-3333-3333-3333-333333333333","kind":"message_history","state":"registered"}""") }
            }
        }
        var scope = GatewayAccountHost.AttachmentStorageScope("scope", GatewayAccountIdentity(server.origin, "vault", "device", 1))
        val client = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, args ->
            when (method.name.substringBefore('-')) {
                "gatewayAdmitRcsMediaDownload" -> when {
                    prepared -> NativeRcsMediaDownloadAdmission.ExistingPrepared(id)
                    args!![2] == null -> NativeRcsMediaDownloadAdmission.CapacityUnavailable
                    (args[2] as ULong) == 0uL -> NativeRcsMediaDownloadAdmission.QuotaFull
                    else -> NativeRcsMediaDownloadAdmission.Held("token")
                }
                "gatewayImportRcsMediaDownload" -> {
                    assertEquals("message", args!![0]); assertEquals(2, args[1]); assertEquals("token", args[2])
                    assertEquals("plain", File(args[3] as String).readText())
                    events += "import"; prepared = true; info()
                }
                "attachmentInfo" -> { assertEquals(id, args!![0]); info() }
                "pendingUploads" -> if (prepared && attachmentState == NativeAttachmentState.PENDING_UPLOAD) listOf(NativeCipherObject(id, 6u, "a".repeat(64), null)) else emptyList()
                "nativeCipherFileForUpload" -> cipher.absolutePath
                "attachmentUploadRetention" -> NativeAttachmentUploadRetention(NativeAttachmentRetentionClass.MESSAGE_MEDIA, true)
                "markAttachmentUploaded" -> { events += "uploaded"; attachmentState = NativeAttachmentState.UPLOADED; Unit }
                "gatewayFinalizeRcsMediaDownload" -> { check(attachmentState == NativeAttachmentState.UPLOADED); events += "finalize"; retentionPending = true; Unit }
                "gatewayMarkRcsMediaDownloadState" -> { states += args!![2] as String; Unit }
                "attachmentRetentionProtocolEnabled" -> true
                "prepareAttachmentRetentionOperations" -> if (retentionPending) listOf(NativePendingAttachmentRetentionOperation(NativeAttachmentUseOperationKind.REGISTER, "22222222-2222-2222-2222-222222222222", id, "33333333-3333-3333-3333-333333333333", NativeAttachmentUseKind.MESSAGE_HISTORY)) else emptyList()
                "applyAttachmentRetentionResponse" -> { retentionPending = false; Unit }
                "attachmentRetentionPreparationPending", "attachmentRetentionReady" -> false
                "markAttachmentRetentionReconciled" -> Unit
                "toString" -> "receive-client"
                else -> throw AssertionError("Unexpected native call: ${method.name}")
            }
        } as NativeClientInterface
        val coordinator = RcsMediaReceiveCoordinator(files, RcsMediaTransferQueue(executor), client, this) { !retired }
        init { RcsMediaReceiveCoordinator.clearCapacityCache() }
        fun info() = NativeAttachmentInfo(id, "image/jpeg", "a.jpg", 5u, 6u, "a".repeat(64), attachmentState)
        fun capacity(bytes: Long) = AttachmentStorageLoad.Available(AttachmentStorageSnapshot(0, bytes, 0, AttachmentStorageCleanup.AVAILABLE, null))
        fun run() {
            coordinator.submit(NativePendingRcsMediaReceive("message", 2u, "private-ref", "image/jpeg", "a.jpg", 5u, "downloading"))
            executor.runAll()
        }
        override fun captureAccount() = scope
        override fun isCurrent(scope: GatewayAccountHost.AttachmentStorageScope) = !retired && scope == this.scope
        override fun capacity(scope: GatewayAccountHost.AttachmentStorageScope): AttachmentStorageLoad { capacityReads++; return storage }
        override fun http(scope: GatewayAccountHost.AttachmentStorageScope) = if (isCurrent(scope)) GatewayHttp(server.origin, "token").also { it.initializeRetentionProtocol(true) } else null
        override fun download(task: NativePendingRcsMediaReceive, destination: File): String {
            assertEquals("private-ref", task.mediaRef)
            events += "download"; downloads++; this.destination = destination; destination.writeText("plain")
            if (throwDuringDownload) throw java.io.IOException("download interrupted")
            if (retireDuringDownload) retired = true
            return """{"code":"$downloadCode"}"""
        }
        override fun nowMs() = clockMs
        override fun wake() { events += "wake" }
        override fun close() { server.close(); cipher.delete(); files.sweep(); RcsMediaReceiveCoordinator.clearCapacityCache() }
    }

    private class QueueExecutor : Executor {
        private val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks += command }
        fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }
}
