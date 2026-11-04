package dev.peppy.mobile.about

import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssetThirdPartyNoticesSideloadTest {
    @Test fun packagedNoticesHaveTextAndTitles() {
        val notices = requireNotNull(AssetThirdPartyNotices.openOrNull(RuntimeEnvironment.getApplication().assets))

        assertTrue(notices.notice().isNotBlank())
        val entries = notices.entries()
        assertTrue(entries.map(ThirdPartyNoticeEntry::fileName).containsAll(listOf("LGPL-2.1.txt", "BoringSSL-LICENSE.txt")))
        entries.forEach { entry -> assertNotNull("${entry.fileName} needs a catalog title", entry.titleRes) }
    }
}
