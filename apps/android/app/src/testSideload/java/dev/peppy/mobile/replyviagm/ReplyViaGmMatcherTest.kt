package dev.peppy.mobile.replyviagm

import android.app.Notification
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Process
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.RemoteInput
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ReplyViaGmMatcherTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Test fun uniqueOneToOneRcsReplyMatches() = assertNotNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification()), canonicalizer))
    @Test fun groupMissingRcsAndDestinationMismatchDoNotMatch() {
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(group = true)), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(rcs = false)), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(destination = "+15555550101")), canonicalizer))
    }
    @Test fun normalizationAndAmbiguityAreStrict() {
        assertNotNull(ReplyViaGmMatcher.match("555-555-0100", arrayOf(notification()), canonicalizer))
        assertNull(ReplyViaGmMatcher.match("5550100", arrayOf(notification()), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(destination = "+15555550101")), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(id = 1), notification(id = 2)), canonicalizer))
    }
    @Test fun requiresGoogleMessagesAndOneFreeFormReplyInput() {
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(packageName = "other")), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(reply = false)), canonicalizer))
        assertNull(ReplyViaGmMatcher.match(RECIPIENT, arrayOf(notification(inputs = 2)), canonicalizer))
    }

    private fun notification(
        id: Int = 1,
        packageName: String = "com.google.android.apps.messaging",
        group: Boolean = false,
        rcs: Boolean = true,
        destination: String = RECIPIENT,
        reply: Boolean = true,
        inputs: Int = 1,
    ): StatusBarNotification {
        val builder = NotificationCompat.Builder(context, "reply")
        if (reply) {
            val action = NotificationCompat.Action.Builder(0, "Reply", PendingIntent.getBroadcast(context, id, Intent("reply-$id"), PendingIntent.FLAG_IMMUTABLE))
                .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
            repeat(inputs) { action.addRemoteInput(RemoteInput.Builder("reply-$it").setAllowFreeFormInput(true).build()) }
            builder.addAction(action.build())
        }
        val notification = builder.build().apply {
            extras.putBoolean(Notification.EXTRA_IS_GROUP_CONVERSATION, group)
            extras.putString("extra_im_notification_participant_normalized_destination", destination)
            if (rcs) extras.putString("extra_im_notification_latest_rcs_message_id", "rcs-$id")
        }
        return StatusBarNotification(packageName, packageName, id, "reply-$id", 0, 0, 0, notification, Process.myUserHandle(), id.toLong())
    }

    private val canonicalizer = ReplyViaGmPhoneCanonicalizer { address ->
        mapOf(RECIPIENT to RECIPIENT, "555-555-0100" to RECIPIENT, "+15555550101" to "+15555550101")[address]
    }
    private companion object { const val RECIPIENT = "+15555550100" }
}
