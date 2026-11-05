package dev.peppy.mobile.googlemessages

import android.content.Intent
import android.os.Bundle
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebStorage
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.CheckBox
import android.widget.LinearLayout
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import dev.peppy.mobile.R
import java.io.ByteArrayInputStream
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

class LinkSignInActivity : AppCompatActivity() {
    private val cookiesCaptured = AtomicBoolean(false)
    private val teardownStarted = AtomicBoolean(false)
    private lateinit var webView: WebView
    private var pendingCookies: FilteredCookies? = null
    private var signInLease: LinkSignInRuntime.Lease? = null
    private var host: TextView? = null

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        WebView.setWebContentsDebuggingEnabled(false)
        Thread {
            val lease = LinkSignInRuntime.acquireLease() ?: run {
                runOnUiThread { finishWith(OUTCOME_BLOCKED) }
                return@Thread
            }
            signInLease = lease
            if (!GoogleMessagesLink.awaitSignInReady(applicationContext)) {
                runOnUiThread { wipe { finishWith(OUTCOME_BLOCKED) } }
                return@Thread
            }
            val opened = LinkRuntime.handle(applicationContext)
            runOnUiThread {
                if (!LinkSignInRuntime.ownsLease(lease) || isFinishing || isDestroyed) return@runOnUiThread
                val handle = (opened as? LinkOpenResult.Open)?.handle
                val status = handle?.let { runCatching { LinkStatus.parse(it.status()) }.getOrNull() }
                if (status?.state !in PAIRABLE_STATES) wipe { finishWith(OUTCOME_ERROR) } else initializeWebView()
            }
        }.start()
    }

    private fun initializeWebView() {
        val cookies = CookieManager.getInstance()
        if (cookies.hasCookies()) {
            wipe { finishWith(OUTCOME_ERROR) }
            return
        }
        webView = WebView(this).apply {
            settings.javaScriptEnabled = true
            webViewClient = signInClient(cookies)
        }
        setContentView(signInLayout())
        webView.loadUrl(ACCOUNT_CHOOSER)
    }

    private fun signInLayout() = LinearLayout(this).padForSystemBars().apply {
        orientation = LinearLayout.VERTICAL
        addView(TextView(this@LinkSignInActivity).apply {
            text = getString(R.string.peppy_google_messages_link_signin_title)
            accessibilityHeading()
        })
        host = TextView(this@LinkSignInActivity).apply {
            text = "accounts.google.com"
            contentDescription = "google-messages-link-signin-host"
        }
        addView(host)
        addView(webView, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        addView(Button(this@LinkSignInActivity).apply {
            text = getString(R.string.peppy_google_messages_link_signin_blocked_action)
            setOnClickListener { wipe { finishWith(OUTCOME_BLOCKED) } }
        })
        addView(Button(this@LinkSignInActivity).apply {
            text = getString(R.string.peppy_cancel)
            setOnClickListener { wipe { finishWith(OUTCOME_CANCELLED) } }
        })
    }

    private fun signInClient(cookies: CookieManager) = object : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, url: String): Boolean = !NavigationAllowlist.permits(url)

        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean =
            !NavigationAllowlist.permits(request.url.toString())

        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
            if (NavigationAllowlist.permits(request.url.toString())) null else blockedResponse()

        override fun onPageFinished(view: WebView, url: String) {
            if (NavigationAllowlist.isConfig(url) && cookiesCaptured.compareAndSet(false, true)) {
                readCookiesAndConfirm(cookies.getCookie(CONFIG_URL).orEmpty())
            }
        }

        override fun onPageStarted(view: WebView, url: String, favicon: android.graphics.Bitmap?) {
            host?.text = runCatching { java.net.URI(url).host }.getOrNull().orEmpty()
        }

        override fun onReceivedError(view: WebView, request: WebResourceRequest, error: android.webkit.WebResourceError) {
            if (request.isForMainFrame) showLoadError()
        }
    }

    private fun readCookiesAndConfirm(header: String) {
        val cookies = runCatching { CookieFilter.required(header) }.getOrElse {
            wipe { finishWith(OUTCOME_COOKIES_MISSING) }
            return
        }
        pendingCookies = cookies
        wipe { showPairConfirmation() }
    }

    private fun showLoadError() {
        setContentView(LinearLayout(this).padForSystemBars().apply {
            orientation = LinearLayout.VERTICAL
            addView(TextView(this@LinkSignInActivity).apply { text = getString(R.string.peppy_google_messages_link_signin_load_error) })
            addView(Button(this@LinkSignInActivity).apply {
                text = getString(R.string.peppy_try_again)
                setOnClickListener {
                    (webView.parent as? ViewGroup)?.removeView(webView)
                    setContentView(signInLayout())
                    webView.loadUrl(ACCOUNT_CHOOSER)
                }
            })
            addView(Button(this@LinkSignInActivity).apply {
                text = getString(R.string.peppy_cancel)
                setOnClickListener { wipe { finishWith(OUTCOME_CANCELLED) } }
            })
        })
    }

    private fun showPairConfirmation() {
        val onlyPhone = CheckBox(this).apply {
            text = getString(R.string.peppy_google_messages_link_confirm_check_only_phone)
            minimumHeight = (48 * resources.displayMetrics.density).toInt()
        }
        val link = Button(this).apply {
            text = getString(R.string.peppy_google_messages_link_confirm_action)
            isEnabled = false
            setOnClickListener { isEnabled = false; startPairing() }
        }
        onlyPhone.setOnCheckedChangeListener { _, checked -> link.isEnabled = checked }
        setContentView(android.widget.ScrollView(this).padForSystemBars().apply {
            contentDescription = "google-messages-link-confirm-screen"
            addView(LinearLayout(this@LinkSignInActivity).apply {
            orientation = LinearLayout.VERTICAL
            addView(TextView(this@LinkSignInActivity).apply {
                text = getString(R.string.peppy_google_messages_link_confirm_title)
                accessibilityHeading()
            })
            addView(TextView(this@LinkSignInActivity).apply { text = getString(R.string.peppy_google_messages_link_confirm_body) })
            addView(onlyPhone)
            addView(TextView(this@LinkSignInActivity).apply { text = getString(R.string.peppy_google_messages_link_replaces_browser) })
            addView(link)
            addView(Button(this@LinkSignInActivity).apply {
                text = getString(R.string.peppy_cancel)
                setOnClickListener { pendingCookies = null; releaseLease(); finishWith(OUTCOME_DECLINED) }
            })
            })
        })
    }

    private fun startPairing() {
        val cookies = pendingCookies ?: return finishWith(OUTCOME_ERROR)
        GoogleMessagesLinkService.start(this)
        val queued = LinkSignInRuntime.submitPairing {
            try {
                (LinkRuntime.handle(applicationContext) as? LinkOpenResult.Open)?.handle?.pair(CookieFilter.json(cookies))
            } catch (_: Exception) {
                LinkRuntimeEventSink.record("link", "pair_failed", 0)
            } finally {
                pendingCookies = null
            }
        }
        releaseLease()
        finishWith(if (queued) OUTCOME_PAIRING_STARTED else OUTCOME_ERROR)
    }

    private fun wipe(after: () -> Unit) {
        if (!teardownStarted.compareAndSet(false, true)) return
        CookieManager.getInstance().removeAllCookies {
            CookieManager.getInstance().flush()
            WebStorage.getInstance().deleteAllData()
            val cleared = WebViewDataFiles.deleteStaleData(File(applicationInfo.dataDir), cacheDir)
            if (::webView.isInitialized) webView.destroy()
            releaseLease()
            if (cleared && !isFinishing && !isDestroyed) after() else if (!cleared) {
                GoogleMessagesLink.blockSignInAfterCleanupFailure()
                finishWith(OUTCOME_BLOCKED)
            }
        }
    }

    override fun onBackPressed() {
        if (::webView.isInitialized && webView.canGoBack()) webView.goBack() else wipe { finishWith(OUTCOME_CANCELLED) }
    }

    override fun onDestroy() {
        pendingCookies = null
        if (::webView.isInitialized && !isFinishing) wipe {}
        if (!::webView.isInitialized) releaseLease()
        super.onDestroy()
    }

    private fun releaseLease() {
        signInLease?.let(LinkSignInRuntime::releaseLease)
        signInLease = null
    }

    private fun finishWith(outcome: String) {
        setResult(RESULT_OK, Intent().putExtra(EXTRA_OUTCOME, outcome))
        finish()
    }

    private fun blockedResponse() = WebResourceResponse("text/plain", "utf-8", ByteArrayInputStream(ByteArray(0)))

    private fun <T : View> T.padForSystemBars(): T {
        val base = intArrayOf(paddingLeft, paddingTop, paddingRight, paddingBottom)
        ViewCompat.setOnApplyWindowInsetsListener(this) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.ime())
            view.setPadding(base[0] + bars.left, base[1] + bars.top, base[2] + bars.right, base[3] + bars.bottom)
            insets
        }
        return this
    }

    private fun View.accessibilityHeading() = ViewCompat.setAccessibilityHeading(this, true)

    private companion object {
        const val EXTRA_OUTCOME = "dev.peppy.mobile.googlemessages.outcome"
        const val OUTCOME_CANCELLED = "cancelled"
        const val OUTCOME_DECLINED = "declined"
        const val OUTCOME_BLOCKED = "blocked"
        const val OUTCOME_COOKIES_MISSING = "cookies_missing"
        const val OUTCOME_ERROR = "error"
        const val OUTCOME_PAIRING_STARTED = "pairing_started"
        const val CONFIG_URL = "https://messages.google.com/web/config"
        const val ACCOUNT_CHOOSER = "https://accounts.google.com/AccountChooser?continue=$CONFIG_URL"
        val PAIRABLE_STATES = setOf("unlinked", "signed_out")
    }
}
