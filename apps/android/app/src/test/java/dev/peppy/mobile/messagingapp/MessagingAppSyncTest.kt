package dev.peppy.mobile.messagingapp

import dev.peppy.mobile.mms.RestrictedMessageAccess
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class MessagingAppSyncTest {
    @Test fun completeReadsPublishAndReconcileArchive() {
        val client = FakeClient()
        sync(
            client = client,
            archive = ProviderArchiveRead.Available(linkedSetOf(7L), truncated = false),
            access = RestrictedMessageAccess.ALLOWED,
            blocklist = BlocklistRead.Entries(listOf(BlocklistEntry("555", "+15555550100")), truncated = false),
        ).run()

        assertEquals(listOf("7"), client.archivedThreadIds)
        assertEquals(listOf("+15555550100") to true, client.published.single())
        assertEquals(listOf("blocklist", "archive"), client.events)
    }

    @Test fun incompleteProviderReadsDoNotPublish() {
        val client = FakeClient()
        sync(
            client = client,
            archive = ProviderArchiveRead.Available(linkedSetOf(7L), truncated = true),
            access = RestrictedMessageAccess.ALLOWED,
            blocklist = BlocklistRead.Entries(emptyList(), truncated = true),
        ).run()

        assertTrue(client.archivedThreadIds.isEmpty())
        assertTrue(client.published.isEmpty())
    }

    @Test fun unavailableAccessRetiresWorkWithoutCallingTheProvider() {
        val client = FakeClient(
            pending = listOf(SenderUnblockWork("pending", listOf("+1"))),
            interrupted = listOf(SenderUnblockWork("interrupted", listOf("+2"))),
        )
        sync(client, access = RestrictedMessageAccess.NOT_ALLOWED).run()

        assertEquals(listOf(null to false), client.published)
        assertEquals(
            setOf("pending" to SenderUnblockResult.NotAllowed, "interrupted" to SenderUnblockResult.NotAllowed),
            client.completed.toSet(),
        )
        assertTrue(client.unblocked.isEmpty())
        assertEquals(listOf("pending"), client.begun)
    }

    @Test fun interruptedWorkReconcilesWithoutUnblockingAndPendingWorkUsesOnePermit() {
        val client = FakeClient(
            pending = listOf(
                SenderUnblockWork("skip", listOf("+1")),
                SenderUnblockWork("run", listOf("+2", "+3")),
            ),
            interrupted = listOf(
                SenderUnblockWork("cleared", listOf("+4")),
                SenderUnblockWork("still-blocked", listOf("+5")),
            ),
            permits = mapOf("skip" to false, "run" to true),
        )
        sync(
            client = client,
            access = RestrictedMessageAccess.ALLOWED,
            blocklist = BlocklistRead.Unavailable,
            isBlocked = { it == "+5" },
            unblock = { number ->
                client.unblocked += number
                when (number) {
                    "+2" -> UnblockOutcome.NotBlocked
                    else -> UnblockOutcome.Unblocked
                }
            },
        ).run()

        assertEquals(listOf("+2", "+3"), client.unblocked)
        assertEquals(
            setOf(
                "cleared" to SenderUnblockResult.Unblocked,
                "still-blocked" to SenderUnblockResult.Failed,
                "run" to SenderUnblockResult.Unblocked,
            ),
            client.completed.toSet(),
        )
    }

    @Test fun unavailableInterruptedReconciliationLeavesTheRequestForANextPass() {
        val client = FakeClient(interrupted = listOf(SenderUnblockWork("interrupted", listOf("+1"))))

        sync(client = client, access = RestrictedMessageAccess.ALLOWED, isBlocked = { null }).run()

        assertTrue(client.completed.isEmpty())
    }

    @Test fun failedStepsDoNotSkipLaterSteps() {
        val client = FakeClient(pending = listOf(SenderUnblockWork("pending", listOf("+1"))))

        MessagingAppSync(
            archiveRead = { ProviderArchiveRead.Available(linkedSetOf(7L), truncated = false) },
            blocklistAccess = { RestrictedMessageAccess.ALLOWED },
            blocklistRead = { error("blocklist unavailable") },
            unblock = { UnblockOutcome.NotBlocked },
            isBlocked = { false },
            client = client,
        ).run()

        assertEquals(listOf("pending" to SenderUnblockResult.NotBlocked), client.completed)
        assertEquals(listOf("7"), client.archivedThreadIds)
    }

    private fun sync(
        client: FakeClient,
        archive: ProviderArchiveRead = ProviderArchiveRead.Unavailable,
        access: RestrictedMessageAccess,
        blocklist: BlocklistRead = BlocklistRead.Unavailable,
        unblock: (String) -> UnblockOutcome = { UnblockOutcome.Unblocked },
        isBlocked: (String) -> Boolean? = { false },
    ) = MessagingAppSync({ archive }, { access }, { blocklist }, unblock, isBlocked, client)

    private class FakeClient(
        private val pending: List<SenderUnblockWork> = emptyList(),
        private val interrupted: List<SenderUnblockWork> = emptyList(),
        private val permits: Map<String, Boolean> = emptyMap(),
    ) : MessagingAppClient {
        val published = mutableListOf<Pair<List<String>?, Boolean>>()
        val archivedThreadIds = mutableListOf<String>()
        val unblocked = mutableListOf<String>()
        val completed = mutableListOf<Pair<String, SenderUnblockResult>>()
        val begun = mutableListOf<String>()
        val events = mutableListOf<String>()
        private val started = interrupted.mapTo(mutableSetOf()) { it.requestId }

        override fun publishSenderBlocklist(numbers: List<String>?, canUnblock: Boolean): Boolean {
            published += numbers to canUnblock
            events += "blocklist"
            return true
        }

        override fun reconcileProviderArchive(archivedThreadIds: List<String>): UInt {
            this.archivedThreadIds += archivedThreadIds
            events += "archive"
            return 0u
        }

        override fun pendingSenderUnblocks(): List<SenderUnblockWork> = pending
        override fun beginSenderUnblock(requestId: String): Boolean = (permits[requestId] ?: true).also {
            if (it) {
                begun += requestId
                started += requestId
            }
        }
        override fun interruptedSenderUnblocks(): List<SenderUnblockWork> = interrupted
        override fun completeSenderUnblock(requestId: String, outcome: SenderUnblockResult) {
            check(started.remove(requestId)) { "complete requires begin" }
            completed += requestId to outcome
        }
    }
}
