package dev.peppy.mobile.mms

import android.app.PendingIntent
import android.graphics.Bitmap
import android.net.Uri
import androidx.core.content.FileProvider
import dev.peppy.mobile.MmsCallbackStore
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.SharedVault
import dev.peppy.mobile.granted
import dev.peppy.mobile.mms.pdu.MmsPduComposer
import dev.peppy.mobile.telephony.SimRoute
import dev.peppy.mobile.telephony.SimRoutes
import java.io.File
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClient
import uniffi.peppy_mobile_bindings.NativeComposeDraftUpdate
import uniffi.peppy_mobile_bindings.NativePermitState


private class FakeMmsCarrier(var failure: RuntimeException? = null) : MmsCarrier {
    data class Call(val subscriptionId: Int, val pdu: Uri, val sent: PendingIntent)
    val calls = mutableListOf<Call>()
    override fun send(subscriptionId: Int, pdu: Uri, sent: PendingIntent) {
        failure?.let { throw it }
        calls += Call(subscriptionId, pdu, sent)
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsDispatcherTest : GatewayTestBase() {

    @Before
    fun clearMmsCallbackAndPduState() {
        context.getSharedPreferences("peppy-mms-callbacks", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        File(context.filesDir, "mms-pdu").deleteRecursively()
        // AndroidX caches a PathStrategy only by authority. Robolectric gives each test a new
        // filesDir while loading FileProvider outside that sandbox, so stale roots must be reset.
        val cacheField = FileProvider::class.java.getDeclaredField("sCache").apply { isAccessible = true }
        val cache = cacheField.get(null) as MutableMap<*, *>
        synchronized(cache) { cache.clear() }
    }


    private val routes = SimRoutes.current(defaultSmsSubscriptionId = 3)
    private val route = SimRoutes.routeId(3)
    private val generous = MmsLimits(300L * 1024, 20, true, MmsLimits.Source.FALLBACK)
    private var mmsCursor = 0

    private fun dispatcher(
        client: NativeClient,
        carrier: MmsCarrier,
        granted: Boolean = true,
        enabled: Boolean = true,
        current: List<SimRoute> = routes,
        cap: MmsLimits = generous,
    ) = MmsDispatcher(context, client, carrier, current, granted, enabled, { cap })

    private fun queueMms(
        gateway: NativeClient,
        body: String,
        recipients: List<String>,
        attachment: File? = null,
        mediaType: String = "image/png",
        installMedia: Boolean = true,
    ): Pair<String, String?> {
        val desktop = SharedVault.desktop
        val prepared = attachment?.let { desktop.prepareAttachment(it.path, mediaType, "photo.${mediaType.substringAfter('/')}") }
        val draft = desktop.createComposeDraft(null)
        val routeJson = org.json.JSONObject().put("gateway_device_id", deviceId).put("subscription_id", route).toString()
        val saved = desktop.saveComposeDraft(
            draft.draftId,
            draft.revision,
            NativeComposeDraftUpdate(body, recipients, listOfNotNull(prepared?.attachmentId), routeJson),
        )
        val queued = desktop.sendComposeDraft(saved.draftId, saved.revision)
        prepared?.let { desktop.markAttachmentUploaded(it.attachmentId, UUID.randomUUID().toString()) }
        val wire = desktop.pendingOutboxJson().single { org.json.JSONObject(it).getString("envelope_id") == queued.envelopeId }
        desktop.ackOutbox(queued.envelopeId)
        gateway.ingestRaw(wire.toByteArray(), (++mmsCursor).toString())
        gateway.applyPending(50uL)
        if (prepared != null && installMedia) {
            val downloaded = File.createTempFile("peppy-download", ".ppss").apply {
                writeBytes(File(desktop.nativeCipherFileForUpload(prepared.attachmentId)).readBytes())
                deleteOnExit()
            }
            gateway.installDownloadedAttachment(prepared.attachmentId, downloaded.path)
        }
        return queued.commandId to prepared?.attachmentId
    }

    @Test
    fun textOnlyGroupCreatesOnePduAndSubmitsExactlyOnce() {
        val client = enrollAndUnlock()
        val (command, _) = queueMms(client, "group hello", listOf("+15551234567", "+15557654321"))
        assertEquals("mms", client.pendingCommands().single { it.commandId == command }.message.transport)
        val carrier = FakeMmsCarrier()

        val summary = dispatcher(client, carrier).dispatch()
        assertEquals(summary.toString(), 1, summary.submitted)
        val pdu = File(context.filesDir, "mms-pdu/$command.pdu")
        assertTrue(pdu.isFile)
        assertTrue(pdu.readBytes().toString(Charsets.ISO_8859_1).contains("group hello"))
        assertEquals(3, carrier.calls.single().subscriptionId)
        assertEquals("SubmittedToOs", client.beginSendAttempt(command).existingSendState)

        dispatcher(client, carrier).dispatch()
        assertEquals("a consumed permit is never resent", 1, carrier.calls.size)
    }

    @Test
    fun mediaOnlyCreatesOnePduAndReleasesPlaintextHandle() {
        val client = enrollAndUnlock()
        val image = File.createTempFile("peppy-mms", ".png").apply { writeBytes(ByteArray(128) { it.toByte() }); deleteOnExit() }
        val (command, attachmentId) = queueMms(client, "", listOf("+15551234567"), image)
        val probe = client.openNativePlaintextFile(checkNotNull(attachmentId))
        val plaintextDirectory = File(probe.nativePlaintextPath()).parentFile!!
        probe.close()
        val carrier = FakeMmsCarrier()

        val summary = dispatcher(client, carrier).dispatch()
        assertEquals(summary.toString(), 1, summary.submitted)
        assertEquals(1, carrier.calls.size)
        assertTrue(File(context.filesDir, "mms-pdu/$command.pdu").length() > image.length())
        assertFalse("native plaintext handles are closed after compose", plaintextDirectory.listFiles()?.any() == true)
    }

    @Test
    fun disabledPreflightConsumesNoPermit() {
        val client = enrollAndUnlock()
        val (id, _) = queueMms(client, "disabled", listOf("+15551234567", "+15557654321"))
        dispatcher(client, FakeMmsCarrier(), enabled = false).dispatch()
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(id).state)
    }

    @Test
    fun missingPermissionConsumesNoPermit() {
        val client = enrollAndUnlock()
        val (id, _) = queueMms(client, "permission", listOf("+15551234567", "+15557654321"))
        dispatcher(client, FakeMmsCarrier(), granted = false).dispatch()
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(id).state)
    }

    @Test
    fun stoppedDuringLimitsLookupDoesNotIssueAnInvalidShapeRefusalPermit() {
        val client = enrollAndUnlock()
        val (command, _) = queueMms(client, "group", listOf("+15551234567", "+15557654321"))
        val stopped = AtomicBoolean(false)
        val carrier = FakeMmsCarrier()

        MmsDispatcher(context, client, carrier, routes, true, true, {
            stopped.set(true)
            generous.copy(maximumRecipients = 1)
        }, mayIssueEffect = { !stopped.get() }).dispatch()

        assertTrue(client.pendingCommands().any { it.commandId == command })
        assertTrue(carrier.calls.isEmpty())
    }

    @Test
    fun routeChangeDuringLimitsLookupLeavesTheCommandPending() {
        val client = enrollAndUnlock()
        val (command, _) = queueMms(client, "group", listOf("+15551234567", "+15557654321"))
        var current = routes
        val carrier = FakeMmsCarrier()

        MmsDispatcher(context, client, carrier, routes, true, true, {
            current = SimRoutes.current(defaultSmsSubscriptionId = 4)
            generous.copy(maximumRecipients = 1)
        }, currentRoutes = { current }).dispatch()

        assertTrue(client.pendingCommands().any { it.commandId == command })
        assertTrue(carrier.calls.isEmpty())
    }

    @Test
    fun stoppedDuringPduPreparationDoesNotIssueValidationRefusalPermit() {
        val client = enrollAndUnlock()
        val attachment = File.createTempFile("peppy", ".bin").apply { writeBytes(ByteArray(4_096)); deleteOnExit() }
        val (command, _) = queueMms(client, "", listOf("+15551234567"), attachment, "application/octet-stream")
        val stopped = AtomicBoolean(false)
        val carrier = FakeMmsCarrier()
        MmsPduComposer.afterPartSizeReadForTest = { stopped.set(true) }
        try {
            MmsDispatcher(context, client, carrier, routes, true, true, { generous.copy(maximumPduBytes = 512) }, mayIssueEffect = { !stopped.get() }).dispatch()
        } finally {
            MmsPduComposer.afterPartSizeReadForTest = null
        }

        assertTrue(client.pendingCommands().any { it.commandId == command })
        assertTrue(carrier.calls.isEmpty())
    }

    @Test
    fun routeChangeDuringPduValidationFailureLeavesTheCommandPending() {
        val client = enrollAndUnlock()
        val attachment = File.createTempFile("peppy", ".bin").apply { writeBytes(ByteArray(4_096)); deleteOnExit() }
        val (command, _) = queueMms(client, "", listOf("+15551234567"), attachment, "application/octet-stream")
        var current = routes
        val carrier = FakeMmsCarrier()
        MmsPduComposer.afterPartSizeReadForTest = { current = SimRoutes.current(defaultSmsSubscriptionId = 4) }
        try {
            MmsDispatcher(context, client, carrier, routes, true, true, { generous.copy(maximumPduBytes = 512) }, currentRoutes = { current }).dispatch()
        } finally {
            MmsPduComposer.afterPartSizeReadForTest = null
        }

        assertTrue(client.pendingCommands().any { it.commandId == command })
        assertTrue(carrier.calls.isEmpty())
    }

    @Test
    fun routeChangeAfterPduPreparationDoesNotIssuePermitOrReroute() {
        val client = enrollAndUnlock()
        val attachment = File.createTempFile("peppy", ".bin").apply { writeBytes(ByteArray(64)); deleteOnExit() }
        val (command, _) = queueMms(client, "", listOf("+15551234567"), attachment, "application/octet-stream")
        var current = routes
        val carrier = FakeMmsCarrier()
        MmsPduComposer.afterPartSizeReadForTest = { current = SimRoutes.current(defaultSmsSubscriptionId = 4) }
        try {
            MmsDispatcher(context, client, carrier, routes, true, true, { generous }, currentRoutes = { current }).dispatch()
        } finally {
            MmsPduComposer.afterPartSizeReadForTest = null
        }

        assertTrue(client.pendingCommands().any { it.commandId == command })
        assertTrue(carrier.calls.isEmpty())
    }

    @Test
    fun invalidMmsShapeStillRefusesWhenEffectAdmissionRemainsOpen() {
        val client = enrollAndUnlock()
        val (command, _) = queueMms(client, "group", listOf("+15551234567", "+15557654321"))
        val carrier = FakeMmsCarrier()

        val summary = MmsDispatcher(context, client, carrier, routes, true, true, { generous.copy(maximumRecipients = 1) }).dispatch()

        assertEquals(1, summary.refusedBeforeCarrier)
        assertTrue(carrier.calls.isEmpty())
        assertEquals("FailedBeforeSubmission", client.beginSendAttempt(command).existingSendState)
    }

    @Test
    fun staleSimConsumesNoPermit() {
        val client = enrollAndUnlock()
        val (id, _) = queueMms(client, "sim", listOf("+15551234567", "+15557654321"))
        dispatcher(client, FakeMmsCarrier(), current = emptyList()).dispatch()
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(id).state)
    }

