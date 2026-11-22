package dev.harw.mobile

import android.content.res.Configuration
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material.icons.outlined.AddPhotoAlternate
import androidx.compose.material3.InputChip
import androidx.compose.runtime.produceState
import androidx.compose.ui.platform.LocalContext
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
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.IconButton
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.rememberModalBottomSheetState
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
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileToolState
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

internal fun modelChipLabel(sessionModel: String?, liveModel: String?): String =
    liveModel ?: sessionModel ?: "Modell"

internal fun modelChipBlock(support: ManageSupport?, active: Boolean): String? = when {
    !active -> "Session nicht live"
    support == null || !support.manage -> NO_MANAGE_TEXT
    !support.control -> "Modellwechsel braucht Profil Voll"
    else -> null
}

/** Genau das erste `/` in einem leeren Entwurf oeffnet die Befehlsliste (nicht bei weiterem Text). */
internal fun slashOpensCommands(draft: String, manageSupported: Boolean): Boolean =
    manageSupported && draft == "/" && slashQuery(draft) != null

/** Waehlbare Modelle fuer den Wechsel: ausgemusterte (`retired`) fehlen. */
internal fun <T> switchableModels(models: List<T>, lifecycle: (T) -> String): List<T> =
    models.filter { lifecycle(it) != "retired" }

