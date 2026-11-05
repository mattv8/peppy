package dev.peppy.mobile.googlemessages

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class LinkRestartReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action !in RESTART_ACTIONS) return
        val pendingResult = goAsync()
        Thread {
            try {
                val opened = LinkRuntime.handle(context.applicationContext) as? LinkOpenResult.Open ?: return@Thread
                val status = LinkStatus.parse(opened.handle.status())
                if (status.state == "linked") GoogleMessagesLinkService.start(context.applicationContext)
            } finally {
                pendingResult.finish()
            }
        }.start()
    }

    private companion object {
        val RESTART_ACTIONS = setOf(Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED)
    }
}
