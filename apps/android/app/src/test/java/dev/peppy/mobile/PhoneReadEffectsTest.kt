package dev.peppy.mobile

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Looper
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationCompat
import androidx.core.app.Person
import androidx.core.content.ContextCompat
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.Shadows.shadowOf
import uniffi.peppy_mobile_bindings.NativePhoneReadEffect
import uniffi.peppy_mobile_bindings.NativeProviderRowRef

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PhoneReadEffectsTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test fun consumeRejectionDoesNotSend() = Fixture().use { fixture ->
        fixture.core.consumeResult = 0
        fixture.runner().run()
        assertEquals(1, fixture.core.consumeCalls)
        assertEquals(emptyList<String>(), fixture.core.events)
        assertEquals(0, fixture.deliveries)
    }
    @Test fun unreadRowsNotSeenDoNotConsume() = assertBlocked { it.core.rowsAreSeen = false }
    @Test fun rowsSeenExceptionDoesNotConsumeOrSend() = assertBlocked { it.core.rowsSeenError = true }
    @Test fun consumeExceptionDoesNotSend() {
        Fixture().use { fixture ->
            fixture.core.consumeError = true
            fixture.runner().run()
            assertEquals(1, fixture.core.consumeCalls)
            assertEquals(emptyList<String>(), fixture.core.events)
            assertEquals(0, fixture.deliveries)
        }
    }
    @Test fun unreadMmsTrueDoesNotConsume() = assertBlocked { it.provider.mms = true }
    @Test fun unavailableMmsDoesNotConsume() = assertBlocked { it.provider.mms = null }
    @Test fun timestampCollisionDoesNotConsume() = assertBlocked { it.provider.collision = true }
    @Test fun unavailableCollisionDoesNotConsume() = assertBlocked { it.provider.collision = null }

    @Test fun safeInvisibleActionSendsWhenDeviceIsUnlocked() {
        Fixture().use { fixture ->
            fixture.notifications.invisibleAction = true
            fixture.useInvisibleNotifications()
            fixture.runner().run()
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("consume", "delivery"), fixture.core.events)
        }
    }

    @Test fun invisibleActionOnLockedDeviceDoesNotConsume() = assertBlocked {
        it.notifications.invisibleAction = true
        it.useInvisibleNotifications()
        it.notifications.deviceUnlockedValues += false
    }

    @Test fun lockTransitionAfterConsumeDoesNotSendInvisibleAction() {
        Fixture().use { fixture ->
            fixture.notifications.invisibleAction = true
            fixture.useInvisibleNotifications()
            fixture.notifications.deviceUnlockedValues += true
            fixture.notifications.deviceUnlockedValues += false
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
            assertEquals(0, fixture.deliveries)
        }
    }

    @Test fun canceledSendConsumesOnceAndDoesNotRetry() {
        Fixture().use { fixture ->
            fixture.intent.cancel()
            fixture.runner().run()
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
            assertEquals(0, fixture.deliveries)
        }
    }

    @Test fun successfulSendConsumesBeforeBroadcastAndOnlyOnce() {
        Fixture().use { fixture ->
            fixture.runner().run()
            fixture.runner().run()
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("consume", "delivery"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
            assertEquals(1, fixture.deliveries)
        }
    }

    @Test fun defaultPackageSwitchAfterConsumeDoesNotSend() {
        Fixture().use { fixture ->
            fixture.notifications.packages.clear()
            fixture.notifications.packages += "sms"
            fixture.notifications.packages += "other"
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
        }
    }

    @Test fun replacedNotificationPostTimeAfterConsumeDoesNotSend() {
        Fixture().use { fixture ->
            fixture.notifications.activeLists.clear()
            fixture.notifications.activeLists += listOf(fixture.notification())
            fixture.notifications.activeLists += listOf(fixture.notification(postTime = 20L))
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
        }
    }

    @Test fun newerUnreadBetweenSelectionAndSendDoesNotSend() {
        Fixture().use { fixture ->
            fixture.provider.threads.clear()
            fixture.provider.threads += fixture.thread()
            fixture.provider.threads += fixture.thread(unread = setOf(100L, 200L))
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
        }
    }

    @Test fun newlyAmbiguousNotificationAfterConsumeDoesNotSend() {
        Fixture().use { fixture ->
            fixture.notifications.activeLists.clear()
            fixture.notifications.activeLists += listOf(fixture.notification())
            fixture.notifications.activeLists += listOf(fixture.notification(), fixture.notification(key = "second"))
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
        }
    }

    @Test fun disabledPolicyAfterConsumeDoesNotSend() {
        Fixture().use { fixture ->
            fixture.core.allowedValues += false
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
        }
    }

    @Test fun replacedActionAtSameKeyAndTimeAfterConsumeDoesNotSend() {
        Fixture().use { fixture ->
            fixture.notifications.activeLists.clear()
            fixture.notifications.activeLists += listOf(fixture.notification())
            fixture.notifications.activeLists += listOf(fixture.notification())
            fixture.notifications.activeLists += listOf(fixture.notification(markRead = false))
            fixture.runner().run()
            assertEquals(listOf("consume"), fixture.core.events)
            assertEquals(1, fixture.core.consumeCalls)
            assertEquals(0, fixture.deliveries)
        }
    }

    private fun assertBlocked(change: (Fixture) -> Unit) {
        Fixture().use { fixture ->
            change(fixture)
            fixture.runner().run()
            assertEquals(0, fixture.core.consumeCalls)
            assertEquals(emptyList<String>(), fixture.core.events)
            assertEquals(0, fixture.deliveries)
        }
    }

    private inner class Fixture : AutoCloseable {
        val row = SmsProviderRecord(1, 9, null, "sender", null, 100L, null, 1, false, null, null, "creator")
        val core = FakeCore(NativePhoneReadEffect("message", reference(row)))
        val provider = FakeProvider(row, thread())
        val notifications = FakeNotifications()
        var deliveries = 0
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) { deliveries++; core.events += "delivery" }
        }
        val intent = PendingIntent.getBroadcast(context, nextRequestCode++, Intent("phone-read-fired").setPackage(context.packageName), PendingIntent.FLAG_IMMUTABLE)

        init {
            ContextCompat.registerReceiver(context, receiver, IntentFilter("phone-read-fired"), ContextCompat.RECEIVER_NOT_EXPORTED)
            repeat(3) { notifications.activeLists += listOf(notification()) }
        }
        fun thread(unread: Set<Long> = setOf(100L)) = PhoneReadThread(
            ProviderThread(9, unread, unread), unread.map { row.copy(dateMs = it, id = it) },
        )
        fun notification(key: String = "key", postTime: Long = 2L, markRead: Boolean = true): StatusBarNotification {
            val action = NotificationCompat.Action.Builder(0, "read", intent)
                .setSemanticAction(if (markRead) NotificationCompat.Action.SEMANTIC_ACTION_MARK_AS_READ else NotificationCompat.Action.SEMANTIC_ACTION_NONE)
                .setShowsUserInterface(false)
                .build()
            val builder = NotificationCompat.Builder(context, "phone-read")
                .setSmallIcon(android.R.drawable.sym_def_app_icon)
                .setStyle(NotificationCompat.MessagingStyle(Person.Builder().setName("me").build()).addMessage("body", 100L, null as Person?))
            if (notifications.invisibleAction) builder.addInvisibleAction(action) else builder.addAction(action)
            val notification = builder.build()
            return StatusBarNotification("sms", "sms", 1, key, 0, 0, 0, notification, android.os.Process.myUserHandle(), postTime)
        }
        fun runner() = PhoneReadEffects(context, core, provider, notifications, nowMs = { 1L })
        fun useInvisibleNotifications() {
            notifications.activeLists.clear()
            repeat(3) { notifications.activeLists += listOf(notification()) }
        }
        override fun close() { context.unregisterReceiver(receiver) }
    }

    private fun reference(row: SmsProviderRecord) = NativeProviderRowRef(
        MmsPreferences(context).sourceGeneration, SmsCapture.UNKNOWN_SUBSCRIPTION, "1", "9", 100L, SmsCapture.sourceRowFingerprint(row),
    )

    private class FakeCore(effect: NativePhoneReadEffect) : PhoneReadCore {
        val events = mutableListOf<String>()
        val allowedValues = ArrayDeque(listOf(true, true, true, true))
        var rowsAreSeen = true
        var rowsSeenError = false
        var consumeResult: Int? = null
        var consumeError = false
        var consumeCalls = 0
        private var pending = listOf(effect)
        override fun allowed() = if (allowedValues.size > 1) allowedValues.removeFirst() else allowedValues.first()
        override fun pending(nowMs: Long) = pending
        override fun complete(ids: List<String>) = Unit
        override fun rowsSeen(rows: List<NativeProviderRowRef>): Boolean {
            if (rowsSeenError) throw RuntimeException("rows seen failed")
            return rowsAreSeen
        }
        override fun consume(ids: List<String>, rows: List<NativeProviderRowRef>, nowMs: Long): Int {
            consumeCalls++
            if (consumeError) throw RuntimeException("consume failed")
            if (consumeResult == 0 || events.contains("consume")) return 0
            events += "consume"
            pending = emptyList()
            return ids.size
        }
    }

    private class FakeProvider(private val row: SmsProviderRecord, initial: PhoneReadThread) : PhoneReadProvider {
        val threads = ArrayDeque(listOf(initial, initial))
        var mms: Boolean? = false
        var collision: Boolean? = false
        override fun row(id: String) = ProviderRowResult.Found(row)
        override fun thread(threadId: Long) = if (threads.size > 1) threads.removeFirst() else threads.first()
        override fun hasUnreadMms(threadId: Long) = mms
        override fun collidesWithAnotherThread(threadId: Long, messageTimes: Set<Long>) = collision
    }

    private class FakeNotifications : PhoneReadNotifications {
        val packages = ArrayDeque(listOf("sms", "sms"))
        val activeLists = ArrayDeque<List<StatusBarNotification>>()
        val deviceUnlockedValues = ArrayDeque<Boolean>()
        var invisibleAction = false
        override fun defaultSmsPackage() = if (packages.size > 1) packages.removeFirst() else packages.first()
        override fun active(packageName: String): List<StatusBarNotification> = if (activeLists.isNotEmpty()) activeLists.removeFirst() else error("fixture must supply notification")
        override fun current(key: String, postTime: Long): StatusBarNotification? = activeLists.firstOrNull()?.firstOrNull { it.key == key && it.postTime == postTime }
        override fun deviceUnlocked() = if (deviceUnlockedValues.size > 1) deviceUnlockedValues.removeFirst() else deviceUnlockedValues.firstOrNull() ?: true
    }

    private companion object { var nextRequestCode = 1 }
}
