package dev.harw.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.MobileLastOrigin
import uniffi.harw_mobile_ffi.MobileProject
import uniffi.harw_mobile_ffi.MobileReadOnlyReason
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileSessionRow
import uniffi.harw_mobile_ffi.MobileShareInfo
import uniffi.harw_mobile_ffi.MobileShareMode
import uniffi.harw_mobile_ffi.MobileTier
import uniffi.harw_mobile_ffi.MobileSessionState

@Composable
internal fun WorkspaceScreen(
    state: WorkspaceState,
    window: HarwWindowModel,
    connection: HarwConnection,
    authorizingApproval: Boolean,
    onDecide: (String, Long, String, ApprovalDecision) -> Unit,
    onOpenSettings: () -> Unit,
    onNewWindow: (String) -> Unit,
    projectView: ProjectSessionsState? = null,
    projectActions: ProjectSessionsActions? = null,
    onSwitchBackend: (Boolean) -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val sessionId = window.sessionId
    val session = state.sessions.firstOrNull { it.sessionId == sessionId }
    var agentVersion by remember(connection, state.profile, sessionId) { mutableIntStateOf(0) }
    val archiveMenu = remember(connection, state.profile, sessionId) { ChatArchiveMenuEntry() }
    val conversationState = rememberSaveableStateHolder()
    var creating by rememberSaveable { mutableStateOf(false) }
    if (creating) {
        NewSessionDialog(
            onDismiss = { creating = false },
            enabled = state.online && state.canCreate && !state.creating,
            onCreate = {
                creating = false
                connection.createSession(it, window::select)
            },
        )
    }

    BoxWithConstraints(modifier.fillMaxSize()) {
        val split = maxWidth >= 650.dp
        val separateActivity = maxWidth >= 820.dp && sessionId != null
        val roomy = maxWidth >= 985.dp
        Column(Modifier.fillMaxSize()) {
            HarwHeader(
                title = session?.displayName() ?: sessionId?.let { "Session ${it.take(8)}" } ?: stringResource(R.string.workspace_title),
                onBack = if (sessionId != null && !split) ({ window.select(null) }) else null,
            ) {
                if (sessionId == null) {
                    ConnectionBadge(state)
                } else {
                    ChatRouteBadge(state)
                    SessionChatSettings(state, window, sessionId, session, connection, agentVersion, archiveMenu) { agentVersion++ }
                    IconButton(onClick = onOpenSettings) { Icon(Icons.Outlined.Settings, "Einstellungen") }
                }
            }
            Row(Modifier.fillMaxSize()) {
                if ((split || sessionId == null) && projectView != null && projectActions != null) {
                    ProjectSessionList(
                        view = projectView,
                        actions = projectActions,
                        selected = sessionId,
                        online = state.online,
                        showNew = split,
                        onSelect = window::select,
                        modifier = when {
                            split -> Modifier.width(if (roomy) 245.dp else 205.dp)
                            else -> Modifier.weight(1f)
                        },
                    )
                } else if (split || sessionId == null) {
                    SessionDirectory(
                        state = state,
                        selected = sessionId,
                        narrow = split,
                        onSwitchBackend = onSwitchBackend,
                        onNew = { creating = true },
                        onRefresh = connection::refreshSessions,
                        onSelect = window::select,
                        onNewWindow = onNewWindow,
                        modifier = when {
                            split -> Modifier.width(if (roomy) 245.dp else 205.dp)
                            else -> Modifier.weight(1f)
                        },
                    )
                }
                if (split) VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                if (sessionId != null) {
                    val alertScope = window.alertTarget?.scope
                    val conversationKey = if (alertScope == null) sessionId
                    else "alert:${alertScope.backend.name}:${alertScope.profileId}:$sessionId"
                    conversationState.SaveableStateProvider(conversationKey) {
                        SessionConversation(
                            state = state,
                            window = window,
                            sessionId = sessionId,
                            session = session,
                            connection = connection,
                            authorizingApproval = authorizingApproval,
                            onDecide = onDecide,
                            archiveMenu = archiveMenu,
                            agentVersion = agentVersion,
                            modifier = Modifier.weight(1f).fillMaxHeight(),
                        )
                    }
                    if (separateActivity) {
                        VerticalDivider(color = MaterialTheme.colorScheme.outlineVariant)
                        SessionActivityPanel(
                            state, session, state.windows[sessionId],
                            Modifier.width(if (roomy) 210.dp else 170.dp).fillMaxHeight(),
                        )
                    }
                } else if (split) {
                    Box(Modifier.weight(1f).fillMaxHeight(), contentAlignment = Alignment.Center) {
                        DirectoryEmpty("Wähle eine Session, um den Chat zu öffnen.")
                    }
                }
            }
        }
    }
}

