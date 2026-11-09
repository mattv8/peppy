package dev.peppy.mobile

import android.app.Notification
import android.content.ComponentName
import android.os.Handler
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import dev.peppy.mobile.gateway.GatewayPolicyHost
import dev.peppy.mobile.gateway.GatewayTestBase
import dev.peppy.mobile.gateway.NativeGateway
import dev.peppy.mobile.gateway.TestKeyHolder
import org.junit.After
import org.junit.Assert.*
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.shadows.ShadowNotificationListenerService
import java.util.concurrent.TimeUnit
import uniffi.peppy_mobile_bindings.NativeNotificationCapture
import uniffi.peppy_mobile_bindings.NativeNotificationCaptureOutcome


private const val LISTENER_ACCESS_SETTING = "enabled_notification_listeners"

/** Records the OS cancellation effect while the Rust client remains real. */
private object NotificationDismissalEffects {
    val cancelledKeys = mutableListOf<String>()
    var onFirstCancel: (() -> Unit)? = null

    fun reset() {
        cancelledKeys.clear()
        onFirstCancel = null
    }
}

/** Robolectric's listener shadow supplies active notifications; this adds effect observation only. */
@Implements(NotificationMirrorService::class, isInAndroidSdk = false)
class RecordingNotificationMirrorServiceShadow : ShadowNotificationListenerService() {
    @Implementation
    override fun cancelNotification(key: String) {
        super.cancelNotification(key)
        NotificationDismissalEffects.cancelledKeys += key
        NotificationDismissalEffects.onFirstCancel?.also {
            NotificationDismissalEffects.onFirstCancel = null
            it()
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [RecordingNotificationMirrorServiceShadow::class])
class NotificationDismissalPolicyTest : GatewayTestBase() {
    private var service: NotificationMirrorService? = null
    private val additionalServices = mutableListOf<NotificationMirrorService>()
    private var notificationId = 0

    @Before
    fun resetNotificationListenerState() {
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        NotificationDismissalEffects.reset()
        Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, "")
    }

    @After
    fun destroyNotificationService() {
        service?.onDestroy()
        additionalServices.forEach(NotificationMirrorService::onDestroy)
        additionalServices.clear()
        service = null
        NotificationDismissalEffects.reset()
        Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, "")
        WorkManagerTestInitHelper.closeWorkDatabase()
    }