    @Test
    fun missingMediaAndIoConsumeNoPermit() {
        val client = enrollAndUnlock()
        val image = File.createTempFile("missing", ".png").apply { writeBytes(byteArrayOf(1)); deleteOnExit() }
        val (missing, _) = queueMms(client, "", listOf("+15551234567"), image, installMedia = false)
        dispatcher(client, FakeMmsCarrier()).dispatch()
        assertEquals(NativePermitState.MEDIA_UNAVAILABLE, client.beginSendAttempt(missing).state)

        val (io, _) = queueMms(client, "io", listOf("+15551234567", "+15557654321"))
        val pduDirectory = File(context.filesDir, "mms-pdu").apply { deleteRecursively(); writeText("not a directory") }
        try {
            dispatcher(client, FakeMmsCarrier()).dispatch()
            assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(io).state)
        } finally {
            pduDirectory.delete()
            pduDirectory.mkdirs()
        }
    }

    @Test
    fun laterCommandsInAThreadWaitBehindEarlierTransientMms() {
        val client = enrollAndUnlock()
        val image = File.createTempFile("missing", ".png").apply { writeBytes(byteArrayOf(1)); deleteOnExit() }
        val (first, _) = queueMms(client, "", listOf("+15551234567"), image, installMedia = false)
        val (later, _) = queueMms(client, "later", listOf("+15551234567"))
        val carrier = FakeMmsCarrier()

        val summary = dispatcher(client, carrier).dispatch()

        assertTrue(summary.waiting >= 2)
        assertTrue(carrier.calls.isEmpty())
        assertEquals(NativePermitState.MEDIA_UNAVAILABLE, client.beginSendAttempt(first).state)
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(later).state)
    }

    @Test
    fun callbackRegistrationWaitBlocksLaterSameThreadMms() {
        val client = enrollAndUnlock()
        val (first, _) = queueMms(client, "first", listOf("+15551234567"))
        val (later, _) = queueMms(client, "later", listOf("+15551234567"))
        val callbacks = MmsCallbackStore(context)
        assertTrue(callbacks.register(first))
        val carrier = FakeMmsCarrier()

        val summary = MmsDispatcher(context, client, carrier, routes, true, true, { generous }, callbacks).dispatch()

        assertTrue(summary.waiting >= 2)
        assertTrue(carrier.calls.isEmpty())
        assertEquals(NativePermitState.PERMIT, client.beginSendAttempt(later).state)
    }

    @Test
    fun sizeFailureMakesNoCarrierCallAndClosesPlaintext() {
        val client = enrollAndUnlock()
        val image = File.createTempFile("oversized", ".png").apply { writeBytes(ByteArray(128)); deleteOnExit() }
        val (tooLarge, attachmentId) = queueMms(client, "", listOf("+15551234567"), image)
        val probe = client.openNativePlaintextFile(checkNotNull(attachmentId))
        val plaintextDirectory = File(probe.nativePlaintextPath()).parentFile!!
        probe.close()
        val carrier = FakeMmsCarrier()
        dispatcher(client, carrier, cap = generous.copy(maximumPduBytes = 10)).dispatch()
        assertTrue(carrier.calls.isEmpty())
        assertEquals("FailedBeforeSubmission", client.beginSendAttempt(tooLarge).existingSendState)
        assertFalse("a refused compose closes every plaintext handle", plaintextDirectory.listFiles()?.any() == true)

    }

    @Test
    fun oversizedJpegIsFittedBeforeTakingTheCarrierPermit() {
        val client = enrollAndUnlock()
        val image = File.createTempFile("oversized", ".jpg").apply {
            val bitmap = Bitmap.createBitmap(1200, 1200, Bitmap.Config.ARGB_8888)
            for (y in 0 until bitmap.height) for (x in 0 until bitmap.width) bitmap.setPixel(x, y, (x * 31) xor (y * 17))
            outputStream().use { bitmap.compress(Bitmap.CompressFormat.JPEG, 100, it) }
            bitmap.recycle()
            deleteOnExit()
        }
        val (command, _) = queueMms(client, "", listOf("+15551234567"), image, mediaType = "image/jpeg")
        val carrier = FakeMmsCarrier()

        val summary = dispatcher(client, carrier, cap = generous.copy(maximumPduBytes = 50L * 1024)).dispatch()

        assertEquals(summary.toString(), 1, summary.submitted)
        assertEquals(1, carrier.calls.size)
        assertEquals("SubmittedToOs", client.beginSendAttempt(command).existingSendState)
    }

    @Test
    fun unknownCarrierFailureDoesNotResend() {
        val client = enrollAndUnlock()
        val (unknown, _) = queueMms(client, "unknown", listOf("+15551234567", "+15557654321"))
        val summary = dispatcher(client, FakeMmsCarrier(IllegalStateException("radio"))).dispatch()
        assertEquals(summary.toString(), NativePermitState.ALREADY_ATTEMPTED, client.beginSendAttempt(unknown).state)
        assertEquals("AttemptRecorded", client.beginSendAttempt(unknown).existingSendState)
        val restartedCarrier = FakeMmsCarrier()
        dispatcher(client, restartedCarrier).dispatch()
        assertTrue(restartedCarrier.calls.isEmpty())
    }

}