@Composable
private fun SessionDirectory(
    state: WorkspaceState,
    selected: String?,
    narrow: Boolean,
    onSwitchBackend: (Boolean) -> Unit,
    onNew: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (String) -> Unit,
    onNewWindow: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val inset = if (narrow) 12.dp else 18.dp
    Column(modifier.fillMaxHeight().padding(top = 9.dp)) {
        if (!narrow) BackendSwitch(local = state.local, enabled = !state.stopping, onSelect = onSwitchBackend)
        Row(
            Modifier.fillMaxWidth().padding(horizontal = inset),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                if (narrow) stringResource(R.string.workspace_title) else if (state.local) stringResource(R.string.directory_local) else state.profile?.label ?: stringResource(R.string.workspace_title),
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            IconButton(onClick = onRefresh, enabled = state.online) { Icon(Icons.Outlined.Refresh, stringResource(R.string.workspace_refresh_sessions)) }
            val canCreate = state.online && state.canCreate && !state.creating
            val newIcon: @Composable () -> Unit = {
                if (state.creating) {
                    CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
                } else {
                    Icon(Icons.Outlined.Add, contentDescription = if (narrow) stringResource(R.string.directory_new_session) else null, Modifier.size(18.dp))
                }
            }
            if (narrow) FilledIconButton(onClick = onNew, enabled = canCreate, content = newIcon)
        }
        if (state.sessions.isEmpty()) {
            EmptySessionDirectory(state, inset, onRefresh)
        } else {
            LazyColumn(
                contentPadding = PaddingValues(start = inset, end = inset, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (!state.online) {
                    item { StatusMessage(offlineMessage(state, hasSessions = true), error = state.phase.isFailure()) }
                }
                state.listError?.let { message ->
                    item { StatusMessage(stringResource(R.string.workspace_session_refresh_error, message), error = true) }
                }
                if (state.online && !state.canCreate) {
                    item { StatusMessage(stringResource(R.string.directory_create_denied)) }
                }
                items(state.sessions, key = { it.sessionId }) { session ->
                    SessionRow(
                        session = session,
                        selected = session.sessionId == selected,
                        onSelect = { onSelect(session.sessionId) },
                        onNewWindow = if (narrow) null else ({ onNewWindow(session.sessionId) }),
                    )
                }
            }
        }
    }
}

@Composable
private fun EmptySessionDirectory(state: WorkspaceState, inset: Dp, onRefresh: () -> Unit) {
    Box(Modifier.fillMaxSize().padding(inset), contentAlignment = Alignment.Center) {
        when {
            state.online && !state.sessionsLoaded && state.listError == null -> Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(22.dp))
                Text(
                    stringResource(R.string.workspace_session_loading),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            state.listError != null -> Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                StatusMessage(stringResource(R.string.workspace_session_load_error, state.listError.orEmpty()), error = true)
                if (state.online) TextButton(onClick = onRefresh) { Text(stringResource(R.string.directory_retry)) }
            }
            !state.online -> DirectoryEmpty(offlineMessage(state, hasSessions = false))
            state.canCreate -> DirectoryEmpty(stringResource(R.string.workspace_sessions_empty))
            else -> DirectoryEmpty(stringResource(R.string.workspace_no_sessions_visible))
        }
    }
}

@Composable
private fun SessionRow(
    session: MobileSession,
    selected: Boolean,
    onSelect: () -> Unit,
    onNewWindow: (() -> Unit)?,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        shape = RoundedCornerShape(15.dp),
        border = BorderStroke(1.dp, if (selected) colors.primary else colors.outlineVariant),
        color = if (selected) colors.primaryContainer else colors.surfaceContainerLowest,
    ) {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onSelect).padding(start = 13.dp, end = 4.dp, top = 10.dp, bottom = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.Forum, contentDescription = null, Modifier.size(18.dp), tint = colors.primary)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(
                    session.displayName(), style = MaterialTheme.typography.titleSmall,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                val detail = listOfNotNull(stringResource(homeStateLabel(session.state)), session.model?.takeUnless { it.isBlank() })
                    .joinToString(" · ")
                Text(detail, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            if (onNewWindow != null) {
                IconButton(onClick = onNewWindow) {
                    Icon(Icons.Outlined.OpenInNew, stringResource(R.string.workspace_open_session_window, session.displayName()), Modifier.size(19.dp))
                }
            }
        }
    }
}

