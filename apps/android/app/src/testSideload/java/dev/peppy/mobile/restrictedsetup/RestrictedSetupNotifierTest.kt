package dev.peppy.mobile.restrictedsetup

import android.app.NotificationManager
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RestrictedSetupNotifierTest {
    private val context = RuntimeEnvironment.getApplication()
    private val notifier = RestrictedSetupNotifierImpl(context)
    private val notifications = context.getSystemService(NotificationManager::class.java)

    @Test fun `searching foreground notice includes cancel`() {
        notifier.show(RestrictedSetupNotice.SEARCHING)

        val notification = notifications.activeNotifications.single { it.id == RestrictedSetupNotifierImpl.NOTIFICATION_ID }.notification
        assertEquals(1, notification.actions.size)
        assertEquals("Cancel", notification.actions.single().title)
    }

    @Test fun `results use notification separate from foreground notification`() {
        notifier.show(RestrictedSetupNotice.SEARCHING)
        notifier.show(RestrictedSetupNotice.DONE)

        assertTrue(notifications.activeNotifications.any { it.id == RestrictedSetupNotifierImpl.NOTIFICATION_ID })
        assertTrue(notifications.activeNotifications.any { it.id == RestrictedSetupNotifierImpl.RESULT_NOTIFICATION_ID })
        assertNotEquals(RestrictedSetupNotifierImpl.NOTIFICATION_ID, RestrictedSetupNotifierImpl.RESULT_NOTIFICATION_ID)
    }

    @Test fun `rejected code notice has cancel without reply action`() {
        notifier.show(RestrictedSetupNotice.CODE_REJECTED)

        val notification = notifications.activeNotifications.single { it.id == RestrictedSetupNotifierImpl.NOTIFICATION_ID }.notification
        assertEquals(1, notification.actions.size)
        assertEquals("Cancel", notification.actions.single().title)
    }

    @Test fun `clear removes foreground and result notices`() {
        notifier.show(RestrictedSetupNotice.SEARCHING)
        notifier.show(RestrictedSetupNotice.FAILED)

        notifier.clear()

        assertFalse(notifications.activeNotifications.any { it.id in setOf(RestrictedSetupNotifierImpl.NOTIFICATION_ID, RestrictedSetupNotifierImpl.RESULT_NOTIFICATION_ID) })
    }
}
