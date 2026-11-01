package dev.peppy.mobile

import java.lang.reflect.Proxy
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeClientInterface

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class ContactPhotoTransferTest {
    private val attachmentId = "11111111-1111-1111-1111-111111111111"
    private val policyDenied = """{"code":"operation_not_permitted"}"""

    @Test fun registrationExactTyped410ReportsRemovalWithoutAcknowledgement() {
        val fake = fakeClient(JSONObject().put("registrations", JSONArray().put(
            JSONObject().put("envelope_id", "envelope").put("attachment_id", attachmentId)
                .put("producer_device_id", "device").put("producer_sequence", "1"),
        )).toString())
        LoopbackHttpServer { LoopbackHttpServer.Response(410, """{"code":"attachment_removed_by_storage_policy"}""") }.use { server ->
            assertFalse(ContactPhotoTransfer(fake.client, GatewayHttp(server.origin, "token")).run())
        }
        assertEquals(listOf(attachmentId), fake.removed)
        assertEquals(0, fake.acks)
    }

    @Test fun reclaimExactTyped410ReportsRemovalWhileGeneric404AcknowledgesNormally() {
        data class Case(val status: Int, val body: String, val removed: Int, val acks: Int)
        for ((status, body, removed, acks) in listOf(
            Case(410, """{"code":"attachment_removed_by_storage_policy"}""", 1, 0),
            Case(404, """{"code":"not_found"}""", 0, 1),
        )) {
            val fake = fakeClient(JSONObject().put("reclaims", JSONArray().put(
                JSONObject().put("attachment_id", attachmentId).put("remote_object_id", attachmentId).put("release_after_cursor", "1"),
            )).toString())
            LoopbackHttpServer { request ->
                if (request.method == "GET") LoopbackHttpServer.Response(200, """{"compaction_supported":true,"compaction_generation":"2","replay_floor_cursor":"3"}""")
                else LoopbackHttpServer.Response(status, body)
            }.use { server -> ContactPhotoTransfer(fake.client, GatewayHttp(server.origin, "token")).run() }
            assertEquals(removed, fake.removed.size)
            assertEquals(acks, fake.acks)
        }
    }

    @Test fun registrationPolicyDenialPausesThePhaseWithoutAcknowledgementOrFurtherRequests() {
        val fake = fakeClient(JSONObject().put("registrations", JSONArray()
            .put(registrationRow("envelope-1"))
            .put(registrationRow("envelope-2")),
        ).toString())
        var registrations = 0
        LoopbackHttpServer {
            registrations++
            LoopbackHttpServer.Response(403, policyDenied)
        }.use { server ->
            assertThrows(GatewayAccountPausedException::class.java) {
                ContactPhotoTransfer(fake.client, GatewayHttp(server.origin, "token")).run()
            }
        }
        assertEquals("the pause stops the remaining registrations", 1, registrations)
        assertEquals(0, fake.acks)
        assertTrue(fake.removed.isEmpty())
    }

    @Test fun reclaimPolicyDenialPausesThePhaseWithoutAcknowledgement() {
        val fake = fakeClient(JSONObject().put("reclaims", JSONArray().put(
            JSONObject().put("attachment_id", attachmentId).put("remote_object_id", attachmentId).put("release_after_cursor", "1"),
        )).toString())
        var deletes = 0
        LoopbackHttpServer { request ->
            if (request.method == "GET") LoopbackHttpServer.Response(200, """{"compaction_supported":true,"compaction_generation":"2","replay_floor_cursor":"3"}""")
            else {
                deletes++
                LoopbackHttpServer.Response(403, policyDenied)
            }
        }.use { server ->
            assertThrows(GatewayAccountPausedException::class.java) {
                ContactPhotoTransfer(fake.client, GatewayHttp(server.origin, "token")).run()
            }
        }
        assertEquals(1, deletes)
        assertEquals(0, fake.acks)
        assertTrue(fake.removed.isEmpty())
    }

    private fun registrationRow(envelopeId: String) = JSONObject().put("envelope_id", envelopeId).put("attachment_id", attachmentId)
        .put("producer_device_id", "device").put("producer_sequence", "1")

    private class Fake(state: String) {
        val removed = mutableListOf<String>()
        var acks = 0
        val client = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, args ->
            when (method.name) {
                "contactPhotoTransferStateJson" -> state
                "reportAttachmentRemovedByStoragePolicy" -> { removed += args!![0] as String; Unit }
                "acknowledgeContactPhotoReferenceJson", "acknowledgeContactPhotoReclaimJson" -> { acks++; "{}" }
                "toString" -> "fake-contact-client"
                else -> throw UnsupportedOperationException(method.name)
            }
        } as NativeClientInterface
    }

    private fun fakeClient(state: String) = Fake(state)
}
