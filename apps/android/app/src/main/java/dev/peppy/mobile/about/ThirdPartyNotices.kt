package dev.peppy.mobile.about

import android.content.res.AssetManager
import androidx.annotation.StringRes
import dev.peppy.mobile.R

internal data class ThirdPartyNoticeEntry(val fileName: String, @StringRes val titleRes: Int?)

internal interface ThirdPartyNotices {
    fun notice(): String
    fun entries(): List<ThirdPartyNoticeEntry>
    fun read(fileName: String): String
}

internal class AssetThirdPartyNotices private constructor(
    private val assets: AssetManager,
    private val files: List<String>,
) : ThirdPartyNotices {
    override fun notice(): String = read(NOTICE_FILE)

    override fun entries(): List<ThirdPartyNoticeEntry> = files
        .filterNot { it == NOTICE_FILE }
        .sortedWith(compareBy({ knownOrder.indexOf(it).takeIf { index -> index >= 0 } ?: Int.MAX_VALUE }, { it }))
        .map { ThirdPartyNoticeEntry(it, labels[it]) }

    override fun read(fileName: String): String = assets.open("$DIRECTORY/$fileName").bufferedReader().use { it.readText() }

    companion object {
        const val DIRECTORY = "third_party"
        const val NOTICE_FILE = "NOTICE.txt"

        private val knownOrder = listOf(
            "LGPL-3.0.txt", "LGPL-2.1.txt", "GPL-3.0.txt", "AGPL-3.0.txt", "MPL-2.0.txt",
            "Apache-2.0.txt", "BSD-3-Clause.txt", "MIT.txt",
            "BouncyCastle-LICENSE.txt", "Conscrypt-NOTICE.txt", "BoringSSL-LICENSE.txt",
            "NOTICE-google-messages.txt", "BSD-3-Clause-google-uuid.txt",
            "BSD-3-Clause-go-x-and-mobile.txt", "BSD-3-Clause-google-protobuf.txt",
            "MIT-go-colorable.txt", "MIT-go-isatty.txt", "MIT-zerolog.txt",
        )
        private val labels = mapOf(
            "LGPL-3.0.txt" to R.string.peppy_about_license_lgpl_3_0,
            "LGPL-2.1.txt" to R.string.peppy_about_license_lgpl_2_1,
            "GPL-3.0.txt" to R.string.peppy_about_license_gpl_3_0,
            "Apache-2.0.txt" to R.string.peppy_about_license_apache_2_0,
            "BSD-3-Clause.txt" to R.string.peppy_about_license_bsd_3_clause,
            "MIT.txt" to R.string.peppy_about_license_mit,
            "BouncyCastle-LICENSE.txt" to R.string.peppy_about_license_bouncy_castle,
            "Conscrypt-NOTICE.txt" to R.string.peppy_about_license_conscrypt,
            "BoringSSL-LICENSE.txt" to R.string.peppy_about_license_boringssl,
            "AGPL-3.0.txt" to R.string.peppy_about_license_agpl_3_0,
            "MPL-2.0.txt" to R.string.peppy_about_license_mpl_2_0,
            "NOTICE-google-messages.txt" to R.string.peppy_about_license_google_messages,
            "BSD-3-Clause-google-uuid.txt" to R.string.peppy_about_license_google_uuid,
            "BSD-3-Clause-go-x-and-mobile.txt" to R.string.peppy_about_license_go_x_and_mobile,
            "BSD-3-Clause-google-protobuf.txt" to R.string.peppy_about_license_google_protobuf,
            "MIT-go-colorable.txt" to R.string.peppy_about_license_go_colorable,
            "MIT-go-isatty.txt" to R.string.peppy_about_license_go_isatty,
            "MIT-zerolog.txt" to R.string.peppy_about_license_zerolog,
        )

        fun openOrNull(assets: AssetManager): ThirdPartyNotices? {
            val files = assets.list(DIRECTORY)?.toList().orEmpty()
            return if (NOTICE_FILE in files) AssetThirdPartyNotices(assets, files) else null
        }
    }
}
