package dev.peppy.mobile.googlemessages

import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LinkSignInActivityTest {
    @After fun reset() = LinkSignInRuntime.resetForTest()

    @Test fun `launches with the sideload AppCompat theme`() {
        Robolectric.buildActivity(LinkSignInActivity::class.java).setup().pause().stop().destroy()
    }

    @Test fun `destroy before startup finishes releases the sign in lease`() {
        Robolectric.buildActivity(LinkSignInActivity::class.java).setup().destroy()

        LinkSignInRuntime.acquireLease()?.let(LinkSignInRuntime::releaseLease)
            ?: error("destroyed activity retained the WebView lease")
    }
}
