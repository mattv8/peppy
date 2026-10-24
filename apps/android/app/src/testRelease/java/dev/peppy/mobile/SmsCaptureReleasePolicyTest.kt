package dev.peppy.mobile

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.work.BackoffPolicy
import java.util.concurrent.TimeUnit
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLooper

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class SmsCaptureReleasePolicyTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test fun releaseDefaultObserverDispatchesOneHintAfterOneSecond() {
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context) { dispatches++ }

        assertFalse(SyncDiagnostics.enabled)
        observer.onChange(false)
        ShadowLooper.idleMainLooper(999, TimeUnit.MILLISECONDS)
        assertEquals(0, dispatches)
        ShadowLooper.idleMainLooper(1, TimeUnit.MILLISECONDS)

        assertEquals(1, dispatches)
        observer.cancel()
    }

    @Test fun releaseDefaultObserverCapsAContinuousBurstAtThreeSeconds() {
        var dispatches = 0
        val observer = SmsCaptureLifecycle.observer(context) { dispatches++ }

        observer.onChange(false)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        observer.onChange(false)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        observer.onChange(false)
        ShadowLooper.idleMainLooper(900, TimeUnit.MILLISECONDS)
        observer.onChange(false)
        ShadowLooper.idleMainLooper(299, TimeUnit.MILLISECONDS)
        assertEquals(0, dispatches)
        ShadowLooper.idleMainLooper(1, TimeUnit.MILLISECONDS)

        assertEquals(1, dispatches)
        observer.cancel()
    }

    @Test fun releaseRequestsKeepNormalDelayAndObserverBackoff() {
        val default = SmsCaptureWork.createRequest(SmsCaptureRequestSource.DEFAULT)
        val observer = SmsCaptureWork.createRequest(SmsCaptureRequestSource.OBSERVER)
        val continuation = SmsCaptureWork.createRequest(SmsCaptureRequestSource.CONTINUATION)

        assertEquals(15_000L, default.workSpec.initialDelay)
        assertEquals(0L, observer.workSpec.initialDelay)
        assertEquals(0L, continuation.workSpec.initialDelay)
        assertEquals(BackoffPolicy.EXPONENTIAL, observer.workSpec.backoffPolicy)
        assertEquals(30_000L, observer.workSpec.backoffDelayDuration)
        assertFalse(observer.workSpec.expedited)
    }
}
