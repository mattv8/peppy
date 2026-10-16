package dev.peppy.mobile

import android.content.Intent
import android.os.Looper
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.Shadows.shadowOf

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class MmsCapturePreferencesTest {
    @Test fun settingsDefaultOffAndSourceGenerationPersists() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-mms-capture", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val first = MmsPreferences(context)
        assertFalse(first.enabled)
        assertFalse(first.importHistory)
        val generation = first.sourceGeneration
        first.enabled = true
        first.importHistory = true

        val restarted = MmsPreferences(context)
        assertEquals(generation, restarted.sourceGeneration)
        assertTrue(restarted.enabled)
        assertTrue(restarted.importHistory)
    }

    @Test fun legacyBackfillStateCanBeClearedAfterCoreMigration() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-mms-capture", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val prefs = MmsPreferences(context)
        prefs.enabled = true
        prefs.setHistoryBaseline(42)
        prefs.importHistory = true
        prefs.completeHistoryImport()
        assertEquals(42L, MmsPreferences(context).historyBaseline())
        assertFalse(MmsPreferences(context).importHistory)

        prefs.clearLegacyBackfillState()
        assertNull(MmsPreferences(context).historyBaseline())
        assertNull(MmsPreferences(context).recheckCursor())
    }

    @Test fun pendingRecheckCursorPersistsAndCanBeCleared() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        context.getSharedPreferences("peppy-mms-capture", android.content.Context.MODE_PRIVATE).edit().clear().commit()
        val prefs = MmsPreferences(context)
        prefs.setRecheckCursor("acquisition-50")
        assertEquals("acquisition-50", MmsPreferences(context).recheckCursor())
        prefs.setRecheckCursor(null)
        assertNull(MmsPreferences(context).recheckCursor())
    }

    @Test fun capturedMessagesWakeNetworkSyncExactlyThroughUniqueGatewayWork() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        MmsCaptureWork.handleResult(context, MmsCaptureSummary(captured = 1, pending = 0, unavailable = 0, more = false))
        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
        assertTrue(WorkManager.getInstance(context).getWorkInfosForUniqueWork(MmsCaptureWork.NAME).get().isEmpty())
    }

    @Test fun onlyMmsWapPushIsAcceptedAsCaptureHint() {
        assertTrue(MmsNotificationReceiver.isMmsWapPush(Intent(MmsNotificationReceiver.WAP_PUSH_RECEIVED).setType(MmsNotificationReceiver.MMS_MIME)))
        assertFalse(MmsNotificationReceiver.isMmsWapPush(Intent(MmsNotificationReceiver.WAP_PUSH_RECEIVED).setType("text/plain")))
        assertFalse(MmsNotificationReceiver.isMmsWapPush(Intent("other").setType(MmsNotificationReceiver.MMS_MIME)))
    }

    @Test fun providerObserverDebouncesHintsAndCancelsPendingDispatch() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        var dispatches = 0
        val observer = MmsCaptureLifecycle.observer(
            context,
            SmsCaptureDebouncePolicy(quietMs = 0),
        ) { dispatches++ }

        observer.onChange(false)
        observer.onChange(false)
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, dispatches)

        observer.onChange(false)
        observer.cancel()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, dispatches)
    }
}
