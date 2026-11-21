package dev.harw.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
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
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.MobileSession
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
                if (split || sessionId == null) {
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
            val message = when {
                state.listError != null -> state.listError
                !state.online -> state.detail ?: "Connect to load sessions."
                !state.sessionsLoaded -> "Loading sessions…"
                state.canCreate -> "No sessions yet. Use New to start one."
                else -> "No sessions are visible to this device."
            }
            Box(Modifier.fillMaxSize().padding(inset), contentAlignment = Alignment.Center) { DirectoryEmpty(message) }
        } else {
            LazyColumn(
                contentPadding = PaddingValues(start = inset, end = inset, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                state.listError?.let { message -> item { StatusMessage(message, error = true) } }
                if (state.online && !state.canCreate) {
                    item { StatusMessage("This device has no permission to create sessions.") }
                }
                if (!state.online) {
                    item { StatusMessage(state.detail ?: "Showing the last loaded sessions.") }
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
