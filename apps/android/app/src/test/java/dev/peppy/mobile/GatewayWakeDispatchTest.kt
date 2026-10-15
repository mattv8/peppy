package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.Configuration
import androidx.work.NetworkType
import androidx.work.WorkManager
import androidx.work.testing.SynchronousExecutor
import androidx.work.testing.WorkManagerTestInitHelper
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class GatewayWakeDispatchTest {
    @Test fun reconnectHandlerPreventsFallback() = withHandlerReset {
        var calls = 0
        GatewayWakeDispatch.reconnectHandler = { calls += 1; true }
        GatewayWakeDispatch.dispatch(ApplicationProvider.getApplicationContext<Context>())
        assertEquals(1, calls)
    }

    @Test fun unhandledReconnectEnqueuesUniqueNetworkConstrainedSync() = withHandlerReset {
        val context = workManagerContext()
        GatewayWakeDispatch.reconnectHandler = { false }
        GatewayWakeDispatch.dispatch(context)

        val work = WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get()
        assertEquals(1, work.size)
        assertEquals(NetworkType.CONNECTED, work.single().constraints.requiredNetworkType)
    }

    @Test fun reconnectHandlerExceptionFallsBackToSync() = withHandlerReset {
        val context = workManagerContext()
        GatewayWakeDispatch.reconnectHandler = { throw IllegalStateException("service unavailable") }
        GatewayWakeDispatch.dispatch(context)

        assertEquals(1, WorkManager.getInstance(context).getWorkInfosForUniqueWork(GatewayWork.NAME).get().size)
    }

    @Test fun expeditedRequestsAreRestrictedToApi31AndAbove() {
        val below31 = GatewayWakeDispatch.createRequest(30)
        val api31 = GatewayWakeDispatch.createRequest(31)

        assertFalse(below31.workSpec.expedited)
        assertTrue(api31.workSpec.expedited)
    }

    private fun workManagerContext(): Context {
        val context = ApplicationProvider.getApplicationContext<Context>()
        WorkManagerTestInitHelper.initializeTestWorkManager(
            context,
            Configuration.Builder().setExecutor(SynchronousExecutor()).build(),
        )
        return context
    }

    private fun withHandlerReset(block: () -> Unit) {
        try {
            block()
        } finally {
            GatewayWakeDispatch.reconnectHandler = null
        }
    }
}