    @Test
    fun masterOffLeavesCurrentClearableDismissalPendingUntilMirroringIsReenabled() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val current = activeNotification(postTime = 101)
        queueDismissal(client, current)
        setMirroringEnabled(false)

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)

        setMirroringEnabled(true)
        assertEquals(1, startService().applyDismissals())
        assertEquals(listOf(current.key), NotificationDismissalEffects.cancelledKeys)
        assertTrue(client.pendingNotificationDismissals(50uL).isEmpty())
        assertEquals(0, startService().applyDismissals())
    }

    @Test
    fun wifiOnlyPolicyLeavesDismissalPendingWhenWifiIsUnavailable() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        val policy = GatewayPolicyHost(context)
        policy.mirroringEnabled = true
        policy.mirroringWifiOnly = true
        assertFalse(policy.decision(notificationIsSilent = false)?.captureNotification ?: true)
        val current = activeNotification(postTime = 102)
        queueDismissal(client, current)

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)
    }

    @Test
    fun revokedListenerAccessLeavesDismissalPendingWithoutAnOsEffect() {
        val client = enrollAndUnlock()
        setMirroringEnabled(true)
        val current = activeNotification(postTime = 103)
        queueDismissal(client, current)

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)
    }

    @Test
    fun noUnlockedSessionLeavesDismissalPendingWithoutAnOsEffect() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val current = activeNotification(postTime = 104)
        queueDismissal(client, current)
        NativeGateway.closeForTest()
        NativeGateway.secretBox = TestKeyHolder().box

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())

        NativeGateway.secretBox = keys.box
        val reopened = checkNotNull(NativeGateway.open(context))
        assertEquals(1, reopened.pendingNotificationDismissals(50uL).size)
    }

    @Test
    fun staleActiveInstanceLeavesRealDismissalPending() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val stale = activeNotification(postTime = 105)
        queueDismissal(client, stale, instance = "${stale.key}:104")

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)
    }

    @Test
    fun nonclearableActiveNotificationLeavesRealDismissalPending() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val nonclearable = activeNotification(postTime = 106, clearable = false)
        queueDismissal(client, nonclearable)

        assertEquals(0, startService().applyDismissals())
        assertTrue(NotificationDismissalEffects.cancelledKeys.isEmpty())
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)
    }

    @Test
    fun removalExecutorFencesRepostedLifetimeButReconcilesAbsentOrPendingKeys() {
        val client = enrollAndUnlock()
        val key = "0|example.chat|lifetime"
        fun capture(instance: String, postedAt: Long) = client.captureNotification(NativeNotificationCapture(
            key, instance, "example.chat", "Chat", "Ada", "body", null, postedAt, true, null,
        ))

        capture("$key:A", 1)
        capture("$key:B", 2)
        val bLifetime = client.notificationSnapshot().notifications.single().target.lifetime
        NotificationRemovalExecutor.remove(client, key, "$key:A", mapOf(key to "$key:B"))
        assertEquals(bLifetime, client.notificationSnapshot().notifications.single().target.lifetime)

        // B may be coalesced and not captured yet: its active OS lifetime still permits retiring A.
        client.removeNotification(key, "$key:B")
        capture("$key:A", 1)
        NotificationRemovalExecutor.remove(client, key, "$key:A", mapOf(key to "$key:B"))
        assertTrue(client.notificationSnapshot().notifications.isEmpty())
        capture("$key:B", 2)
        assertEquals(1, client.notificationSnapshot().notifications.size)

        NotificationRemovalExecutor.remove(client, key, "$key:B", emptyMap())
        assertTrue(client.notificationSnapshot().notifications.isEmpty())
    }

    @Test
    fun lateRemovalKeepsTheNewPendingCaptureAndItsCoalescedCallback() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val service = startConnectedService()
        val id = ++notificationId
        val removed = activeNotification(postTime = 201, id = id, addToActiveNotifications = false)
        val reposted = activeNotification(postTime = 202, id = id)
        capture(client, removed)

        service.onNotificationPosted(reposted)
        idleListener(service)
        service.onNotificationRemoved(removed)
        idleListener(service)
        assertTrue(client.notificationSnapshot().notifications.isEmpty())

        advanceListenerPastCoalesce(service)
        assertRepostedSnapshot(client, reposted)
        client.removeNotificationIfCurrentInstance(reposted.key, "${reposted.key}:${reposted.postTime}")
        assertTrue(client.notificationSnapshot().notifications.isEmpty())
    }

    @Test
    fun lateRemovalAfterRepostCaptureKeepsTheNewLifetimeAndRemovesItsCurrentInstance() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val service = startConnectedService()
        val id = ++notificationId
        val removed = activeNotification(postTime = 203, id = id, addToActiveNotifications = false)
        val reposted = activeNotification(postTime = 204, id = id)
        capture(client, removed)

        service.onNotificationPosted(reposted)
        idleListener(service)
        advanceListenerPastCoalesce(service)
        service.onNotificationRemoved(removed)
        idleListener(service)
        assertRepostedSnapshot(client, reposted)

        val removalService = startFreshService()
        removalService.onNotificationRemoved(reposted)
        idleListener(removalService)
        assertTrue(client.notificationSnapshot().notifications.isEmpty())
    }

    @Test
    fun policyTurningOffAfterFirstCancellationAcknowledgesOnlyThatCompletedEffect() {
        val client = enrollAndUnlock()
        grantListenerAccess()
        setMirroringEnabled(true)
        val first = activeNotification(postTime = 107)
        val second = activeNotification(postTime = 108)
        queueDismissal(client, first)
        queueDismissal(client, second)
        NotificationDismissalEffects.onFirstCancel = { setMirroringEnabled(false) }

        assertEquals(1, startService().applyDismissals())
        assertEquals(1, NotificationDismissalEffects.cancelledKeys.size)
        assertEquals(1, client.pendingNotificationDismissals(50uL).size)
        assertEquals(0, startService().applyDismissals())
        assertEquals(1, NotificationDismissalEffects.cancelledKeys.size)
    }

    private fun startService(): NotificationMirrorService = service ?: Robolectric
        .buildService(NotificationMirrorService::class.java)
        .create()
        .get()
        .also { service = it }

    private fun startConnectedService(): NotificationMirrorService = startService().also {
        it.onListenerConnected()
        idleListener(it)
    }

    private fun startFreshService(): NotificationMirrorService = Robolectric
        .buildService(NotificationMirrorService::class.java)
        .create()
        .get()
        .also { additionalServices += it }

    private fun listenerHandler(service: NotificationMirrorService): Handler =
        NotificationMirrorService::class.java.getDeclaredField("handler\$delegate").run {
            isAccessible = true
            (get(service) as Lazy<*>).value as Handler
        }

    private fun idleListener(service: NotificationMirrorService) {
        Shadows.shadowOf(listenerHandler(service).looper).idle()
    }

    private fun advanceListenerPastCoalesce(service: NotificationMirrorService) {
        Shadows.shadowOf(listenerHandler(service).looper).idleFor(2, TimeUnit.SECONDS)
    }

    private fun grantListenerAccess() {
        val component = ComponentName(context, NotificationMirrorService::class.java).flattenToString()
        assertTrue(Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, component))
    }

    private fun setMirroringEnabled(enabled: Boolean) {
        GatewayPolicyHost(context).mirroringEnabled = enabled
    }

    private fun activeNotification(
        postTime: Long,
        clearable: Boolean = true,
        id: Int = ++notificationId,
        addToActiveNotifications: Boolean = true,
    ): StatusBarNotification {
        val notification = Notification.Builder(ApplicationProvider.getApplicationContext(), "test")
            .setContentTitle("Title")
            .setContentText("Body")
            .setOngoing(!clearable)
            .build()
        return StatusBarNotification(
            "example.chat", "example.chat", id, "tag", 1000, 0, 0, notification,
            android.os.Process.myUserHandle(), postTime,
        ).also {
            if (addToActiveNotifications) Shadows.shadowOf(startService() as NotificationListenerService).addActiveNotification(it)
        }
    }

    private fun capture(client: uniffi.peppy_mobile_bindings.NativeClient, notification: StatusBarNotification) {
        assertEquals(
            NativeNotificationCaptureOutcome.CAPTURED,
            client.captureNotification(NativeNotificationCapture(
                notification.key,
                "${notification.key}:${notification.postTime}",
                notification.packageName,
                "Chat",
                "Title",
                "Body",
                null,
                notification.postTime,
                true,
                null,
            )),
        )
    }

    private fun assertRepostedSnapshot(
        client: uniffi.peppy_mobile_bindings.NativeClient,
        reposted: StatusBarNotification,
    ) {
        val mirrored = client.notificationSnapshot().notifications.single()
        assertEquals("Title", mirrored.title)
        assertEquals(reposted.key, mirrored.target.notificationKey)
        assertEquals(client.notificationSourceDeviceId(), mirrored.target.sourceDeviceId)
    }

    private fun queueDismissal(
        client: uniffi.peppy_mobile_bindings.NativeClient,
        notification: StatusBarNotification,
        instance: String = "${notification.key}:${notification.postTime}",
    ) {
        assertEquals(
            NativeNotificationCaptureOutcome.CAPTURED,
            client.captureNotification(NativeNotificationCapture(
                notification.key, instance, notification.packageName, "Chat", "Title", "Body", null,
                notification.postTime, true, null,
            )),
        )
        client.dismissNotification(
            client.notificationSnapshot().notifications.single { it.target.notificationKey == notification.key }.target,
        )
    }
}
