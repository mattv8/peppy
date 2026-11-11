package dev.peppy.mobile.googlemessages

import java.io.File
import java.nio.file.Files
import java.util.ArrayDeque
import java.util.concurrent.Executor
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RcsMediaSendCoordinatorTest {
    @Test fun `uploads before the ordinary native proof pipeline`() {
        val h = Harness()
        h.coordinator.submit(h.attachment)
        h.executor.runAll()
        assertEquals(listOf("decrypt", "uploading", "upload", "ready", "native_send"), h.events)
    }

    @Test fun `upload failure does not issue a permit and a later submission can retry`() {
        val h = Harness(uploadCode = "failed")
        h.coordinator.submit(h.attachment); h.executor.runAll()
        assertFalse(h.events.contains("native_send"))
        h.uploadCode = "ok"
        h.coordinator.submit(h.attachment.copy(attachmentIndex = 1u)); h.executor.runAll()
        assertEquals(1, h.events.count { it == "native_send" })
    }

    @Test fun `upload failure is durably accounted before a safe retry`() {
        val h = Harness(uploadCode = "failed")
        h.coordinator.submit(h.attachment); h.executor.runAll()
        assertEquals(listOf("decrypt", "uploading", "upload", "outcome"), h.events)
        h.uploadCode = "ok"
        h.coordinator.submit(h.attachment.copy(attachmentIndex = 1u)); h.executor.runAll()
        assertEquals(2, h.events.count { it == "upload" })
    }

    @Test fun `unknown native result is durably recorded and not retried by this execution`() {
        val h = Harness(nativeOutcome = RcsNativeSendOutcome.OUTCOME_UNKNOWN)
        h.coordinator.submit(h.attachment); h.executor.runAll()
        assertEquals(1, h.events.count { it == "native_send" })
        assertEquals("outcome_unknown", h.outcomes.single())
    }

    @Test fun `reference ready attachment reuses its upload and runs a fresh native proof`() {
        val h = Harness()
        h.coordinator.submit(h.attachment.copy(uploadRequired = false)); h.executor.runAll()
        assertEquals(listOf("native_send"), h.events)
    }

    @Test fun `early native group member change records a definite media refusal`() {
        val h = Harness(nativeOutcome = RcsNativeSendOutcome.MEMBERS_CHANGED)
        h.coordinator.submit(h.attachment.copy(uploadRequired = false)); h.executor.runAll()
        assertEquals(listOf("members_changed"), h.outcomes)
    }

    @Test fun `early refusal waits for an external wake instead of scheduling another discovery`() {
        val h = Harness(nativeOutcome = RcsNativeSendOutcome.REFUSED)
        h.coordinator.submit(h.attachment.copy(uploadRequired = false)); h.executor.runAll()

        assertTrue(h.outcomes.isEmpty())
        assertEquals(0, h.statusChanges)
    }

    @Test fun `terminal progress callback can queue the next discovery after identity release`() {
        val executor = QueueExecutor()
        val queue = RcsMediaTransferQueue(executor)
        val attachment = RcsMediaAttachment("command", "conversation", "recipient", 0u, "attachment", "image/jpeg", "a.jpg", 1u, false)
        var continuationAccepted = false
        var continuations = 0
        val coordinator = RcsMediaSendCoordinator(
            RcsMediaPrivateFiles(ApplicationProvider.getApplicationContext<Context>()), queue,
            object : RcsMediaSendCore {
                override fun decrypt(attachmentId: String, destinationDir: String) = error("unused")
                override fun markUploading(attachment: RcsMediaAttachment) = error("unused")
                override fun markReferenceReady(attachment: RcsMediaAttachment, mediaRef: String, uploadedAtMs: Long) = error("unused")
                override fun recordOutcome(attachment: RcsMediaAttachment, outcome: String, atMs: Long) = Unit
            },
            object : RcsMediaSendGo { override fun upload(path: File, attachment: RcsMediaAttachment) = error("unused") },
            { _, _, _ -> RcsNativeSendOutcome.MEMBERS_CHANGED }, { 1L },
            { continuationAccepted = queue.submit(attachment.identity) { continuations++ } },
        )

        coordinator.submit(attachment)
        executor.runAll()

        assertTrue(continuationAccepted)
        assertEquals(1, continuations)
    }

    @Test fun `invalid request settles an empty upload as rejected`() {
        val h = Harness(uploadCode = "invalid_request")
        h.coordinator.submit(h.attachment); h.executor.runAll()

        assertEquals(listOf("rejected"), h.outcomes)
        assertFalse(h.events.contains("native_send"))
    }

    private class Harness(var uploadCode: String = "ok", var nativeOutcome: RcsNativeSendOutcome = RcsNativeSendOutcome.SUBMITTED) {
        val executor = QueueExecutor()
        val files = RcsMediaPrivateFiles(ApplicationProvider.getApplicationContext<Context>()).also { it.sweep() }
        val events = mutableListOf<String>()
        val outcomes = mutableListOf<String>()
        val attachment = RcsMediaAttachment("command", "conversation", "recipient", 0u, "attachment", "image/jpeg", "a.jpg", 1u)
        val coordinator = RcsMediaSendCoordinator(files, RcsMediaTransferQueue(executor), object : RcsMediaSendCore {
            override fun decrypt(attachmentId: String, destinationDir: String): String { events += "decrypt"; return File(destinationDir, "${java.util.UUID.randomUUID()}.bin").also { it.writeText("plain") }.toString() }
            override fun markUploading(attachment: RcsMediaAttachment) { events += "uploading" }
            override fun markReferenceReady(attachment: RcsMediaAttachment, mediaRef: String, uploadedAtMs: Long) { events += "ready" }
            override fun recordOutcome(attachment: RcsMediaAttachment, outcome: String, atMs: Long) { events += "outcome"; outcomes += outcome }
        }, object : RcsMediaSendGo {
            override fun upload(path: File, attachment: RcsMediaAttachment): RcsMediaUploadResult { events += "upload"; return RcsMediaUploadResult(uploadCode, if (uploadCode == "ok") "ref" else null) }
        }, { _, _, _ -> events += "native_send"; nativeOutcome }, { 1L }, { statusChanges++ })
        var statusChanges = 0
    }
    private class QueueExecutor : Executor { private val tasks = ArrayDeque<Runnable>(); override fun execute(command: Runnable) { tasks += command }; fun runAll() { while (tasks.isNotEmpty()) tasks.removeFirst().run() } }
}
