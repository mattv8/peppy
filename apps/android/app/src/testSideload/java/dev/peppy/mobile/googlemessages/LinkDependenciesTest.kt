package dev.peppy.mobile.googlemessages

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import dev.peppy.mobile.PeppyApplication
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

internal class LinkTestApplication : Application(), LinkDependenciesOverride {
    override var runtime = LinkRuntimeDeps.DEFAULTS
    override var service = LinkServiceDeps.DEFAULTS
    override var signIn = LinkSignInDeps.DEFAULTS
    override var restart = LinkRestartDeps.DEFAULTS
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = LinkTestApplication::class)
class LinkDependenciesTest {
    @Test
    fun `resolvers return application overrides`() {
        val app = ApplicationProvider.getApplicationContext<LinkTestApplication>()
        app.runtime = LinkRuntimeDeps.DEFAULTS.copy()
        app.service = LinkServiceDeps.DEFAULTS.copy()
        app.signIn = LinkSignInDeps.DEFAULTS.copy()
        app.restart = LinkRestartDeps.DEFAULTS.copy()
        assertSame(app.runtime, linkRuntimeDeps(app))
        assertSame(app.service, linkServiceDeps(app))
        assertSame(app.signIn, linkSignInDeps(app))
        assertSame(app.restart, linkRestartDeps(app))
    }

    @Test
    @Config(application = PeppyApplication::class)
    fun `resolvers return defaults without override`() {
        val app = ApplicationProvider.getApplicationContext<Application>()
        assertSame(LinkRuntimeDeps.DEFAULTS, linkRuntimeDeps(app))
        assertSame(LinkServiceDeps.DEFAULTS, linkServiceDeps(app))
        assertSame(LinkSignInDeps.DEFAULTS, linkSignInDeps(app))
        assertSame(LinkRestartDeps.DEFAULTS, linkRestartDeps(app))
    }
}