@Composable
internal fun DirectoryEmpty(message: String) {
    Column(
        Modifier.padding(12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Icon(Icons.Outlined.Forum, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
        Text(message, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun NewSessionDialog(enabled: Boolean, onDismiss: () -> Unit, onCreate: (String?) -> Unit) {
    var title by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.directory_new_session)) },
        text = {
            OutlinedTextField(
                value = title, onValueChange = { title = it },
                label = { Text(stringResource(R.string.workspace_optional_title)) },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
            )
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.directory_cancel)) } },
        confirmButton = {
            Button(onClick = { onCreate(title.trim().ifBlank { null }) }, enabled = enabled) { Text(stringResource(R.string.directory_create)) }
        },
    )
}

private fun ConnectionPhase.isFailure(): Boolean =
    this == ConnectionPhase.FAILED || this == ConnectionPhase.REVOKED

@Composable
private fun offlineMessage(state: WorkspaceState, hasSessions: Boolean): String {
    val base = if (state.stopping) R.string.workspace_disconnecting else when (state.phase) {
        ConnectionPhase.CONNECTING -> R.string.workspace_connecting
        ConnectionPhase.RECONNECTING -> R.string.workspace_reconnecting
        ConnectionPhase.FAILED -> R.string.workspace_connection_failed
        ConnectionPhase.REVOKED -> R.string.workspace_connection_revoked
        ConnectionPhase.DISCONNECTED, ConnectionPhase.CONNECTED -> R.string.workspace_connection_offline
    }
    val detail = state.detail?.takeUnless { it.isBlank() }
    val tail = if (hasSessions) R.string.workspace_sessions_cached else R.string.workspace_connect_to_load
    return listOfNotNull(stringResource(base), detail, stringResource(tail)).joinToString(" ")
}

internal fun MobileSession.displayName(): String = title?.takeUnless { it.isBlank() } ?: "Session ${sessionId.take(8)}"

internal fun MobileSessionState.displayName(): String = when (this) {
    MobileSessionState.IDLE -> "Bereit"
    MobileSessionState.RUNNING -> "Läuft"
    MobileSessionState.WAITING_FOR_APPROVAL -> "Wartet auf Freigabe"
    MobileSessionState.WAITING_FOR_CHILD -> "Wartet auf Unter-Session"
    MobileSessionState.QUEUED -> "In der Warteschlange"
    MobileSessionState.INTERRUPTED -> "Unterbrochen"
    MobileSessionState.FAILED -> "Fehlgeschlagen"
    MobileSessionState.CLOSED -> "Geschlossen"
}

// ---- Projektansicht (Paket A4) ----

/** Zustand der Sessions eines Projekts; wird von HarwConnection (A5) befuellt. */
internal data class ProjectSessionsState(
    val project: MobileProject?,
    val rows: List<MobileSessionRow> = emptyList(),
    val hasMore: Boolean = false,
    val loading: Boolean = false,
    val error: String? = null,
    val legacy: Boolean = false,
    val share: MobileShareInfo? = null,
    /** Ziele fuer "Projekt zuweisen". */
    val projects: List<MobileProject> = emptyList(),
    val query: String = "",
    /** Schluessel der offenen Ansicht; "" = "Ohne Projekt". */
    val projectKey: String = NoProjectKey,
)

internal data class ProjectSessionSection(val heading: String?, val rows: List<MobileSessionRow>)

internal fun projectSessionSections(
    view: ProjectSessionsState,
    monthTitle: (String?) -> String = ::monthHeading,
): List<ProjectSessionSection> = when {
    view.rows.isEmpty() -> emptyList()
    view.projectKey == NoProjectKey ->
        groupByMonth(view.rows, { it.updatedAt }).map { (key, rows) -> ProjectSessionSection(monthTitle(key), rows) }
    else -> listOf(ProjectSessionSection(null, view.rows))
}

