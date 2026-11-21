package dev.peppy.mobile.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import dev.peppy.mobile.R

internal data class PageAction(
    val icon: ImageVector,
    val label: String,
    val tag: String,
    val enabled: Boolean = true,
    val onClick: () -> Unit,
)

internal data class PageChrome(
    val title: String,
    val tag: String,
    val onBack: () -> Unit,
    val actions: List<PageAction> = emptyList(),
)

@Stable
internal class PageChromeHost {
    var current: PageChrome? by mutableStateOf<PageChrome?>(null)
        private set

    fun show(chrome: PageChrome) {
        current = chrome
    }

    fun clear(tag: String) {
        if (current?.tag == tag) current = null
    }
}

internal val LocalPageChromeHost = staticCompositionLocalOf<PageChromeHost?> { null }

@Composable
internal fun DetailPage(chrome: PageChrome, content: @Composable ColumnScope.() -> Unit) {
    val host = LocalPageChromeHost.current
    val bodyTag = "${chrome.tag}-body"
    if (host == null) {
        ScreenBody(bodyTag) {
            DetailsPageHeader(chrome.title, chrome.tag, chrome.onBack)
            if (chrome.actions.isNotEmpty()) FallbackActions(chrome.actions)
            content()
        }
        return
    }
    val callbacks = remember { PageCallbacks() }
    val registered = callbacks.register(chrome)
    SideEffect {
        callbacks.update(chrome)
        host.show(registered)
    }
    DisposableEffect(host, chrome.tag) {
        onDispose { host.clear(chrome.tag) }
    }
    BackHandler(onBack = chrome.onBack)
    ScreenBody(bodyTag) { content() }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PageChromeTopAppBar(
    host: PageChromeHost,
    fallbackTitle: String,
    fallbackNavigationIcon: (@Composable () -> Unit)? = null,
) {
    val current = host.current
    if (current != null) {
        TopAppBar(
            modifier = Modifier.testTag("${current.tag}-top-bar"),
            title = {
                Text(
                    current.title,
                    modifier = Modifier.semantics {
                        heading()
                        liveRegion = LiveRegionMode.Polite
                    },
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            },
            navigationIcon = {
                IconButton(onClick = current.onBack, modifier = Modifier.testTag("${current.tag}-back")) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.peppy_back))
                }
            },
            actions = {
                current.actions.forEach { action -> ActionButton(action) }
            },
        )
    } else {
        TopAppBar(
            modifier = Modifier.testTag("app-top-bar"),
            title = { Text(fallbackTitle) },
            navigationIcon = { fallbackNavigationIcon?.invoke() },
        )
    }
}

@Composable
private fun ActionButton(action: PageAction) {
    IconButton(onClick = action.onClick, enabled = action.enabled, modifier = Modifier.testTag(action.tag)) {
        Icon(action.icon, contentDescription = action.label)
    }
}

@Composable
private fun FallbackActions(actions: List<PageAction>) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        actions.forEach { action -> ActionButton(action) }
    }
}

/**
 * Keeps the registered chrome structurally equal across recompositions: back and action clicks are
 * stable wrappers that call the latest lambdas, one wrapper per action tag.
 */
private class PageCallbacks {
    private var source: PageChrome? = null
    private val actionClicks = HashMap<String, () -> Unit>()
    private val back: () -> Unit = { source?.onBack?.invoke() }

    fun update(chrome: PageChrome) {
        source = chrome
        actionClicks.keys.retainAll(chrome.actions.map { it.tag }.toSet())
    }

    fun register(chrome: PageChrome): PageChrome = PageChrome(
        title = chrome.title,
        tag = chrome.tag,
        onBack = back,
        actions = chrome.actions.map { action -> action.copy(onClick = actionClick(action.tag)) },
    )

    private fun actionClick(tag: String): () -> Unit = actionClicks.getOrPut(tag) {
        { source?.actions?.firstOrNull { it.tag == tag }?.onClick?.invoke() }
    }
}
