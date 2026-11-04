package dev.peppy.mobile.testing

import android.R
import android.content.ComponentName
import androidx.activity.ComponentActivity
import androidx.test.core.app.ApplicationProvider
import org.junit.rules.ExternalResource
import org.robolectric.Shadows.shadowOf

/** Registers Compose's test-only host without adding it to a shipped application manifest. */
class ComposeTestActivityRule : ExternalResource() {
    override fun before() {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        shadowOf(context.packageManager)
            .addActivityIfNotPresent(ComponentName(context, ComponentActivity::class.java))
            .apply {
                exported = true
                theme = R.style.Theme_Material_Light_NoActionBar
            }
    }
}
