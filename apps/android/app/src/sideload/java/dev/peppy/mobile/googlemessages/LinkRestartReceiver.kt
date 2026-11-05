package dev.peppy.mobile.googlemessages

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.util.concurrent.Executor
import java.util.concurrent.Executors

internal data class RestartReceiverTestHooks(
    val executor: Executor = RESTART_EXECUTOR,
    val startService: (Context) -> Unit = GoogleMessagesLinkService::start,
    val pendingFinished: () -> Unit = {},
)

private val RESTART_EXECUTOR = Executors.newSingleThreadExecutor()

class LinkRestartReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action !in RESTART_ACTIONS) return
        val pendingResult = goAsync()
        val work = Runnable {
            try {
                val applicationContext = context.applicationContext
                val opened = LinkRuntime.handle(applicationContext) as? LinkOpenResult.Open ?: return@Runnable
                val status = LinkStatus.parse(opened.handle.status())
                if (status.state == "linked") testHooks.startService(applicationContext)
            } catch (_: Exception) {
                // A sealed-state or gomobile failure must not leave the broadcast pending.
            } finally {
                pendingResult.finish()
                testHooks.pendingFinished()
            }
        }
        try {
            testHooks.executor.execute(work)
        } catch (_: RuntimeException) {
            pendingResult.finish()
            testHooks.pendingFinished()
        }
    }

    companion object {
        internal var testHooks = RestartReceiverTestHooks()
        private val RESTART_ACTIONS = setOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED)
    }
}
