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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
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
) {
    val sessionId = window.sessionId
    val session = state.sessions.firstOrNull { it.sessionId == sessionId }
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

    BoxWithConstraints(Modifier.fillMaxSize()) {
        val split = maxWidth >= 650.dp
        val separateActivity = maxWidth >= 820.dp && sessionId != null
        val roomy = maxWidth >= 985.dp
        Column(Modifier.fillMaxSize()) {
            HarwHeader(
                title = session?.displayName() ?: sessionId?.let { "Session ${it.take(8)}" } ?: "Sessions",
                onBack = if (sessionId != null && !split) ({ window.select(null) }) else null,
            ) {
                ConnectionBadge(state)
                if (sessionId != null) {
                    IconButton(onClick = { onNewWindow(sessionId) }) {
                        Icon(Icons.Outlined.OpenInNew, "Open session in a new window")
                    }
                }
                IconButton(onClick = onOpenSettings) { Icon(Icons.Outlined.Settings, "Connection settings") }
            }
            Row(Modifier.fillMaxSize()) {
                if ((split || sessionId == null) && projectView != null && projectActions != null) {
                    ProjectSessionList(
                        view = projectView,
                        actions = projectActions,
                        selected = sessionId,
                        online = state.online,
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
                    conversationState.SaveableStateProvider(sessionId) {
                        SessionConversation(
                            state = state,
                            window = window,
                            sessionId = sessionId,
                            session = session,
                            connection = connection,
                            authorizingApproval = authorizingApproval,
                            onDecide = onDecide,
                            inlineActivity = !separateActivity,
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
                        DirectoryEmpty("Select a session to open its chat.")
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
    onNew: () -> Unit,
    onRefresh: () -> Unit,
    onSelect: (String) -> Unit,
    onNewWindow: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val inset = if (narrow) 12.dp else 18.dp
    Column(modifier.fillMaxHeight().padding(top = 9.dp)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = inset),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                if (narrow) "Sessions" else state.profile?.label ?: "Sessions",
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            IconButton(onClick = onRefresh, enabled = state.online) { Icon(Icons.Outlined.Refresh, "Refresh sessions") }
            val canCreate = state.online && state.canCreate && !state.creating
            val newIcon: @Composable () -> Unit = {
                if (state.creating) {
                    CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(16.dp))
                } else {
                    Icon(Icons.Outlined.Add, contentDescription = if (narrow) "New session" else null, Modifier.size(18.dp))
                }
            }
            if (narrow) {
                FilledIconButton(onClick = onNew, enabled = canCreate, content = newIcon)
            } else {
                Button(onClick = onNew, enabled = canCreate) {
                    newIcon()
                    Text("New", Modifier.padding(start = 4.dp))
                }
            }
        }
        if (state.sessions.isEmpty()) {
            Box(Modifier.fillMaxSize().padding(inset), contentAlignment = Alignment.Center) {
                when {
                    state.online && !state.sessionsLoaded && state.listError == null -> Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(22.dp))
                        Text(
                            "Loading sessions…",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                    state.listError != null -> Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        StatusMessage("Could not load sessions: ${state.listError}", error = true)
                        if (state.online) TextButton(onClick = onRefresh) { Text("Try again") }
                    }
                    !state.online -> DirectoryEmpty(offlineMessage(state, hasSessions = false))
                    state.canCreate -> DirectoryEmpty("No sessions yet. Use New to start one.")
                    else -> DirectoryEmpty("No sessions are visible to this device.")
                }
            }
        } else {
            LazyColumn(
                contentPadding = PaddingValues(start = inset, end = inset, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (!state.online) {
                    item { StatusMessage(offlineMessage(state, hasSessions = true), error = state.phase.isFailure()) }
                }
                state.listError?.let { message ->
                    item { StatusMessage("Could not refresh sessions: $message", error = true) }
                }
                if (state.online && !state.canCreate) {
                    item { StatusMessage("This device has no permission to create sessions.") }
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
                val detail = listOfNotNull(session.state.displayName(), session.model?.takeUnless { it.isBlank() })
                    .joinToString(" · ")
                Text(detail, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            if (onNewWindow != null) {
                IconButton(onClick = onNewWindow) {
                    Icon(Icons.Outlined.OpenInNew, "Open ${session.displayName()} in a new window", Modifier.size(19.dp))
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
        title = { Text("New session") },
        text = {
            OutlinedTextField(
                value = title, onValueChange = { title = it },
                label = { Text("Title (optional)") },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
            )
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        confirmButton = {
            Button(onClick = { onCreate(title.trim().ifBlank { null }) }, enabled = enabled) { Text("Create") }
        },
    )
}

private fun ConnectionPhase.isFailure(): Boolean =
    this == ConnectionPhase.FAILED || this == ConnectionPhase.REVOKED

private fun offlineMessage(state: WorkspaceState, hasSessions: Boolean): String {
    val base = when {
        state.stopping -> "Disconnecting…"
        else -> when (state.phase) {
            ConnectionPhase.CONNECTING -> "Connecting to the server…"
            ConnectionPhase.RECONNECTING -> "Connection lost. Reconnecting…"
            ConnectionPhase.FAILED -> "Connection failed."
            ConnectionPhase.REVOKED -> "Access to this server was stopped."
            ConnectionPhase.DISCONNECTED, ConnectionPhase.CONNECTED -> "Offline."
        }
    }
    val detail = state.detail?.takeUnless { it.isBlank() }
    val tail = if (hasSessions) "Showing the last loaded sessions." else "Connect to load sessions."
    return listOfNotNull(base, detail, tail).joinToString(" ")
}

internal fun MobileSession.displayName(): String = title?.takeUnless { it.isBlank() } ?: "Session ${sessionId.take(8)}"

internal fun MobileSessionState.displayName(): String = when (this) {
    MobileSessionState.IDLE -> "Ready"
    MobileSessionState.RUNNING -> "Running"
    MobileSessionState.WAITING_FOR_APPROVAL -> "Waiting for approval"
    MobileSessionState.WAITING_FOR_CHILD -> "Waiting for child"
    MobileSessionState.QUEUED -> "Queued"
    MobileSessionState.INTERRUPTED -> "Interrupted"
    MobileSessionState.FAILED -> "Failed"
    MobileSessionState.CLOSED -> "Closed"
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

internal fun projectSessionSections(view: ProjectSessionsState): List<ProjectSessionSection> = when {
    view.rows.isEmpty() -> emptyList()
    view.projectKey == NoProjectKey ->
        groupByMonth(view.rows, { it.updatedAt }).map { (key, rows) -> ProjectSessionSection(monthHeading(key), rows) }
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

internal fun MobileSessionRow.displayTitle(): String =
    title?.takeUnless { it.isBlank() } ?: firstMessage?.takeUnless { it.isBlank() }?.take(60) ?: "Session ${sessionId.take(8)}"

/** Leer-Text der Projektansicht; null, wenn Zeilen vorhanden sind. */
internal fun projectEmptyText(view: ProjectSessionsState, canCreate: Boolean): String? {
    if (view.rows.isNotEmpty()) return null
    return when {
        view.query.isNotBlank() -> "Keine Treffer für \u201E${view.query.trim()}\u201C."
        view.share?.mode == MobileShareMode.OFF && !view.legacy -> "Dieses Gateway teilt keine TUI-Sessions."
        view.legacy -> "Dieser Server kennt keine Projekte. Es werden nur Handy-Sessions angezeigt."
        canCreate -> "In diesem Projekt gibt es noch keine Sessions. Mit + startest du eine."
        else -> "In diesem Projekt sind für dieses Gerät keine Sessions sichtbar."
    }
}

@Composable
private fun ProjectSessionList(
    view: ProjectSessionsState,
    actions: ProjectSessionsActions,
    selected: String?,
    online: Boolean,
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
                    view.project?.label ?: "Sessions",
                    style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                view.share?.let { share ->
                    Text(share.tier.label(), style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            IconButton(onClick = actions.onRefresh, enabled = online) { Icon(Icons.Outlined.Refresh, "Aktualisieren") }
            FilledIconButton(onClick = actions.onNew, enabled = canWrite) { Icon(Icons.Outlined.Add, "Neue Session", Modifier.size(18.dp)) }
        }
        val empty = projectEmptyText(view, canWrite)
        when {
            view.loading && view.rows.isEmpty() -> Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(22.dp))
            }
            view.error != null && view.rows.isEmpty() -> Box(Modifier.fillMaxSize().padding(14.dp), contentAlignment = Alignment.Center) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    StatusMessage("Sessions konnten nicht geladen werden: ${view.error}", error = true)
                    TextButton(onClick = actions.onRefresh, enabled = online) { Text("Erneut versuchen") }
                }
            }
            empty != null -> Box(Modifier.fillMaxSize().padding(14.dp), contentAlignment = Alignment.Center) { DirectoryEmpty(empty) }
            else -> LazyColumn(
                contentPadding = PaddingValues(start = 14.dp, end = 14.dp, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                view.error?.let { item { StatusMessage("Aktualisieren fehlgeschlagen: $it", error = true) } }
                for (section in projectSessionSections(view)) {
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
                                DropdownMenuItem(text = { Text("Umbenennen") }, enabled = canWrite, onClick = { menuFor = null; renaming = row })
                                DropdownMenuItem(text = { Text("Projekt zuweisen") }, enabled = canWrite, onClick = { menuFor = null; assigning = row })
                                DropdownMenuItem(text = { Text("Archivieren") }, enabled = canWrite, onClick = { menuFor = null; actions.onArchive(row.sessionId) })
                            }
                        }
                    }
                }
                if (view.hasMore) {
                    item {
                        TextButton(onClick = actions.onLoadMore, enabled = online && !view.loading, modifier = Modifier.fillMaxWidth()) {
                            Text(if (view.loading) "Lädt …" else "Weitere laden")
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
                row.state.displayName(),
                row.model?.takeUnless { it.isBlank() },
                row.turns?.let { "$it Turns" },
            ).joinToString(" · ")
            Text(detail, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            val marks = listOfNotNull(
                row.ownerMark()?.label(),
                if (!row.continuable) (row.reason ?: MobileReadOnlyReason.UNKNOWN).label() else null,
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
        title = { Text("Umbenennen") },
        text = { OutlinedTextField(value = title, onValueChange = { title = it }, label = { Text("Titel") }, modifier = Modifier.fillMaxWidth(), singleLine = true) },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Abbrechen") } },
        confirmButton = { Button(onClick = { onConfirm(title.trim()) }, enabled = title.isNotBlank()) { Text("Speichern") } },
    )
}

@Composable
private fun AssignDialog(projects: List<MobileProject>, onDismiss: () -> Unit, onPick: (String) -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Projekt zuweisen") },
        text = {
            LazyColumn {
                items(projects, key = { it.projectKey }) { project ->
                    val name = project.label.ifBlank { "Ohne Projekt" }
                    Text(
                        listOfNotNull(name, project.pathHint).joinToString("  "),
                        Modifier.fillMaxWidth().clickable { onPick(project.projectKey) }.padding(vertical = 12.dp),
                        style = MaterialTheme.typography.bodyLarge, maxLines = 2, overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = { TextButton(onClick = onDismiss) { Text("Abbrechen") } },
    )
}
