package dev.harw.mobile

import android.content.SharedPreferences
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.outlined.PushPin
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.FolderOpen
import androidx.compose.material.icons.outlined.ViewList
import androidx.compose.material.icons.outlined.Sort
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import uniffi.harw_mobile_ffi.MobileSessionState
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.MobileSessionPage
import uniffi.harw_mobile_ffi.MobileSessionRow

internal const val SessionHomePageSize = 50
private const val SessionHomeMaxRows = 500
private const val MaxInitialPinPages = 4

internal data class HomeProjectLinks(
    val open: (String) -> Unit,
    val create: () -> Unit,
    val newSession: (String) -> Unit,
    val rename: (String, String?) -> Unit,
    val openInternal: (() -> Unit)?,
    val suggestion: ProjectSuggestion?,
    val acceptSuggestion: (ProjectSuggestion) -> Unit,
    val dismissSuggestion: (ProjectSuggestion) -> Unit,
)

@Composable
internal fun SessionHomeScreen(
    state: WorkspaceState,
    connection: HarwConnection,
    projectNames: Map<String, String>,
    onOpen: (String) -> Unit,
    projectLinks: HomeProjectLinks,
    modifier: Modifier = Modifier,
) {
    val scope = remember(state.local, state.profile, state.deviceNodeId) { sessionHomeScope(state) }
    val generation = state.generation
    if (scope == null || generation == null || !state.online) {
        Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            DirectoryEmpty(
                if (state.local) stringResource(R.string.unified_home_no_local)
                else stringResource(R.string.unified_home_no_cloud),
            )
        }
        return
    }
    val context = LocalContext.current
    val prefs = remember(context) { SessionHomePreferences(context) }
    var pins by remember(prefs, scope) { mutableStateOf(prefs.pins(scope)) }
    var grouping by remember(prefs, scope) { mutableStateOf(prefs.grouping(scope)) }
    var sort by remember(prefs, scope) { mutableStateOf(prefs.sort(scope)) }
    var sortOpen by remember { mutableStateOf(false) }
    var renamedKey by remember(scope, generation) { mutableStateOf<String?>(null) }
    val uiScope = rememberCoroutineScope()
    var revision by remember(scope, generation) { mutableIntStateOf(0) }
    var rows by remember(scope, generation) { mutableStateOf<List<MobileSessionRow>>(emptyList()) }
    var cursor by remember(scope, generation) { mutableStateOf<String?>(null) }
    var bounded by remember(scope, generation) { mutableStateOf(false) }
    var loading by remember(scope, generation) { mutableStateOf(true) }
    var error by remember(scope, generation) { mutableStateOf(false) }
    var legacy by remember(scope, generation) { mutableStateOf(false) }
    var pinNotice by remember(scope, generation) { mutableStateOf<String?>(null) }

    DisposableEffect(prefs, scope) {
        val key = prefs.pinKey(scope)
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changed ->
            if (changed == key) pins = prefs.pins(scope)
        }
        prefs.prefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { prefs.prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }

    fun applyPage(page: MobileSessionPage, replace: Boolean, previousCursor: String?) {
        val all = (if (replace) emptyList() else rows) + page.sessions
        val ordered = all.distinctBy { it.sessionId }
        rows = ordered.take(SessionHomeMaxRows)
        legacy = page.legacy
        val next = page.nextCursor?.takeUnless { it == previousCursor }
        bounded = ordered.size > SessionHomeMaxRows ||
            (rows.size == SessionHomeMaxRows && next != null)
        cursor = if (bounded) null else next
    }

    LaunchedEffect(connection, scope, generation, revision) {
        loading = true
        rows = emptyList()
        cursor = null
        error = false
        try {
            val first = connection.sessionHomePage(scope, generation, null) ?: return@LaunchedEffect
            applyPage(first, replace = true, previousCursor = null)
            // Resolve a few older pins without keeping unverified cached titles or scanning endlessly.
            val wanted = pins.mapTo(hashSetOf()) { it.sessionId }
            repeat(MaxInitialPinPages - 1) {
                val next = cursor ?: return@repeat
                if (wanted.isEmpty() || rows.mapTo(hashSetOf()) { it.sessionId }.containsAll(wanted)) return@repeat
                val page = connection.sessionHomePage(scope, generation, next) ?: return@LaunchedEffect
                applyPage(page, replace = false, previousCursor = next)
            }
        } catch (failure: CancellationException) {
            throw failure
        } catch (_: Exception) {
            if (connection.canUseSessionHome(scope, generation)) error = true
        } finally {
            loading = false
        }
    }

    val byId = remember(rows) { rows.associateBy { it.sessionId } }
    val projection = remember(scope, rows, pins) {
        projectUnifiedSessionHome(
            scope, rows.map { row ->
                SessionHomeRow(
                    SessionHomeId(scope, row.sessionId),
                    if (row.purpose.isSystemRun()) SessionHomeKind.Internal
                    else SessionHomeKind.UnknownLegacy,
                    parseActivityMillis(row.updatedAt), row.displayTitle(),
                )
            },
            pins,
        )
    }
    val projects = remember(state.projects) { state.projects.associateBy { it.projectKey } }

    fun label(projectKey: String?): String {
        if (projectKey.isNullOrEmpty()) return if (legacy) context.getString(R.string.unified_home_project_unknown) else context.getString(R.string.unified_home_no_project)
        val project = projects[projectKey]
        return projectNames[projectKey]?.takeUnless(String::isBlank)
            ?: project?.label?.takeUnless(String::isBlank)
            ?: context.getString(R.string.unified_home_unnamed)
    }

    val locale = context.resources.configuration.locales[0] ?: java.util.Locale.ENGLISH
    val ordinary = remember(projection, sort, locale) {
        orderUnifiedHome(projection.ordinary, sort, { it.title.orEmpty() }, { it.activityAtMs }, { it.id.sessionId }, locale)
    }
    val orderedProjects = remember(state.projects, projectNames, sort, locale) {
        orderUnifiedHome(
            state.projects, sort,
            { projectNames[it.projectKey] ?: if (it.projectKey == NoProjectKey) context.getString(R.string.unified_home_no_project) else it.displayLabel() },
            { parseActivityMillis(it.lastActivity) }, { it.projectKey }, locale,
        )
    }
    renamedKey?.let { key ->
        val renamed = projects[key]
        if (renamed != null) {
            var name by remember(key) { mutableStateOf(projectNames[key] ?: renamed.displayLabel()) }
            AlertDialog(
                onDismissRequest = { renamedKey = null },
                title = { Text(stringResource(R.string.unified_home_rename)) },
                text = { OutlinedTextField(value = name, onValueChange = { name = it }, singleLine = true, label = { Text(stringResource(R.string.unified_home_name)) }) },
                confirmButton = {
                    TextButton(enabled = name.isNotBlank(), onClick = {
                        projectLinks.rename(key, name)
                        renamedKey = null
                    }) { Text(stringResource(R.string.unified_home_save)) }
                },
                dismissButton = {
                    Row {
                        if (key in projectNames) TextButton(onClick = {
                            projectLinks.rename(key, null)
                            renamedKey = null
                        }) { Text(stringResource(R.string.unified_home_reset)) }
                        TextButton(onClick = { renamedKey = null }) { Text(stringResource(R.string.unified_home_cancel)) }
                    }
                },
            )
        }
    }
    val canLoadMore = cursor != null && !loading
    fun browseMore() {
        val next = cursor ?: return
        if (loading || !connection.canUseSessionHome(scope, generation)) return
        loading = true
        error = false
        uiScope.launch {
            try {
                val page = connection.sessionHomePage(scope, generation, next) ?: return@launch
                if (cursor == next) {
                    applyPage(page, replace = false, previousCursor = next)
                }
            } catch (failure: CancellationException) {
                throw failure
            } catch (_: Exception) {
                if (connection.canUseSessionHome(scope, generation)) error = true
            } finally {
                loading = false
            }
        }
    }

    val rowContent: @Composable (SessionHomeRow) -> Unit = { summary ->
        byId[summary.id.sessionId]?.let { row ->
            val isInternal = summary.kind == SessionHomeKind.Internal
            val togglePin: () -> Unit = {
                if (connection.canUseSessionHome(scope, generation)) {
                    pinNotice = if (prefs.togglePin(summary.id)) {
                        pins = prefs.pins(scope)
                        null
                    } else {
                        context.getString(R.string.unified_home_limit, SessionHomePinsLimit)
                    }
                }
            }
            SessionHomeTile(
                title = summary.title?.takeUnless { it.isBlank() } ?: row.displayTitle(),
                project = label(row.projectKey),
                detail = stringResource(homeStateLabel(row.state)),
                pinned = !isInternal && summary.id in pins,
                enabled = !isInternal,
                onOpen = {
                    if (!isInternal && connection.canUseSessionHome(scope, generation)) onOpen(summary.id.sessionId)
                },
                onPin = if (isInternal) null else togglePin,
            )
        }
    }
    Column(modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                stringResource(R.string.unified_home_header), Modifier.weight(1f),
                style = MaterialTheme.typography.titleSmall,
            )
            IconButton(onClick = {
                grouping = if (grouping == HomeGrouping.Projects) HomeGrouping.Sessions else HomeGrouping.Projects
                prefs.setGrouping(scope, grouping)
            }) {
                if (grouping == HomeGrouping.Projects) Icon(Icons.Outlined.ViewList, stringResource(R.string.unified_home_show_sessions))
                else Icon(Icons.Outlined.FolderOpen, stringResource(R.string.unified_home_show_projects))
            }
            Box {
                IconButton(onClick = { sortOpen = true }) { Icon(Icons.Outlined.Sort, stringResource(R.string.unified_home_sort, stringResource(sort.labelRes))) }
                DropdownMenu(expanded = sortOpen, onDismissRequest = { sortOpen = false }) {
                    for (option in HomeListSort.entries) {
                        DropdownMenuItem(
                            text = { Text((if (option == sort) "✓ " else "") + stringResource(option.labelRes)) },
                            onClick = { sort = option; prefs.setSort(scope, option); sortOpen = false },
                        )
                    }
                }
            }
            IconButton(onClick = { revision++ }, enabled = !loading) {
                Icon(Icons.Outlined.Refresh, stringResource(R.string.unified_home_refresh))
            }
        }
        if (loading) LinearProgressIndicator(Modifier.fillMaxWidth())
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(start = 12.dp, top = 8.dp, end = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (error) item(key = "refresh-error") {
                StatusMessage(stringResource(R.string.unified_home_page_error), error = true)
            }
            pinNotice?.let { note -> item(key = "pin-notice") { StatusMessage(note, error = true) } }
            if (legacy) item(key = "legacy-note") {
                Text(stringResource(R.string.unified_home_warning), style = MaterialTheme.typography.bodySmall)
            }
            homeHeading("pinned", R.string.unified_home_favorites)
            if (projection.pinned.isEmpty()) item(key = "empty-favorites") {
                Text(stringResource(R.string.unified_home_no_favorites), style = MaterialTheme.typography.bodySmall)
            }
            items(projection.pinned, key = { "session_${it.id.sessionId}" }) { rowContent(it) }

            homeHeading("recent", R.string.unified_home_recent)
            if (!loading && projection.recent.isEmpty()) item(key = "empty-recent") {
                Text(stringResource(R.string.unified_home_no_recent), style = MaterialTheme.typography.bodySmall)
            }
            items(projection.recent, key = { "session_${it.id.sessionId}" }) { rowContent(it) }

            if (grouping == HomeGrouping.Projects) {
                homeHeading("projects", R.string.unified_home_projects)
                projectLinks.openInternal?.let { open ->
                    item(key = "virtual-internal-project") { SystemVirtualProjectTile(onOpen = open, modifier = Modifier.fillMaxWidth()) }
                }
                item(key = "new-project") {
                    TextButton(enabled = state.online, onClick = projectLinks.create) { Text(stringResource(R.string.unified_home_new_project)) }
                }
                state.projectsError?.let { _ -> item(key = "project-error") {
                    StatusMessage(stringResource(R.string.unified_home_project_error), error = true)
                } }
                projectLinks.suggestion?.let { suggestion ->
                    item(key = "project-suggestion") {
                        Surface(shape = RoundedCornerShape(14.dp), color = MaterialTheme.colorScheme.surfaceContainerLow) {
                            Column(Modifier.fillMaxWidth().padding(12.dp)) {
                                Text(stringResource(R.string.unified_home_listed_project, suggestion.name), style = MaterialTheme.typography.titleSmall)
                                Row {
                                    TextButton(onClick = { projectLinks.acceptSuggestion(suggestion) }) { Text(stringResource(R.string.unified_home_create)) }
                                    TextButton(onClick = { projectLinks.dismissSuggestion(suggestion) }) { Text(stringResource(R.string.unified_home_not_now)) }
                                }
                            }
                        }
                    }
                }
                items(orderedProjects, key = { "project_${it.projectKey}" }) { project ->
                    ProjectRow(
                        label = projectNames[project.projectKey] ?: projectDisplayLabel(project),
                        onRename = if (project.projectKey == NoProjectKey) null else ({ renamedKey = project.projectKey }),
                        project = project,
                        nowMillis = System.currentTimeMillis(),
                        canCreate = state.online && state.canCreate && !state.creating,
                        onSelect = { projectLinks.open(project.projectKey) },
                        onNewSession = { projectLinks.newSession(project.projectKey) },
                    )
                }
                if (orderedProjects.isEmpty() && state.projectsLoaded) item(key = "empty-projects") {
                    Text(stringResource(R.string.unified_home_no_projects), style = MaterialTheme.typography.bodySmall)
                }
            } else {
                homeHeading("all-sessions", R.string.unified_home_other)
                items(ordinary, key = { "session_${it.id.sessionId}" }) { rowContent(it) }
                if (!loading && !error && ordinary.isEmpty()) item(key = "empty-other-sessions") {
                    Text(stringResource(R.string.unified_home_no_other), style = MaterialTheme.typography.bodySmall)
                }
            }
            if (grouping == HomeGrouping.Sessions && bounded) item(key = "bounded") {
                Text(
                    stringResource(R.string.unified_home_bounded, SessionHomeMaxRows),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            if (grouping == HomeGrouping.Sessions && canLoadMore) item(key = "more") {
                TextButton(onClick = ::browseMore, modifier = Modifier.fillMaxWidth()) { Text(stringResource(R.string.unified_home_load_more)) }
            }
        }
    }
}

