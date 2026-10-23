package dev.peppy.mobile

import android.content.pm.PackageManager
import android.content.pm.ApplicationInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.util.Base64
import androidx.core.content.pm.PackageInfoCompat
import java.io.ByteArrayOutputStream
import java.util.LinkedHashMap

internal data class ResolvedNotificationAppMetadata(val label: String, val iconPngBase64: String?)

/** Resolves only installed application metadata; notification media is never an icon source. */
internal object NotificationAppMetadata {
    private const val ICON_EDGE_PX = 32
    private const val MAX_ICON_BYTES = 8192
    private const val MAX_LABEL_BYTES = 256
    private const val CACHE_LIMIT = 64
    private val iconCache = object : LinkedHashMap<String, String>(CACHE_LIMIT, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, String>?): Boolean = size > CACHE_LIMIT
    }

    fun resolve(packageManager: PackageManager, packageName: String): ResolvedNotificationAppMetadata? = try {
        val applicationInfo = packageManager.getApplicationInfo(packageName, 0)
        val label = displayLabel(packageManager, applicationInfo, packageName)
        val icon = runCatching { PackageInfoCompat.getLongVersionCode(packageManager.getPackageInfo(packageName, 0)) }
            .getOrNull()
            ?.let { versionCode ->
                cachedIcon(packageName, versionCode) {
                    runCatching { packageManager.getApplicationIcon(packageName) }
                        .getOrNull()
                        ?.let { drawable -> encodeIcon(drawable) }
                }
            }
        ResolvedNotificationAppMetadata(label, icon)
    } catch (_: Exception) {
        null
    }

    internal fun displayLabel(
        packageManager: PackageManager,
        applicationInfo: ApplicationInfo,
        packageName: String,
    ): String = applicationInfo.loadLabel(packageManager).toString().trim()
            .takeUnless { it.isEmpty() || it == packageName }
            ?.let { NotificationExtraction.utf8Truncate(it, MAX_LABEL_BYTES) }
            ?: fallbackLabel(packageName)

    internal fun encodeIcon(drawable: android.graphics.drawable.Drawable): String? = try {
        val bitmap = Bitmap.createBitmap(ICON_EDGE_PX, ICON_EDGE_PX, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bitmap)
        drawable.bounds = android.graphics.Rect(0, 0, ICON_EDGE_PX, ICON_EDGE_PX)
        drawable.draw(canvas)
        val bytes = ByteArrayOutputStream().use { stream ->
            if (!bitmap.compress(Bitmap.CompressFormat.PNG, 100, stream)) return null
            stream.toByteArray()
        }
        bytes.takeIf { it.size <= MAX_ICON_BYTES }?.let { Base64.encodeToString(it, Base64.NO_WRAP) }
    } catch (_: Exception) {
        null
    }

    internal fun cachedIcon(packageName: String, versionCode: Long, encode: () -> String?): String? = synchronized(iconCache) {
        val key = "$packageName:$versionCode"
        iconCache[key] ?: encode()?.also { encoded -> iconCache[key] = encoded }
    }

    internal fun fallbackLabel(packageName: String): String = NotificationExtraction.utf8Truncate(packageName, MAX_LABEL_BYTES)
}
