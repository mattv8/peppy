package dev.peppy.mobile.phoneread

import dev.peppy.mobile.PhoneReadNotificationInstance
import dev.peppy.mobile.PhoneReadNotificationMemo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PhoneReadNotificationMemoTest {
    private val notifications = setOf(PhoneReadNotificationInstance("key", 1L))

    @Test fun unchangedNotificationRunArmsMemoOnlyAfterEvaluationWithoutAction() {
        val memo = PhoneReadNotificationMemo.next(notifications, PhoneReadRun(pending = true, evaluatedNotifications = true))
        assertEquals(notifications, memo)
        assertNull(PhoneReadNotificationMemo.next(notifications, PhoneReadRun(pending = true, evaluatedNotifications = true, acted = true)))
        assertNull(PhoneReadNotificationMemo.next(notifications, PhoneReadRun(pending = true)))
    }

    @Test fun captureTriggeredRunClearsMemo() {
        assertNull(PhoneReadNotificationMemo.next(null, PhoneReadRun(evaluatedNotifications = true)))
    }
}
