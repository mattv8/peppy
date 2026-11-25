package dev.peppy.mobile.googlemessages

import android.app.Application
import android.content.Context
import android.webkit.CookieManager
import androidx.test.core.app.ApplicationProvider
import java.util.concurrent.Executor

/** Plain test Application: supplies link dependencies without running PeppyApplication's startup. */
internal class LinkTestApplication : Application(), LinkDependenciesOverride {
    override var runtime = LinkRuntimeDeps.DEFAULTS
    override var service = LinkServiceDeps.DEFAULTS
    override var signIn = LinkSignInDeps.DEFAULTS
    override var restart = LinkRestartDeps.DEFAULTS
}

/**
 * Replaces runtime collaborators for the current test by rewriting the test Application's [LinkRuntimeDeps].
 * Robolectric creates a new Application for every test, so nothing set here outlives the test.
 * The properties are write-only; read the Application's deps to inspect them.
 */
internal object LinkTestDeps {
    private val app: LinkTestApplication get() = ApplicationProvider.getApplicationContext()

    var engine: LinkEngine?
        get() = null
        set(value) {
            app.runtime = app.runtime.copy(engine = value?.let { engine -> { engine } } ?: LinkRuntimeDeps.DEFAULTS.engine)
        }

    var storeFactory: ((Context) -> LinkStateStorage)?
        get() = null
        set(value) { app.runtime = app.runtime.copy(storeFactory = value ?: LinkRuntimeDeps.DEFAULTS.storeFactory) }

    var rcsReceiveSink: RcsReceiveSink?
        get() = null
        set(value) { app.runtime = app.runtime.copy(rcsReceiveSink = value) }

    var typingReceiveSink: TypingReceiveSink?
        get() = null
        set(value) {
            app.runtime = app.runtime.copy(typingReceiveSink = value?.let { sink -> { sink } } ?: LinkRuntimeDeps.DEFAULTS.typingReceiveSink)
        }

    var callbackExecutor: Executor?
        get() = null
        set(value) { app.runtime = app.runtime.copy(callbackExecutor = value ?: LinkRuntimeDeps.DEFAULTS.callbackExecutor) }

    /** Reports the engine as available or not while it still opens through the configured engine. */
    var available: Boolean?
        get() = null
        set(value) {
            val base = (app.runtime.engine as? AvailabilityProvider)?.base ?: app.runtime.engine
            app.runtime = app.runtime.copy(engine = if (value == null) base else AvailabilityProvider(base, value))
        }

    private class AvailabilityProvider(val base: () -> LinkEngine, val available: Boolean) : () -> LinkEngine {
        override fun invoke(): LinkEngine = AvailabilityEngine(base(), available)
    }

    var startupExecutor: ((() -> Unit) -> Unit)?
        get() = null
        set(value) { app.signIn = app.signIn.copy(startupExecutor = value ?: LinkSignInDeps.DEFAULTS.startupExecutor) }

    var awaitSignInReady: ((Context) -> Boolean)?
        get() = null
        set(value) { app.signIn = app.signIn.copy(awaitSignInReady = value ?: LinkSignInDeps.DEFAULTS.awaitSignInReady) }

    var cookieManager: (() -> CookieManager)?
        get() = null
        set(value) { app.signIn = app.signIn.copy(cookieManager = value ?: LinkSignInDeps.DEFAULTS.cookieManager) }

    var pairingExecutor: Executor?
        get() = null
        set(value) { app.signIn = app.signIn.copy(pairingExecutor = value ?: LinkSignInDeps.DEFAULTS.pairingExecutor) }

    private class AvailabilityEngine(private val delegate: LinkEngine, override val available: Boolean) : LinkEngine by delegate
}
