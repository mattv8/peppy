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
import java.lang.reflect.Proxy
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.NativeCarrierCommand
import uniffi.peppy_mobile_bindings.NativeClientInterface
import uniffi.peppy_mobile_bindings.NativeMessage
import uniffi.peppy_mobile_bindings.NativePermit
import uniffi.peppy_mobile_bindings.NativePermitState
import uniffi.peppy_mobile_bindings.NativeSendResult
import dev.peppy.mobile.telephony.SimRoute

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ReplyViaGmDispatcherTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()

    @Test fun unavailableAndIneligibleCommandsAreUntouched() {
        val events = mutableListOf<String>()
        dispatch(events, settings = false).dispatch(10)
        dispatch(events, command = command(body = "", attachments = listOf("a"))).dispatch(10)
        assertEquals(emptyList<String>(), events)
    }

    @Test fun eligibleOrdersPermitSubmissionSendAndResult() {
        val events = mutableListOf<String>()
        val result = dispatch(events).dispatch(10)
        assertEquals(listOf("permit", "submission", "send", "SUBMITTED"), events)
        assertEquals(1, result.submitted)
    }

    @Test fun deniedPermitAndSubmissionFailureDoNotSend() {
        val denied = mutableListOf<String>()
        dispatch(denied, permit = NativePermitState.ALREADY_ATTEMPTED).dispatch(10)
        assertEquals(listOf("permit"), denied)
        val failed = mutableListOf<String>()
        dispatch(failed, submissionFails = true).dispatch(10)
        assertEquals(listOf("permit", "submission", "FAILED_BEFORE_SUBMISSION"), failed)
    }

    @Test fun canceledSendIsTerminalAndBudgetCountsHandledCommands() {
        val events = mutableListOf<String>()
        val result = dispatch(events, cancelSend = true, commands = listOf(command("one"), command("two"))).dispatch(1)
        assertEquals(listOf("permit", "submission", "send", "FAILED_BEFORE_SUBMISSION"), events)
        assertEquals(1, result.refusedBeforeCarrier)
        assertEquals(true, result.moreRemaining)
    }

    @Test fun submittedRecordFailureAfterSendNeverMarksTheReplyFailed() {
        val events = mutableListOf<String>()
        val result = dispatch(events, submittedRecordThrows = true).dispatch(10)
        assertEquals(listOf("permit", "submission", "send", "SUBMITTED"), events)
        assertEquals(0, result.refusedBeforeCarrier)
    }

    @Test fun unexpectedSendExceptionLeavesTheOutcomeUnknown() {
        val events = mutableListOf<String>()
        val result = dispatch(events, sendThrows = IllegalStateException("send")).dispatch(10)
        assertEquals(listOf("permit", "submission", "send"), events)
        assertEquals(0, result.submitted)
        assertEquals(0, result.refusedBeforeCarrier)
    }

    @Test fun notificationGoneBeforeSendFailsWithoutSending() {
        val events = mutableListOf<String>()
        var reads = 0
        dispatch(events, notifications = { if (reads++ < 2) arrayOf(notification()) else emptyArray() }).dispatch(10)
        assertEquals(listOf("permit", "submission", "FAILED_BEFORE_SUBMISSION"), events)
    }

    @Test fun secondReplyAfterTheNotificationClearsIsLeftForCarrierDispatch() {
        val events = mutableListOf<String>()
        var sent = false
        val first = command(id = "one")
        val second = command(id = "two")
        dispatch(
            events,
            commands = listOf(first, second),
            permitFor = { id -> listOf(first, second).single { it.commandId == id } },
            onSend = { sent = true },
            notifications = { if (sent) emptyArray() else arrayOf(notification()) },
        ).dispatch(10)
        assertEquals(listOf("permit", "submission", "send", "SUBMITTED"), events)
    }

    @Test fun moreThanOneActiveModemIsIneligible() {
        val events = mutableListOf<String>()
        dispatch(events, activeModems = 2).dispatch(10)
        dispatch(events, activeModems = null).dispatch(10)
        assertEquals(emptyList<String>(), events)
    }

    @Test fun commandOnAnUnavailableRouteIsIneligible() {
        val events = mutableListOf<String>()
        dispatch(events, command = command(route = "other")).dispatch(10)
        assertEquals(emptyList<String>(), events)
    }

    @Test fun groupNotificationsAreIneligible() {
        val missingFlag = mutableListOf<String>()
        dispatch(missingFlag, notifications = { arrayOf(notification(groupFlag = null)) }).dispatch(10)
        assertEquals(emptyList<String>(), missingFlag)
        val summary = mutableListOf<String>()
        dispatch(summary, notifications = { arrayOf(notification(groupSummary = true)) }).dispatch(10)
        assertEquals(emptyList<String>(), summary)
        val group = mutableListOf<String>()
        dispatch(group, notifications = { arrayOf(notification(groupFlag = true)) }).dispatch(10)
        assertEquals(emptyList<String>(), group)
    }

    private fun dispatch(
        events: MutableList<String>, settings: Boolean = true, permit: NativePermitState = NativePermitState.PERMIT,
        submissionFails: Boolean = false, cancelSend: Boolean = false, command: NativeCarrierCommand = command(), commands: List<NativeCarrierCommand> = listOf(command),
        submittedRecordThrows: Boolean = false, sendThrows: Exception? = null, activeModems: Int? = 1,
        notifications: () -> Array<StatusBarNotification> = { arrayOf(notification()) },
        permitFor: (String) -> NativeCarrierCommand = { command },
        onSend: () -> Unit = {},
    ): ReplyViaGmDispatcher {
        val client = Proxy.newProxyInstance(javaClass.classLoader, arrayOf(NativeClientInterface::class.java)) { _, method, arguments ->
            when (method.name) {
                "pendingCommands" -> commands
                "beginSendAttempt" -> {
                    events += "permit"
                    NativePermit(permit, permitFor(arguments?.get(0) as String).takeIf { permit == NativePermitState.PERMIT }, null)
                }
                "recordSendResult" -> {
                    val result = arguments?.get(1) as NativeSendResult
                    events += result.name
                    if (submittedRecordThrows && result == NativeSendResult.SUBMITTED) throw IllegalStateException("record")
                    null
                }
                else -> null
            }
        } as NativeClientInterface
        return ReplyViaGmDispatcher(
            context, client,
            ReplyViaGmSettings { settings },
            ReplyViaGmSubmissionRecorder { _, _ -> events += "submission"; if (submissionFails) error("submission") },
            accessGranted = { true }, listenerConnected = { true }, notifications = notifications,
            sender = { _, _ ->
                events += "send"
                if (cancelSend) throw PendingIntent.CanceledException()
                sendThrows?.let { throw it }
                onSend()
            },
            routes = listOf(SimRoute("route", 1, "SIM")),
            canonicalizer = ReplyViaGmPhoneCanonicalizer { address -> if (address == "+15555550100") address else null },
            activeModemCount = { activeModems },
        )
    }

    private fun command(body: String = "body", attachments: List<String> = emptyList(), route: String = "route", id: String = "command"): NativeCarrierCommand {
        val message = NativeMessage("message-$id", "conversation", null, listOf("+15555550100"), body, null, "sms", attachments, false, false, null, null)
        return NativeCarrierCommand(id, route, message)
    }

    private fun notification(groupFlag: Boolean? = false, groupSummary: Boolean = false): StatusBarNotification {
        val action = NotificationCompat.Action.Builder(0, "Reply", PendingIntent.getBroadcast(context, 1, Intent("reply"), PendingIntent.FLAG_IMMUTABLE))
            .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
            .addRemoteInput(RemoteInput.Builder("reply").setAllowFreeFormInput(true).build()).build()
        val notification = NotificationCompat.Builder(context, "reply").addAction(action).build().apply {
            groupFlag?.let { extras.putBoolean(Notification.EXTRA_IS_GROUP_CONVERSATION, it) }
            if (groupSummary) flags = flags or Notification.FLAG_GROUP_SUMMARY
            extras.putString("extra_im_notification_latest_rcs_message_id", "rcs")
            extras.putString("extra_im_notification_participant_normalized_destination", "+15555550100")
        }
        return StatusBarNotification("com.google.android.apps.messaging", "gm", 1, "reply", 0, 0, 0, notification, Process.myUserHandle(), 1L)
    }
}
