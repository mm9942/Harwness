package dev.harw.mobile

import androidx.activity.compose.BackHandler
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import java.time.DateTimeException
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.MobileSessionPage
import uniffi.harw_mobile_ffi.MobileSessionPurpose
import uniffi.harw_mobile_ffi.MobileSessionRow

private const val MaxSystemRows = 250

internal enum class SystemRunGroup(
    val purpose: MobileSessionPurpose,
    @StringRes val labelRes: Int,
) {
    Dream(MobileSessionPurpose.DREAM, R.string.internal_directory_category_dream),
    Diary(MobileSessionPurpose.DIARY, R.string.internal_directory_category_diary),
    Ledger(MobileSessionPurpose.LEDGER, R.string.internal_directory_category_ledger),
    Memory(MobileSessionPurpose.MEMORY_RECONCILIATION, R.string.internal_directory_category_memory),
    Workbench(MobileSessionPurpose.AUTOMATIC_WORKBENCH, R.string.internal_directory_category_workbench),
}

internal fun MobileSessionPurpose.isSystemRun(): Boolean =
    SystemRunGroup.entries.any { it.purpose == this }

internal data class SystemSessionPage(
    val page: MobileSessionPage?,
    val systemCatalog: Boolean,
    val legacyDiscovery: Boolean = false,
)

@Composable
internal fun SystemVirtualProjectTile(onOpen: () -> Unit, modifier: Modifier = Modifier.fillMaxWidth()) {
    OutlinedCard(onClick = onOpen, modifier = modifier) {
        Column(Modifier.padding(horizontal = 14.dp, vertical = 11.dp)) {
            Text(stringResource(R.string.internal_directory_name), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.internal_directory_subtitle), style = MaterialTheme.typography.bodySmall)
        }
    }
}

