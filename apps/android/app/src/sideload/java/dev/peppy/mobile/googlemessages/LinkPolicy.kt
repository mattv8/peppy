package dev.peppy.mobile.googlemessages

import java.io.File
import java.net.URI
import org.json.JSONObject

class RunBackoff {
    private var retry = 0

    fun nextDelayMillis(): Long = DELAYS_MS[retry.coerceAtMost(DELAYS_MS.lastIndex)].also { retry++ }

    fun reset() {
        retry = 0
    }

    private companion object {
        val DELAYS_MS = longArrayOf(30_000L, 60_000L, 120_000L, 300_000L, 600_000L)
    }
}

internal enum class ServiceAction { STOP, RUN, WAIT }

internal fun nextServiceAction(status: LinkStatus, pairingInProgress: Boolean, running: Boolean): ServiceAction = when {
    pairingInProgress || status.state == "pairing" -> ServiceAction.WAIT
    status.state in TERMINAL_STATES -> ServiceAction.STOP
    status.state == "linked" && !running -> ServiceAction.RUN
    else -> ServiceAction.WAIT
}

private val TERMINAL_STATES = setOf("unlinked", "needs_relink", "signed_out")

data class FilteredCookies(val values: Map<String, String>) {
    override fun toString() = "FilteredCookies(names=${values.keys})"
}

object CookieFilter {
    val names = linkedSetOf("OSID", "SID", "HSID", "SSID", "APISID", "SAPISID", "__Secure-1PSIDTS")
    val requiredNames = names - "__Secure-1PSIDTS"

    fun required(header: String): FilteredCookies {
        val selected = header.split(';').mapNotNull { part ->
            val separator = part.indexOf('=')
            if (separator <= 0) null else part.substring(0, separator).trim() to part.substring(separator + 1)
        }.filter { (name, _) -> name in names }
        require(selected.groupBy { it.first }.values.none { it.size > 1 }) { "duplicate cookie name" }
        val cookies = selected.toMap()
        require(cookies.keys.containsAll(requiredNames) && cookies.values.none(String::isEmpty)) { "required cookies missing" }
        return FilteredCookies(cookies)
    }

    fun json(cookies: FilteredCookies): ByteArray = JSONObject(cookies.values).toString().toByteArray(Charsets.UTF_8)
}

object NavigationAllowlist {
    private const val MESSAGES_HOST = "messages.google.com"
    private const val CONFIG_PATH = "/web/config"
    private val messagesPaths = setOf(CONFIG_PATH, "/accounts/SetOSID")
    private val safeHost = Regex("^[a-z0-9.-]+$")
    private val safePath = Regex("^/[A-Za-z0-9/._-]{0,80}$")

    fun permits(url: String): Boolean = runCatching {
        val uri = URI(url)
        val host = uri.host ?: return false
        uri.scheme == "https" && (host == "accounts.google.com" ||
            host == "accounts.youtube.com" ||
            host == "www.google.com" && uri.path.startsWith("/accounts/") ||
            host == MESSAGES_HOST && uri.path in messagesPaths)
    }.getOrDefault(false)

    fun blocksSubresource(url: String): Boolean {
        val uri = runCatching { URI(url) }.getOrNull()
        return if (uri != null) {
            uri.host?.trimEnd('.').equals(MESSAGES_HOST, ignoreCase = true) && !permits(url)
        } else {
            url.startsWith("http://$MESSAGES_HOST", ignoreCase = true) ||
                url.startsWith("https://$MESSAGES_HOST", ignoreCase = true)
        }
    }

    fun isConfig(url: String): Boolean = runCatching {
        val uri = URI(url)
        uri.scheme == "https" && uri.host == MESSAGES_HOST && uri.path == CONFIG_PATH
    }.getOrDefault(false)

    fun blockedCode(url: String): String {
        val uri = runCatching { URI(url) }.getOrNull()
        val host = uri?.host?.takeIf { it.matches(safeHost) } ?: return "navigation_blocked"
        val path = uri.rawPath?.takeIf { it.matches(safePath) }.orEmpty()
        return "navigation_blocked:$host$path"
    }
}

object WebViewDataFiles {
    fun deleteStaleData(dataDir: File, cacheDir: File): Boolean {
        // This app has no other WebView; the default profile belongs exclusively to Google Messages link.
        val profileDeleted = File(dataDir, "app_webview").deleteRecursivelyOrMissing()
        val cacheDeleted = File(cacheDir, "WebView").deleteRecursivelyOrMissing()
        val chromiumCacheDeleted = File(cacheDir, "org.chromium.android_webview").deleteRecursivelyOrMissing()
        return profileDeleted && cacheDeleted && chromiumCacheDeleted
    }

    private fun File.deleteRecursivelyOrMissing(): Boolean = !exists() || deleteRecursively()
}