internal class ProjectSessionsActions(
    val onNew: () -> Unit,
    val onRefresh: () -> Unit,
    val onLoadMore: () -> Unit,
    val onRename: (sessionId: String, title: String) -> Unit,
    val onArchive: (sessionId: String) -> Unit,
    val onAssign: (sessionId: String, projectKey: String) -> Unit,
)

internal enum class OwnerMark { RUNNING_ON_GATEWAY, HELD_BY_TERMINAL, HELD_BY_JOB, HELD, LAST_PHONE, LAST_TERMINAL }

/** Besitz-Marke: aktueller Halter hat Vorrang vor der zuletzt schreibenden Seite. */
internal fun MobileSessionRow.ownerMark(): OwnerMark? {
    holder?.let {
        return when (it.kind) {
            MobileHolderKind.GATEWAY -> OwnerMark.RUNNING_ON_GATEWAY
            MobileHolderKind.TUI -> OwnerMark.HELD_BY_TERMINAL
            MobileHolderKind.JOB -> OwnerMark.HELD_BY_JOB
            MobileHolderKind.UNKNOWN -> OwnerMark.HELD
        }
    }
    return when (lastOrigin) {
        MobileLastOrigin.GATEWAY -> OwnerMark.LAST_PHONE
        MobileLastOrigin.TUI -> OwnerMark.LAST_TERMINAL
        else -> null
    }
}

internal fun OwnerMark.label(): String = when (this) {
    OwnerMark.RUNNING_ON_GATEWAY -> "Läuft auf Gateway"
    OwnerMark.HELD_BY_TERMINAL -> "Belegt im Terminal"
    OwnerMark.HELD_BY_JOB -> "Belegt durch Job"
    OwnerMark.HELD -> "Belegt"
    OwnerMark.LAST_PHONE -> "Zuletzt: Handy"
    OwnerMark.LAST_TERMINAL -> "Zuletzt: Terminal"
}

internal fun MobileTier.label(): String = when (this) {
    MobileTier.OBSERVER -> "Nur beobachten"
    MobileTier.OPERATOR -> "Bedienen"
    MobileTier.MAINTAINER -> "Verwalten"
    MobileTier.OWNER -> "Besitzer"
}

internal fun MobileReadOnlyReason.label(): String = when (this) {
    MobileReadOnlyReason.SHARE_READ_ONLY -> "Dieses Gateway teilt nur lesend"
    MobileReadOnlyReason.FOREIGN_ORIGIN -> "Stammt aus einem anderen Kanal"
    MobileReadOnlyReason.NO_WORKDIR -> "Arbeitsordner fehlt"
    MobileReadOnlyReason.NO_STEER -> "Keine Berechtigung zum Schreiben"
    MobileReadOnlyReason.CLOSED -> "Beendet"
    MobileReadOnlyReason.UNKNOWN -> "Nur lesen"
}

private fun OwnerMark.workspaceLabel(): Int = when (this) {
    OwnerMark.RUNNING_ON_GATEWAY -> R.string.workspace_gateway_running
    OwnerMark.HELD_BY_TERMINAL -> R.string.workspace_terminal_held
    OwnerMark.HELD_BY_JOB -> R.string.workspace_job_held
    OwnerMark.HELD -> R.string.workspace_held
    OwnerMark.LAST_PHONE -> R.string.workspace_last_phone
    OwnerMark.LAST_TERMINAL -> R.string.workspace_last_terminal
}

private fun MobileTier.workspaceLabel(): Int = when (this) {
    MobileTier.OBSERVER -> R.string.workspace_observer
    MobileTier.OPERATOR -> R.string.workspace_operator
    MobileTier.MAINTAINER -> R.string.workspace_maintainer
    MobileTier.OWNER -> R.string.workspace_owner
}

private fun MobileReadOnlyReason.workspaceLabel(): Int = when (this) {
    MobileReadOnlyReason.SHARE_READ_ONLY -> R.string.workspace_share_read_only
    MobileReadOnlyReason.FOREIGN_ORIGIN -> R.string.workspace_foreign_origin
    MobileReadOnlyReason.NO_WORKDIR -> R.string.workspace_no_workdir
    MobileReadOnlyReason.NO_STEER -> R.string.workspace_no_write_permission
    MobileReadOnlyReason.CLOSED -> R.string.workspace_closed
    MobileReadOnlyReason.UNKNOWN -> R.string.workspace_read_only
}

