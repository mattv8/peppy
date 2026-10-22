package dev.peppy.mobile

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.peppy_mobile_bindings.MobileBindingsException

private const val DisabledRowAlpha = 0.38f

internal sealed interface NotificationAppFilterLoadResult {
    data object Loading : NotificationAppFilterLoadResult
    data object Locked : NotificationAppFilterLoadResult
    data class Ready(val apps: List<ObservedNotificationApp>, val mutedPackages: Set<String>) : NotificationAppFilterLoadResult
    data object Error : NotificationAppFilterLoadResult
}

internal interface NotificationAppFilterActions {
    suspend fun load(): NotificationAppFilterLoadResult
    suspend fun setMuted(app: ObservedNotificationApp, muted: Boolean): Boolean
}

internal sealed interface NotificationAppFilterSummaryState {
    data object Loading : NotificationAppFilterSummaryState
    data object Locked : NotificationAppFilterSummaryState
    data object Error : NotificationAppFilterSummaryState
    data class Ready(val mutedPackages: Set<String>) : NotificationAppFilterSummaryState
}

internal class NativeNotificationAppFilterActions(
    private val context: Context,
    private val includeAppInventory: Boolean = true,
) : NotificationAppFilterActions {
    override suspend fun load(): NotificationAppFilterLoadResult = withContext(Dispatchers.IO) {
        val session = NativeGateway.session(context) ?: return@withContext NotificationAppFilterLoadResult.Locked
        try {
            val source = session.client.notificationSourceDeviceId()
            val muted = session.client.notificationSnapshot().appFilters
                .filter { it.sourceDeviceId == source && it.muted }
                .mapTo(mutableSetOf()) { it.packageName }
            NotificationAppFilterLoadResult.Ready(if (includeAppInventory) notificationApps(context) else emptyList(), muted)
        } catch (_: MobileBindingsException) {
            NotificationAppFilterLoadResult.Locked
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            NotificationAppFilterLoadResult.Error
        }
    }

    override suspend fun setMuted(app: ObservedNotificationApp, muted: Boolean): Boolean = withContext(Dispatchers.IO) {
        try {
            val session = NativeGateway.session(context) ?: return@withContext false
            session.client.setAppMuted(session.client.notificationSourceDeviceId(), app.packageName, app.label, muted)
            GatewayWork.enqueue(context)
            true
        } catch (_: MobileBindingsException) {
            false
        }
    }
}

