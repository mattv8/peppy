package dev.peppy.mobile.about

import org.junit.Assert.assertEquals
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

        val expectedTitles = mapOf(
            "AGPL-3.0.txt" to "GNU Affero General Public License v3.0",
            "MPL-2.0.txt" to "Mozilla Public License 2.0",
            "NOTICE-google-messages.txt" to "Google Messages link notices",
            "BSD-3-Clause-google-uuid.txt" to "google/uuid — BSD 3-Clause License",
            "BSD-3-Clause-go-x-and-mobile.txt" to "Go x/mobile and x modules — BSD 3-Clause License",
            "BSD-3-Clause-google-protobuf.txt" to "Google Protocol Buffers — BSD 3-Clause License",
            "MIT-go-colorable.txt" to "go-colorable — MIT License",
            "MIT-go-isatty.txt" to "go-isatty — MIT License",
            "MIT-zerolog.txt" to "zerolog — MIT License",
        )
        val application = RuntimeEnvironment.getApplication()
        val titlesByFile = entries.associate { entry ->
            entry.fileName to entry.titleRes?.let(application::getString)
        }
        if ("AGPL-3.0.txt" in titlesByFile) {
            expectedTitles.forEach { (fileName, title) -> assertEquals(title, titlesByFile[fileName]) }
        }
    }
}