internal fun MobileSessionRow.displayTitle(): String =
    title?.takeUnless { it.isBlank() } ?: "Session ${sessionId.take(8)}"

/** Leer-Text der Projektansicht; null, wenn Zeilen vorhanden sind. */
internal fun projectEmptyText(
    view: ProjectSessionsState,
    canCreate: Boolean,
    localized: (resource: Int, substitute: String?, fallback: String) -> String = { _, _, fallback -> fallback },
): String? {
    if (view.rows.isNotEmpty()) return null
    return when {
        view.query.isNotBlank() -> localized(
            R.string.workspace_project_search_empty, view.query.trim(), "Keine Treffer für „${view.query.trim()}“.",
        )
        view.share?.mode == MobileShareMode.OFF && !view.legacy -> localized(
            R.string.workspace_project_share_off, null, "Dieses Gateway teilt keine TUI-Sessions.",
        )
        view.legacy -> localized(
            R.string.workspace_project_legacy, null, "Dieser Server kennt keine Projekte. Es werden nur Handy-Sessions angezeigt.",
        )
        canCreate -> localized(
            R.string.workspace_project_sessions_empty, null, "In diesem Projekt gibt es noch keine Sessions. Mit + startest du eine.",
        )
        else -> localized(
            R.string.workspace_project_none_visible, null, "In diesem Projekt sind für dieses Gerät keine Sessions sichtbar.",
        )
    }
}

