package dev.peppy.mobile.billing

import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import dev.peppy.mobile.testing.ComposeTestActivityRule
import java.util.concurrent.atomic.AtomicInteger
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.rules.RuleChain
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34])
class PlaySubscriptionPanelComposeTest {
    private val compose = createComposeRule()
    @get:Rule val rules: RuleChain = RuleChain.outerRule(ComposeTestActivityRule()).around(compose)

    @Test fun sameAccountReauthenticationRevisionTriggersOneAdditionalLoad() {
        val loads = AtomicInteger()
        compose.setContent {
            MaterialTheme {
                var revision by remember { mutableIntStateOf(0) }
                PlaySubscriptionLoadEffect("account", "vault", revision) { _, _ -> loads.incrementAndGet() }
                LaunchedEffect(Unit) { revision = 1 }
            }
        }

        compose.waitForIdle()
        compose.runOnIdle { assertEquals(2, loads.get()) }
    }
}
