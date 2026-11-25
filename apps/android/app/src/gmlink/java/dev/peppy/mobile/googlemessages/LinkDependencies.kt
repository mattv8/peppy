package dev.peppy.mobile.googlemessages

import android.content.Context
import android.os.SystemClock
import android.webkit.CookieManager
import dev.peppy.mobile.RemoteMessagingRuntime
import dev.peppy.mobile.gateway.NativeGateway
import java.util.concurrent.Executor
import java.util.concurrent.atomic.AtomicBoolean
import uniffi.peppy_mobile_bindings.NativeClient

/** Collaborators used by the link runtime. */
internal data class LinkRuntimeDeps(
    val engine: () -> LinkEngine,
    val storeFactory: (Context) -> LinkStateStorage,
    /** Replaces the receive coordinator each handle opens with; null keeps the handle's own. */
    val rcsReceiveSink: RcsReceiveSink?,
    val typingReceiveSink: () -> TypingReceiveSink?,
    val callbackExecutor: Executor,
) {
    companion object {
        val DEFAULTS = LinkRuntimeDeps(
            engine = { LinkEngines.current },
            storeFactory = { LinkStateStoreAdapter(LinkStateStore(it.applicationContext.noBackupFilesDir)) },
            rcsReceiveSink = null,
            typingReceiveSink = { TypingReceiveSinkRegistry.sink },
            callbackExecutor = LinkRuntime.callbackExecutor,
        )
    }
}

/** Collaborators used by the link service. */
internal data class LinkServiceDeps(
    val elapsedRealtime: () -> Long,
    val sleeper: (Long) -> Unit,
    val notificationExecutor: Executor,
    val closeExecutor: Executor,
    val receiveSweepExecutor: Executor,
    val receiveSweep: (LinkHandleLease, AtomicBoolean) -> Unit,
    val invalidateSweepMemo: (LinkHandleLease) -> Unit,
    val currentTimeMillis: () -> Long,
    val bootstrapExecutor: Executor,
    val eligibilityExecutor: Executor,
    val bootstrapFactory: (Context, LinkHandleLease, () -> Boolean) -> RcsLinkBootstrap,
    val eligibilityPortFactory: (NativeClient) -> RcsEligibilityPublisherPort,
    val typingBridgeFactory: (Context) -> TypingBridge,
    val typingSession: (Context) -> TypingCredentials?,
    val typingExecutor: Executor,
    val peersPresent: () -> Boolean?,
) {
    companion object {
        val DEFAULTS = LinkServiceDeps(
            elapsedRealtime = SystemClock::elapsedRealtime,
            sleeper = Thread::sleep,
            notificationExecutor = DEFAULT_NOTIFICATION_EXECUTOR,
            closeExecutor = DEFAULT_CLOSE_EXECUTOR,
            receiveSweepExecutor = DEFAULT_RCS_RECEIVE_EXECUTOR,
            receiveSweep = LinkRuntime::resumeRcsReceive,
            invalidateSweepMemo = LinkRuntime::invalidateRcsSweepMemo,
            currentTimeMillis = System::currentTimeMillis,
            bootstrapExecutor = DEFAULT_RCS_BOOTSTRAP_EXECUTOR,
            eligibilityExecutor = DEFAULT_RCS_ELIGIBILITY_EXECUTOR,
            bootstrapFactory = ::RcsLinkBootstrap,
            eligibilityPortFactory = ::GeneratedRcsEligibilityPublisherPort,
            typingBridgeFactory = ::TypingBridge,
            typingSession = { context ->
                NativeGateway.session(context)?.let { TypingCredentials(it.origin, it.bearerToken, it.deviceId, it.generation) }
            },
            typingExecutor = DEFAULT_TYPING_EXECUTOR,
            peersPresent = RemoteMessagingRuntime::peersPresent,
        )
    }
}

/** Collaborators used during sign-in and pairing. */
internal data class LinkSignInDeps(
    val startupExecutor: ((() -> Unit) -> Unit),
    val awaitSignInReady: (Context) -> Boolean,
    val cookieManager: () -> CookieManager,
    val pairingExecutor: Executor,
) {
    companion object {
        val DEFAULTS = LinkSignInDeps(
            { task -> Thread(task).start() },
            GoogleMessagesLink::awaitSignInReady,
            CookieManager::getInstance,
            LinkSignInRuntime.pairingExecutor,
        )
    }
}

/** Collaborators used by the restart receiver. */
internal data class LinkRestartDeps(val executor: Executor, val startService: (Context) -> Unit) {
    companion object {
        val DEFAULTS = LinkRestartDeps(RESTART_EXECUTOR, GoogleMessagesLinkService::start)
    }
}

internal interface LinkDependenciesOverride {
    val runtime: LinkRuntimeDeps
    val service: LinkServiceDeps
    val signIn: LinkSignInDeps
    val restart: LinkRestartDeps
}

internal fun linkRuntimeDeps(context: Context) =
    (context.applicationContext as? LinkDependenciesOverride)?.runtime ?: LinkRuntimeDeps.DEFAULTS

internal fun linkServiceDeps(context: Context) =
    (context.applicationContext as? LinkDependenciesOverride)?.service ?: LinkServiceDeps.DEFAULTS

internal fun linkSignInDeps(context: Context) =
    (context.applicationContext as? LinkDependenciesOverride)?.signIn ?: LinkSignInDeps.DEFAULTS

internal fun linkRestartDeps(context: Context) =
    (context.applicationContext as? LinkDependenciesOverride)?.restart ?: LinkRestartDeps.DEFAULTS
