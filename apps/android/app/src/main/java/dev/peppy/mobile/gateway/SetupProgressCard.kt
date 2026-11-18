package dev.peppy.mobile.gateway

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import dev.peppy.mobile.R
import dev.peppy.mobile.sms.syncStopped
import dev.peppy.mobile.ui.theme.PeppyTokens
import java.text.NumberFormat
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** How long the in-session "Setup finished" notice stays on the SMS tab (design 3.1, Q10). */
internal const val SETUP_FINISHED_NOTICE_MS = 6_000L

/** A queued "Setup finished" notice expires if the SMS tab was not showing when the import ended (UX-N3). */
internal const val SETUP_FINISHED_QUEUE_MS = 30_000L

/**
 * In-memory only: remembers that this process saw an import and then reached LIVE, so the SMS tab can show
 * "Setup finished" once. A cold start into LIVE never queues the notice.
 */
internal object SetupFinishTracker {
    private var sawSetup = false
    private var queuedAtMs = 0L
    private val pendingFinish = MutableStateFlow(false)
    val pending: StateFlow<Boolean> = pendingFinish.asStateFlow()

    /** Called with every status the app loads. An import, then LIVE, queues one notice. */
    @Synchronized
    fun observe(status: GatewayStatus?, nowMs: Long = System.currentTimeMillis()) {
        when {
            status != null && status.syncPhase.isBootstrapPhase() -> sawSetup = true
            sawSetup -> {
                sawSetup = false
                if (status?.syncPhase == SyncPhase.LIVE) {
                    queuedAtMs = nowMs
                    pendingFinish.value = true
                }
            }
        }
    }

    /** Takes the queued notice. It is shown only if it is still fresh; either way the queue is emptied. */
    @Synchronized
    fun takeFresh(nowMs: Long = System.currentTimeMillis()): Boolean {
        val queued = pendingFinish.value
        pendingFinish.value = false
        return queued && nowMs - queuedAtMs <= SETUP_FINISHED_QUEUE_MS
    }

    internal fun resetForTest() = synchronized(this) {
        sawSetup = false
        queuedAtMs = 0L
        pendingFinish.value = false
    }
}

/** Fraction for the determinate bar, clamped. Null when the total is unknown or zero, so the bar stays indeterminate. */
internal fun setupFraction(staged: Long, expected: Long): Float? =
    if (expected <= 0L) null else staged.coerceIn(0L, expected).toFloat() / expected.toFloat()

/** Locale grouping, as the design requires for every count in `%1$s` slots. */
internal fun formatSetupCount(value: Long, format: NumberFormat = NumberFormat.getIntegerInstance()): String = format.format(value)

/**
 * The SMS tab card for a phone that is still importing history. Visible only while bootstrapping, with vault keys ready,
 * and not stopped or too large. Shows "Setup finished" once after an in-session import reaches LIVE, and never while
 * stopped. Reads only [status], which the SMS tab loads off the main thread; it never calls the core.
 */
@Composable
internal fun SetupProgressCard(status: GatewayStatus?) {
    val finishPending by SetupFinishTracker.pending.collectAsState()
    var showFinished by remember { mutableStateOf(false) }
    // Two effects: acknowledging flips `finishPending` back to false, which would cancel a timer keyed on it.
    LaunchedEffect(finishPending) {
        if (finishPending && SetupFinishTracker.takeFresh()) showFinished = true
    }
    LaunchedEffect(showFinished) {
        if (showFinished) {
            delay(SETUP_FINISHED_NOTICE_MS)
            showFinished = false
        }
    }
    if (status == null || !status.sharedKeysReady || syncStopped(status) || status.historyTooLarge) return
    when {
        status.syncPhase.isBootstrapPhase() -> SetupProgressBody(status.setupProgress ?: SetupProgress.Checking)
        showFinished -> SetupFinishedBody()
    }
}

private data class SetupPresentation(val phase: String, val fraction: Float?, val count: String?)

@Composable
private fun SetupProgressBody(progress: SetupProgress) {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    val presentation = when (progress) {
        SetupProgress.Checking -> SetupPresentation(stringResource(R.string.peppy_sync_setup_phase_checking), null, null)
        is SetupProgress.Downloading -> {
            // The visible count never exceeds its total, even when a stale write races the expected value.
            val staged = progress.staged.coerceIn(0L, progress.expected.coerceAtLeast(0L))
            val fraction = setupFraction(staged, progress.expected)
            SetupPresentation(
                stringResource(R.string.peppy_sync_setup_phase_downloading),
                fraction,
                fraction?.let {
                    stringResource(
                        R.string.peppy_sync_setup_count_downloading,
                        formatSetupCount(staged),
                        formatSetupCount(progress.expected),
                    )
                },
            )
        }
        is SetupProgress.Organizing -> SetupPresentation(
            stringResource(R.string.peppy_sync_setup_phase_organizing),
            null,
            stringResource(R.string.peppy_sync_setup_count_organizing, formatSetupCount(progress.remaining)),
        )
    }
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("sync-setup-card"),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(12.dp).semantics(mergeDescendants = true) {}, verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(stringResource(R.string.peppy_sync_setup_title), style = MaterialTheme.typography.titleSmall, modifier = Modifier.testTag("sync-setup-title"))
            Text(stringResource(R.string.peppy_toggle_android_sync_setup_body), style = MaterialTheme.typography.bodyMedium, modifier = Modifier.testTag("sync-setup-body"))
            Text(
                presentation.phase,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag("sync-setup-phase"),
            )
            val barState = presentation.count ?: presentation.phase
            val barModifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).height(4.dp).testTag("sync-setup-progress").semantics {
                stateDescription = barState
                presentation.fraction?.let { progressBarRangeInfo = ProgressBarRangeInfo(it, 0f..1f) }
            }
            val fraction = presentation.fraction
            if (fraction != null) {
                LinearProgressIndicator(progress = { fraction }, modifier = barModifier, color = colors.accent, trackColor = MaterialTheme.colorScheme.outlineVariant)
            } else {
                LinearProgressIndicator(modifier = barModifier, color = colors.accent, trackColor = MaterialTheme.colorScheme.outlineVariant)
            }
            presentation.count?.let { Text(it, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("sync-setup-count")) }
            Text(stringResource(R.string.peppy_toggle_android_sync_setup_others_wait), style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("sync-setup-others-wait"))
        }
    }
}

/**
 * The finished notice is one live region: TalkBack reads the title and the body together, not only the second line.
 */
@Composable
private fun SetupFinishedBody() {
    val colors = if (isSystemInDarkTheme()) PeppyTokens.Dark else PeppyTokens.Light
    Surface(
        modifier = Modifier.fillMaxWidth().testTag("sync-setup-card"),
        shape = RoundedCornerShape(12.dp),
        color = colors.success.copy(alpha = 0.12f),
    ) {
        Row(
            Modifier.padding(12.dp).semantics(mergeDescendants = true) { liveRegion = LiveRegionMode.Polite },
        ) {
            Text("✓", color = colors.success, modifier = Modifier.clearAndSetSemantics {})
            Spacer(Modifier.width(8.dp))
            Column {
                Text(stringResource(R.string.peppy_sync_setup_done_title), style = MaterialTheme.typography.titleSmall)
                Text(
                    stringResource(R.string.peppy_toggle_android_sync_setup_done_body),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag("sync-setup-done"),
                )
            }
        }
    }
}