@Composable
private fun ProjectSessionList(
    view: ProjectSessionsState,
    actions: ProjectSessionsActions,
    selected: String?,
    online: Boolean,
    showNew: Boolean,
    onSelect: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val canWrite = online && view.share?.tier?.let { it != MobileTier.OBSERVER } != false
    var menuFor by remember { mutableStateOf<String?>(null) }
    var renaming by remember { mutableStateOf<MobileSessionRow?>(null) }
    var assigning by remember { mutableStateOf<MobileSessionRow?>(null) }
    renaming?.let { row ->
        RenameDialog(
            initial = row.title.orEmpty(),
            onDismiss = { renaming = null },
            onConfirm = { actions.onRename(row.sessionId, it); renaming = null },
        )
    }
    assigning?.let { row ->
        AssignDialog(
            projects = view.projects.filter { it.projectKey != row.projectKey.orEmpty() },
            onDismiss = { assigning = null },
            onPick = { actions.onAssign(row.sessionId, it); assigning = null },
        )
    }
    Column(modifier.fillMaxHeight().padding(top = 9.dp)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 14.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    view.project?.label ?: stringResource(R.string.workspace_title),
                    style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                view.share?.let { share ->
                    Text(stringResource(share.tier.workspaceLabel()), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            IconButton(onClick = actions.onRefresh, enabled = online) { Icon(Icons.Outlined.Refresh, stringResource(R.string.workspace_refresh_sessions)) }
            if (showNew) FilledIconButton(onClick = actions.onNew, enabled = canWrite) { Icon(Icons.Outlined.Add, stringResource(R.string.directory_new_session), Modifier.size(18.dp)) }
        }
        val context = LocalContext.current
        val empty = projectEmptyText(view, canWrite) { resource, substitute, _ ->
            if (substitute == null) context.getString(resource) else context.getString(resource, substitute)
        }
        when {
            view.loading && view.rows.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(22.dp))
            }
            view.error != null && view.rows.isEmpty() -> Box(Modifier.fillMaxSize().padding(14.dp), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    StatusMessage(stringResource(R.string.workspace_session_load_error, view.error.orEmpty()), error = true)
                    TextButton(onClick = actions.onRefresh, enabled = online) { Text(stringResource(R.string.directory_retry)) }
                }
            }
            empty != null && !view.hasMore -> Box(Modifier.fillMaxSize().padding(14.dp), contentAlignment = Alignment.Center) { DirectoryEmpty(empty) }
            else -> LazyColumn(
                contentPadding = PaddingValues(start = 14.dp, end = 14.dp, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                view.error?.let { item { StatusMessage(stringResource(R.string.workspace_session_refresh_error, it), error = true) } }
                val locale = context.resources.configuration.locales[0] ?: java.util.Locale.ENGLISH
                val monthFormatter = java.time.format.DateTimeFormatter.ofPattern("LLLL yyyy", locale)
                val sections = projectSessionSections(view) { key ->
                    if (key == null) context.getString(R.string.workspace_month_unknown)
                    else try {
                        java.time.YearMonth.parse(key).format(monthFormatter)
                    } catch (_: java.time.DateTimeException) {
                        context.getString(R.string.workspace_month_unknown)
                    }
                }
                for (section in sections) {
                    section.heading?.let { heading ->
                        item(key = "month-$heading") {
                            Text(heading, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    items(section.rows, key = { it.sessionId }) { row ->
                        Box {
                            ProjectSessionRow(
                                row = row,
                                selected = row.sessionId == selected,
                                onClick = { onSelect(row.sessionId) },
                                onLongClick = { if (!view.legacy) menuFor = row.sessionId },
                            )
                            DropdownMenu(expanded = menuFor == row.sessionId, onDismissRequest = { menuFor = null }) {
                                DropdownMenuItem(text = { Text(stringResource(R.string.workspace_rename)) }, enabled = canWrite, onClick = { menuFor = null; renaming = row })
                                DropdownMenuItem(text = { Text(stringResource(R.string.workspace_assign_project)) }, enabled = canWrite, onClick = { menuFor = null; assigning = row })
                                DropdownMenuItem(text = { Text(stringResource(R.string.workspace_archive)) }, enabled = canWrite, onClick = { menuFor = null; actions.onArchive(row.sessionId) })
                            }
                        }
                    }
                }
                if (view.hasMore) {
                    item {
                        TextButton(onClick = actions.onLoadMore, enabled = online && !view.loading, modifier = Modifier.fillMaxWidth()) {
                            Text(stringResource(if (view.loading) R.string.workspace_loading else R.string.workspace_load_more))
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ProjectSessionRow(row: MobileSessionRow, selected: Boolean, onClick: () -> Unit, onLongClick: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Surface(
        shape = RoundedCornerShape(15.dp),
        border = BorderStroke(1.dp, if (selected) colors.primary else colors.outlineVariant),
        color = if (selected) colors.primaryContainer else colors.surfaceContainerLowest,
    ) {
        Column(
            Modifier.fillMaxWidth().combinedClickable(onClick = onClick, onLongClick = onLongClick).padding(13.dp, 10.dp),
            verticalArrangement = Arrangement.spacedBy(3.dp),
        ) {
            Text(row.displayTitle(), style = MaterialTheme.typography.titleSmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val detail = listOfNotNull(
                stringResource(homeStateLabel(row.state)),
                row.model?.takeUnless { it.isBlank() },
                row.turns?.let { stringResource(R.string.workspace_session_turns, it.toString()) },
            ).joinToString(" · ")
            Text(detail, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val marks = listOfNotNull(
                row.ownerMark()?.let { stringResource(it.workspaceLabel()) },
                if (!row.continuable) stringResource((row.reason ?: MobileReadOnlyReason.UNKNOWN).workspaceLabel()) else null,
            )
            if (marks.isNotEmpty()) {
                Text(marks.joinToString(" · "), style = MaterialTheme.typography.labelSmall, color = colors.primary, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

@Composable
private fun RenameDialog(initial: String, onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    var title by rememberSaveable { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.workspace_rename)) },
        text = { OutlinedTextField(value = title, onValueChange = { title = it }, label = { Text(stringResource(R.string.workspace_title_field)) }, modifier = Modifier.fillMaxWidth(), singleLine = true) },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.directory_cancel)) } },
        confirmButton = { Button(onClick = { onConfirm(title.trim()) }, enabled = title.isNotBlank()) { Text(stringResource(R.string.directory_save)) } },
    )
}

@Composable
private fun AssignDialog(projects: List<MobileProject>, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.workspace_assign_project)) },
        text = {
            LazyColumn {
                items(projects, key = { it.projectKey }) { project ->
                    val name = project.label.ifBlank { stringResource(R.string.directory_no_project) }
                    Text(
                        listOfNotNull(name, project.pathHint).joinToString("  "),
                        Modifier.fillMaxWidth().clickable { onPick(project.projectKey) }.padding(vertical = 12.dp),
                        style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.directory_cancel)) } },
    )
}
