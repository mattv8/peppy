package dev.peppy.mobile

import uniffi.peppy_mobile_bindings.NativeAttachmentRetentionErrorClassification
import uniffi.peppy_mobile_bindings.NativeAttachmentUseOperationKind
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.attachmentRetentionCapabilityRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionPrepareLimitMax
import uniffi.peppy_mobile_bindings.attachmentRetentionRegisterRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionReleaseRequest
import uniffi.peppy_mobile_bindings.attachmentRetentionResponseReadLimitBytes
import uniffi.peppy_mobile_bindings.classifyAttachmentRetentionError
import uniffi.peppy_mobile_bindings.parseAttachmentRetentionResponse

/** Transports the core-owned retention protocol before producer work. */
internal class AttachmentRetentionDriver(
    private val client: NativeClientInterface,
    private val http: GatewayHttp,
    private val sessionCurrent: () -> Boolean,
) {
    enum class Outcome { DRAINED, LEGACY, DEFERRED, STALE, MORE }

    fun run(): Outcome {
        http.initializeRetentionProtocol(client.attachmentRetentionProtocolEnabled())
        if (!client.attachmentRetentionProtocolEnabled()) {
            val capability = attachmentRetentionCapabilityRequest(false)
            val response = http.retentionRequest(capability, attachmentRetentionResponseReadLimitBytes())
            if (!sessionCurrent()) return Outcome.STALE
            requireAuthentication(response)
            if (response.code == 404 || response.code == 405) return Outcome.LEGACY
            if (!response.ok) return Outcome.DEFERRED
            requireExpected(capability.expectedSuccessStatus, response)
            client.configureAttachmentRetentionProtocol(true)
            http.initializeRetentionProtocol(true)
        }

        repeat(MAX_PREPARATION_PASSES) {
            if (!sessionCurrent()) return Outcome.STALE
            val operations = client.prepareAttachmentRetentionOperations(attachmentRetentionPrepareLimitMax())
            if (operations.isEmpty()) {
                if (client.attachmentRetentionPreparationPending()) return@repeat
                client.markAttachmentRetentionReconciled()
                if (client.attachmentRetentionReady()) {
                    val capability = attachmentRetentionCapabilityRequest(true)
                    val response = http.retentionRequest(capability, attachmentRetentionResponseReadLimitBytes())
                    if (!sessionCurrent()) return Outcome.STALE
                    requireAuthentication(response)
                    requireExpected(capability.expectedSuccessStatus, response)
                }
                return Outcome.DRAINED
            }
            for (operation in operations) {
                if (!sessionCurrent()) return Outcome.STALE
                val request = when (operation.operation) {
                    NativeAttachmentUseOperationKind.REGISTER -> attachmentRetentionRegisterRequest(operation)
                    NativeAttachmentUseOperationKind.RELEASE -> attachmentRetentionReleaseRequest(operation)
                }
                val response = http.retentionRequest(request, attachmentRetentionResponseReadLimitBytes())
                if (!sessionCurrent()) return Outcome.STALE
                requireAuthentication(response)
                if (response.code == 410 && response.body != null &&
                    classifyAttachmentRetentionError(410u, response.body!!) == NativeAttachmentRetentionErrorClassification.REMOVED_BY_STORAGE_POLICY
                ) {
                    client.reportAttachmentRemovedByStoragePolicy(operation.attachmentId)
                    continue
                }
                requireExpected(request.expectedSuccessStatus, response)
                client.applyAttachmentRetentionResponse(
                    parseAttachmentRetentionResponse(response.body ?: throw GatewayTransportException("empty retention response")),
                )
            }
        }
        return Outcome.MORE
    }

    private fun requireAuthentication(response: HttpResult) {
        if (response.code == 401 || response.code == 403) throw GatewayAuthException()
    }

    private fun requireExpected(expected: UShort, response: HttpResult) {
        if (response.code != expected.toInt()) throw GatewayTransportException("retention HTTP ${response.code}")
        if (expected == 204.toUShort() && !response.body.isNullOrEmpty()) throw GatewayTransportException("retention 204 body")
    }

    private companion object { const val MAX_PREPARATION_PASSES = 4 }
}
