package dev.peppy.mobile.restrictedsetup

import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class RestrictedSetupRuntimeTest {
    @Test fun `runtime cleanup removes stale restricted setup preference`() {
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("restricted_setup", 0).edit().putBoolean("cleanup_needed", true).commit()

        RestrictedSetupRuntime.createController(context)

        assertFalse(context.getSharedPreferences("restricted_setup", 0).contains("cleanup_needed"))
    }
}
