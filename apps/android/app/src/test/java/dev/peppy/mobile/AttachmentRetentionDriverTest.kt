package dev.peppy.mobile

import java.lang.reflect.Proxy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeAttachmentUseKind
import uniffi.peppy_mobile_bindings.NativeAttachmentUseOperationKind
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativePendingAttachmentRetentionOperation

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class AttachmentRetentionDriverTest {
    private val attachmentId = "11111111-1111-1111-1111-111111111111"

    @Test fun registerAndReleaseExact410ReportRemovalWhileOther410Fails() {
        for (kind in NativeAttachmentUseOperationKind.values()) {
            val exact = fake(kind)
            LoopbackHttpServer { LoopbackHttpServer.Response(410, """{"code":"attachment_removed_by_storage_policy"}""") }.use { server ->
                assertEquals(
                    AttachmentRetentionDriver.Outcome.DRAINED,
                    AttachmentRetentionDriver(exact.client, GatewayHttp(server.origin, "token"), { true }).run(),
                )
            }
            assertEquals(listOf(attachmentId), exact.removed)

            val generic = fake(kind)
            LoopbackHttpServer { LoopbackHttpServer.Response(410, """{"code":"gone"}""") }.use { server ->
                assertThrows(GatewayTransportException::class.java) {
                    AttachmentRetentionDriver(generic.client, GatewayHttp(server.origin, "token"), { true }).run()
                }
            }
            assertEquals(emptyList<String>(), generic.removed)
        }
    }

    @Test fun emptyPendingPreparationUsesFiniteBudgetWithoutAdvertisingReady() {
        var prepares = 0
        var marked = 0
        val client = proxy { method, _ -> when (method) {
            "attachmentRetentionProtocolEnabled" -> true
            "prepareAttachmentRetentionOperations" -> { prepares++; emptyList<NativePendingAttachmentRetentionOperation>() }
            "attachmentRetentionPreparationPending" -> true
            "markAttachmentRetentionReconciled" -> { marked++; Unit }
            "toString" -> "pending-client"
            else -> throw UnsupportedOperationException(method)
        } }
        LoopbackHttpServer { throw AssertionError("pending preparation must not send readiness") }.use { server ->
            assertEquals(
                AttachmentRetentionDriver.Outcome.MORE,
                AttachmentRetentionDriver(client, GatewayHttp(server.origin, "token"), { true }).run(),
            )
        }
        assertEquals(4, prepares)
        assertEquals(0, marked)
    }

    private class Fake(val client: NativeClientInterface, val removed: MutableList<String>)

    private fun fake(kind: NativeAttachmentUseOperationKind): Fake {
        val removed = mutableListOf<String>()
        var prepared = false
        val operation = NativePendingAttachmentRetentionOperation(
            kind, "22222222-2222-2222-2222-222222222222", attachmentId, "33333333-3333-3333-3333-333333333333", NativeAttachmentUseKind.MESSAGE_HISTORY,
        )
        val client = proxy { method, args -> when (method) {
            "attachmentRetentionProtocolEnabled" -> true
            "prepareAttachmentRetentionOperations" -> if (prepared) emptyList<NativePendingAttachmentRetentionOperation>() else listOf(operation).also { prepared = true }
            "attachmentRetentionPreparationPending" -> false
            "reportAttachmentRemovedByStoragePolicy" -> { removed += args!![0] as String; Unit }
            "markAttachmentRetentionReconciled" -> Unit
            "attachmentRetentionReady" -> false
            "toString" -> "retention-client"
            else -> throw UnsupportedOperationException(method)
        } }
        return Fake(client, removed)
    }

    private fun proxy(handler: (String, Array<out Any?>?) -> Any?): NativeClientInterface =
        Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, args ->
            handler(method.name.substringBefore('-'), args)
        } as NativeClientInterface
}