private fun LazyListScope.homeHeading(key: String, titleRes: Int) {
    item(key = "heading_$key") {
        Text(stringResource(titleRes), Modifier.semantics { heading() }, style = MaterialTheme.typography.titleSmall)
    }
}

@Composable
private fun SessionHomeTile(
    title: String,
    project: String,
    detail: String,
    pinned: Boolean,
    enabled: Boolean = true,
    onOpen: () -> Unit,
    onPin: (() -> Unit)?,
) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Surface(
            onClick = onOpen,
            enabled = enabled,
            modifier = Modifier.weight(1f),
            shape = RoundedCornerShape(13.dp),
            color = MaterialTheme.colorScheme.surfaceContainerLow,
        ) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 9.dp)) {
                Text(title, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
                Text(
                    "$project · $detail", style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2, overflow = TextOverflow.Ellipsis,
                )
            }
        }
        if (onPin != null) IconButton(onClick = onPin) {
            Icon(
                if (pinned) Icons.Filled.PushPin else Icons.Outlined.PushPin,
                stringResource(if (pinned) R.string.unified_home_unpin else R.string.unified_home_pin, title),
            )
        }
    }
}

internal fun homeStateLabel(state: MobileSessionState): Int = when (state) {
    MobileSessionState.IDLE -> R.string.unified_home_ready
    MobileSessionState.RUNNING -> R.string.unified_home_running
    MobileSessionState.WAITING_FOR_APPROVAL -> R.string.unified_home_approval
    MobileSessionState.WAITING_FOR_CHILD -> R.string.unified_home_child
    MobileSessionState.QUEUED -> R.string.unified_home_queued
    MobileSessionState.INTERRUPTED -> R.string.unified_home_interrupted
    MobileSessionState.FAILED -> R.string.unified_home_failed
    MobileSessionState.CLOSED -> R.string.unified_home_closed
}
