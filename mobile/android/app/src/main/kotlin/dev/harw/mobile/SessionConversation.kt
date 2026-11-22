package dev.harw.mobile

import android.content.res.Configuration
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.Construction
import androidx.compose.material.icons.outlined.Security
import androidx.compose.material.icons.outlined.StopCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.ApprovalRisk
import uniffi.harw_mobile_ffi.MobileApproval
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileToolState
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

@Composable
internal fun SessionConversation(
    state: WorkspaceState,
    window: HarwWindowModel,
    sessionId: String,
    session: MobileSession?,
    connection: HarwConnection,
    authorizingApproval: Boolean,
    onDecide: (String, Long, String, ApprovalDecision) -> Unit,
    inlineActivity: Boolean,
    modifier: Modifier = Modifier,
) {
    val view = state.windows[sessionId]
    val snapshot = view?.snapshot
    val generation = view?.generation?.takeIf { state.online && it == state.generation }
    val active = generation != null && snapshot?.online == true
    val approvals = snapshot?.approvals.orEmpty()
    var approvalId by rememberSaveable(sessionId) { mutableStateOf<String?>(null) }
    var confirmingInterrupt by rememberSaveable(sessionId) { mutableStateOf(false) }
    var transcriptJump by remember(sessionId) { mutableIntStateOf(0) }
    val approval = approvals.firstOrNull { it.id == approvalId }
    val turnId = snapshot?.runningTurnId
    val approvalPending = SessionAction(sessionId, SessionActionKind.Decide) in state.pendingActions
    val interruptPending = SessionAction(sessionId, SessionActionKind.Interrupt) in state.pendingActions
    val sendPending = SessionAction(sessionId, SessionActionKind.Send) in state.pendingActions

    if (approval != null) {
        ApprovalScreen(
            approval = approval,
            allowed = active && snapshot?.canApprove == true,
            busy = authorizingApproval || approvalPending,
            failure = view?.error,
            onClose = { approvalId = null },
            onDecide = { decision ->
                if (generation != null) onDecide(sessionId, generation, approval.id, decision)
            },
        )
    }
    LaunchedEffect(approvalId, approval, snapshot) {
        if (approval == null && snapshot != null) approvalId = null
    }
    if (confirmingInterrupt) {
        AlertDialog(
            onDismissRequest = { confirmingInterrupt = false },
            title = { Text("Interrupt this session?") },
            text = { Text("The server stops whichever execution is active in this session when it receives the request.") },
            dismissButton = { TextButton(onClick = { confirmingInterrupt = false }) { Text("Cancel") } },
            confirmButton = {
                Button(
                    enabled = active && snapshot?.canInterrupt == true && turnId != null && !interruptPending,
                    onClick = {
                        confirmingInterrupt = false
                        if (generation != null && turnId != null) connection.interrupt(sessionId, generation, turnId)
                    },
                ) { Text("Interrupt session") }
            },
        )
    }

    BoxWithConstraints(modifier.imePadding()) {
        val availableHeight = maxHeight
        Column(Modifier.fillMaxSize()) {
            if (inlineActivity || turnId != null) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 5.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    if (inlineActivity) {
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                            Text(
                                sessionStatus(state, session, view),
                                style = MaterialTheme.typography.labelLarge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            val tool = snapshot?.transcript?.lastOrNull { it.kind == TranscriptKind.TOOL }
                            if (tool != null) {
                                Text(
                                    toolSummary(tool), style = MaterialTheme.typography.bodySmall,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                    if (turnId != null) {
                        TextButton(
                            onClick = { confirmingInterrupt = true },
                            enabled = active && snapshot?.canInterrupt == true && !interruptPending,
                        ) {
                            Icon(Icons.Outlined.StopCircle, contentDescription = null, Modifier.size(18.dp))
                            Text("Interrupt session", Modifier.padding(start = 5.dp))
                        }
                    }
                }
            }
            if (approvals.isNotEmpty()) {
                ApprovalStrip(
                    approvals.size,
                    canDecide = active && snapshot?.canApprove == true,
                    onClick = { approvalId = approvals.first().id },
                    modifier = Modifier.padding(horizontal = 15.dp, vertical = 5.dp),
                )
            }
            view?.error?.let { StatusMessage(it, Modifier.padding(horizontal = 15.dp, vertical = 4.dp), error = true) }
            (snapshot?.lastFailure ?: state.detail?.takeUnless { state.online } ?: snapshot?.detail?.takeUnless { it == view?.error })?.let {
                StatusMessage(it, Modifier.padding(horizontal = 15.dp, vertical = 4.dp), error = snapshot?.lastFailure != null)
            }
        if (LocalConfiguration.current.orientation == Configuration.ORIENTATION_PORTRAIT && snapshot != null) {
            MissionHeader(
                agents = snapshot.agents,
                jobs = snapshot.jobs,
                reasoning = snapshot.reasoning,
                finished = snapshot.finished,
                availableHeight = availableHeight,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 4.dp),
            )
        }
            ConversationFeed(
                entries = snapshot?.transcript.orEmpty(),
                liveText = snapshot?.liveText?.takeUnless { it.isEmpty() },
                jumpToLatest = transcriptJump,
                emptyMessage = when {
                    snapshot == null && state.online -> "Opening session…"
                    !active -> "Waiting for a live session."
                    else -> "No messages yet."
                },
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
            val head = snapshot?.head
            val composerEnabled = active && head != null && snapshot?.canSubmit == true && !sendPending
            PromptComposer(
                draft = window.draft(sessionId),
                enabled = composerEnabled,
                busy = sendPending,
                onEdit = { window.edit(sessionId, it) },
                onReview = { transcriptJump++ },
                onDiscard = { window.discardUncertainDraft(sessionId) },
                onSend = {
                    if (generation != null && head != null) window.submit(connection, sessionId, generation, head)
                },
                hint = when {
                    !active -> "Read-only until the session reconnects"
                    snapshot?.canSubmit != true -> "The server is not accepting prompts in this session"
                    else -> "Message Harw"
                },
            )
        }
    }
}

@Composable
private fun ConversationFeed(
    entries: List<TranscriptEntry>,
    liveText: String?,
    jumpToLatest: Int,
    emptyMessage: String,
    modifier: Modifier,
) {
    if (entries.isEmpty() && liveText == null) {
        Box(modifier, contentAlignment = Alignment.Center) { DirectoryEmpty(emptyMessage) }
        return
    }
    // A reversed list keeps the latest line at the bottom during bursty token updates.
    val reversed = remember(entries) { entries.asReversed() }
    val listState = rememberLazyListState()
    var followLatest by rememberSaveable { mutableStateOf(true) }
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress to listState.canScrollBackward }.collect { (scrolling, awayFromLatest) ->
            if (scrolling || !awayFromLatest) followLatest = !awayFromLatest
        }
    }
    LaunchedEffect(liveText != null, entries.lastOrNull()?.id) {
        if (followLatest) listState.requestScrollToItem(0)
    }
    LaunchedEffect(jumpToLatest) {
        if (jumpToLatest > 0) {
            followLatest = true
            listState.requestScrollToItem(0)
        }
    }
    LazyColumn(
        modifier, state = listState, reverseLayout = true,
        contentPadding = PaddingValues(horizontal = 15.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        if (liveText != null) {
            item(key = "live-reply") { MessageBlock("Harw · Streaming", liveText, TranscriptKind.ASSISTANT) }
        }
        items(reversed, key = { "entry-${it.id}" }) { entry ->
            val label = when (entry.kind) {
                TranscriptKind.USER -> "You"
                TranscriptKind.ASSISTANT -> "Harw"
                TranscriptKind.TOOL -> entry.toolState?.let { "Tool · ${it.displayName()}" } ?: "Tool"
                TranscriptKind.ERROR -> "Error"
            }
            MessageBlock(label, entry.text, entry.kind)
        }
    }
}

@Composable
private fun MessageBlock(label: String, text: String, kind: TranscriptKind) {
    val colors = MaterialTheme.colorScheme
    val user = kind == TranscriptKind.USER
    val (background, foreground) = when (kind) {
        TranscriptKind.USER -> colors.primaryContainer to colors.onPrimaryContainer
        TranscriptKind.ASSISTANT -> colors.surfaceContainerLowest to colors.onSurface
        TranscriptKind.TOOL -> colors.surfaceContainer to colors.onSurfaceVariant
        TranscriptKind.ERROR -> colors.errorContainer to colors.onErrorContainer
    }
    Box(Modifier.fillMaxWidth(), contentAlignment = if (user) Alignment.CenterEnd else Alignment.CenterStart) {
        Surface(
            Modifier.fillMaxWidth(if (user) 0.88f else 1f),
            shape = RoundedCornerShape(15.dp),
            color = background,
            contentColor = foreground,
            border = if (user) null else BorderStroke(1.dp, colors.outlineVariant),
        ) {
            Column(Modifier.padding(horizontal = 13.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(label, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
                SelectionContainer {
                    Text(
                        text,
                        style = MaterialTheme.typography.bodyMedium,
                        fontFamily = if (kind == TranscriptKind.TOOL) FontFamily.Monospace else null,
                    )
                }
            }
        }
    }
}

@Composable
private fun PromptComposer(
    draft: SessionDraft,
    enabled: Boolean,
    busy: Boolean,
    hint: String,
    onEdit: (String) -> Unit,
    onReview: () -> Unit,
    onSend: () -> Unit,
    onDiscard: () -> Unit,
) {
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        draft.error?.let { StatusMessage(it, error = true) }
        if (draft.uncertain) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                OutlinedButton(onClick = onReview) { Text("Review latest transcript") }
                TextButton(enabled = !draft.sending, onClick = onDiscard) { Text("Discard draft") }
            }
        }
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = draft.text,
                onValueChange = onEdit,
                modifier = Modifier.weight(1f),
                readOnly = !enabled || draft.uncertain,
                placeholder = { Text(hint, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                shape = RoundedCornerShape(17.dp),
                maxLines = 4,
            )
            FilledIconButton(onClick = onSend, enabled = enabled && draft.text.isNotBlank() && !draft.uncertain) {
                if (busy) {
                    CircularProgressIndicator(Modifier.size(19.dp), strokeWidth = 2.dp)
                } else {
                    Icon(Icons.AutoMirrored.Outlined.Send, "Send message")
                }
            }
        }
    }
}

