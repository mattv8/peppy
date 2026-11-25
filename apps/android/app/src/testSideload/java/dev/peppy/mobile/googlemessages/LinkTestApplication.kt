package dev.peppy.mobile.googlemessages

import android.app.Application
import android.content.Context
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
            val base = app.runtime.engine
            app.runtime = app.runtime.copy(engine = if (value == null) base else {
                { AvailabilityEngine(base(), value) }
            })
        }

    private class AvailabilityEngine(private val delegate: LinkEngine, override val available: Boolean) : LinkEngine by delegate
}
