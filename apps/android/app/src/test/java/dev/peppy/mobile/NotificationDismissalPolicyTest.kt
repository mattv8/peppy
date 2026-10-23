package dev.peppy.mobile

import android.app.Notification
import android.content.ComponentName
import android.provider.Settings
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import androidx.test.core.app.ApplicationProvider
import org.junit.After
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
    private var notificationId = 0

    @Before
    fun resetNotificationListenerState() {
        NotificationDismissalEffects.reset()
        Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, "")
    }

    @After
    fun destroyNotificationService() {
        service?.onDestroy()
        service = null
        NotificationDismissalEffects.reset()
        Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, "")
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

    private fun grantListenerAccess() {
        val component = ComponentName(context, NotificationMirrorService::class.java).flattenToString()
        assertTrue(Settings.Secure.putString(context.contentResolver, LISTENER_ACCESS_SETTING, component))
    }

    private fun setMirroringEnabled(enabled: Boolean) {
        GatewayPolicyHost(context).mirroringEnabled = enabled
    }

    private fun activeNotification(postTime: Long, clearable: Boolean = true): StatusBarNotification {
        notificationId++
        val notification = Notification.Builder(ApplicationProvider.getApplicationContext(), "test")
            .setContentTitle("Title")
            .setContentText("Body")
            .setOngoing(!clearable)
            .build()
        return StatusBarNotification(
            "example.chat", "example.chat", notificationId, "tag", 1000, 0, 0, notification,
            android.os.Process.myUserHandle(), postTime,
        ).also { Shadows.shadowOf(startService() as NotificationListenerService).addActiveNotification(it) }
    }

    private fun queueDismissal(
        client: uniffi.peppy_mobile_bindings.NativeClient,
        notification: StatusBarNotification,
        instance: String = "${notification.key}:${notification.postTime}",
    ) {
        assertEquals(
            NativeNotificationCaptureOutcome.CAPTURED,
            client.captureNotification(
                NativeNotificationCapture(
                    notification.key,
                    instance,
                    notification.packageName,
                    "Chat",
                    "Title",
                    "Body",
                    null,
                    notification.postTime,
                    true,
                    null,
                ),
            ),
        )
        client.dismissNotification(
            client.notificationSnapshot().notifications.single { it.target.notificationKey == notification.key }.target,
        )
    }
}
