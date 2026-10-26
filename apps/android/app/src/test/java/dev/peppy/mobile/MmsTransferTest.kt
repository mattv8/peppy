package dev.peppy.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import androidx.test.core.app.ApplicationProvider
import uniffi.peppy_mobile_bindings.NativeCipherObject
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMmsBackfillPause
import uniffi.peppy_mobile_bindings.NativeMmsBackfillStatus
import java.io.File
import java.lang.reflect.Proxy
import java.nio.file.Files

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsTransferTest {
    private val id = "11111111-1111-1111-1111-111111111111"
    private val sha = "a".repeat(64)

    @Test fun finalizeFailureDoesNotMarkAndRetryUsesSameId() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(listOf(cipher()))
        val seen = mutableListOf<String>()
        LoopbackHttpServer { request ->
            seen += request.target
            when {
                request.target.endsWith("/reserve") -> LoopbackHttpServer.Response(200, """{"attachment_id":"$id"}""")
                request.target.endsWith("/upload") -> LoopbackHttpServer.Response(204)
                else -> LoopbackHttpServer.Response(503, """{"code":"busy"}""")
            }
        }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "secret-token"), file.parentFile).run()
            assertEquals("transient", result.failures.single().reason)
            assertFalse("a transient failure waits for a later gateway pass", result.more)
            assertTrue(client.marked.isEmpty())
            assertEquals(listOf("/v1/attachments/reserve", "/v1/attachments/$id/upload", "/v1/attachments/$id/finalize"), seen)
        }
        file.delete()
    }

    @Test fun repeatedUpload503sRemainVisibleAndDoNotRequestAnImmediateRetry() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(List(6) { cipher() })
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-mms-transfer-health", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        var reserves = 0
        LoopbackHttpServer { request ->
            if (request.target.endsWith("/reserve")) reserves++
            LoopbackHttpServer.Response(503, """{"code":"busy"}""")
        }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "secret-token"), file.parentFile).run(4)
            MmsTransferHealth(context).record(result.failures)

            assertEquals("the per-pass media budget bounds repeated failures", 4, reserves)
            assertEquals(4, result.failures.size)
            assertTrue(result.failures.all { it.reason == "transient" })
            assertFalse("the next transfer waits for a later gateway pass", result.more)
            assertEquals(listOf("upload: transient", "upload: transient", "upload: transient", "upload: transient"),
                MmsTransferHealth(context).reasons())
            assertTrue(client.marked.isEmpty())
        }
        file.delete()
    }

    @Test fun finalizedReservationRecoveryMarksOnlyAfterFinalize() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(listOf(cipher()))
        val sequence = mutableListOf<String>()
        LoopbackHttpServer { request ->
            sequence += request.target
            when {
                request.target.endsWith("/reserve") -> LoopbackHttpServer.Response(200, """{"attachment_id":"$id"}""")
                request.target.endsWith("/upload") -> LoopbackHttpServer.Response(404, """{"code":"attachment_unavailable"}""")
                else -> LoopbackHttpServer.Response(200, """{"attachment_id":"$id","duplicate":false}""")
            }
        }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "secret-token"), file.parentFile).run()
            assertEquals(1, result.uploaded)
            assertEquals(listOf(id to id), client.marked)
            assertEquals(listOf("/v1/attachments/reserve", "/v1/attachments/$id/upload", "/v1/attachments/$id/finalize"), sequence)
        }
        file.delete()
    }

    @Test fun reservationConflictProbesFinalizeAndClassifiesFailure() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(listOf(cipher()))
        LoopbackHttpServer { request -> if (request.target.endsWith("reserve")) LoopbackHttpServer.Response(409) else LoopbackHttpServer.Response(404) }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile).run()
            assertEquals("permanent", result.failures.single().reason)
            assertTrue(client.marked.isEmpty())
        }
        file.delete()
    }

    @Test fun reserveConflictAcceptsMatchingDuplicateFinalizeProof() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(listOf(cipher()))
        LoopbackHttpServer { request ->
            if (request.target.endsWith("reserve")) {
                LoopbackHttpServer.Response(409)
            } else {
                LoopbackHttpServer.Response(200, """{"attachment_id":"$id","duplicate":true}""")
            }
        }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile).run()
            assertEquals(1, result.uploaded)
            assertEquals(listOf(id to id), client.marked)
        }
        file.delete()
    }

    @Test fun reserveConflictRequiresMatchingDuplicateFinalizeProof() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        for (body in listOf(
            """{"attachment_id":"$id","duplicate":false}""",
            """{"attachment_id":"22222222-2222-2222-2222-222222222222","duplicate":true}""",
            """{"duplicate":true}""",
        )) {
            val client = fakeClient(listOf(cipher()))
            LoopbackHttpServer { request ->
                if (request.target.endsWith("reserve")) LoopbackHttpServer.Response(409) else LoopbackHttpServer.Response(200, body)
            }.use { server ->
                val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile).run()
                assertEquals("permanent", result.failures.single().reason)
                assertTrue(client.marked.isEmpty())
            }
        }
        file.delete()
    }

    @Test fun normalFinalizeRequiresTypedMatchingAttachmentId() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        for (body in listOf("{}", """{"attachment_id":"22222222-2222-2222-2222-222222222222","duplicate":false}""")) {
            val client = fakeClient(listOf(cipher()))
            LoopbackHttpServer { request -> when {
                request.target.endsWith("reserve") -> LoopbackHttpServer.Response(200, """{"attachment_id":"$id"}""")
                request.target.endsWith("upload") -> LoopbackHttpServer.Response(204)
                else -> LoopbackHttpServer.Response(200, body)
            } }.use { server ->
                val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile).run()
                assertEquals("permanent", result.failures.single().reason)
                assertTrue(client.marked.isEmpty())
            }
        }
        file.delete()
    }

    @Test fun givesDownloadsShareAndDoesNotClaimMoreForExactBudget() {
        val downloadId = "22222222-2222-2222-2222-222222222222"
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(List(5) { cipher() }, List(2) { cipher(downloadId, downloadId) })
        LoopbackHttpServer { request ->
            when (request.method) {
                "GET" -> LoopbackHttpServer.Response(200, "cipher")
                else -> LoopbackHttpServer.Response(413, """{"code":"attachment_too_large"}""")
            }
        }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile).run(4)
            assertTrue(client.installed.isNotEmpty())
            assertTrue(result.failures.any { it.reason == "permanent" })
        }
        val exact = fakeClient(List(4) { cipher() })
        LoopbackHttpServer { LoopbackHttpServer.Response(413, """{"code":"attachment_too_large"}""") }.use { server ->
            assertFalse(MmsMediaTransfer(exact.client, GatewayHttp(server.origin, "token"), file.parentFile).run(4).more)
        }
        file.delete()
    }

    @Test fun quotaFailurePausesRemainingUploadReservationsButKeepsFutureDownloadsEligible() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(List(3) { cipher() })
        var reserves = 0
        var pauses = 0
        LoopbackHttpServer { request ->
            if (request.target.endsWith("/reserve")) {
                reserves++
                LoopbackHttpServer.Response(413, """{"code":"vault_quota_exceeded"}""")
            } else {
                LoopbackHttpServer.Response(500)
            }
        }.use { server ->
            val result = MmsMediaTransfer(
                client.client,
                GatewayHttp(server.origin, "token"),
                file.parentFile,
                onVaultQuotaExceeded = { pauses++ },
            ).run(4)
            assertEquals(1, reserves)
            assertEquals(1, pauses)
            assertEquals("quota", result.failures.single().reason)
        }
        file.delete()
    }

    @Test fun knownRemoteQuotaPauseSkipsReserves() {
        val file = Files.createTempFile("cipher", ".bin").toFile().apply { writeText("cipher") }
        val client = fakeClient(listOf(cipher()))
        LoopbackHttpServer { throw AssertionError("quota-paused uploads must not reserve") }.use { server ->
            val result = MmsMediaTransfer(client.client, GatewayHttp(server.origin, "token"), file.parentFile, uploadsAllowed = false).run()
            assertFalse(result.more)
            assertTrue(result.failures.isEmpty())
        }
        file.delete()
    }

    @Test fun pausedUploadsWithOnlyFailedDownloadsDoNotTransferProgress() {
        val downloadId = "22222222-2222-2222-2222-222222222222"
        val client = fakeClient(downloads = List(2) { cipher(downloadId, downloadId) })
        LoopbackHttpServer { LoopbackHttpServer.Response(503, """{"code":"busy"}""") }.use { server ->
            val result = MmsMediaTransfer(
                client.client,
                GatewayHttp(server.origin, "token"),
                client.file.parentFile,
                uploadsAllowed = false,
            ).run()

            assertEquals(0, result.uploaded)
            assertEquals(0, result.downloaded)
            assertEquals(2, result.failures.size)
            assertTrue(result.failures.all { it.reason == "transient" })
            assertTrue("the media queue still has pending downloads", result.more)
        }
        client.file.delete()
    }

    @Test fun pausedUploadsKeepContinuationWhenADownloadActuallySucceeds() {
        val firstId = "22222222-2222-2222-2222-222222222222"
        val secondId = "33333333-3333-3333-3333-333333333333"
        val client = fakeClient(downloads = listOf(cipher(firstId, firstId), cipher(secondId, secondId)))
        LoopbackHttpServer { request ->
            if (request.target.endsWith(secondId)) LoopbackHttpServer.Response(503, """{"code":"busy"}""")
            else LoopbackHttpServer.Response(200, "cipher")
        }.use { server ->
            val result = MmsMediaTransfer(
                client.client,
                GatewayHttp(server.origin, "token"),
                client.file.parentFile,
                uploadsAllowed = false,
            ).run()

            assertEquals(0, result.uploaded)
            assertEquals(1, result.downloaded)
            assertEquals("transient", result.failures.single().reason)
            assertTrue(result.more)
        }
        client.file.delete()
    }

    @Test fun positiveUsageHeadroomClearsPauseButZeroOrFailedUsageDoesNot() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-mms-remote-quota", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val client = fakeClient(remotePaused = true)
        LoopbackHttpServer { request ->
            assertEquals("/v1/attachments/usage", request.target)
            LoopbackHttpServer.Response(200, """{"used_bytes":4,"limit_bytes":10}""")
        }.use { server ->
            assertTrue(MmsRemoteQuota(context, client.client).uploadsAllowed(GatewayHttp(server.origin, "token")))
        }
        assertEquals(listOf(true), client.remoteCapacity)

        val zeroLimit = fakeClient(remotePaused = true)
        LoopbackHttpServer { LoopbackHttpServer.Response(200, """{"used_bytes":0,"limit_bytes":0}""") }.use { server ->
            assertFalse(MmsRemoteQuota(context, zeroLimit.client).uploadsAllowed(GatewayHttp(server.origin, "token")))
        }
        assertTrue(zeroLimit.remoteCapacity.isEmpty())

        val unavailable = fakeClient(remotePaused = true)
        LoopbackHttpServer { LoopbackHttpServer.Response(503, """{"code":"busy"}""") }.use { server ->
            assertFalse(MmsRemoteQuota(context, unavailable.client).uploadsAllowed(GatewayHttp(server.origin, "token")))
        }
        assertTrue(unavailable.remoteCapacity.isEmpty())
    }

    private fun cipher(attachment: String = id, remote: String? = null) = NativeCipherObject(attachment, 6u, sha, remote)
    private class Fake(val uploads: List<NativeCipherObject>, val downloads: List<NativeCipherObject>, val file: File, var remotePaused: Boolean) {
        val marked = mutableListOf<Pair<String, String>>(); val installed = mutableListOf<String>()
        val remoteCapacity = mutableListOf<Boolean>()
        val client = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, args ->
            when (method.name) {
                "pendingUploads" -> uploads
                "pendingDownloads" -> downloads
                "nativeCipherFileForUpload" -> file.path
                "markAttachmentUploaded" -> { marked += args!![0] as String to args[1] as String; Unit }
                "installDownloadedAttachment" -> { installed += args!![1] as String; Unit }
                "mmsBackfillStatus" -> NativeMmsBackfillStatus(false, null, false, if (remotePaused) NativeMmsBackfillPause.REMOTE_STORAGE else null, 0u, 0u, null)
                "reportMmsBackfillRemoteCapacity" -> { remoteCapacity += args!![2] as Boolean; remotePaused = !(args[2] as Boolean); Unit }
                "toString" -> "fake-client"
                else -> throw UnsupportedOperationException(method.name)
            }
        } as NativeClientInterface
    }
    private fun fakeClient(uploads: List<NativeCipherObject> = emptyList(), downloads: List<NativeCipherObject> = emptyList(), remotePaused: Boolean = false): Fake {
        val file = Files.createTempFile("fake-cipher", ".bin").toFile().apply { writeText("cipher") }
        return Fake(uploads, downloads, file, remotePaused)
    }
}