@Composable
internal fun SystemSessionsScreen(
    state: WorkspaceState,
    connection: HarwConnection,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val scope = remember(state.local, state.profile, state.deviceNodeId) { sessionHomeScope(state) }
    val generation = state.generation
    var group by remember(scope, generation) { mutableStateOf<SystemRunGroup?>(null) }
    var selected by remember(scope, generation) { mutableStateOf<String?>(null) }
    fun back() {
        when {
            selected != null -> selected = null
            group != null -> group = null
            else -> onBack()
        }
    }
    BackHandler { back() }
    if (scope == null || generation == null || !state.online) {
        Column(modifier.fillMaxSize()) {
            TextButton(onClick = ::back) { Text(stringResource(R.string.internal_directory_back_projects)) }
            Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                DirectoryEmpty(stringResource(R.string.internal_directory_disconnected))
            }
        }
        return
    }
    var revision by remember(scope, generation) { mutableIntStateOf(0) }
    var rows by remember(scope, generation) { mutableStateOf(emptyList<MobileSessionRow>()) }
    var next by remember(scope, generation) { mutableStateOf<String?>(null) }
    var loading by remember(scope, generation) { mutableStateOf(true) }
    var unavailable by remember(scope, generation) { mutableStateOf(false) }
    var partialCapability by remember(scope, generation) { mutableStateOf(false) }
    var legacyDiscovery by remember(scope, generation) { mutableStateOf(false) }
    var error by remember(scope, generation) { mutableStateOf(false) }
    val coroutine = rememberCoroutineScope()

    suspend fun load(cursor: String?) {
        loading = true
        error = false
        try {
            val result = connection.systemSessionsPage(scope, generation, cursor) ?: return
            unavailable = result.page == null
            partialCapability = !result.systemCatalog && !unavailable && !result.legacyDiscovery
            legacyDiscovery = result.legacyDiscovery
            val page = result.page ?: return
            val combined = (if (cursor == null) emptyList() else rows) + page.sessions
            rows = combined.distinctBy { it.sessionId }.take(MaxSystemRows)
            next = page.nextCursor?.takeIf { rows.size < MaxSystemRows && it != cursor }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            if (connection.canUseSessionHome(scope, generation)) error = true
        } finally {
            loading = false
        }
    }
    LaunchedEffect(connection, scope, generation, revision) {
        rows = emptyList()
        next = null
        load(null)
    }

    val selectedRow = rows.singleOrNull { it.sessionId == selected }
    val selectedGroup = group
    Column(modifier.fillMaxSize().padding(horizontal = 12.dp)) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = ::back) { Text(stringResource(R.string.internal_directory_back)) }
            Text(
                if (selectedGroup != null) {
                    stringResource(selectedGroup.labelRes)
                } else {
                    val scopeLabel = stringResource(
                        if (state.local) R.string.internal_directory_scope_local
                        else R.string.internal_directory_scope_cloud,
                    )
                    stringResource(
                        R.string.internal_directory_scope_title,
                        stringResource(R.string.internal_directory_name),
                        scopeLabel,
                    )
                },
                Modifier.weight(1f), style = MaterialTheme.typography.titleMedium,
            )
            TextButton(onClick = { revision++ }, enabled = !loading) { Text(stringResource(R.string.internal_directory_refresh)) }
        }
        if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
        LazyColumn(Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (unavailable) item { Text(stringResource(R.string.internal_directory_unsupported)) }
            if (partialCapability) item { Text(stringResource(R.string.internal_directory_dream_only)) }
            if (legacyDiscovery) item { Text(stringResource(R.string.internal_directory_legacy_dream)) }
            if (error) item { StatusMessage(stringResource(R.string.internal_directory_load_error), error = true) }
            when {
                selectedRow != null -> item {
                    Column(Modifier.padding(10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(systemRunLabel(selectedRow.purpose), style = MaterialTheme.typography.titleSmall)
                        Text(stringResource(R.string.internal_directory_last_activity, systemRunTime(selectedRow.updatedAt)))
                        // The host projects these rows as non-continuable. Its
                        // synthetic state is not evidence of the run outcome.
                        Text(stringResource(R.string.internal_directory_access))
                        Text(
                            stringResource(R.string.internal_directory_details_note),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                selectedGroup == null -> {
                    items(SystemRunGroup.entries, key = { it.name }) { category ->
                        OutlinedCard(onClick = { group = category }, modifier = Modifier.fillMaxWidth()) {
                            Text(stringResource(category.labelRes), Modifier.padding(13.dp), style = MaterialTheme.typography.titleSmall)
                        }
                    }
                }
                else -> {
                    val entries = rows.filter { it.purpose == selectedGroup.purpose }
                    if (!loading && entries.isEmpty()) item { Text(stringResource(R.string.internal_directory_empty_group)) }
                    items(entries, key = { it.sessionId }) { row ->
                        OutlinedCard(onClick = { selected = row.sessionId }, modifier = Modifier.fillMaxWidth()) {
                            Column(Modifier.padding(13.dp)) {
                                Text(systemRunLabel(row.purpose), style = MaterialTheme.typography.titleSmall)
                                Text(systemRunTime(row.updatedAt), style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                }
            }
            val cursor = next
            if (cursor != null && !loading && selectedRow == null) {
                item { TextButton(onClick = { coroutine.launch { load(cursor) } }) { Text(stringResource(R.string.internal_directory_load_more)) } }
            }
        }
    }
}

@Composable
private fun systemRunLabel(purpose: MobileSessionPurpose): String = stringResource(
    SystemRunGroup.entries.firstOrNull { it.purpose == purpose }?.labelRes
        ?: R.string.internal_directory_unknown_run,
)

@Composable
private fun systemRunTime(isoTime: String): String {
    val locale = LocalConfiguration.current.locales[0] ?: Locale.getDefault()
    val value = try {
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.SHORT)
            .withLocale(locale)
            .withZone(ZoneId.systemDefault())
            .format(Instant.parse(isoTime))
    } catch (_: DateTimeException) {
        null
    }
    return value ?: stringResource(R.string.internal_directory_time_unavailable)
}
