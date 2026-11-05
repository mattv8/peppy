package dev.peppy.mobile.googlemessages

import android.content.Context
import androidx.compose.runtime.Composable
import dev.peppy.mobile.googlemessages.ui.GoogleMessagesLinkPanel
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.Future

object GoogleMessagesLink {
    @Volatile private var staleWebViewWipeFailed = false
    private val startupExecutor = Executors.newSingleThreadExecutor()
    private val startupLock = Any()
    private var startupWipe: Future<Boolean>? = null
    @Composable
    fun Entry(onChanged: () -> Unit) {
        GoogleMessagesLinkPanel(onChanged)
    }

    fun available(context: Context): Boolean = GoogleMessagesLinkTestHooks.available ?: LinkEngines.current.available

    fun onAppStart(context: Context) {
        val appContext = context.applicationContext
        GoogleMessagesLinkService.createNotificationChannel(appContext)
        synchronized(startupLock) {
            if (startupWipe == null) {
                startupWipe = startupExecutor.submit<Boolean> {
                    val wiped = wipeStaleWebViewData(appContext)
                    staleWebViewWipeFailed = !wiped
                    LinkRuntimeEventSink.record("link", if (wiped) "stale_webview_wiped" else "stale_webview_wipe_failed", 0)
                    wiped
                }
            }
        }
    }

    fun signInBlocked(context: Context): Boolean = staleWebViewWipeFailed || LinkSignInRuntime.isSignInBlocked()

    fun blockSignInAfterCleanupFailure() {
        runCatching {
            staleWebViewWipeFailed = true
            LinkSignInRuntime.blockSignIn()
            LinkRuntimeEventSink.record("link", "stale_webview_wipe_failed", 0)
        }
    }

    internal fun awaitSignInReady(context: Context): Boolean {
        onAppStart(context)
        return runCatching { synchronized(startupLock) { checkNotNull(startupWipe) }.get() }.getOrDefault(false)
    }

    internal fun wipeStaleWebViewData(context: Context): Boolean = runCatching {
        WebViewDataFiles.deleteStaleData(File(context.applicationInfo.dataDir), context.cacheDir)
    }.getOrDefault(false)
}

internal object GoogleMessagesLinkTestHooks {
    @Volatile var available: Boolean? = null
}
