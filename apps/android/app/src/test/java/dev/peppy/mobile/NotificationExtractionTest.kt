package dev.peppy.mobile

import android.app.Notification
import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import androidx.core.content.pm.PackageInfoCompat
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.util.Base64
import android.service.notification.StatusBarNotification
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class NotificationExtractionTest {
    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun sbn(notification: Notification, postTime: Long = 42L) = StatusBarNotification(
        "example.chat", "example.chat", 1, "tag", 1000, 0, 0, notification, android.os.Process.myUserHandle(), postTime,
    )

    @Test fun extractsPublicTitleTextAndStableInstance() {
        val notification = Notification.Builder(context, "test")
            .setContentTitle("Title").setContentText("Text").build()
        val captured = checkNotNull(NotificationExtraction.extract(context, sbn(notification)))
        assertEquals("Title", captured.title)
        assertEquals("Text", captured.text)
        assertTrue(captured.instance.endsWith(":42"))
    }

    @Test fun retainsListenerSilentRankingFactForPolicyEvaluation() {
        val notification = Notification.Builder(context, "test").setContentText("Text").build()
        assertTrue(checkNotNull(NotificationExtraction.extract(context, sbn(notification), isSilent = true)).isSilent)
    }

    @Test fun prefersNonemptyBigTextAndBoundsUtf8WithoutBrokenSurrogates() {
        val notification = Notification.Builder(context, "test").setContentTitle("😀".repeat(2000))
            .setContentText("").setStyle(Notification.BigTextStyle().bigText("expanded")).build()
        val captured = checkNotNull(NotificationExtraction.extract(context, sbn(notification)))
        assertEquals("expanded", captured.text)
        assertTrue(captured.title.toByteArray(Charsets.UTF_8).size + captured.text.toByteArray(Charsets.UTF_8).size <= 4096)
        assertEquals("x", NotificationExtraction.utf8Truncate("x\uD800", 10))
    }

    @Test fun excludesOngoingGroupSummaryAndEmptyNotifications() {
        assertNull(NotificationExtraction.extract(context, sbn(Notification.Builder(context, "test").setOngoing(true).setContentText("x").build())))
        assertNull(NotificationExtraction.extract(context, sbn(Notification.Builder(context, "test").setGroupSummary(true).setContentText("x").build())))
        assertNull(NotificationExtraction.extract(context, sbn(Notification.Builder(context, "test").build())))
    }

    @Test fun masterDefaultsOff() {
        context.getSharedPreferences("peppy-notification-mirroring", Context.MODE_PRIVATE).edit().clear().commit()
        assertTrue(!NotificationMirrorPreferences.enabled(context))
    }

    @Test fun removingOldInstanceIsNotBlockedByImmediateSameKeyRepost() {
        val notification = Notification.Builder(context, "test").setContentText("body").build()
        val old = sbn(notification, 42)
        val repost = sbn(notification, 43)
        assertTrue(NotificationExtraction.removedInstanceStillActive(old, arrayOf(old)))
        assertTrue(!NotificationExtraction.removedInstanceStillActive(old, arrayOf(repost)))
    }

    @Test fun applicationMetadataUsesTheInstalledApplicationLabel() {
        assertEquals(
            context.applicationInfo.loadLabel(context.packageManager).toString(),
            NotificationAppMetadata.displayLabel(context.packageManager, context.applicationInfo, context.packageName),
        )
    }

    @Test @Config(sdk = [26]) fun applicationMetadataResolvesInstalledIconOnApi26() {
        val packageName = "example.notification.app"
        val iconResource = android.R.drawable.btn_star
        shadowOf(context.packageManager).addPackage(PackageInfo().apply {
            this.packageName = packageName
            versionCode = 1
            applicationInfo = ApplicationInfo().apply {
                this.packageName = packageName
                icon = iconResource
                nonLocalizedLabel = "Example"
            }
        })
        shadowOf(context.packageManager).setApplicationIcon(packageName, ColorDrawable(Color.CYAN))
        assertEquals(1, PackageInfoCompat.getLongVersionCode(context.packageManager.getPackageInfo(packageName, 0)))
        val metadata = checkNotNull(NotificationAppMetadata.resolve(context.packageManager, packageName))

        assertEquals("Example", metadata.label)
        assertTrue(checkNotNull(metadata.iconPngBase64).isNotBlank())
    }

    @Test fun applicationIconRasterizationProducesBounded32PixelPng() {
        val icon = checkNotNull(NotificationAppMetadata.encodeIcon(ColorDrawable(Color.CYAN)))
        val iconBytes = Base64.decode(icon, Base64.NO_WRAP)
        val bitmap = checkNotNull(BitmapFactory.decodeByteArray(iconBytes, 0, iconBytes.size))
        assertEquals(32, bitmap.width)
        assertEquals(32, bitmap.height)
        assertTrue(Base64.decode(icon, Base64.NO_WRAP).size <= 8192)
    }

    @Test fun missingApplicationMetadataDoesNotDropNotificationContent() {
        val notification = Notification.Builder(context, "test").setContentText("Text").build()
        val captured = checkNotNull(NotificationExtraction.extract(context, sbn(notification)))

        assertNull(captured.appIconPngBase64)
        assertEquals("Text", captured.text)
    }

    @Test fun iconCacheReusesBytesForTheSamePackageVersionAndBoundsOldVersions() {
        val packageName = "test.cache.${System.nanoTime()}"
        assertEquals("first", NotificationAppMetadata.cachedIcon(packageName, 1) { "first" })
        assertEquals("first", NotificationAppMetadata.cachedIcon(packageName, 1) { "second" })
        repeat(64) { version -> NotificationAppMetadata.cachedIcon(packageName, version + 2L) { "version-$version" } }
        assertEquals("replaced", NotificationAppMetadata.cachedIcon(packageName, 1) { "replaced" })
    }

    @Test fun failedIconEncodingIsOmittedRatherThanCachedAsContent() {
        val packageName = "test.failure.${System.nanoTime()}"
        assertNull(NotificationAppMetadata.cachedIcon(packageName, 1) { null })
        assertEquals("available", NotificationAppMetadata.cachedIcon(packageName, 1) { "available" })
    }

    @Test fun missingApplicationLabelUsesThePackageSentinelInsteadOfAGenericTerminalSegment() {
        assertEquals("com.openchamber.app", NotificationAppMetadata.fallbackLabel("com.openchamber.app"))
        assertEquals("android", NotificationAppMetadata.fallbackLabel("android"))
    }
}