@Composable
internal fun NotificationAppFiltersNavRow(
    state: NotificationAppFilterSummaryState,
    onOpenAppFilters: () -> Unit,
) {
    val ready = state is NotificationAppFilterSummaryState.Ready
    val summary = when (state) {
        NotificationAppFilterSummaryState.Loading -> stringResource(R.string.peppy_app_filters_loading)
        NotificationAppFilterSummaryState.Locked -> stringResource(R.string.peppy_app_filters_locked)
        NotificationAppFilterSummaryState.Error -> stringResource(R.string.peppy_app_filters_load_error)
        is NotificationAppFilterSummaryState.Ready -> when (state.mutedPackages.size) {
            0 -> stringResource(R.string.peppy_app_filters_all_allowed)
            1 -> stringResource(R.string.peppy_app_filters_muted_one)
            else -> stringResource(R.string.peppy_app_filters_muted_summary, state.mutedPackages.size)
        }
    }
    Row(
        Modifier
            .fillMaxWidth()
            .testTag("app-filters-nav-row")
            .clickable(enabled = ready, role = Role.Button, onClick = onOpenAppFilters)
            .alpha(if (ready) 1f else DisabledRowAlpha)
            .padding(vertical = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(stringResource(R.string.peppy_app_filters), style = MaterialTheme.typography.bodyMedium)
            Text(summary, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("app-filters-summary"))
        }
        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, contentDescription = null)
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
internal fun NotificationAppFiltersScreen(
    onBack: () -> Unit,
    actions: NotificationAppFilterActions = rememberNativeNotificationAppFilterActions(),
) {
    BackHandler(onBack = onBack)
    var reload by remember { mutableIntStateOf(0) }
    var result by remember { mutableStateOf<NotificationAppFilterLoadResult>(NotificationAppFilterLoadResult.Loading) }
    var query by remember { mutableStateOf("") }
    var saving by remember { mutableStateOf(false) }
    var saveError by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    LaunchedEffect(reload) {
        saveError = false
        result = NotificationAppFilterLoadResult.Loading
        result = actions.load()
    }

    Scaffold(
        modifier = Modifier.fillMaxSize().testTag("notification-app-filters-screen"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.peppy_app_filters)) },
                navigationIcon = {
                    IconButton(onClick = onBack, modifier = Modifier.testTag("app-filters-back")) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.peppy_back))
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .fillMaxSize()
                .padding(padding)
                .consumeWindowInsets(padding)
                .imePadding()
                .padding(top = 8.dp)
                .testTag("app-filters-content"),
        ) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                label = { Text(stringResource(R.string.peppy_app_filters_search)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp).testTag("notification-app-search"),
            )
            OutlinedButton(
                onClick = { reload += 1 },
                enabled = !saving,
                modifier = Modifier.padding(horizontal = 16.dp).testTag("notification-refresh-apps"),
            ) { Text(stringResource(R.string.peppy_app_filters_refresh)) }
            if (saveError) Text(
                stringResource(R.string.peppy_app_filters_save_error),
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier
                    .padding(horizontal = 16.dp)
                    .semantics { liveRegion = LiveRegionMode.Assertive }
                    .testTag("notification-filter-error"),
            )
            when (val state = result) {
                NotificationAppFilterLoadResult.Loading -> Text(
                    stringResource(R.string.peppy_app_filters_loading),
                    modifier = Modifier
                        .padding(horizontal = 16.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite }
                        .testTag("app-filter-loading"),
                )
                NotificationAppFilterLoadResult.Locked -> Text(
                    stringResource(R.string.peppy_app_filters_locked),
                    modifier = Modifier
                        .padding(horizontal = 16.dp)
                        .semantics { liveRegion = LiveRegionMode.Polite }
                        .testTag("app-filter-locked"),
                )
                NotificationAppFilterLoadResult.Error -> Text(
                    stringResource(R.string.peppy_app_filters_load_error),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .padding(horizontal = 16.dp)
                        .semantics { liveRegion = LiveRegionMode.Assertive }
                        .testTag("notification-filter-load-error"),
                )
                is NotificationAppFilterLoadResult.Ready -> {
                    val filtered = state.apps.filter { app ->
                        query.isBlank() || app.label.contains(query, ignoreCase = true) || app.packageName.contains(query, ignoreCase = true)
                    }
                    when {
                        state.apps.isEmpty() -> Text(
                            stringResource(R.string.peppy_app_filters_empty),
                            modifier = Modifier.padding(horizontal = 16.dp).testTag("app-filter-empty"),
                        )
                        filtered.isEmpty() -> Text(
                            stringResource(R.string.peppy_app_filters_no_match),
                            modifier = Modifier.padding(horizontal = 16.dp).testTag("app-filter-no-match"),
                        )
                        else -> LazyColumn(Modifier.fillMaxWidth().weight(1f).testTag("app-filters-list")) {
                            items(filtered, key = { it.packageName }) { app ->
                                Row(
                                    Modifier.fillMaxWidth().testTag("app-filter-${app.packageName}").padding(16.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    Column(Modifier.weight(1f)) {
                                        Text(app.label, style = MaterialTheme.typography.bodyMedium)
                                        Text(app.packageName, style = MaterialTheme.typography.bodySmall)
                                    }
                                    Switch(
                                        checked = app.packageName !in state.mutedPackages,
                                        enabled = !saving,
                                        onCheckedChange = { allowed ->
                                            saving = true
                                            saveError = false
                                            // The checked value changes only after the durable native write succeeds.
                                            scope.launch {
                                                val saved = actions.setMuted(app, !allowed)
                                                val current = result
                                                when {
                                                    !saved -> saveError = true
                                                    current is NotificationAppFilterLoadResult.Ready -> result = current.copy(
                                                        mutedPackages = if (allowed) current.mutedPackages - app.packageName else current.mutedPackages + app.packageName,
                                                    )
                                                }
                                                saving = false
                                            }
                                        },
                                        modifier = Modifier.testTag("app-filter-switch-${app.packageName}"),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun rememberNativeNotificationAppFilterActions(): NotificationAppFilterActions {
    val context = LocalContext.current
    return remember(context) { NativeNotificationAppFilterActions(context.applicationContext) }
}
