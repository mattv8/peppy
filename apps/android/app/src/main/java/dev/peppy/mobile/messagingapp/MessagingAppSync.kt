package dev.peppy.mobile.messagingapp

import android.content.Context
import android.util.Log
import dev.peppy.mobile.advanced.blockedSendersAccess
import dev.peppy.mobile.mms.RestrictedMessageAccess
import kotlinx.coroutines.CancellationException
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeSenderUnblockOutcome

data class SenderUnblockWork(val requestId: String, val numbers: List<String>)

enum class SenderUnblockResult { Unblocked, NotBlocked, NotAllowed, Failed }

interface MessagingAppClient {
    fun publishSenderBlocklist(numbers: List<String>?, canUnblock: Boolean): Boolean
    fun reconcileProviderArchive(archivedThreadIds: List<String>): UInt
    fun pendingSenderUnblocks(): List<SenderUnblockWork>
    fun beginSenderUnblock(requestId: String): Boolean
    fun interruptedSenderUnblocks(): List<SenderUnblockWork>
    fun completeSenderUnblock(requestId: String, outcome: SenderUnblockResult)
}

/** Bounded reconciliation of the companion messaging app from the gateway worker thread. */
internal class MessagingAppSync(
    private val archiveRead: () -> ProviderArchiveRead,
    private val blocklistAccess: () -> RestrictedMessageAccess,
    private val blocklistRead: () -> BlocklistRead,
    private val unblock: (String) -> UnblockOutcome,
    private val isBlocked: (String) -> Boolean?,
    private val client: MessagingAppClient,
) {
    fun run() {
        runStep(::syncBlocklist)
        runStep(::syncUnblocks)
        runStep(::syncArchive)
    }

    private fun runStep(step: () -> Unit) {
        try {
            step()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            runCatching { Log.w(TAG, "Messaging app reconciliation step failed") }
        }
    }

    private fun syncBlocklist() {
        when (blocklistAccess()) {
            RestrictedMessageAccess.ALLOWED -> when (val blocklist = blocklistRead()) {
                is BlocklistRead.Entries -> if (!blocklist.truncated) {
                    client.publishSenderBlocklist(
                        blocklist.entries.map { it.e164 ?: it.original },
                        canUnblock = true,
                    )
                }
                BlocklistRead.NotAllowed -> client.publishSenderBlocklist(null, canUnblock = false)
                BlocklistRead.Unavailable -> Unit
            }
            RestrictedMessageAccess.NOT_ALLOWED -> client.publishSenderBlocklist(null, canUnblock = false)
            RestrictedMessageAccess.UNSUPPORTED, RestrictedMessageAccess.UNKNOWN -> Unit
        }
    }

    private fun syncUnblocks() {
        when (blocklistAccess()) {
            RestrictedMessageAccess.ALLOWED -> {
                reconcileInterrupted()
                unblockPending()
            }
            RestrictedMessageAccess.NOT_ALLOWED -> completeWithoutAccess()
            RestrictedMessageAccess.UNSUPPORTED, RestrictedMessageAccess.UNKNOWN -> Unit
        }
    }

    private fun syncArchive() {
        (archiveRead() as? ProviderArchiveRead.Available)
            ?.takeUnless { it.truncated }
            ?.let { client.reconcileProviderArchive(it.threadIds.map(Long::toString)) }
    }

    private fun completeWithoutAccess() {
        client.interruptedSenderUnblocks().forEach {
            client.completeSenderUnblock(it.requestId, SenderUnblockResult.NotAllowed)
        }
        client.pendingSenderUnblocks().forEach {
            if (client.beginSenderUnblock(it.requestId)) {
                client.completeSenderUnblock(it.requestId, SenderUnblockResult.NotAllowed)
            }
        }
    }

    private fun reconcileInterrupted() {
        client.interruptedSenderUnblocks().forEach { work ->
            val blocked = work.numbers.map(isBlocked)
            if (blocked.any { it == null }) return@forEach
            val outcome = if (blocked.none { it == true }) SenderUnblockResult.Unblocked else SenderUnblockResult.Failed
            client.completeSenderUnblock(work.requestId, outcome)
        }
    }

    private fun unblockPending() {
        client.pendingSenderUnblocks().forEach { work ->
            if (!client.beginSenderUnblock(work.requestId)) return@forEach
            client.completeSenderUnblock(work.requestId, aggregate(work.numbers.map(unblock)))
        }
    }

    private fun aggregate(outcomes: List<UnblockOutcome>): SenderUnblockResult = when {
        outcomes.any { it == UnblockOutcome.Failed } -> SenderUnblockResult.Failed
        outcomes.any { it == UnblockOutcome.NotAllowed } -> SenderUnblockResult.NotAllowed
        outcomes.all { it == UnblockOutcome.NotBlocked } -> SenderUnblockResult.NotBlocked
        else -> SenderUnblockResult.Unblocked
    }

    companion object {
        private const val TAG = "PeppyMessagingApp"

        internal fun forGateway(context: Context, nativeClient: NativeClientInterface): MessagingAppSync {
            val blocklist = SystemBlocklist(
                context,
                access = { blockedSendersAccess(context) == RestrictedMessageAccess.ALLOWED },
            )
            return MessagingAppSync(
                archiveRead = ProviderArchiveReader(context.contentResolver)::archivedThreadIds,
                blocklistAccess = { blockedSendersAccess(context) },
                blocklistRead = blocklist::read,
                unblock = blocklist::unblock,
                isBlocked = blocklist::isBlocked,
                client = NativeMessagingAppClient(nativeClient),
            )
        }
    }

}

private class NativeMessagingAppClient(private val client: NativeClientInterface) : MessagingAppClient {
    override fun publishSenderBlocklist(numbers: List<String>?, canUnblock: Boolean): Boolean =
        client.publishSenderBlocklist(numbers, canUnblock)

    override fun reconcileProviderArchive(archivedThreadIds: List<String>): UInt =
        client.reconcileProviderArchive(archivedThreadIds)

    override fun pendingSenderUnblocks(): List<SenderUnblockWork> = client.pendingSenderUnblocks().map {
        SenderUnblockWork(it.requestId, it.numbers)
    }

    override fun beginSenderUnblock(requestId: String): Boolean = client.beginSenderUnblock(requestId)

    override fun interruptedSenderUnblocks(): List<SenderUnblockWork> = client.interruptedSenderUnblocks().map {
        SenderUnblockWork(it.requestId, it.numbers)
    }

    override fun completeSenderUnblock(requestId: String, outcome: SenderUnblockResult) {
        client.completeSenderUnblock(
            requestId,
            when (outcome) {
                SenderUnblockResult.Unblocked -> NativeSenderUnblockOutcome.UNBLOCKED
                SenderUnblockResult.NotBlocked -> NativeSenderUnblockOutcome.NOT_BLOCKED
                SenderUnblockResult.NotAllowed -> NativeSenderUnblockOutcome.NOT_ALLOWED
                SenderUnblockResult.Failed -> NativeSenderUnblockOutcome.FAILED
            },
        )
    }
}