@Composable
private fun ApprovalStrip(count: Int, canDecide: Boolean, onClick: () -> Unit, modifier: Modifier) {
    val colors = MaterialTheme.colorScheme
    Surface(
        onClick = onClick,
        modifier = modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
        color = colors.primaryContainer,
        contentColor = colors.onPrimaryContainer,
    ) {
        Row(
            Modifier.padding(horizontal = 12.dp, vertical = 9.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.Security, contentDescription = null, Modifier.size(18.dp))
            Text(
                if (count == 1) "Approval requested" else "$count approvals requested",
                modifier = Modifier.weight(1f), style = MaterialTheme.typography.labelLarge,
            )
            Text(if (canDecide) "Review" else "View", style = MaterialTheme.typography.labelMedium)
        }
    }
}

@Composable
private fun ApprovalScreen(
    approval: MobileApproval,
    allowed: Boolean,
    busy: Boolean,
    failure: String?,
    onClose: () -> Unit,
    onDecide: (ApprovalDecision) -> Unit,
) {
    var selection by rememberSaveable(approval.id) { mutableStateOf<String?>(null) }
    val choice = approval.decisions.firstOrNull { it.name == selection }
    Dialog(
        onDismissRequest = onClose,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
                HarwHeader("Approval", onBack = onClose) {}
                LazyColumn(
                    Modifier.weight(1f).fillMaxWidth(),
                    contentPadding = PaddingValues(horizontal = 20.dp, vertical = 17.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    item {
                        Text("Review the request", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
                    }
                    item {
                        Text("${approval.risk.displayName()} risk", color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.titleSmall)
                    }
                    item {
                        SelectionContainer { Text(approval.summary, style = MaterialTheme.typography.bodyLarge) }
                    }
                    if (approval.timeoutAt.isNotBlank()) {
                        item { Text("Expires ${approval.timeoutAt}", style = MaterialTheme.typography.bodySmall) }
                    }
                    if (!allowed) item { StatusMessage("This device cannot decide this approval in the current connection.") }
                    failure?.let { failureText -> item { StatusMessage(failureText, error = true) } }
                    item {
                        Column(Modifier.selectableGroup(), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                            approval.decisions.forEach { decision ->
                                val selected = decision == choice
                                Surface(
                                    Modifier.fillMaxWidth(),
                                    shape = RoundedCornerShape(13.dp),
                                    color = if (selected) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceContainer,
                                ) {
                                    Row(
                                        Modifier.selectable(selected, enabled = allowed && !busy, role = Role.RadioButton) {
                                            selection = decision.name
                                        }.padding(horizontal = 11.dp, vertical = 8.dp),
                                        horizontalArrangement = Arrangement.spacedBy(13.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        RadioButton(selected, onClick = null, enabled = allowed && !busy)
                                        Text(decision.displayName(), modifier = Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                                    }
                                }
                            }
                        }
                    }
                }
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 9.dp),
                    horizontalArrangement = Arrangement.End,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    TextButton(onClick = onClose) { Text("Close") }
                    Button(
                        enabled = allowed && !busy && choice != null,
                        onClick = { if (choice != null) onDecide(choice) },
                    ) { Text(if (busy) "Confirming…" else "Confirm decision") }
                }
            }
        }
    }
}

