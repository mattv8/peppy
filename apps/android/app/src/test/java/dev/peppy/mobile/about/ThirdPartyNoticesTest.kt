package dev.peppy.mobile.about

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ThirdPartyNoticesTest {
    private val googleMessagesFiles = listOf(
        "AGPL-3.0.txt",
        "LICENSE-exceptions-google-messages.txt",
        "NOTICE-google-messages.txt",
        "MPL-2.0.txt",
        "BSD-3-Clause-google-uuid.txt",
        "BSD-3-Clause-go-x-and-mobile.txt",
        "BSD-3-Clause-google-protobuf.txt",
        "MIT-go-colorable.txt",
        "MIT-go-isatty.txt",
        "MIT-zerolog.txt",
    )
    private val restrictedSetupFiles = listOf(
        "NOTICE.txt",
        "LGPL-3.0.txt",
        "LGPL-2.1.txt",
        "GPL-3.0.txt",
        "Apache-2.0.txt",
        "BSD-3-Clause.txt",
        "MIT.txt",
        "BouncyCastle-LICENSE.txt",
        "Conscrypt-NOTICE.txt",
        "BoringSSL-LICENSE.txt",
    )

    @Test fun linkNoticesOnlyAreAvailableAndListedInDeclaredOrder() {
        assertTrue(AssetThirdPartyNotices.isAvailable(googleMessagesFiles))
        assertEquals(googleMessagesFiles, AssetThirdPartyNotices.listedEntries(googleMessagesFiles).map(ThirdPartyNoticeEntry::fileName))
    }

    @Test fun restrictedSetupNoticesOnlyAreAvailableWithoutTheHeaderListed() {
        assertTrue(AssetThirdPartyNotices.isAvailable(restrictedSetupFiles))
        assertEquals(restrictedSetupFiles.drop(1), AssetThirdPartyNotices.listedEntries(restrictedSetupFiles).map(ThirdPartyNoticeEntry::fileName))
    }

    @Test fun bothSetsAreAvailableAndListedInDeclaredOrder() {
        val both = googleMessagesFiles + restrictedSetupFiles
        assertTrue(AssetThirdPartyNotices.isAvailable(both))
        assertEquals(
            listOf(
                "AGPL-3.0.txt",
                "LICENSE-exceptions-google-messages.txt",
                "NOTICE-google-messages.txt",
                "LGPL-3.0.txt",
                "LGPL-2.1.txt",
                "GPL-3.0.txt",
                "MPL-2.0.txt",
                "Apache-2.0.txt",
                "BSD-3-Clause.txt",
                "MIT.txt",
                "BouncyCastle-LICENSE.txt",
                "Conscrypt-NOTICE.txt",
                "BoringSSL-LICENSE.txt",
                "BSD-3-Clause-google-uuid.txt",
                "BSD-3-Clause-go-x-and-mobile.txt",
                "BSD-3-Clause-google-protobuf.txt",
                "MIT-go-colorable.txt",
                "MIT-go-isatty.txt",
                "MIT-zerolog.txt",
            ),
            AssetThirdPartyNotices.listedEntries(both).map(ThirdPartyNoticeEntry::fileName),
        )
    }

    @Test fun noPackagedNoticesMeansNoLicensesRow() {
        assertFalse(AssetThirdPartyNotices.isAvailable(emptyList()))
        assertFalse(AssetThirdPartyNotices.isAvailable(listOf("MIT.txt", "BoringSSL-LICENSE.txt")))
    }
}
