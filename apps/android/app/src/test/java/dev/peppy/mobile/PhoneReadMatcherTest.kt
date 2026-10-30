package dev.peppy.mobile

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.app.RemoteInput
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PhoneReadMatcherTest {
    private val thread = ProviderThread(7, setOf(100L, 200L), setOf(50L, 100L, 200L, 300L))
    private fun notification(
        key: String = "key",
        times: Set<Long> = setOf(100L, 200L),
        summary: Boolean = false,
        actions: List<ReadActionFacts> = listOf(ReadActionFacts(ActionSource.VISIBLE, 0, true, 0, false)),
    ) = ConversationNotification(key, 1L, summary, times, actions)

    @Test fun uniqueCandidateRequiresUnreadAndAllMessageTimesInThread() {
        assertTrue(PhoneReadMatcher.match(thread, listOf(notification())) is ReadMatch.Ready)
        assertEquals(ReadMatch.NoCandidate, PhoneReadMatcher.match(thread, listOf(notification(times = setOf(100L, 999L)))))
        assertEquals(ReadMatch.NoCandidate, PhoneReadMatcher.match(thread, listOf(notification(times = setOf(100L)))))
    }

    @Test fun candidateIsAmbiguousAndGroupSummariesAreExcluded() {
        assertEquals(ReadMatch.Ambiguous, PhoneReadMatcher.match(thread, listOf(notification("a"), notification("b"))))
        assertEquals(ReadMatch.NoCandidate, PhoneReadMatcher.match(thread, listOf(notification(summary = true))))
    }

    @Test fun requiresExactlyOneSafeSemanticActionAcrossSources() {
        assertEquals(ReadMatch.NoAction, PhoneReadMatcher.match(thread, listOf(notification(actions = listOf(ReadActionFacts(ActionSource.VISIBLE, 0, false, 0, false))))))
        assertEquals(ReadMatch.NoAction, PhoneReadMatcher.match(thread, listOf(notification(actions = listOf(ReadActionFacts(ActionSource.VISIBLE, 0, true, 1, false))))))
        assertEquals(ReadMatch.NoAction, PhoneReadMatcher.match(thread, listOf(notification(actions = listOf(
            ReadActionFacts(ActionSource.VISIBLE, 0, true, 0, false), ReadActionFacts(ActionSource.INVISIBLE, 0, true, 0, false),
        )))))
        val match = PhoneReadMatcher.match(thread, listOf(notification(actions = listOf(ReadActionFacts(ActionSource.INVISIBLE, 2, true, 0, false)))))
        assertEquals(ActionSource.INVISIBLE, (match as ReadMatch.Ready).action.source)
    }

    @Test fun unsafeSecondSemanticActionStillMakesActionSelectionAmbiguous() {
        val actions = listOf(
            ReadActionFacts(ActionSource.VISIBLE, 0, true, 0, false, true),
            ReadActionFacts(ActionSource.INVISIBLE, 1, true, 0, false, false),
        )
        assertEquals(ReadMatch.NoAction, PhoneReadMatcher.match(thread, listOf(notification(actions = actions))))
    }

    @Test fun factsIncludesSelfMessagingTimesAndDoesNotPrintContent() {
        val context: Context = ApplicationProvider.getApplicationContext()
        val pending = PendingIntent.getBroadcast(context, 0, Intent("phone-read"), PendingIntent.FLAG_IMMUTABLE)
        val self = Person.Builder().setName("Me").build()
        val notification = NotificationCompat.Builder(context, "phone-read")
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .setStyle(NotificationCompat.MessagingStyle(self).addMessage("private body", 100L, self))
            .addAction(NotificationCompat.Action.Builder(0, "ignored", pending)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
                .setShowsUserInterface(false)
                .build())
            .build()
        val facts = PhoneReadMatcher.facts(StatusBarNotification("sms", "sms", 1, "tag", 0, 0, 0, notification, android.os.Process.myUserHandle(), 1L))
        assertEquals(setOf(100L), facts.messageTimes)
        assertEquals(ActionSource.VISIBLE, PhoneReadMatcher.markReadAction(facts)?.source)
        assertFalse(facts.toString().contains("private body"))
    }

    @Test fun factsRejectsNullAndActivityPendingIntents() {
        val context: Context = ApplicationProvider.getApplicationContext()
        val activity = PendingIntent.getActivity(context, 27, Intent(context, PhoneReadMatcherTest::class.java), PendingIntent.FLAG_IMMUTABLE)
        val activityNotification = NotificationCompat.Builder(context, "phone-read")
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .addAction(NotificationCompat.Action.Builder(0, "ignored", activity)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ).build())
            .build()
        val nullIntentNotification = NotificationCompat.Builder(context, "phone-read")
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .addAction(NotificationCompat.Action.Builder(0, "ignored", null)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ).build())
            .build()
        val activityFacts = PhoneReadMatcher.facts(StatusBarNotification("sms", "sms", 2, "a", 0, 0, 0, activityNotification, android.os.Process.myUserHandle(), 1L))
        val nullFacts = PhoneReadMatcher.facts(StatusBarNotification("sms", "sms", 3, "b", 0, 0, 0, nullIntentNotification, android.os.Process.myUserHandle(), 1L))
        assertFalse(activityFacts.actions.single().actionPermitted)
        assertFalse(nullFacts.actions.single().actionPermitted)
    }

    @Test fun factsRequireAnUnlockedDeviceWhenInvisibleAuthenticationMetadataIsLost() {
        val context: Context = ApplicationProvider.getApplicationContext()
        val pending = PendingIntent.getBroadcast(context, 28, Intent("phone-read-auth"), PendingIntent.FLAG_IMMUTABLE)
        val notification = NotificationCompat.Builder(context, "phone-read")
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .addInvisibleAction(NotificationCompat.Action.Builder(0, "ignored", pending)
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ)
                .setAuthenticationRequired(true)
                .build())
            .build()
        val convertedAction = NotificationCompat.getInvisibleActions(notification).single()
        assertFalse(convertedAction.isAuthenticationRequired)

        val facts = PhoneReadMatcher.facts(StatusBarNotification("sms", "sms", 4, "c", 0, 0, 0, notification, android.os.Process.myUserHandle(), 1L))
        assertTrue(facts.actions.single().actionPermitted)
        assertTrue(facts.actions.single().requiresUnlockedDevice)
    }
}