@Composable
internal fun SessionActivityPanel(state: WorkspaceState, session: MobileSession?, view: WindowState?, modifier: Modifier) {
    val snapshot = view?.snapshot
    val tools = snapshot?.transcript.orEmpty().filter { it.kind == TranscriptKind.TOOL }.takeLast(8).asReversed()
    LazyColumn(
        modifier, contentPadding = PaddingValues(13.dp),
        verticalArrangement = Arrangement.spacedBy(9.dp),
    ) {
        item { Text("Activity", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold) }
        item { StatusMessage(sessionStatus(state, session, view)) }
        snapshot?.lastSyncMillis?.let { millis ->
            item {
                val time = Instant.ofEpochMilli(millis).atZone(ZoneId.systemDefault()).format(ShortTime)
                Text("Last synced $time", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
        items(tools, key = { it.id }) { tool ->
            Row(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                Icon(Icons.Outlined.Construction, contentDescription = null, Modifier.size(16.dp), tint = MaterialTheme.colorScheme.primary)
                Text(toolSummary(tool), style = MaterialTheme.typography.bodySmall, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}

private fun sessionStatus(state: WorkspaceState, session: MobileSession?, view: WindowState?): String = when {
    !state.online -> "Offline · Read-only"
    view?.snapshot?.online != true -> "Attaching…"
    view.snapshot.approvals.isNotEmpty() -> "Waiting for approval"
    view.snapshot.runningTurnId != null -> "Turn running"
    else -> session?.state?.displayName() ?: "Ready"
}

private fun toolSummary(entry: TranscriptEntry): String = listOfNotNull(entry.toolState?.displayName(), entry.text.takeIf { it.isNotBlank() })
    .joinToString(" · ")

private fun MobileToolState.displayName() = when (this) {
    MobileToolState.RUNNING -> "Running"
    MobileToolState.DONE -> "Finished"
    MobileToolState.FAILED -> "Failed"
}

private fun ApprovalRisk.displayName() = when (this) {
    ApprovalRisk.LOW -> "Low"
    ApprovalRisk.MEDIUM -> "Medium"
    ApprovalRisk.HIGH -> "High"
    ApprovalRisk.CRITICAL -> "Critical"
}

internal fun ApprovalDecision.displayName() = when (this) {
    ApprovalDecision.APPROVED_ONCE -> "Allow once"
    ApprovalDecision.APPROVED -> "Approve"
    ApprovalDecision.REJECTED -> "Deny"
}

private val ShortTime = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
