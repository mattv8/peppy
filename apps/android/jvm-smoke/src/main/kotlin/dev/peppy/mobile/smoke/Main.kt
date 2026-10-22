package dev.peppy.mobile.smoke

import java.io.File
import java.util.UUID
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeIncomingSms
import uniffi.peppy_mobile_bindings.NativeMmsAcquisitionInput
import uniffi.peppy_mobile_bindings.NativeMmsSource
import uniffi.peppy_mobile_bindings.NativeOpenConfig
import uniffi.peppy_mobile_bindings.createVaultMaterial
import uniffi.peppy_mobile_bindings.openNativeClient

fun main() {
    val passphrase = "mobile smoke passphrase"
    val vaultId = UUID.randomUUID().toString()
    val deviceId = UUID.randomUUID().toString()
    val database = File(System.getProperty("java.io.tmpdir"), "peppy-native-smoke-${UUID.randomUUID()}.db")
    val material = createVaultMaterial(vaultId, passphrase)
    val config = NativeOpenConfig(
        database.absolutePath,
        vaultId,
        deviceId,
        ByteArray(32) { 7 },
    )

    val client = openNativeClient(config)
    client.unlock(material.profileJson, material.headerJson, passphrase)
    val captured = client.captureIncoming(
        NativeIncomingSms(
            null,
            "+15551234567",
            "SQLCipher-backed generated Kotlin smoke",
            "kotlin-smoke-1",
            false,
        )
    )
    check(!captured.duplicate)
    check(client.pendingOutboxJson().size == 1)
    check(client.requestMmsRecheck("generation", "android-mms-provider", 32uL) == 0uL)
    try {
        client.requestMmsRecheck("generation", "android-mms-provider", 50uL)
        error("MMS recheck accepted a request above the core bound")
    } catch (_: MobileBindingsException.InvalidRequest) {
        // The Android caller must remain at or below the real core-owned bound.
    }
    println("Kotlin MMS recheck bounds passed (32 accepted, 50 rejected)")

    client.planMmsBackfillScans("jni-generation", "sim-1", "42")
    client.beginMmsAcquisitionOutcome(mmsInput("first-pdu"))
    client.beginMmsAcquisitionOutcome(mmsInput("second-pdu"))
    check(client.mmsBackfillStatus("jni-generation", "sim-1").quarantinedCount == 1uL)
    println("Kotlin MMS quarantine status passed (count 1)")
    client.dispose()

    val reopened = openNativeClient(config)
    reopened.unlock(material.profileJson, material.headerJson, passphrase)
    val messages = reopened.messages(captured.conversationId)
    check(messages.size == 1)
    check(messages.single().body == "SQLCipher-backed generated Kotlin smoke")
    check(reopened.markSeen(captured.messageId))
    reopened.dispose()
    try {
        reopened.listConversations()
        error("closed NativeClient unexpectedly accepted a read")
    } catch (_: MobileBindingsException.Closed) {
        println("Kotlin SQLCipher UniFFI smoke passed")
    }
}


private fun mmsInput(transactionId: String) = NativeMmsAcquisitionInput(
    source = NativeMmsSource("jni-generation", "sim-1", "42", "7"),
    incoming = true,
    senderAddress = "+15551234567",
    recipients = listOf("+15557654321"),
    subject = "JNI MMS boundary",
    body = "message body",
    imported = false,
    observedAtMs = 1L,
    transactionId = transactionId,
)