@OptIn(ExperimentalMaterial3Api::class)
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
    val holder = view?.holder
    val access = conversationAccess(
        active = active,
        canSubmit = snapshot?.canSubmit == true,
        holder = holder,
        turnRunning = turnId != null,
    )
    val manageSupport = state.manage.support
    val manageOn = manageSupport?.manage == true
    var commandsOpen by rememberSaveable(sessionId) { mutableStateOf(false) }
    var modelsOpen by rememberSaveable(sessionId) { mutableStateOf(false) }
    val draftText = window.draft(sessionId).text
    LaunchedEffect(sessionId, state.online, state.generation) {
        connection.refreshManageSupport()
        connection.refreshCatalog(sessionId)
    }
    LaunchedEffect(draftText, manageOn) {
        if (slashOpensCommands(draftText, manageOn)) commandsOpen = true
    }
    // Nach erfolgreichem Ausfuehren im Blatt wird ein mit `/` begonnener Entwurf geleert.
    var seenCard by remember(sessionId) { mutableStateOf(state.manage.card) }
    LaunchedEffect(state.manage.card) {
        val card = state.manage.card
        if (card != null && card !== seenCard) {
            val effect = card.result.effect
            if (commandsOpen && effect != ManageEffect.REFUSED && effect != ManageEffect.STEPUP_REQUIRED &&
                window.draft(sessionId).text.startsWith("/")
            ) {
                window.edit(sessionId, "")
            }
        }
        seenCard = card
    }
    if (commandsOpen && manageOn) {
        ModalBottomSheet(
            onDismissRequest = { commandsOpen = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            CommandsScreen(
                state = state,
                connection = connection,
                sessionId = sessionId,
                initialQuery = slashQuery(draftText) ?: "",
                onBack = { commandsOpen = false },
                onInsert = { line -> window.edit(sessionId, line) },
            )
        }
    }
    if (modelsOpen) {
        ModalBottomSheet(
            onDismissRequest = { modelsOpen = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            ModelSwitchSheet(state, connection, sessionId, active)
        }
    }

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
            title = { Text("Diese Session unterbrechen?") },
            text = { Text("Der Server stoppt die gerade aktive Ausführung dieser Session, sobald er die Anfrage erhält.") },
            dismissButton = { TextButton(onClick = { confirmingInterrupt = false }) { Text("Abbrechen") } },
            confirmButton = {
                Button(
                    enabled = active && snapshot?.canInterrupt == true && turnId != null && !interruptPending,
                    onClick = {
                        confirmingInterrupt = false
                        if (generation != null && turnId != null) connection.interrupt(sessionId, generation, turnId)
                    },
                ) { Text("Session unterbrechen") }
            },
        )
    }

    BoxWithConstraints(modifier.imePadding()) {
        val availableHeight = maxHeight
        Column(Modifier.fillMaxSize()) {
            when (sessionFreshness(state.online, state.generation, view?.generation)) {
                SessionFreshness.Live -> Unit
                SessionFreshness.Stale -> StatusMessage(
                    "Älterer Stand wird angezeigt – warte auf den Abgleich mit dem Server",
                    Modifier.padding(horizontal = 15.dp, vertical = 4.dp),
                )
                SessionFreshness.Offline -> StatusMessage(
                    listOfNotNull(
                        state.phase.name.lowercase().replace('_', ' ').replaceFirstChar { it.uppercase() },
                        state.detail?.takeIf { it.isNotBlank() },
                    ).joinToString(" · "),
                    Modifier.padding(horizontal = 15.dp, vertical = 4.dp),
                    error = true,
                )
            }
            if (manageSupport != null) {
                val block = modelChipBlock(manageSupport, active)
                Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 2.dp)) {
                    AssistChip(
                        onClick = { modelsOpen = true },
                        enabled = block == null,
                        label = {
                            Text(
                                modelChipLabel(session?.model, state.manage.catalog?.liveModel),
                                maxLines = 1, overflow = TextOverflow.Ellipsis,
                            )
                        },
                    )
                    block?.let {
                        Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                }
            }
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
            HolderBanner(access, holder, Modifier.padding(horizontal = 15.dp, vertical = 4.dp))
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
                    snapshot == null && state.online -> "Session wird geöffnet …"
                    !active -> "Warte auf eine aktive Session."
                    else -> "Noch keine Nachrichten."
                },
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
            val head = snapshot?.head
            val composerEnabled = access == ConversationAccess.Writable && head != null && !sendPending
            val queuedFirst = window.firstMessage(sessionId)
            LaunchedEffect(sessionId, queuedFirst, composerEnabled, generation) {
                if (queuedFirst != null && composerEnabled && generation != null && head != null) {
                    window.takeFirstMessage(sessionId)
                    window.edit(sessionId, queuedFirst)
                    window.submit(connection, sessionId, generation, head)
                }
            }
            val imagesSupported by produceState(false, state.online, state.generation) {
                value = state.online && connection.supportsImages()
            }
            PromptComposer(
                draft = window.draft(sessionId),
                onAttachImage = if (imagesSupported) ({ image -> window.attachImage(sessionId, image) }) else null,
                onDetachImage = { index -> window.detachImage(sessionId, index) },
                onImageError = { message -> window.failImage(sessionId, message) },
                enabled = composerEnabled,
                busy = sendPending,
                onEdit = { window.edit(sessionId, it) },
                onReview = { transcriptJump++ },
                onDiscard = { window.discardUncertainDraft(sessionId) },
                onSend = {
                    if (generation != null && head != null) window.submit(connection, sessionId, generation, head)
                },
                hint = composerHint(access),
                onCommands = if (manageOn) ({ commandsOpen = true }) else null,
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
                TranscriptKind.USER -> "Du"
                TranscriptKind.ASSISTANT -> "Harw"
                TranscriptKind.TOOL -> entry.toolState?.let { "Werkzeug · ${it.displayName()}" } ?: "Werkzeug"
                TranscriptKind.ERROR -> "Fehler"
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
                    if (rendersMarkdown(kind)) {
                        // MarkdownText (A8) behandelt Code-Blöcke, Listen, Links und Kopieren selbst.
                        MarkdownText(text = text, modifier = Modifier.fillMaxWidth())
                    } else {
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
}

/** Was der Nutzer in dieser Unterhaltung gerade darf. */
internal enum class ConversationAccess {
    /** Senden möglich. */
    Writable,
    /** Verbindung fehlt oder ist veraltet. */
    Offline,
    /** Ein anderer Prozess (Terminal, Job, unbekannt) hält die Session. */
    HeldElsewhere,
    /** Ein anderes Gerät lässt gerade einen Turn über das Gateway laufen. */
    RunningOnGateway,
    /** Der Server nimmt keine Eingaben an (Freigabe nur lesend, fehlende Rechte). */
    ReadOnly,
}

internal fun conversationAccess(
    active: Boolean,
    canSubmit: Boolean,
    holder: MobileHolder?,
    turnRunning: Boolean,
): ConversationAccess = when {
    !active -> ConversationAccess.Offline
    holder != null && holder.kind != MobileHolderKind.GATEWAY -> ConversationAccess.HeldElsewhere
    !canSubmit -> ConversationAccess.ReadOnly
    // Eigener Turn läuft: Halter ist das Gateway, Senden bleibt dem Host überlassen (BUSY).
    holder != null && !turnRunning -> ConversationAccess.RunningOnGateway
    else -> ConversationAccess.Writable
}

internal fun composerHint(access: ConversationAccess): String = when (access) {
    ConversationAccess.Writable -> "Nachricht an Harw"
    ConversationAccess.Offline -> "Nur lesen, bis die Verbindung wieder steht"
    ConversationAccess.HeldElsewhere -> "Nur lesen, die Session ist belegt"
    ConversationAccess.RunningOnGateway -> "Nur lesen, ein Turn läuft auf dem Gateway"
    ConversationAccess.ReadOnly -> "Nur lesen, der Server nimmt hier keine Eingaben an"
}

/** Abzeichen-Text für den aktuellen Halter; `null` = frei. */
internal fun holderBadge(holder: MobileHolder?): String? = holder?.let {
    when (it.kind) {
        MobileHolderKind.GATEWAY -> "Läuft auf Gateway"
        MobileHolderKind.TUI -> "Belegt im Terminal"
        MobileHolderKind.JOB -> "Belegt durch Job"
        MobileHolderKind.UNKNOWN -> "Belegt"
    }
}

internal fun holderBannerText(access: ConversationAccess, holder: MobileHolder?): String? = when (access) {
    ConversationAccess.HeldElsewhere -> when (holder?.kind) {
        MobileHolderKind.TUI -> "Die Session ist im Terminal geöffnet. Senden ist pausiert, bis sie dort frei wird."
        MobileHolderKind.JOB -> "Ein Job schreibt gerade in diese Session. Senden ist pausiert."
        else -> "Die Session ist gerade belegt. Senden ist pausiert."
    }
    ConversationAccess.RunningOnGateway -> "Auf dem Gateway läuft gerade ein Turn. Du kannst mitlesen."
    ConversationAccess.ReadOnly -> "Nur lesen: Der Server nimmt in dieser Session keine Eingaben an."
    else -> null
}

/** Nur Antworten des Modells werden als Markdown gesetzt; Nutzer-, Tool- und Fehlertexte bleiben wörtlich. */
internal fun rendersMarkdown(kind: TranscriptKind): Boolean = kind == TranscriptKind.ASSISTANT

@Composable
private fun HolderBanner(access: ConversationAccess, holder: MobileHolder?, modifier: Modifier) {
    val text = holderBannerText(access, holder) ?: return
    val badge = holderBadge(holder)
    Column(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        if (badge != null) {
            Surface(
                shape = RoundedCornerShape(50),
                color = MaterialTheme.colorScheme.secondaryContainer,
                contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            ) {
                Text(badge, Modifier.padding(horizontal = 10.dp, vertical = 3.dp), style = MaterialTheme.typography.labelMedium)
            }
        }
        StatusMessage(text)
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
    onCommands: (() -> Unit)? = null,
    onAttachImage: ((DraftImage) -> Unit)? = null,
    onDetachImage: (Int) -> Unit = {},
    onImageError: (String) -> Unit = {},
) {
    val context = LocalContext.current
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri: Uri? ->
        if (uri != null && onAttachImage != null) {
            val label = "Bild ${draft.images.size + 1}"
            // Dekodieren und Verkleinern gehoert nicht auf den Hauptthread.
            Thread {
                val image = prepareImage(context, uri, label)
                android.os.Handler(android.os.Looper.getMainLooper()).post {
                    if (image != null) onAttachImage(image) else onImageError("Das Bild konnte nicht gelesen oder nicht klein genug gemacht werden.")
                }
            }.start()
        }
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
    Column(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        draft.error?.let { StatusMessage(it, error = true) }
        if (draft.uncertain) {
            StatusMessage("Zustellung unbekannt – prüfe den Verlauf, bevor du erneut sendest", error = true)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                OutlinedButton(onClick = onReview) { Text("Neuesten Verlauf ansehen") }
                TextButton(enabled = !draft.sending, onClick = onDiscard) { Text("Entwurf verwerfen") }
            }
        }
        if (draft.images.isNotEmpty()) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(7.dp)) {
                draft.images.forEachIndexed { index, image ->
                    InputChip(
                        selected = false,
                        enabled = enabled && !draft.uncertain,
                        onClick = { onDetachImage(index) },
                        label = { Text("${image.label} (${image.bytes / 1024} KB) entfernen") },
                    )
                }
            }
        }
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (onCommands != null) {
                IconButton(onClick = onCommands) { Icon(Icons.Outlined.Terminal, "Befehle") }
            }
            if (onAttachImage != null) {
                IconButton(
                    enabled = enabled && !draft.uncertain && draft.images.size < MaxPromptImages,
                    onClick = { picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
                ) { Icon(Icons.Outlined.AddPhotoAlternate, "Bild anhängen") }
            }
            OutlinedTextField(
                value = draft.text,
                onValueChange = onEdit,
                modifier = Modifier.weight(1f),
                readOnly = !enabled || draft.uncertain,
                placeholder = { Text(hint, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                shape = RoundedCornerShape(17.dp),
                maxLines = 4,
            )
            FilledIconButton(onClick = onSend, enabled = enabled && (draft.text.isNotBlank() || draft.images.isNotEmpty()) && !draft.uncertain) {
                if (busy) {
                    CircularProgressIndicator(Modifier.size(19.dp), strokeWidth = 2.dp)
                } else {
                    Icon(Icons.AutoMirrored.Outlined.Send, "Nachricht senden")
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
                if (count == 1) "Freigabe angefragt" else "$count Freigaben angefragt",
                modifier = Modifier.weight(1f), style = MaterialTheme.typography.labelLarge,
            )
            Text(if (canDecide) "Prüfen" else "Ansehen", style = MaterialTheme.typography.labelMedium)
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
                HarwHeader("Freigabe", onBack = onClose) {}
                val now by rememberTicker(true)
                val remainingMs = approvalRemainingMs(approval.timeoutAt, now)
                val expired = remainingMs != null && remainingMs <= 0L
                LazyColumn(
                    Modifier.weight(1f).fillMaxWidth(),
                    contentPadding = PaddingValues(horizontal = 20.dp, vertical = 17.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    item {
                        Text("Anfrage prüfen", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
                    }
                    item {
                        Text("Risiko: ${approval.risk.displayName()}", color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.titleSmall)
                    }
                    item {
                        SelectionContainer { Text(approval.summary, style = MaterialTheme.typography.bodyLarge) }
                    }
                    remainingMs?.let { ms ->
                        item {
                            if (ms <= 0L) {
                                Text("Abgelaufen", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall, fontWeight = FontWeight.SemiBold)
                            } else {
                                Text("Läuft ab in ${formatRemaining(ms)}", style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                    if (!allowed) item { StatusMessage("Dieses Gerät kann diese Freigabe in der aktuellen Verbindung nicht entscheiden.") }
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
                                        Modifier.selectable(selected, enabled = allowed && !busy && !expired, role = Role.RadioButton) {
                                            selection = decision.name
                                        }.padding(horizontal = 11.dp, vertical = 8.dp),
                                        horizontalArrangement = Arrangement.spacedBy(13.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        RadioButton(selected, onClick = null, enabled = allowed && !busy && !expired)
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
                    TextButton(onClick = onClose) { Text("Schließen") }
                    Button(
                        enabled = allowed && !busy && !expired && choice != null,
                        onClick = { if (choice != null) onDecide(choice) },
                    ) { Text(if (busy) "Wird bestätigt …" else "Entscheidung bestätigen") }
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
        item { Text("Aktivität", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold) }
        item { StatusMessage(sessionStatus(state, session, view)) }
        snapshot?.lastSyncMillis?.let { millis ->
            item {
                val time = Instant.ofEpochMilli(millis).atZone(ZoneId.systemDefault()).format(ShortTime)
                Text("Zuletzt abgeglichen $time", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
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
    !state.online -> "Offline · Nur lesen"
    view?.snapshot?.online != true -> "Verbinde mit Session …"
    view.snapshot.approvals.isNotEmpty() -> "Wartet auf Freigabe"
    view.snapshot.runningTurnId != null -> "Antwort läuft"
    else -> session?.state?.displayName() ?: "Bereit"
}

private fun toolSummary(entry: TranscriptEntry): String = listOfNotNull(entry.toolState?.displayName(), entry.text.takeIf { it.isNotBlank() })
    .joinToString(" · ")

private fun MobileToolState.displayName() = when (this) {
    MobileToolState.RUNNING -> "Läuft"
    MobileToolState.DONE -> "Fertig"
    MobileToolState.FAILED -> "Fehlgeschlagen"
}

private fun ApprovalRisk.displayName() = when (this) {
    ApprovalRisk.LOW -> "Niedrig"
    ApprovalRisk.MEDIUM -> "Mittel"
    ApprovalRisk.HIGH -> "Hoch"
    ApprovalRisk.CRITICAL -> "Kritisch"
}

internal fun ApprovalDecision.displayName() = when (this) {
    ApprovalDecision.APPROVED_ONCE -> "Einmal erlauben"
    ApprovalDecision.APPROVED -> "Genehmigen"
    ApprovalDecision.REJECTED -> "Ablehnen"
}

private val ShortTime = DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)

@Composable
private fun ModelSwitchSheet(state: WorkspaceState, connection: HarwConnection, sessionId: String, active: Boolean) {
    val manage = state.manage
    val models = switchableModels(manage.catalog?.models.orEmpty()) { it.lifecycle }
    val allowed = modelChipBlock(manage.support, active) == null
    val running = manage.running != null
    Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text("Modell wechseln", style = MaterialTheme.typography.titleMedium)
        manage.card?.let { CommandResultCard(card = it, onClose = connection::clearCommandCard) }
        manage.catalogError?.let { StatusMessage(it, error = true) }
        if (models.isEmpty()) {
            StatusMessage("Keine Modelle im Katalog.")
        } else {
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
                items(models, key = { "${it.provider}/${it.id}" }) { model ->
                    val line = modelSwitchLine(model.provider, model.id)
                    val live = manage.catalog?.liveProvider == model.provider && manage.catalog?.liveModel == model.id
                    Row(
                        Modifier.fillMaxWidth()
                            .selectable(selected = live, enabled = allowed && !running && line != null, role = Role.RadioButton) {
                                if (line != null) connection.runCommand(sessionId, line)
                            }
                            .padding(vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        RadioButton(selected = live, onClick = null)
                        Column(Modifier.weight(1f)) {
                            Text(model.name ?: model.id, style = MaterialTheme.typography.bodyMedium)
                            Text(
                                "${model.provider} · ${model.id}",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }
    }
}
