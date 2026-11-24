package dev.peppy.mobile

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.RemoteInput
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.diagnostics.NotificationMarkReadDiagnostics
import dev.peppy.mobile.diagnostics.SyncDiagnostics
import dev.peppy.mobile.diagnostics.SyncTraceEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.peppy_mobile_bindings.MobileBindingsException
import uniffi.peppy_mobile_bindings.NativeNotificationMarkRead
import uniffi.peppy_mobile_bindings.NativeNotificationTarget

/**
 * The phone's Mark as read path. The core is a fake that models one-use requests; PendingIntent
 * sends go through the runner's seam, so tests observe each send without a real broadcast.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class NotificationMarkReadTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun captureReportsMarkReadOnlyForOneSafeSemanticAction() {
        assertTrue(captured(notification(action())).markReadSupported)
        assertFalse(captured(notification(action(semantic = NotificationCompat.Action.SEMANTIC_ACTION_NONE))).markReadSupported)
        assertFalse(captured(notification(action(), action())).markReadSupported)
        assertFalse(captured(notification(action(remoteInput = true))).markReadSupported)
    }

    @Test
    fun staleInstanceLeavesRequestPending() {
        val current = notification(action())
        val fixture = Fixture(
            active = listOf(current),
            requests = listOf(request(current, instance = "${current.key}:${current.postTime - 1}")),
        )

        assertEquals(0, fixture.run())
        assertEquals(emptyList<String>(), fixture.events)
        assertEquals(1, fixture.core.outstanding.size)
    }

    @Test
    fun lockedDeviceLeavesRequestPendingWhenActionNeedsUnlock() {
        val current = notification(action(), invisible = true)
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)), unlocked = false)

        assertEquals(0, fixture.run())
        assertEquals(emptyList<String>(), fixture.events)
        assertEquals(1, fixture.core.outstanding.size)

        // Positive control: the same request runs once the device is unlocked.
        fixture.os.unlocked = true
        assertEquals(1, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)
    }

    @Test
    fun settingOffSendsNothing() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)), permitted = false)

        assertEquals(0, fixture.run())
        assertEquals(emptyList<String>(), fixture.events)
        assertEquals(1, fixture.core.outstanding.size)

        // Positive control: the same request is sent once policy allows it.
        fixture.permitted = true
        assertEquals(1, fixture.run())
    }

    @Test
    fun policyRevokedBetweenFirstCheckAndBeginSendsNothing() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        var checks = 0

        // The first check passes; the re-check before begin fails.
        val effects = fixture.effects(allow = { checks++ == 0 })

        assertEquals(0, effects.run())
        assertEquals(2, checks)
        assertEquals(emptyList<String>(), fixture.events)
        assertEquals(1, fixture.core.outstanding.size)
    }

    @Test
    fun instanceReplacedAfterBeginIsNotSent() {
        val current = notification(action())
        val reposted = notification(action(), postTime = 2_000L)
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        // The app reposts the same key after the request is consumed but before the send.
        fixture.core.onBegin = { fixture.os.notifications = listOf(reposted) }

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin"), fixture.events)
        assertEquals(0, fixture.sent.size)
        assertEquals(emptyList<NativeNotificationMarkRead>(), fixture.core.outstanding)
    }

    @Test
    fun securityExceptionStopsTheBatchAndLeavesLaterRequestsPending() {
        val first = notification(action())
        val second = notification(action(), id = 2)
        val fixture = Fixture(
            active = listOf(first, second),
            requests = listOf(request(first, id = "request-1"), request(second, id = "request-2")),
            sendFailure = SecurityException("denied"),
        )

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)
        assertEquals(listOf("request-2"), fixture.core.outstanding.map { it.id })
    }

    @Test
    fun bindingsExceptionStopsTheBatchAndLeavesLaterRequestsPending() {
        val first = notification(action())
        val second = notification(action(), id = 2)
        val fixture = Fixture(
            active = listOf(first, second),
            requests = listOf(request(first, id = "request-1"), request(second, id = "request-2")),
        )
        fixture.core.beginFailure = MobileBindingsException.Closed()

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin"), fixture.events)
        assertEquals(listOf("request-1", "request-2"), fixture.core.outstanding.map { it.id })
    }

    @Test
    fun requestIsConsumedBeforeSendAndNeverRetried() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))

        assertEquals(1, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)
        assertEquals(0, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)
        assertEquals(1, fixture.sent.size)
    }

    @Test
    fun consumedElsewhereIsNotSent() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        fixture.core.beginAccepts = false

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin"), fixture.events)
        assertEquals(0, fixture.sent.size)
    }

    @Test
    fun missingSafeActionConsumesWithoutDismissing() {
        val current = notification(action(semantic = NotificationCompat.Action.SEMANTIC_ACTION_NONE))
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin"), fixture.events)
        assertEquals(0, fixture.sent.size)
        // The request is consumed and the notification is left as it was: the runner never dismisses.
        assertEquals(emptyList<NativeNotificationMarkRead>(), fixture.core.outstanding)
        assertEquals(listOf(current), fixture.os.notifications)
    }

    @Test
    fun canceledPendingIntentIsNotRetried() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)), cancelSends = true)

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)

        assertEquals(0, fixture.run())
        assertEquals(listOf("begin", "send"), fixture.events)
        assertEquals(1, fixture.sent.size)
    }

    @Test
    fun countersCountAttemptedSentAndCanceledRequests() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        val counters = NotificationMarkReadDiagnostics.Counters()

        assertEquals(1, fixture.effects(counters = counters).run())
        assertEquals(1, counters.attempted)
        assertEquals(1, counters.sent)
        assertEquals(0, counters.canceled)
        assertEquals(0, counters.failed)

        val canceling = Fixture(active = listOf(current), requests = listOf(request(current)), cancelSends = true)
        val canceled = NotificationMarkReadDiagnostics.Counters()
        assertEquals(0, canceling.effects(counters = canceled).run())
        assertEquals(1, canceled.attempted)
        assertEquals(0, canceled.sent)
        assertEquals(1, canceled.canceled)
        assertEquals(0, canceled.failed)
    }

    @Test
    fun countersRecordStoppedBatchesAsFailedWithoutContent() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)), sendFailure = SecurityException("denied"))
        val counters = NotificationMarkReadDiagnostics.Counters()

        assertEquals(0, fixture.effects(counters = counters).run())
        assertEquals(1, counters.attempted)
        assertEquals(0, counters.sent)
        assertEquals(1, counters.failed)

        val pendingFixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        pendingFixture.core.pendingFailure = MobileBindingsException.Closed()
        val pendingCounters = NotificationMarkReadDiagnostics.Counters()
        assertEquals(0, pendingFixture.effects(counters = pendingCounters).run())
        assertEquals(0, pendingCounters.attempted)
        assertEquals(1, pendingCounters.failed)
    }

    @Test
    fun passTraceCarriesOnlyNumericMarkReadCounters() {
        val current = notification(action())
        val fixture = Fixture(active = listOf(current), requests = listOf(request(current)))
        val lines = mutableListOf<String>()

        SyncDiagnostics.withTestConfiguration(true, { 1L }, { 1L }, lines::add) {
            assertEquals(1, fixture.run())
            // An idle pass with nothing examined records nothing.
            assertEquals(0, fixture.run())
        }

        val line = lines.single()
        assertTrue(line.startsWith("event=${SyncTraceEvent.NOTIFICATION_MARK_READ_PASS.code} "))
        assertTrue(line.endsWith(" 64=1 65=1 66=0 67=0"))
        assertFalse(line.contains(current.key))
        assertFalse(line.contains("request-1"))
    }

    private fun captured(notification: StatusBarNotification): NotificationExtraction.Captured =
        checkNotNull(NotificationExtraction.extract(context, notification))

    private fun notification(
        vararg actions: NotificationCompat.Action,
        invisible: Boolean = false,
        id: Int = 1,
        postTime: Long = 2L,
    ): StatusBarNotification {
        val builder = NotificationCompat.Builder(context, "mark-read")
            .setSmallIcon(android.R.drawable.sym_def_app_icon)
            .setContentTitle("Title")
            .setContentText("Body")
        for (action in actions) {
            if (invisible) builder.addInvisibleAction(action) else builder.addAction(action)
        }
        return StatusBarNotification(
            "example.chat", "example.chat", id, "tag", 1000, 0, 0, builder.build(),
            android.os.Process.myUserHandle(), postTime,
        )
    }

    private fun action(
        semantic: Int = NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ,
        remoteInput: Boolean = false,
    ): NotificationCompat.Action {
        val builder = NotificationCompat.Action.Builder(0, "read", pendingIntent())
            .setSemanticAction(semantic)
            .setShowsUserInterface(false)
        if (remoteInput) builder.addRemoteInput(RemoteInput.Builder("reply").build())
        return builder.build()
    }

    private fun pendingIntent(): PendingIntent = PendingIntent.getBroadcast(
        context, nextRequestCode++, Intent("mark-read-fired").setPackage(context.packageName), PendingIntent.FLAG_IMMUTABLE,
    )

    private fun request(
        notification: StatusBarNotification,
        id: String = "request-1",
        instance: String = "${notification.key}:${notification.postTime}",
    ) = NativeNotificationMarkRead(id, NativeNotificationTarget("device", notification.key, "lifetime"), instance)

    /** Core fake: a request leaves `pending` once begin accepts it, as the real one-use permit does. */
    private class FakeCore(requests: List<NativeNotificationMarkRead>) : NotificationMarkReadCore {
        val events = mutableListOf<String>()
        var beginAccepts = true
        var beginFailure: Throwable? = null
        var pendingFailure: Throwable? = null
        var onBegin: (() -> Unit)? = null
        var outstanding = requests.toList()

        override fun pending(nowMs: Long): List<NativeNotificationMarkRead> {
            pendingFailure?.let { throw it }
            return outstanding
        }

        override fun begin(id: String, nowMs: Long): Boolean {
            events += "begin"
            beginFailure?.let { throw it }
            if (!beginAccepts) return false
            outstanding = outstanding.filterNot { it.id == id }
            onBegin?.invoke()
            return true
        }
    }

    private class FakeNotifications(var notifications: List<StatusBarNotification>, var unlocked: Boolean) :
        NotificationMarkReadNotifications {
        override fun active(): List<StatusBarNotification> = notifications
        override fun deviceUnlocked(): Boolean = unlocked
    }

    private inner class Fixture(
        active: List<StatusBarNotification>,
        requests: List<NativeNotificationMarkRead>,
        unlocked: Boolean = true,
        var permitted: Boolean = true,
        private val cancelSends: Boolean = false,
        private val sendFailure: RuntimeException? = null,
    ) {
        val core = FakeCore(requests)
        val os = FakeNotifications(active, unlocked)
        val sent = mutableListOf<PendingIntent>()
        val events: List<String> get() = core.events

        fun run(): Int = effects().run()

        fun effects(
            allow: () -> Boolean = { permitted },
            counters: NotificationMarkReadDiagnostics.Counters = NotificationMarkReadDiagnostics.Counters(),
        ) = NotificationMarkReadEffects(
            core = core,
            notifications = os,
            permitted = allow,
            send = { intent ->
                core.events += "send"
                sent += intent
                if (cancelSends) throw PendingIntent.CanceledException()
                sendFailure?.let { throw it }
            },
            nowMs = { 1L },
            counters = counters,
        )
    }

    private companion object { var nextRequestCode = 1 }
}
