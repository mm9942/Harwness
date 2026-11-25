package dev.harw.mobile

import android.content.res.Configuration
import android.net.Uri
import android.os.SystemClock
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.material.icons.outlined.AddPhotoAlternate
import androidx.compose.material3.InputChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.produceState
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.material.icons.outlined.CallSplit
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowLeft
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.Construction
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material.icons.outlined.Security
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.outlined.Terminal
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.ApprovalRisk
import uniffi.harw_mobile_ffi.MobileApproval
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.MobileSessionState
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileToolState
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.MobileCloudLibraryCommand
import uniffi.harw_mobile_ffi.MobileCloudLibraryReply
import uniffi.harw_mobile_ffi.MobileCloudLibraryScope
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.ModelFileState
import uniffi.harw_mobile_ffi.ProviderKind
import uniffi.harw_mobile_ffi.ProviderStore
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

internal fun modelChipLabel(sessionModel: String?, liveModel: String?, liveProvider: String? = null): String =
    when {
        liveModel != null && liveProvider != null -> "$liveProvider/$liveModel"
        else -> liveModel ?: sessionModel ?: "Modell"
    }

/** Aktiver Provider/Modell des On-Device-Hosts als "provider/modell" (nächster Turn nutzt genau dieses). */
internal fun localModelLabel(provider: String?, model: String?): String = when {
    provider.isNullOrBlank() || model.isNullOrBlank() -> "Modell"
    provider == "local-gguf" -> "Gerät/$model"
    else -> "$provider/$model"
}

/** Whether the vision projector of the on-device model is on the phone, so it reads images. */
internal fun localModelSeesImages(context: android.content.Context, modelId: String?): Boolean {
    if (modelId.isNullOrBlank()) return false
    if (localVisionFailure(modelId) != null) return false
    return try {
        uniffi.harw_mobile_ffi.modelProjector(ModelStore(context.applicationContext).modelsDir().path, modelId)
            ?.compatible == true
    } catch (_: Exception) {
        false
    }
}

/** Ob der Provider beim letzten Laden wirklich Bilder lesen kann (Status der Engine, nicht Dateiexistenz). */
internal fun localVisionLoaded(modelId: String?): Boolean {
    if (modelId.isNullOrBlank()) return false
    val status = try {
        uniffi.harw_mobile_ffi.localVisionStatus()
    } catch (_: Exception) {
        null
    } ?: return false
    return status.modelId == modelId && status.loaded
}

/** Fehlertext, wenn die Projektor-Datei des Modells vorhanden ist, aber beim letzten Laden nicht benutzbar war. */
internal fun localVisionFailure(modelId: String?): String? {
    val status = try {
        uniffi.harw_mobile_ffi.localVisionStatus()
    } catch (_: Exception) {
        null
    } ?: return null
    if (status.modelId != modelId || !status.projectorPresent || status.loaded) return null
    return status.error ?: "unbekannter Fehler"
}

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

private class OpenArchiveSheet(val controller: ArchiveForkController, val attachment: ArchiveAttachment? = null)

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
    archiveMenu: ChatArchiveMenuEntry,
    modifier: Modifier = Modifier,
    agentVersion: Int = 0,
    showQuickPickers: Boolean = true,
) {
    val view = state.windows[sessionId]
    val snapshot = view?.snapshot
    val generation = view?.generation?.takeIf { state.online && it == state.generation }
    val active = generation != null && snapshot?.online == true
    val approvals = snapshot?.approvals.orEmpty()
    val snapshotCanApprove = snapshot?.canApprove == true
    val snapshotCanInterrupt = snapshot?.canInterrupt == true
    var approvalId by rememberSaveable(sessionId) { mutableStateOf<String?>(null) }
    var transcriptJump by remember(sessionId) { mutableIntStateOf(0) }
    val approval = approvals.firstOrNull { it.id == approvalId }
    val turnId = snapshot?.runningTurnId
    val replyRunning = turnId != null || session?.state?.let(ReplyRunningSessionStates::contains) == true
    val preservingReply = view?.preserveAnswerWhileReloading == true
    val attachDetail = snapshot?.detail
    val replayPending = isAnswerReplayPending(attachDetail)
    val reloading = view?.answerReloadPending == true || preservingReply && replayPending
    val replyStatus = if (state.local && (!active || reloading)) null else chatReplyStatus(
        attached = active, running = replyRunning, hasReplyText = !snapshot?.liveText.isNullOrBlank(),
        reloading = reloading, approval = approvals.isNotEmpty() || session?.state == MobileSessionState.WAITING_FOR_APPROVAL,
    )
    val approvalPending = SessionAction(sessionId, SessionActionKind.Decide) in state.pendingActions
    val interruptPending = SessionAction(sessionId, SessionActionKind.Interrupt) in state.pendingActions
    val sendPending = SessionAction(sessionId, SessionActionKind.Send) in state.pendingActions
    // Ein veralteter Gateway-Halter bei ruhender Session zaehlt nicht (Holder-Frame kam nicht an).
    val holder = view?.holder?.takeUnless {
        it.kind == MobileHolderKind.GATEWAY && turnId == null && session?.state == MobileSessionState.IDLE
    }
    val access = conversationAccess(
        active = active,
        canSubmit = snapshot?.canSubmit == true,
        holder = holder,
        turnRunning = turnId != null,
    )
    val manageSupport = state.manage.support
    val manageOn = manageSupport?.manage == true
    var commandsOpen by rememberSaveable(sessionId) { mutableStateOf(false) }
    var managedRead by remember(connection, sessionId, state.generation) { mutableStateOf<ComposerManagedRequest?>(null) }
    val ctx = androidx.compose.ui.platform.LocalContext.current
    val titleFallback = rememberAutomaticChatTitle(
        context = ctx, connection = connection, profile = state.profile, deviceNodeId = state.deviceNodeId,
        generation = generation, sessionId = sessionId,
        committedCursor = snapshot?.head?.durable?.takeIf { active && !reloading && !replyRunning },
        hasTitle = session?.title?.isNotBlank() == true,
    )
    val draftText = window.draft(sessionId).text
    LaunchedEffect(sessionId, state.online, state.generation) {
        connection.refreshManageSupport()
        if (!state.local) connection.refreshCatalog(sessionId)
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
    val requestedRead = managedRead
    if (requestedRead != null && generation != null) {
        val expectedProfile = state.profile
        val expectedNode = state.deviceNodeId
        val currentSession = {
            val now = connection.state.value
            managedRead === requestedRead && window.sessionId == sessionId && now.online &&
                now.generation == generation && now.profile == expectedProfile && now.deviceNodeId == expectedNode &&
                now.windows[sessionId]?.generation == generation
        }
        val closeRead = { managedRead = null }
        ModalBottomSheet(
            onDismissRequest = closeRead,
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            ComposerManagedCommandSheet(requestedRead, connection, state, sessionId, generation, currentSession, closeRead) {
                if (currentSession() && window.draft(sessionId).text == requestedRead.draft) window.edit(sessionId, "")
            }
        }
    } else if (commandsOpen && manageOn) {
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
    if (approval != null) {
        ApprovalScreen(
            approval = approval,
            allowed = active && snapshotCanApprove,
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
    val archiveRevision = observedArchiveRevision(
        sessionId, snapshot?.sessionId, active, reloading, snapshot?.head,
    )
    val archiveAttachment by produceState<ArchiveAttachment?>(
        null, connection, sessionId, active, state.generation, state.profile,
    ) {
        value = if (active) connection.archiveAttachment(sessionId) else null
    }
    var archiveSheet by remember(connection, sessionId, window.projectKey) { mutableStateOf<OpenArchiveSheet?>(null) }
    var restoredTimeline by remember(connection, state.profile, state.generation, sessionId, window.projectKey) {
        mutableStateOf<RetainedTimeline?>(null)
    }
    var retainedForkBusy by remember(connection, state.profile, state.generation, sessionId, window.projectKey) {
        mutableStateOf(false)
    }
    val historicalTimeline = restoredTimeline?.takeIf { active && archiveAttachment == it.attachment }
    val reloadProject = window.projectKey
    val canReloadAnswer = active && !state.stopping && !reloading && !replyRunning &&
        historicalTimeline == null && approvals.isEmpty() &&
        state.pendingActions.none { it.sessionId == sessionId } && !window.draft(sessionId).sending

    // Reload reattaches the reader. Reserve exact Stop, Send and approval
    // controls by never manually detaching their live attachment mid-action.
    fun reloadCurrentAnswer() {
        val expectedGeneration = generation ?: return
        if (!canReloadAnswer || restoredTimeline != null || window.sessionId != sessionId ||
            window.projectKey != reloadProject || window.draft(sessionId).sending
        ) return
        val latest = connection.state.value
        if (!latest.online || latest.stopping || latest.generation != expectedGeneration ||
            latest.local != state.local || latest.profile != state.profile ||
            latest.deviceNodeId != state.deviceNodeId ||
            latest.pendingActions.any { it.sessionId == sessionId }
        ) return
        val latestWindow = latest.windows[sessionId] ?: return
        val latestSnapshot = latestWindow.snapshot ?: return
        val latestSummary = latest.sessions.firstOrNull { it.sessionId == sessionId }
            ?.takeIf { latest.sessionSummariesGeneration == expectedGeneration }
        if (latestWindow.generation != expectedGeneration || !latestSnapshot.online ||
            latestSnapshot.runningTurnId != null || latestSnapshot.approvals.isNotEmpty() ||
            latestSummary?.state?.let(ReplyRunningSessionStates::contains) == true ||
            latestWindow.answerReloadPending ||
            latestWindow.preserveAnswerWhileReloading && isAnswerReplayPending(latestSnapshot.detail)
        ) return
        connection.reloadAnswer(sessionId)
    }

    LaunchedEffect(restoredTimeline, historicalTimeline) {
        if (historicalTimeline == null) restoredTimeline = null
    }
    BackHandler(enabled = historicalTimeline != null) { restoredTimeline = null }
    var archiveOpening by remember(connection, sessionId) { mutableStateOf(false) }
    val archiveCoroutine = rememberCoroutineScope()
    val menuAttachment = archiveAttachment?.takeIf {
        active && it.epoch == generation &&
            (archiveRevision != null || archiveSheet != null || it.archiveRetainedRewriteV1)
    }
    SideEffect {
        archiveMenu.publish(
            action = menuAttachment?.let { expected ->
                {
                    if (!archiveOpening) {
                        val project = window.projectKey
                        val profile = state.profile
                        archiveOpening = true
                        archiveCoroutine.launch {
                            try {
                                val controller = connection.archiveAccess(sessionId)
                                val actual = connection.archiveAttachment(sessionId)
                                val current = connection.state.value
                                if (window.sessionId != sessionId || window.projectKey != project ||
                                    current.profile != profile || current.generation != expected.epoch
                                ) return@launch
                                if (controller != null && actual == expected) {
                                    archiveSheet = OpenArchiveSheet(controller, actual)
                                } else {
                                    android.widget.Toast.makeText(ctx, "Das Verlaufsarchiv ist nicht verfügbar.", android.widget.Toast.LENGTH_SHORT).show()
                                }
                            } finally {
                                archiveOpening = false
                            }
                        }
                    }
                }
            },
            opening = archiveOpening,
        )
    }
    DisposableEffect(archiveMenu) {
        onDispose { archiveMenu.publish(null) }
    }
    archiveSheet?.let { sheet ->
        val controller = sheet.controller
        val archiveState by controller.state.collectAsState()
        val attached = (archiveAttachment ?: sheet.attachment)?.takeIf { state.online && it.epoch == state.generation }
        val submitArchiveFork: (ArchiveRowLocator?) -> Unit = { locator ->
            val project = window.projectKey
            archiveCoroutine.launch {
                val currentAttachment = connection.archiveAttachment(sessionId) ?: return@launch
                val route = ArchiveForkRoute(sessionId, project, currentAttachment, sheet)
                if (!route.isStillOpen(window.sessionId, window.projectKey, currentAttachment, archiveSheet)) return@launch
                val child = if (locator == null) controller.retryUnknown() else controller.fork(locator)
                if (child != null) {
                    val current = connection.archiveAttachment(sessionId)
                    if (route.isStillOpen(window.sessionId, window.projectKey, current, archiveSheet)) {
                        archiveSheet = null
                        window.select(child)
                    }
                }
            }
        }
        ModalBottomSheet(
            onDismissRequest = { archiveSheet = null },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            Column(Modifier.fillMaxWidth().heightIn(max = 740.dp).padding(horizontal = 16.dp, vertical = 8.dp)) {
                if (!archiveState.matches(attached)) {
                    Text("Das Verlaufsarchiv ist auf dieser Verbindung nicht verfügbar.")
                } else {
                    if (archiveRevision != null && !archiveState.outcomeUnknown) {
                        if (archiveState.checkpoint == null) {
                            Text("Sichere eine vom Host bestätigte Fassung. Nicht bestätigte ältere Fassungen bleiben davon getrennt.")
                        }
                        TextButton(
                            enabled = !archiveState.busy,
                            onClick = { archiveCoroutine.launch { controller.issue(archiveRevision) } },
                        ) { Text("Diesen Stand sichern") }
                    }
                    if (attached?.archiveRetainedRewriteV1 == true && !archiveState.outcomeUnknown) {
                        archiveState.predecessor?.let { prior ->
                            val reason = if (prior.userEdit) "Vor der Bearbeitung" else "Vor der Verlaufsersetzung"
                            Text("$reason · ${prior.recordedAt}", style = MaterialTheme.typography.bodySmall)
                        }
                        TextButton(
                            enabled = !archiveState.busy && !archiveState.noOlderPredecessor,
                            onClick = { archiveCoroutine.launch { controller.openPreviousRevision() } },
                        ) { Text(if (archiveState.predecessor == null) "Frühere bestätigte Fassung öffnen" else "Noch frühere Fassung öffnen") }
                        TextButton(
                            enabled = !archiveState.busy,
                            onClick = {
                                val expected = attached
                                val project = window.projectKey
                                val profile = state.profile
                                val stillOpen = {
                                    archiveSheet === sheet && window.sessionId == sessionId && window.projectKey == project &&
                                        connection.state.value.profile == profile && connection.state.value.generation == expected.epoch
                                }
                                archiveCoroutine.launch {
                                    val timeline = try {
                                        controller.readRetainedTimeline(expected, stillOpen, selected = archiveState.predecessor)
                                    } catch (cancelled: CancellationException) {
                                        throw cancelled
                                    } catch (_: Exception) {
                                        null
                                    }
                                    if (!stillOpen() || connection.archiveAttachment(sessionId) != expected) return@launch
                                    if (timeline == null) {
                                        android.widget.Toast.makeText(
                                            ctx, ctx.getString(R.string.chat_history_retained_unavailable), android.widget.Toast.LENGTH_LONG,
                                        ).show()
                                    } else {
                                        archiveSheet = null
                                        restoredTimeline = timeline
                                    }
                                }
                            },
                        ) { Text(stringResource(R.string.chat_history_show_retained_in_chat)) }
                    }
                    ArchiveForkPanel(
                        state = archiveState,
                        attachment = attached,
                        fetchMedia = { row, index, mode ->
                            val binding = attached
                            val checkpoint = archiveState.checkpoint
                            if (binding == null || checkpoint == null) {
                                uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
                            } else {
                                connection.archiveMediaPreview(binding, checkpoint.checkpointId, row, index, mode)
                            }
                        },
                        onOlder = { archiveCoroutine.launch { controller.loadOlder() } },
                        onNewest = { archiveCoroutine.launch { controller.loadNewest() } },
                        onFork = submitArchiveFork,
                        onResolveUnknown = { submitArchiveFork(null) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        }
    }

    BoxWithConstraints(modifier.imePadding()) {
        val availableHeight = maxHeight
        Column(Modifier.fillMaxSize()) {
            if (historicalTimeline != null) {
                val expected = historicalTimeline.attachment
                val project = window.projectKey
                val profile = state.profile
                val stillOpen = {
                    restoredTimeline === historicalTimeline && window.sessionId == sessionId &&
                        window.projectKey == project && connection.state.value.profile == profile &&
                        connection.state.value.generation == expected.epoch
                }
                VerifiedRetainedTimelinePane(
                    timeline = historicalTimeline,
                    busy = retainedForkBusy,
                    onBackToCurrent = { restoredTimeline = null },
                    onOpenArchive = {
                        archiveCoroutine.launch {
                            val controller = connection.archiveAccess(sessionId)
                            val opened = try {
                                controller?.openPagedTimeline(historicalTimeline, stillOpen) == true
                            } catch (cancelled: CancellationException) {
                                throw cancelled
                            } catch (_: Exception) {
                                false
                            }
                            if (!stillOpen() || connection.archiveAttachment(sessionId) != expected) return@launch
                            if (controller != null && opened) {
                                archiveSheet = OpenArchiveSheet(controller, expected)
                                restoredTimeline = null
                            } else {
                                android.widget.Toast.makeText(ctx,
                                    ctx.getString(R.string.chat_history_retained_unavailable),
                                    android.widget.Toast.LENGTH_LONG).show()
                            }
                        }
                    },
                    fetchMedia = { row, index, mode ->
                        if (!stillOpen()) uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
                        else connection.archiveMediaPreview(
                            expected, historicalTimeline.checkpoint.checkpointId, row, index, mode,
                        ).takeIf { stillOpen() } ?: uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
                    },
                    onFork = { row ->
                        if (!retainedForkBusy && stillOpen()) {
                            retainedForkBusy = true
                            archiveCoroutine.launch {
                                try {
                                    val controller = connection.archiveAccess(sessionId)
                                    val child = controller?.forkRetainedTimeline(historicalTimeline, row, stillOpen)
                                    if (!stillOpen() || connection.archiveAttachment(sessionId) != expected) return@launch
                                    if (child != null) {
                                        restoredTimeline = null
                                        window.select(child)
                                    } else {
                                        android.widget.Toast.makeText(
                                            ctx, ctx.getString(R.string.chat_history_retained_fork_unconfirmed),
                                            android.widget.Toast.LENGTH_LONG,
                                        ).show()
                                    }
                                } finally {
                                    retainedForkBusy = false
                                }
                            }
                        }
                    },
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                )
                return@Column
            }
            if (canReloadAnswer) {
                TextButton(
                    onClick = ::reloadCurrentAnswer,
                    modifier = Modifier.align(Alignment.End).padding(horizontal = 10.dp),
                ) { Text(stringResource(R.string.chat_reply_reload)) }
            }
            val scope = rememberCoroutineScope()
            HolderBanner(
                access, holder, Modifier.padding(horizontal = 15.dp, vertical = 4.dp), state.local,
                onResume = { scope.launch { connection.resumeSession(sessionId) } },
            )
            if (approvals.isNotEmpty()) {
                ApprovalStrip(
                    approvals.size,
                    canDecide = active && snapshotCanApprove,
                    onClick = { approvalId = approvals.first().id },
                    modifier = Modifier.padding(horizontal = 15.dp, vertical = 5.dp),
                )
            }
            // Failure records remain in the source journal for diagnosis. Only
            // the failure of the latest run is surfaced, as a dismissible notice.
            val currentFailure = snapshot?.transcript.orEmpty().asReversed()
                .takeWhile { it.kind != TranscriptKind.USER }
                .firstOrNull { it.kind == TranscriptKind.ERROR }
            titleFallback?.let { ChatTitleFallbackButton(it, Modifier.padding(horizontal = 15.dp)) }
            val detailedFailure = currentFailure?.text?.takeIf { !replyRunning && isIncompleteResponseNotice(it) }
            val failure = view?.error ?: detailedFailure ?: snapshot?.lastFailure
                ?: attachDetail?.takeIf { preservingReply && !reloading }
                ?: currentFailure?.text
            snapshot?.detail?.takeUnless { it == failure || reloading || replayPending }?.let {
                StatusMessage(it, Modifier.padding(horizontal = 15.dp, vertical = 4.dp))
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
            val agentName = rememberChatAgentName(sessionId, state.local, agentVersion) ?: "Harw"
            val versionScope = sessionHomeScope(state)
            var versions by remember(connection, versionScope, sessionId) {
                mutableStateOf<Map<String, List<ChatVersion>>>(emptyMap())
            }
            var versionsLoaded by remember(connection, versionScope, sessionId) { mutableStateOf(false) }
            var viewing by remember(connection, versionScope, sessionId) { mutableStateOf(emptyMap<String, Int>()) }
            var awaitingRewrite by remember(connection, versionScope, sessionId) { mutableStateOf<AwaitingChatRewrite?>(null) }
            var journalLoaded by remember(connection, versionScope, sessionId) { mutableStateOf(false) }
            var journalUnreadable by remember(connection, versionScope, sessionId) { mutableStateOf(false) }
            var journalBusy by remember(connection, versionScope, sessionId) { mutableStateOf(false) }
            val rewriteCoroutine = rememberCoroutineScope()
            LaunchedEffect(connection, versionScope, sessionId, active) {
                val owner = versionScope ?: return@LaunchedEffect
                if (!active || versionsLoaded) return@LaunchedEffect
                val (savedVersions, pending) = withContext(Dispatchers.IO) {
                    ChatVersions.load(ctx, owner, sessionId) to ChatVersions.readPending(ctx, owner, sessionId)
                }
                if (window.sessionId != sessionId || sessionHomeScope(connection.state.value) != owner) return@LaunchedEffect
                versions = savedVersions
                when (pending) {
                    is ChatVersions.Pending.Found -> window.restoreUncertainRewrite(connection, sessionId, pending)
                    ChatVersions.Pending.Unreadable -> journalUnreadable = true
                    ChatVersions.Pending.Missing -> Unit
                }
                journalLoaded = true
                versionsLoaded = true
            }
            LaunchedEffect(awaitingRewrite, snapshot?.head, snapshot?.transcript, active, reloading, versionsLoaded) {
                val owner = versionScope ?: return@LaunchedEffect
                val pending = awaitingRewrite ?: return@LaunchedEffect
                val observed = snapshot ?: return@LaunchedEffect
                val cursor = observed.head ?: return@LaunchedEffect
                if (active && !reloading && versionsLoaded && pending.observedOn(observed.transcript, cursor.durable)) {
                    if (!pending.previous.captureComplete) {
                        withContext(Dispatchers.IO) { ChatVersions.removePending(ctx, owner, sessionId, pending.requestId) }
                        if (window.sessionId != sessionId || sessionHomeScope(connection.state.value) != owner) return@LaunchedEffect
                        awaitingRewrite = null
                        android.widget.Toast.makeText(ctx, ctx.getString(R.string.chat_rewrite_archive_only),
                            android.widget.Toast.LENGTH_LONG).show()
                        return@LaunchedEffect
                    }
                    val saved = withContext(Dispatchers.IO) {
                        ChatVersions.append(ctx, owner, sessionId, versions, pending.anchor, pending.previous).also {
                            if (it.persisted) ChatVersions.removePending(ctx, owner, sessionId, pending.requestId)
                        }
                    }
                    if (window.sessionId != sessionId || sessionHomeScope(connection.state.value) != owner) return@LaunchedEffect
                    versions = saved.variants
                    viewing = emptyMap()
                    awaitingRewrite = null
                    if (!saved.persisted) {
                        android.widget.Toast.makeText(
                            ctx, ctx.getString(R.string.chat_history_variant_temporary), android.widget.Toast.LENGTH_LONG,
                        ).show()
                    }
                }
            }
            var forkPreparing by remember(sessionId) { mutableStateOf(false) }
            val forkError: (String) -> Unit = { message ->
                android.widget.Toast.makeText(ctx, message, android.widget.Toast.LENGTH_LONG).show()
            }
            val uncertainId = window.uncertainRewriteRequestId(connection, sessionId)
            val uncertainRewrite = uncertainId != null
            val canManageRewrite = generation != null && active && !replyRunning && !reloading && !sendPending
            val canResolveRewrite = generation != null && canManageRewrite &&
                connection.exactRewindAvailable(sessionId, generation)
            val rewindAvailable = canResolveRewrite && journalLoaded && !journalUnreadable && !journalBusy &&
                !uncertainRewrite && awaitingRewrite == null
            val reportRewrite: (DraftResult, RewriteVersionCandidate) -> Unit = report@{ outcome, candidate ->
                val current = connection.state.value
                if (window.sessionId != sessionId || sessionHomeScope(current) != versionScope ||
                    current.deviceNodeId != state.deviceNodeId || current.profile != state.profile ||
                    current.generation != generation
                ) return@report
                when (outcome) {
                    is DraftResult.ExactAccepted -> {
                        val before = candidate.previous.beforeDurable ?: return@report
                        awaitingRewrite = AwaitingChatRewrite(
                            candidate.anchor, before, candidate.previous, outcome.replacementItemId, candidate.requestId,
                        )
                    }
                    DraftResult.Accepted -> {
                        // Older hosts only acknowledge enqueueing. No invented
                        // replacement item or variant for that receipt.
                        forkError("Änderung angenommen. Dieser Host meldet die neue Fassung nicht eindeutig zurück.")
                    }
                    is DraftResult.Rejected -> forkError(outcome.error)
                }
            }
            val resend = { entryId: String, replacement: String? ->
                val all = snapshot?.transcript.orEmpty()
                val at = all.withIndex().singleOrNull { it.value.id == entryId }
                    ?.takeIf { it.value.kind == TranscriptKind.USER }?.index
                val currentHead = snapshot?.head
                if (!rewindAvailable || !isDurableRewriteItemId(entryId) || at == null) {
                    forkError("Bearbeiten und Neu generieren brauchen eine eindeutig geladene Nachricht und einen aktuellen Host mit exakter Verlaufsbearbeitung.")
                } else if (generation != null && currentHead != null) {
                    val anchor = ChatVersions.anchorOf(all, at)
                    val previous = ChatVersion(
                        all.drop(at).map { it.copy(images = it.images.toList()) },
                        currentHead.durable,
                    )
                    val owner = versionScope
                    if (owner != null) {
                        val id = java.util.UUID.randomUUID().toString()
                        val pending = ChatVersions.Pending.Found(
                            DraftAttempt(replacement.orEmpty(), currentHead, id, rewindBefore = entryId),
                            RewriteVersionCandidate(anchor, previous, id),
                        )
                        journalBusy = true
                        rewriteCoroutine.launch {
                            try {
                                val reserved = withContext(Dispatchers.IO) {
                                    ChatVersions.reservePending(ctx, owner, sessionId, pending)
                                }
                                val current = connection.state.value
                                if (window.sessionId != sessionId || sessionHomeScope(current) != owner ||
                                    current.generation != generation || current.deviceNodeId != state.deviceNodeId) return@launch
                                if (!reserved) {
                                    when (val existing = withContext(Dispatchers.IO) {
                                        ChatVersions.readPending(ctx, owner, sessionId)
                                    }) {
                                        is ChatVersions.Pending.Found -> window.restoreUncertainRewrite(connection, sessionId, existing)
                                        ChatVersions.Pending.Unreadable -> journalUnreadable = true
                                        ChatVersions.Pending.Missing -> Unit
                                    }
                                    forkError(ctx.getString(R.string.chat_rewrite_journal_unavailable))
                                    return@launch
                                }
                                val admitted = window.regenerate(connection, sessionId, generation, pending) { outcome, candidate ->
                                    reportRewrite(outcome, candidate)
                                    if (outcome == DraftResult.Accepted &&
                                        window.sessionId == sessionId &&
                                        sessionHomeScope(connection.state.value) == owner &&
                                        connection.state.value.generation == generation
                                    ) {
                                        // Queue-only responses (for example a route compatibility change)
                                        // cannot settle the journal. Show the recovery affordance.
                                        window.restoreUncertainRewrite(connection, sessionId, pending)
                                    }
                                    if (outcome is DraftResult.Rejected && !outcome.ambiguous) {
                                        rewriteCoroutine.launch(Dispatchers.IO) {
                                            ChatVersions.removePending(ctx, owner, sessionId, id)
                                        }
                                    }
                                }
                                if (!admitted) {
                                    window.restoreUncertainRewrite(connection, sessionId, pending)
                                    forkError(ctx.getString(R.string.chat_rewrite_resolve_unavailable))
                                }
                            } finally {
                                journalBusy = false
                            }
                        }
                    }
                }
            }
            var forgetRewrite by remember(connection, versionScope, sessionId) { mutableStateOf(false) }
            var forgetRequestId by remember(connection, versionScope, sessionId) { mutableStateOf<String?>(null) }
            if (journalUnreadable || uncertainRewrite || awaitingRewrite != null) {
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.padding(horizontal = 12.dp),
                ) {
                    if (uncertainRewrite && !journalUnreadable) {
                        TextButton(
                            enabled = canResolveRewrite && !journalBusy,
                            onClick = {
                                val epoch = generation
                                if (epoch != null && !window.resolveUncertainRewrite(connection, sessionId, epoch, reportRewrite)) {
                                    forkError(ctx.getString(R.string.chat_rewrite_resolve_unavailable))
                                }
                            },
                        ) { Text(stringResource(R.string.chat_rewrite_resolve)) }
                    } else {
                        TextButton(
                            enabled = canReloadAnswer,
                            onClick = ::reloadCurrentAnswer,
                        ) { Text(stringResource(R.string.chat_rewrite_reload)) }
                    }
                    TextButton(
                        enabled = canManageRewrite && !journalBusy,
                        onClick = {
                            forgetRequestId = awaitingRewrite?.requestId ?: uncertainId
                            forgetRewrite = true
                        },
                    ) { Text(stringResource(R.string.chat_rewrite_forget)) }
                }
            }
            if (forgetRewrite) {
                AlertDialog(
                    onDismissRequest = { forgetRewrite = false },
                    title = { Text(stringResource(R.string.chat_rewrite_forget)) },
                    text = { Text(stringResource(R.string.chat_rewrite_forget_explanation)) },
                    confirmButton = {
                        TextButton(
                            enabled = canManageRewrite && !journalBusy,
                            onClick = {
                                val current = connection.state.value
                                val owner = versionScope
                                if (owner != null && window.sessionId == sessionId &&
                                    sessionHomeScope(current) == owner && current.generation == generation &&
                                    current.deviceNodeId == state.deviceNodeId
                                ) {
                                    journalBusy = true
                                    val request = forgetRequestId
                                    rewriteCoroutine.launch {
                                        try {
                                            val cleared = withContext(Dispatchers.IO) {
                                                ChatVersions.removePending(ctx, owner, sessionId, request)
                                            }
                                            val live = connection.state.value
                                            if (window.sessionId != sessionId || sessionHomeScope(live) != owner ||
                                                live.generation != generation || live.deviceNodeId != current.deviceNodeId) return@launch
                                            if (!cleared) {
                                                forkError(ctx.getString(R.string.chat_rewrite_journal_unavailable))
                                                return@launch
                                            }
                                            journalUnreadable = false
                                            window.forgetUncertainRewrite(connection, sessionId, request)
                                            awaitingRewrite = null
                                            reloadCurrentAnswer()
                                        } finally {
                                            journalBusy = false
                                        }
                                    }
                                }
                                forgetRewrite = false
                            },
                        ) { Text(stringResource(R.string.chat_rewrite_forget_confirm)) }
                    },
                    dismissButton = {
                        TextButton(onClick = { forgetRewrite = false }) {
                            Text(stringResource(android.R.string.cancel))
                        }
                    },
                )
            }
            // Only offer Retry for an observed interruption and an exact
            // unique durable prompt. Never turn an unacknowledged send or
            // a policy refusal into a second request.
            val retryPrompt = snapshot?.transcript.orEmpty().let { transcript ->
                val latestUser = transcript.lastOrNull { it.kind == TranscriptKind.USER }
                latestUser?.takeIf { isDurableRewriteItemId(it.id) && transcript.count { row -> row.id == it.id } == 1 }
            }
            val interruptedReply = snapshot?.lastTurnInterrupted == true ||
                currentFailure?.text?.let(::isInterruptedResponseNotice) == true
            val retryMessage = if (interruptedReply && !replyRunning) {
                failure ?: stringResource(R.string.chat_reply_interrupted)
            } else failure
            retryMessage?.let {
                val failureId = currentFailure?.id ?: retryPrompt?.id ?: turnId
                val retry: (() -> Unit)? = if (interruptedReply && rewindAvailable && retryPrompt != null) {
                    { resend(retryPrompt.id, null) }
                } else null
                ChatErrorNotice(window.chatNotices, listOf(connection, state.profile, sessionId, failureId, it), it, retry)
            }
            val versionsUi = VersionsUi(
                if (active && versionsLoaded) versions else emptyMap(), viewing,
                onView = { anchor, index -> viewing = if (index == null) viewing - anchor else viewing + (anchor to index) },
                onEdit = { entry, _, text -> resend(entry.id, text) },
            )
            val displayedEntries = remember(snapshot?.transcript) { dedupeUserRuns(snapshot?.transcript.orEmpty()) }
            val exportDocument = rememberChatTranscriptExporter()
            val exportTranscript: () -> Unit = export@{
                fun notice(message: Int) = android.widget.Toast.makeText(ctx, message, android.widget.Toast.LENGTH_LONG).show()
                val cursor = snapshot?.head
                if (!active || generation == null || cursor == null || reloading || replyRunning) {
                    notice(R.string.chat_slash_export_wait)
                    return@export
                }
                // The historical device cache isn't the authority for this peer/profile.
                if (applyViewing(displayedEntries, versionsUi).second != Int.MAX_VALUE) {
                    notice(R.string.chat_slash_export_current_version)
                    return@export
                }
                val expectedProfile = state.profile
                val expectedDevice = state.deviceNodeId
                val expectedProject = window.projectKey
                val authorized = {
                    val current = connection.state.value
                    val currentWindow = current.windows[sessionId]
                    val currentSnapshot = currentWindow?.snapshot
                    window.sessionId == sessionId && window.projectKey == expectedProject &&
                        current.online && !current.stopping && current.profile == expectedProfile &&
                        current.deviceNodeId == expectedDevice && current.generation == generation &&
                        currentWindow?.generation == generation && currentWindow?.answerReloadPending == false &&
                        currentSnapshot?.online == true && currentSnapshot.sessionId == sessionId &&
                        currentSnapshot.runningTurnId == null &&
                        currentSnapshot.head?.hasSameRewriteBoundary(cursor) == true
                }
                if (!authorized()) {
                    notice(R.string.chat_slash_export_changed)
                    return@export
                }
                when (val transcript = renderChatTranscript(
                    displayedEntries, ctx.getString(R.string.chat_slash_export_user),
                    ctx.getString(R.string.chat_slash_export_agent),
                    { count -> ctx.getString(R.string.chat_slash_export_images, count) },
                )) {
                    is ChatTranscriptText.Ready -> {
                        if (exportDocument(transcript.text, authorized)) window.edit(sessionId, "")
                        else notice(R.string.chat_slash_export_failed)
                    }
                    ChatTranscriptText.Empty -> notice(R.string.chat_slash_export_empty)
                    ChatTranscriptText.TooLarge -> notice(R.string.chat_slash_export_too_large)
                }
            }
            val mediaProject = window.projectKey
            ConversationFeed(
                agentName = agentName,
                entries = displayedEntries,
                mediaScope = listOf(
                    connection, state.profile, state.deviceNodeId, generation, sessionId, mediaProject,
                    snapshot?.head?.generation,
                ),
                fetchMedia = { itemId, index, media, mode ->
                    connection.sessionMediaPreview(
                        state.profile, state.deviceNodeId, generation, snapshot?.head?.generation,
                        sessionId, itemId, index, media, mode,
                    )
                },
                resolveHistoricalMedia = resolve@{ selection, beforeDurable, imageCount ->
                    val epoch = generation ?: return@resolve null
                    val stillOpen = {
                        val live = connection.state.value
                        window.sessionId == sessionId && window.projectKey == mediaProject &&
                            live.online && live.profile == state.profile && live.deviceNodeId == state.deviceNodeId &&
                            live.generation == epoch && live.windows[sessionId]?.generation == epoch
                    }
                    if (!stillOpen()) return@resolve null
                    val controller = connection.archiveAccess(sessionId) ?: return@resolve null
                    controller.resolveDisplayedArchiveMedia(selection, beforeDurable, imageCount, stillOpen)
                        ?.takeIf { stillOpen() && it.attachment.epoch == epoch }
                },
                fetchHistoricalMedia = { binding, index, mode ->
                    connection.archiveMediaPreview(binding.attachment, binding.checkpoint.checkpointId, binding.row, index, mode)
                },
                onRegenerate = if (rewindAvailable) ({ itemId -> resend(itemId, null) }) else null,
                rewritesAvailable = rewindAvailable,
                onHistoricalMedia = { selection ->
                    if (!forkPreparing) {
                        val project = window.projectKey
                        val expectedProfile = state.profile
                        forkPreparing = true
                        scope.launch {
                            try {
                                val current = connection.archiveAttachment(sessionId)
                                val controller = connection.archiveAccess(sessionId)
                                if (current == null || controller == null) {
                                    forkError("Für Bilder älterer Fassungen wird die bestätigte Archivverbindung benötigt.")
                                    return@launch
                                }
                                if (window.sessionId != sessionId || window.projectKey != project ||
                                    connection.state.value.profile != expectedProfile
                                ) return@launch
                                val sheet = OpenArchiveSheet(controller, current)
                                archiveSheet = sheet
                                val route = ArchiveForkRoute(sessionId, project, current, sheet)
                                controller.openDisplayedArchiveMedia(selection) {
                                    route.isStillOpen(window.sessionId, window.projectKey, current, archiveSheet) &&
                                        connection.state.value.generation == current.epoch
                                }
                            } finally { forkPreparing = false }
                        }
                    }
                },
                versionsUi = versionsUi,
                onFork = { rows ->
                    val head = snapshot?.head
                    val epoch = generation
                    if (!active || head == null || epoch == null || reloading || replyRunning) {
                        forkError("Warte, bis der angezeigte Chat vollständig geladen ist, und versuche Fork erneut.")
                    } else if (!forkPreparing) {
                        val project = window.projectKey
                        val profile = state.profile
                        forkPreparing = true
                        scope.launch {
                            try {
                                val child = connection.forkDisplayedSession(sessionId, epoch, head, rows)
                                if (window.sessionId == sessionId && window.projectKey == project &&
                                    connection.state.value.profile == profile && connection.state.value.generation == epoch
                                ) window.select(child)
                            } catch (cancelled: CancellationException) {
                                throw cancelled
                            } catch (failure: Exception) {
                                if (window.sessionId == sessionId && connection.state.value.profile == profile) {
                                    forkError("Fork nicht bestätigt: ${displayedForkFailure(failure)}")
                                }
                            } finally { forkPreparing = false }
                        }
                    }
                },
                onHistoricalFork = { rows ->
                    val selection = retainedDisplayedSelection(rows)
                    if (selection == null) {
                        forkError("Die angezeigte ältere Fassung kann nicht vollständig geprüft werden.")
                    } else if (!forkPreparing) {
                        val project = window.projectKey
                        val profile = state.profile
                        val pickedVersion = viewing
                        forkPreparing = true
                        scope.launch {
                            try {
                                val current = connection.archiveAttachment(sessionId)
                                val controller = connection.archiveAccess(sessionId)
                                val stillSelected: () -> Boolean = {
                                    window.sessionId == sessionId && window.projectKey == project &&
                                        viewing == pickedVersion && connection.state.value.profile == profile &&
                                        connection.state.value.generation == current?.epoch
                                }
                                if (current == null || controller == null) {
                                    if (window.sessionId == sessionId && viewing == pickedVersion) {
                                        forkError("Die angezeigte ältere Fassung ist auf diesem Host nicht bestätigt.")
                                    }
                                    return@launch
                                }
                                if (!stillSelected()) return@launch
                                val child = controller.forkDisplayed(selection, null, rows, stillSelected)
                                if (!stillSelected() || connection.archiveAttachment(sessionId) != current) return@launch
                                if (child != null) {
                                    window.select(child)
                                } else if (controller.state.value.outcomeUnknown) {
                                    forkError("Fork unbestätigt. Tippe denselben Fork erneut, um nur diesen Vorgang zu prüfen.")
                                } else {
                                    forkError(controller.state.value.notice ?: "Diese ältere Fassung konnte nicht exakt geprüft werden.")
                                }
                            } finally { forkPreparing = false }
                        }
                    }
                },
                liveText = snapshot?.liveText?.takeUnless { it.isEmpty() },
                liveReasoning = snapshot?.liveReasoning?.takeIf { !it.isBlank() },
                showReasoning = readShowReasoningPref(ctx),
                replyStatus = replyStatus,
                jumpToLatest = transcriptJump,
                emptyMessage = when {
                    snapshot == null && state.online -> "Session wird geöffnet …"
                    !active -> "Warte auf eine aktive Session."
                    else -> "Noch keine Nachrichten."
                },
                modifier = Modifier.weight(1f).fillMaxWidth(),
            )
            val head = snapshot?.head
            // Tippen und Bilder anhaengen gehen immer (ausser echt nur lesend); nur Senden wartet.
            val composerEnabled = access != ConversationAccess.Offline && access != ConversationAccess.ReadOnly
            val sendHead = head?.takeIf {
                access == ConversationAccess.Writable && turnId == null && !sendPending && !interruptPending
            }
            val sendEnabled = sendHead != null
            val queuedFirst = window.firstMessage(sessionId)
            LaunchedEffect(sessionId, queuedFirst, sendEnabled, generation) {
                if (queuedFirst != null && generation != null && sendHead != null) {
                    window.takeFirstMessage(sessionId)
                    window.edit(sessionId, queuedFirst)
                    window.submit(connection, sessionId, generation, sendHead)
                }
            }
            val imagesSupported by produceState(false, state.online, state.generation) {
                value = state.online && connection.supportsImages()
            }
            val draft = window.draft(sessionId)
            val libraryProfile = state.profile
            val lifecycle = LocalLifecycleOwner.current.lifecycle
            var foreground by remember(lifecycle) { mutableStateOf(lifecycle.currentState == Lifecycle.State.RESUMED) }
            var cloudLibraryItem by remember(connection, sessionId, libraryProfile, generation) {
                mutableStateOf<CloudChatLibraryItem?>(null)
            }
            var cloudLibraryPicker by remember(connection, sessionId, libraryProfile, generation) {
                mutableStateOf<ScopedCloudChatLibraryTarget?>(null)
            }
            var resolvingLibrarySend by remember(connection, sessionId, libraryProfile, generation) { mutableStateOf(false) }
            DisposableEffect(lifecycle, connection, sessionId, libraryProfile, generation) {
                val observer = LifecycleEventObserver { _, _ ->
                    foreground = lifecycle.currentState == Lifecycle.State.RESUMED
                    if (!foreground) {
                        cloudLibraryItem = null
                        cloudLibraryPicker = null
                    }
                }
                lifecycle.addObserver(observer)
                onDispose { lifecycle.removeObserver(observer) }
            }
            val libraryTarget by produceState<ScopedCloudChatLibraryTarget?>(
                null, connection, libraryProfile, generation, sessionId, active, imagesSupported, foreground, state.local,
            ) {
                value = null
                if (foreground && !state.local && active && imagesSupported &&
                    libraryProfile != null && generation != null
                ) {
                    value = try {
                        connection.cloudLibraryScope()?.let {
                            CloudLibraryChatTarget(libraryProfile, it, generation, sessionId)
                        }
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (_: Exception) {
                        null
                    }
                }
            }

            fun libraryTargetCurrent(target: ScopedCloudChatLibraryTarget, checkedScope: MobileCloudLibraryScope?): Boolean {
                val actual = connection.state.value
                val viewNow = actual.windows[sessionId]
                val liveSnapshot = viewNow?.snapshot
                return target.permits(
                    actual.profile, checkedScope, actual.generation, window.sessionId, actual.local,
                    actual.online && !actual.stopping && viewNow?.generation == target.generation &&
                        liveSnapshot?.online == true && liveSnapshot.canSubmit,
                    lifecycle.currentState == Lifecycle.State.RESUMED,
                )
            }

            // This selector never downloads bytes. The draft keeps only one
            // scope-fenced selection, resolved on the existing manual Send.
            val cloudLibraryReady = libraryTarget?.takeIf { libraryTargetCurrent(it, it.scope) }
            val pickCloudLibrary: (() -> Unit)? = cloudLibraryReady?.takeIf {
                !resolvingLibrarySend && !draft.sending && !draft.uncertain &&
                    queuedFirst == null && draft.rewindBefore == null
            }?.let { target ->
                {
                    val latest = window.draft(sessionId)
                    if (libraryTargetCurrent(target, target.scope) && !resolvingLibrarySend &&
                        !latest.sending && !latest.uncertain && latest.rewindBefore == null &&
                        window.firstMessage(sessionId) == null
                    ) cloudLibraryPicker = target
                }
            }
            val attachmentProject = window.projectKey
            val attachmentIdentity = remember(connection, state.profile, state.deviceNodeId, state.local,
                sessionId, generation, attachmentProject) { Any() }
            val galleryCurrent: () -> Boolean = {
                val latest = connection.state.value
                val currentWindow = latest.windows[sessionId]
                val currentSnapshot = currentWindow?.snapshot
                val currentDraft = window.draft(sessionId)
                generation != null && window.sessionId == sessionId && window.projectKey == attachmentProject &&
                    latest.profile == state.profile && latest.deviceNodeId == state.deviceNodeId &&
                    latest.local == state.local && latest.online && !latest.stopping &&
                    latest.generation == generation && currentWindow?.generation == generation &&
                    currentSnapshot?.online == true && currentSnapshot.canSubmit &&
                    !currentDraft.sending && !currentDraft.uncertain
            }
            val choosing = cloudLibraryPicker
            if (choosing != null && choosing == cloudLibraryReady) {
                CloudLibraryChatPicker(
                    connection, choosing,
                    onPick = { selected ->
                        if (libraryTargetCurrent(selected.target, choosing.scope)) cloudLibraryItem = selected
                        cloudLibraryPicker = null
                    },
                    onClose = { cloudLibraryPicker = null },
                )
            }
            val selectedCloudImage = cloudLibraryItem
            if (selectedCloudImage != null) {
                Row(Modifier.padding(horizontal = 14.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        stringResource(
                            if (resolvingLibrarySend) R.string.chat_attachment_library_loading
                            else R.string.chat_attachment_library_selected,
                            selectedCloudImage.label,
                        ),
                        Modifier.weight(1f),
                        maxLines = 2, overflow = TextOverflow.Ellipsis,
                    )
                    TextButton(enabled = !resolvingLibrarySend, onClick = { cloudLibraryItem = null }) {
                        Text(stringResource(R.string.chat_attachment_remove))
                    }
                }
            }

            val interrupt = if (active && generation != null && turnId != null &&
                snapshotCanInterrupt && !interruptPending
            ) {
                val action: () -> Unit = {
                    val latest = connection.state.value
                    val latestWindow = latest.windows[sessionId]
                    val live = latestWindow?.snapshot
                    if (window.sessionId == sessionId && latest.online && !latest.stopping &&
                        latest.profile == state.profile && latest.generation == generation &&
                        latestWindow?.generation == generation && live?.online == true &&
                        live.runningTurnId == turnId && live.canInterrupt &&
                        SessionAction(sessionId, SessionActionKind.Interrupt) !in latest.pendingActions
                    ) {
                        connection.interrupt(sessionId, generation, turnId)
                    }
                }
                action
            } else null
            PromptComposer(
                draft = draft,
                beforeInput = { if (showQuickPickers) ChatQuickPickerRow(archiveMenu.quickPickers) },
                showStop = active && (turnId != null || sendPending || draft.sending || interruptPending),
                onInterrupt = interrupt,
                onAttachImage = if (imagesSupported) ({ image ->
                    if (galleryCurrent()) window.attachImage(sessionId, image)
                }) else null,
                attachmentIdentity = attachmentIdentity,
                galleryOwnerCurrent = galleryCurrent,
                onChooseCloudLibrary = pickCloudLibrary,
                onDetachImage = { index -> window.detachImage(sessionId, index) },
                onImageError = { message -> window.failImage(sessionId, message) },
                enabled = composerEnabled,
                canSend = sendEnabled && !resolvingLibrarySend &&
                    (selectedCloudImage == null || selectedCloudImage.target == cloudLibraryReady),
                busy = sendPending || resolvingLibrarySend,
                hasDeferredAttachment = selectedCloudImage != null,
                onEdit = { window.edit(sessionId, it) },
                onReview = { transcriptJump++ },
                onDiscard = { window.discardUncertainDraft(sessionId) },
                onSend = {
                    if (generation != null && head != null) {
                        val picked = cloudLibraryItem
                        if (picked == null) {
                            if (!resolvingLibrarySend) window.submit(connection, sessionId, generation, head)
                        } else if (!resolvingLibrarySend && picked.target == cloudLibraryReady &&
                            libraryTargetCurrent(picked.target, picked.target.scope)
                        ) {
                            val originalDraft = window.draft(sessionId)
                            resolvingLibrarySend = true
                            scope.launch {
                                try {
                                    val freshScope = connection.cloudLibraryScope()
                                    if (!libraryTargetCurrent(picked.target, freshScope)) return@launch
                                    val payload = connection.cloudLibrary(
                                        picked.target.scope,
                                        MobileCloudLibraryCommand.Preview(picked.revision, picked.assetId),
                                    ) as? MobileCloudLibraryReply.Preview
                                        ?: error("Wrong Cloud Library reply")
                                    try {
                                        currentCoroutineContext().ensureActive()
                                        val finalScope = connection.cloudLibraryScope()
                                        val current = connection.state.value.windows[sessionId]?.snapshot?.head
                                        if (payload.assetId != picked.assetId ||
                                            !libraryTargetCurrent(picked.target, finalScope) ||
                                            !sameCloudLibrarySend(picked, cloudLibraryItem, originalDraft, window.draft(sessionId), head, current) ||
                                            !window.submitCloudLibraryImage(connection, picked.target, finalScope, originalDraft, head, payload.jpeg)
                                        ) {
                                            if (libraryTargetCurrent(picked.target, finalScope)) {
                                                window.failImage(sessionId, "Chat oder Entwurf hat sich geändert. Es wurde nichts gesendet.")
                                            }
                                            return@launch
                                        }
                                        cloudLibraryItem = null
                                    } finally { payload.jpeg.fill(0) }
                                } catch (cancelled: CancellationException) {
                                    throw cancelled
                                } catch (_: Exception) {
                                    if (libraryTargetCurrent(picked.target, picked.target.scope)) {
                                        cloudLibraryItem = null
                                        window.failImage(sessionId, "Das Cloud-Bild ist nicht mehr verfügbar. Es wurde nichts gesendet; wähle es erneut.")
                                    }
                                } finally { resolvingLibrarySend = false }
                            }
                        }
                    }
                },
                hint = composerHint(access, turnId != null),
                onCommands = when {
                    !manageOn -> null
                    state.local && active -> ({ managedRead = ComposerManagedRequest("/") })
                    state.local -> null
                    else -> ({ commandsOpen = true })
                },
                onExportTranscript = exportTranscript,
                onManagedSlash = if (active && generation != null) ({ text ->
                    managedRead = ComposerManagedRequest(text)
                }) else null,
            )
        }
    }
}

/** A complete retained revision, not a synthetic per-message alternate/current session. */
@Composable
private fun VerifiedRetainedTimelinePane(
    timeline: RetainedTimeline,
    busy: Boolean,
    onBackToCurrent: () -> Unit,
    onOpenArchive: () -> Unit,
    fetchMedia: suspend (ArchiveRowLocator, UInt, uniffi.harw_mobile_ffi.MobileMediaPreviewMode) ->
        uniffi.harw_mobile_ffi.MobileMediaPreview,
    onFork: (ArchiveRowLocator) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 15.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(stringResource(R.string.chat_history_retained_readonly), Modifier.weight(1f),
                style = MaterialTheme.typography.labelMedium)
            IconButton(onClick = onBackToCurrent) {
                Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, stringResource(R.string.chat_history_return_current))
            }
        }
        if (!timeline.complete) {
            Text(stringResource(R.string.chat_history_partial_readonly),
                Modifier.padding(horizontal = 15.dp), style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = onOpenArchive, modifier = Modifier.padding(horizontal = 7.dp)) {
                Text(stringResource(R.string.chat_history_open_paged_archive))
            }
        }
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(horizontal = 15.dp, vertical = 10.dp),
            verticalArrangement = Arrangement.spacedBy(9.dp),
        ) {
            items(timeline.rows, key = { it.locator.itemId }) { row ->
                val user = row.role == ArchiveRowRole.USER
                MessageBlock(
                    label = stringResource(if (user) R.string.chat_slash_export_user else R.string.chat_slash_export_agent),
                    text = row.textParts.joinToString("\n"),
                    kind = if (user) TranscriptKind.USER else TranscriptKind.ASSISTANT,
                    imageCount = row.imageCount.toInt(),
                    mediaStrip = {
                        SessionMediaPreviewStrip(
                            scope = Triple(timeline.attachment, timeline.checkpoint, row.locator),
                            retainedCount = row.imageCount.toInt(),
                            legacyCount = 0,
                            fetch = { index, mode -> fetchMedia(row.locator, index, mode) },
                        )
                    },
                    onFork = if (busy || !timeline.complete) null else ({ onFork(row.locator) }),
                )
            }
        }
    }
}

private val ReplyRunningSessionStates = setOf(
    MobileSessionState.QUEUED, MobileSessionState.RUNNING,
    MobileSessionState.WAITING_FOR_APPROVAL, MobileSessionState.WAITING_FOR_CHILD,
)

private fun isAnswerReplayPending(detail: String?): Boolean = detail == null || detail in setOf(
    "Attaching to session", "Reconnecting to the session stream", "Catching up with the server",
    "Synchronizing the session transcript",
)

/** Only report observable turn/transport status; never infer or copy reasoning text. */
internal fun chatReplyStatus(
    attached: Boolean,
    running: Boolean,
    hasReplyText: Boolean,
    reloading: Boolean,
    approval: Boolean,
): String? = when {
    reloading -> "Antwort wird nachgeladen …"
    !running -> null
    !attached -> "Antwortstatus wird abgeglichen …"
    approval -> "Wartet auf Freigabe."
    hasReplyText -> "Antwort wird gestreamt …"
    else -> "Antwort wird erstellt … Noch kein Antworttext empfangen."
}

internal fun isIncompleteResponseNotice(text: String): Boolean =
    text.startsWith("Antwort unvollständig:") || text.equals("model output was truncated", ignoreCase = true)

private fun isInterruptedResponseNotice(text: String): Boolean =
    text.startsWith("Antwort unvollständig: Der Antwortstream endete vor der Abschlussmeldung") ||
        text.startsWith("Antwort unvollständig: Die Übertragung der Anbieterantwort wurde unterbrochen")

@Composable
private fun ChatErrorNotice(
    ledger: ChatNoticeLedger,
    key: Any,
    message: String,
    onRetry: (() -> Unit)? = null,
) {
    LaunchedEffect(ledger, key) {
        val deadline = ledger.show(key)
        delay((deadline - SystemClock.elapsedRealtime()).coerceAtLeast(0))
        ledger.dismiss(key)
    }
    if (ledger.isDismissed(key)) return
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        StatusMessage(message, Modifier.weight(1f), error = true)
        if (onRetry != null) {
            TextButton(onClick = onRetry) {
                Text(stringResource(R.string.chat_reply_retry))
            }
        }
        IconButton(onClick = { ledger.dismiss(key) }) {
            Icon(Icons.Outlined.Close, contentDescription = "Fehlermeldung schließen", Modifier.size(18.dp))
        }
    }
}

@Composable
private fun ConversationFeed(
    agentName: String,
    entries: List<TranscriptEntry>,
    mediaScope: Any,
    fetchMedia: suspend (String, UInt, String, uniffi.harw_mobile_ffi.MobileMediaPreviewMode) ->
        uniffi.harw_mobile_ffi.MobileMediaPreview,
    resolveHistoricalMedia: suspend (HistoricalForkSelection, ULong, Int) -> VerifiedHistoricalMedia?,
    fetchHistoricalMedia: suspend (VerifiedHistoricalMedia, UInt, uniffi.harw_mobile_ffi.MobileMediaPreviewMode) ->
        uniffi.harw_mobile_ffi.MobileMediaPreview,
    onRegenerate: ((String) -> Unit)? = null,
    rewritesAvailable: Boolean = false,
    onHistoricalMedia: ((HistoricalForkSelection) -> Unit)? = null,
    onFork: ((List<TranscriptEntry>) -> Unit)? = null,
    onHistoricalFork: ((List<TranscriptEntry>) -> Unit)? = null,
    versionsUi: VersionsUi? = null,
    liveText: String?,
    liveReasoning: String?,
    showReasoning: Boolean,
    replyStatus: String?,
    jumpToLatest: Int,
    emptyMessage: String,
    modifier: Modifier,
) {
    // A reversed list keeps the latest line at the bottom during bursty token updates.
    val viewed = remember(entries, versionsUi?.versions, versionsUi?.viewing) {
        if (versionsUi == null) entries to Int.MAX_VALUE else applyViewing(entries, versionsUi)
    }
    val shown = viewed.first
    val readOnlyFrom = viewed.second
    val currentVariant = readOnlyFrom == Int.MAX_VALUE
    val retainedBefore = if (currentVariant || versionsUi == null) null else {
        val anchor = ChatVersions.anchorOf(entries, readOnlyFrom)
        versionsUi.viewing[anchor]?.let { versionsUi.versions[anchor]?.getOrNull(it)?.beforeDurable }
    }
    val ambiguousIds = remember(shown) {
        shown.groupingBy { it.id }.eachCount().filterValues { it > 1 }.keys
    }
    val visibleReasoning = liveReasoning.takeIf { currentVariant && showReasoning }
    val visibleReply = liveText.takeIf { currentVariant }
    val visibleStatus = replyStatus.takeIf { currentVariant }
    if (shown.none { it.kind != TranscriptKind.ERROR && (showReasoning || it.kind != TranscriptKind.REASONING) } &&
        visibleReply == null && visibleStatus == null && visibleReasoning == null
    ) {
        Box(modifier, contentAlignment = Alignment.Center) { DirectoryEmpty(emptyMessage) }
        return
    }
    val reversed = remember(shown, showReasoning) {
        // Keep the original indices for version boundaries and fork locators;
        // the render-only projection never replaces the source list.
        shown.withIndex().filter { (_, entry) ->
            entry.kind != TranscriptKind.ERROR && (showReasoning || entry.kind != TranscriptKind.REASONING)
        }.asReversed()
    }
    val listState = rememberLazyListState()
    var followLatest by rememberSaveable { mutableStateOf(true) }
    LaunchedEffect(listState) {
        snapshotFlow { listState.isScrollInProgress to listState.canScrollBackward }.collect { (scrolling, awayFromLatest) ->
            if (scrolling || !awayFromLatest) followLatest = !awayFromLatest
        }
    }
    LaunchedEffect(visibleReply != null, visibleReasoning != null, visibleStatus != null, shown.lastOrNull()?.id) {
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
        if (visibleReply != null) {
            item(key = "live-reply") { MessageBlock("$agentName · Streaming", visibleReply, TranscriptKind.ASSISTANT) }
        }
        if (visibleStatus != null) {
            item(key = "live-reply-status") { InlineReplyMetadata(visibleStatus) }
        }
        if (visibleReasoning != null) {
            item(key = "live-reasoning") { InlineReasoning(visibleReasoning, live = true) }
        }
        items(reversed, key = { row ->
            if (row.value.id in ambiguousIds) "ambiguous-entry-${row.index}" else "entry-${row.value.id}"
        }) { (index, entry) ->
            if (entry.kind == TranscriptKind.REASONING) {
                InlineReasoning(entry.text, live = false)
                return@items
            }
            val label = when (entry.kind) {
                TranscriptKind.USER -> "Du"
                TranscriptKind.ASSISTANT -> agentName
                TranscriptKind.TOOL -> entry.toolState?.let { "Werkzeug · ${it.displayName()}" } ?: "Werkzeug"
                TranscriptKind.ERROR -> "Fehler"
                TranscriptKind.REASONING -> "Denkprozess"
            }
            val exactRow = entry.id.isNotBlank() && entry.id !in ambiguousIds
            val fork = when {
                !exactRow -> null
                index in 0 until readOnlyFrom && onFork != null -> {
                    { onFork(displayedForkPrefix(shown, index)) }
                }
                index >= readOnlyFrom && onHistoricalFork != null -> {
                    { onHistoricalFork(displayedForkPrefix(shown, index)) }
                }
                else -> null
            }
            if (entry.kind == TranscriptKind.USER) {
                val anchor = ChatVersions.anchorOf(shown, index)
                val known = versionsUi?.versions?.get(anchor).orEmpty()
                val viewingIdx = versionsUi?.viewing?.get(anchor)
                val canAct = index in 0 until readOnlyFrom
                val canRewrite = exactRow && canAct && rewritesAvailable && isDurableRewriteItemId(entry.id)
                val nav = if (known.isEmpty() || versionsUi == null || !exactRow) null else {
                    val current = (viewingIdx ?: known.size) + 1
                    VersionNav(
                        current = current,
                        total = known.size + 1,
                        onPrev = if (current > 1) ({ versionsUi.onView(anchor, current - 2) }) else null,
                        onNext = if (current < known.size + 1) ({ versionsUi.onView(anchor, if (current >= known.size) null else current) }) else null,
                    )
                }
                val (shownText, count) = entry.userTextAndImageCount()
                val forkSelection = selectedForkRow(shown, index, readOnlyFrom).takeIf { exactRow }
                MessageBlock(
                    label, shownText, entry.kind, imageCount = count,
                    mediaStrip = {
                        if (canAct && exactRow) {
                            SessionMediaPreviewStrip(
                                scope = Triple(mediaScope, entry.id, entry.images),
                                retainedCount = entry.images.size,
                                legacyCount = (count - entry.images.size).coerceAtLeast(0),
                                fetch = { ordinal, mode -> fetchMedia(entry.id, ordinal, entry.images[ordinal.toInt()], mode) },
                            )
                        } else if (!canAct && forkSelection != null && retainedBefore != null && entry.images.isNotEmpty()) {
                            HistoricalSessionMediaStrip(
                                scope = listOf(mediaScope, retainedBefore, forkSelection, entry.images),
                                resolve = { resolveHistoricalMedia(forkSelection, retainedBefore, entry.images.size) },
                                fetch = fetchHistoricalMedia,
                            )
                        } else if (!canAct && forkSelection != null && onHistoricalMedia != null) {
                            TextButton(onClick = { onHistoricalMedia(forkSelection) }) {
                                Text("Server-Archiv öffnen")
                            }
                        } else {
                            Text("Bildreferenz nicht eindeutig bestätigt", style = MaterialTheme.typography.labelSmall)
                        }
                    },
                    onRegenerate = if (canRewrite) onRegenerate?.let { cb -> { cb(entry.id) } } else null,
                    onFork = fork,
                    onEdit = if (canRewrite && versionsUi != null) ({ newText -> versionsUi.onEdit(entry, anchor, newText) }) else null,
                    nav = nav,
                    oldVersion = viewingIdx != null && index >= 0 && !canAct,
                )
                return@items
            }
            MessageBlock(label, entry.text, entry.kind,
                onFork = fork.takeIf { entry.kind == TranscriptKind.ASSISTANT })
        }
    }
}

/** Capture all shown conversational rows through the click, without truncation, search or reread. */
internal fun displayedForkPrefix(displayed: List<TranscriptEntry>, index: Int): List<TranscriptEntry> {
    if (index !in displayed.indices) return emptyList()
    return displayed.subList(0, index + 1).filter {
        it.kind == TranscriptKind.USER || it.kind == TranscriptKind.ASSISTANT
    }.map { it.copy(images = it.images.toList()) }
}

/** Show only the provider-facing display text, never treat it as Markdown or a command. */
@Composable
private fun InlineReasoning(text: String, live: Boolean) {
    val preview = if (live && text.length > 1_200) "…" + text.takeLast(1_200) else text
    SelectionContainer {
        Text(
            preview, Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
            style = MaterialTheme.typography.bodySmall,
            fontStyle = FontStyle.Italic,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun InlineReplyMetadata(text: String) {
    Text(
        text, Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun MessageBlock(
    label: String,
    text: String,
    kind: TranscriptKind,
    imageCount: Int = 0,
    mediaStrip: (@Composable () -> Unit)? = null,
    onRegenerate: (() -> Unit)? = null,
    onFork: (() -> Unit)? = null,
    onEdit: ((String) -> Unit)? = null,
    nav: VersionNav? = null,
    oldVersion: Boolean = false,
) {
    val colors = MaterialTheme.colorScheme
    val user = kind == TranscriptKind.USER
    val (background, foreground) = when (kind) {
        TranscriptKind.USER -> colors.primaryContainer to colors.onPrimaryContainer
        TranscriptKind.ASSISTANT -> colors.surfaceContainerLowest to colors.onSurface
        TranscriptKind.TOOL, TranscriptKind.REASONING -> colors.surfaceContainer to colors.onSurfaceVariant
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
                if (imageCount > 0) mediaStrip?.invoke()
                if (imageCount == 0 || text.isNotBlank()) SelectionContainer {
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
                if (!user && onFork != null) {
                    IconButton(onClick = onFork, modifier = Modifier.align(Alignment.End).size(36.dp)) {
                        Icon(Icons.Outlined.CallSplit, contentDescription = "Bis hier in neuem Chat fortsetzen (Fork)", modifier = Modifier.size(20.dp))
                    }
                }
                if (user && (onRegenerate != null || onFork != null || onEdit != null || nav != null)) {
                    var editing by remember(text) { mutableStateOf(false) }
                    var draftText by remember(text) { mutableStateOf(text) }
                    if (editing && onEdit != null) {
                        androidx.compose.material3.OutlinedTextField(
                            value = draftText, onValueChange = { draftText = it }, modifier = Modifier.fillMaxWidth(),
                            label = { Text("Nachricht bearbeiten") },
                        )
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                            TextButton(onClick = { editing = false; draftText = text }) { Text("Abbrechen") }
                            TextButton(
                                enabled = draftText.isNotBlank(),
                                onClick = { editing = false; onEdit(draftText.trim()) },
                            ) { Text("Senden") }
                        }
                    } else {
                        if (oldVersion) {
                            Text("Ältere Fassung (nur Ansicht)", style = MaterialTheme.typography.labelSmall)
                        }
                        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                            if (nav != null) {
                                IconButton(onClick = { nav.onPrev?.invoke() }, enabled = nav.onPrev != null, modifier = Modifier.size(32.dp)) {
                                    Icon(Icons.AutoMirrored.Outlined.KeyboardArrowLeft, contentDescription = "Ältere Fassung", modifier = Modifier.size(20.dp))
                                }
                                Text("${nav.current}/${nav.total}", style = MaterialTheme.typography.labelSmall)
                                IconButton(onClick = { nav.onNext?.invoke() }, enabled = nav.onNext != null, modifier = Modifier.size(32.dp)) {
                                    Icon(Icons.AutoMirrored.Outlined.KeyboardArrowRight, contentDescription = "Neuere Fassung", modifier = Modifier.size(20.dp))
                                }
                            }
                            androidx.compose.foundation.layout.Spacer(Modifier.weight(1f))
                            if (onFork != null) {
                                IconButton(onClick = onFork, modifier = Modifier.size(36.dp)) {
                                    Icon(Icons.Outlined.CallSplit, contentDescription = "Bis hier in neuem Chat fortsetzen (Fork)", modifier = Modifier.size(20.dp))
                                }
                            }
                            if (onRegenerate != null) {
                                IconButton(onClick = onRegenerate, modifier = Modifier.size(36.dp)) {
                                    Icon(Icons.Outlined.Refresh, contentDescription = "Neu generieren", modifier = Modifier.size(20.dp))
                                }
                            }
                            if (onEdit != null) {
                                IconButton(onClick = { editing = true }, modifier = Modifier.size(36.dp)) {
                                    Icon(Icons.Outlined.Edit, contentDescription = "Nachricht bearbeiten", modifier = Modifier.size(20.dp))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

internal data class VersionNav(val current: Int, val total: Int, val onPrev: (() -> Unit)?, val onNext: (() -> Unit)?)

/**
 * Text für den Fork: der Verlauf vor der gewählten Nachricht als Kontext, dann die Nachricht selbst.
 * Der ursprüngliche Chat bleibt unverändert; der neue Chat bekommt den Text als unversendeten Entwurf.
 */
internal fun forkText(entries: List<TranscriptEntry>, selected: TranscriptEntry, selectedText: String): String {
    val at = entries.indexOfFirst { it.id == selected.id }.let { if (it < 0) entries.size else it }
    val lines = entries.take(at).mapNotNull { entry ->
        val who = when (entry.kind) {
            TranscriptKind.USER -> "Du"
            TranscriptKind.ASSISTANT -> "Harw"
            else -> return@mapNotNull null
        }
        val (clean, _) = if (entry.kind == TranscriptKind.USER) entry.userTextAndImageCount() else entry.text to 0
        clean.trim().takeIf { it.isNotEmpty() }?.let { "$who: $it" }
    }
    var context = lines.joinToString("\n\n")
    val limit = 12_000
    if (context.length > limit) context = "…" + context.takeLast(limit)
    if (context.isEmpty()) return selectedText
    return "[Fortsetzung eines früheren Chats. Der folgende Verlauf ist nur Kontext, beantworte ihn nicht noch einmal.]\n\n" +
        context + "\n\n---\n\n" + selectedText
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

internal fun composerHint(access: ConversationAccess, turnRunning: Boolean = false): String = when (access) {
    ConversationAccess.Writable -> if (turnRunning) "Nachricht schreiben, Senden nach der Antwort" else "Nachricht an Harw"
    ConversationAccess.Offline -> "Nur lesen, bis die Verbindung wieder steht"
    ConversationAccess.HeldElsewhere -> "Nachricht schreiben, Senden sobald die Session frei ist"
    ConversationAccess.RunningOnGateway -> "Nachricht schreiben, Senden nach der Antwort"
    ConversationAccess.ReadOnly -> "Nur lesen, der Server nimmt hier keine Eingaben an"
}

/** Abzeichen-Text für den aktuellen Halter; `null` = frei. */
internal fun holderBadge(holder: MobileHolder?, local: Boolean = false): String? = holder?.let {
    when (it.kind) {
        MobileHolderKind.GATEWAY -> if (local) "Läuft auf diesem Gerät" else "Läuft auf Gateway"
        MobileHolderKind.TUI -> "Belegt im Terminal"
        MobileHolderKind.JOB -> "Belegt durch Job"
        MobileHolderKind.UNKNOWN -> "Belegt"
    }
}

internal fun holderBannerText(access: ConversationAccess, holder: MobileHolder?, local: Boolean = false): String? = when (access) {
    ConversationAccess.HeldElsewhere -> when (holder?.kind) {
        MobileHolderKind.TUI -> "Die Session ist im Terminal geöffnet. Senden ist pausiert, bis sie dort frei wird."
        MobileHolderKind.JOB -> "Ein Job schreibt gerade in diese Session. Senden ist pausiert."
        else -> "Die Session ist gerade belegt. Senden ist pausiert."
    }
    ConversationAccess.RunningOnGateway ->
        if (local) "Auf diesem Gerät läuft gerade eine Antwort." else "Auf dem Gateway läuft gerade ein Turn. Du kannst mitlesen."
    ConversationAccess.ReadOnly -> "Nur lesen: Diese Session nimmt keine Eingaben an."
    else -> null
}

/** Nur Antworten des Modells werden als Markdown gesetzt; Nutzer-, Tool- und Fehlertexte bleiben wörtlich. */
internal fun rendersMarkdown(kind: TranscriptKind): Boolean = kind == TranscriptKind.ASSISTANT

@Composable
private fun HolderBanner(access: ConversationAccess, holder: MobileHolder?, modifier: Modifier, local: Boolean, onResume: () -> Unit) {
    // The composer and collapsible monitor already show the ordinary running state.
    if (access == ConversationAccess.RunningOnGateway) return
    val text = holderBannerText(access, holder, local) ?: return
    val badge = holderBadge(holder?.takeUnless { it.kind == MobileHolderKind.GATEWAY }, local)
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
        if (access == ConversationAccess.ReadOnly) {
            OutlinedButton(onClick = onResume) { Text("Fortsetzen") }
        }
    }
}

private class ComposerGalleryRequest(
    val accepts: () -> Boolean,
    val attach: (DraftImage) -> Unit,
    val onError: (String) -> Unit,
)

@Composable
internal fun PromptComposer(
    draft: SessionDraft,
    enabled: Boolean,
    canSend: Boolean = enabled,
    busy: Boolean,
    hint: String,
    onEdit: (String) -> Unit,
    onReview: () -> Unit,
    onSend: () -> Unit,
    onDiscard: () -> Unit,
    onCommands: (() -> Unit)? = null,
    onAttachImage: ((DraftImage) -> Unit)? = null,
    attachmentIdentity: Any? = null,
    galleryOwnerCurrent: () -> Boolean = { true },
    onChooseCloudLibrary: (() -> Unit)? = null,
    onDetachImage: (Int) -> Unit = {},
    onImageError: (String) -> Unit = {},
    hasDeferredAttachment: Boolean = false,
    showStop: Boolean = false,
    onInterrupt: (() -> Unit)? = null,
    onExportTranscript: (() -> Unit)? = null,
    onManagedSlash: ((String) -> Unit)? = null,
    beforeInput: (@Composable () -> Unit)? = null,
) {
    val context = LocalContext.current
    val slashCommand = parseChatSlashCommand(draft.text)
    var slashNotice by remember { mutableStateOf<Int?>(null) }
    slashNotice?.let { message ->
        AlertDialog(
            onDismissRequest = { slashNotice = null },
            title = { Text(stringResource(R.string.chat_slash_title)) },
            text = { Text(stringResource(message)) },
            confirmButton = {
                TextButton(onClick = { slashNotice = null }) { Text(stringResource(R.string.chat_slash_ok)) }
            },
        )
    }
    val preparationScope = rememberCoroutineScope()
    var preparingImages by remember(attachmentIdentity) { mutableIntStateOf(0) }
    var preparedImages by remember(attachmentIdentity) { mutableIntStateOf(0) }
    var galleryRequest by remember(attachmentIdentity) { mutableStateOf<ComposerGalleryRequest?>(null) }
    var chooseImageSource by remember(attachmentIdentity) { mutableStateOf(false) }
    val galleryAllowed by rememberUpdatedState(enabled && onAttachImage != null && !draft.uncertain && !draft.sending)
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris: List<Uri> ->
        val request = galleryRequest ?: return@rememberLauncherForActivityResult
        galleryRequest = null
        val existing = draft.images.size
        val selected = uris.take((MaxPromptImages - existing).coerceAtLeast(0))
        if (selected.isNotEmpty() && request.accepts() && preparingImages == 0) {
            preparingImages = selected.size
            preparedImages = 0
            preparationScope.launch {
                try {
                    for ((batchIndex, batch) in selected.chunked(2).withIndex()) {
                        if (!request.accepts()) break
                        // Cap decoders independently of native inference and preserve picker order.
                        val images = coroutineScope {
                            batch.mapIndexed { offset, uri ->
                                async {
                                    val index = existing + batchIndex * 2 + offset + 1
                                    val image = withContext(Dispatchers.IO) {
                                        prepareImage(context, uri, "Bild ${index}")
                                    }
                                    preparedImages += 1
                                    image
                                }
                            }.awaitAll()
                        }
                        if (!request.accepts()) break
                        images.forEach { image ->
                            if (image != null) request.attach(image)
                            else request.onError(context.getString(R.string.chat_attachment_gallery_decode_failed))
                        }
                    }
                } finally {
                    preparingImages = 0
                    preparedImages = 0
                }
            }
        }
    }
    fun launchGallery() {
        val attach = onAttachImage ?: return
        if (!galleryAllowed || preparingImages != 0 || galleryRequest != null || !galleryOwnerCurrent()) return
        galleryRequest = ComposerGalleryRequest(
            { galleryAllowed && galleryOwnerCurrent() }, attach, onImageError,
        )
        try {
            picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
        } catch (_: Exception) {
            galleryRequest = null
            if (galleryOwnerCurrent()) onImageError(context.getString(R.string.chat_attachment_gallery_open_failed))
        }
    }
    val chooseLibrary = onChooseCloudLibrary
    if (chooseImageSource && chooseLibrary != null && galleryAllowed) {
        AlertDialog(
            onDismissRequest = { chooseImageSource = false },
            title = { Text(stringResource(R.string.chat_attachment_source_title)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(stringResource(R.string.chat_attachment_source_disclosure))
                    if (hasDeferredAttachment) Text(
                        stringResource(R.string.chat_attachment_gallery_releases_library),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    TextButton(onClick = { chooseImageSource = false; launchGallery() }) {
                        Text(stringResource(R.string.chat_attachment_gallery))
                    }
                    TextButton(onClick = {
                        chooseImageSource = false
                        if (galleryOwnerCurrent()) chooseLibrary()
                    }) { Text(stringResource(R.string.chat_attachment_library)) }
                }
            },
            confirmButton = { TextButton(onClick = { chooseImageSource = false }) { Text(stringResource(android.R.string.cancel)) } },
        )
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
        if (preparingImages > 0) {
            Text("Bilder vorbereiten: ${preparedImages}/${preparingImages}", style = MaterialTheme.typography.labelSmall)
        }
        if (draft.images.isNotEmpty()) {
            Row(
                modifier = Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                draft.images.forEachIndexed { index, image ->
                    DraftThumbnail(image, enabled = preparingImages == 0 && enabled && !draft.uncertain) { onDetachImage(index) }
                }
            }
        }
        beforeInput?.invoke()
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (onCommands != null) {
                IconButton(onClick = onCommands) { Icon(Icons.Outlined.Terminal, "Befehle") }
            }
            if (onAttachImage != null) {
                IconButton(
                    enabled = preparingImages == 0 && galleryRequest == null && galleryAllowed &&
                        draft.images.size < MaxPromptImages,
                    onClick = {
                        if (chooseLibrary == null) launchGallery()
                        else if (galleryOwnerCurrent()) chooseImageSource = true
                    },
                ) { Icon(Icons.Outlined.AddPhotoAlternate, stringResource(R.string.chat_attachment_source_title)) }
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
            val actionPalette = MaterialTheme.colorScheme
            val actionColors = IconButtonDefaults.filledIconButtonColors(
                containerColor = actionPalette.harwAccentButtonContainer(true),
                contentColor = actionPalette.harwAccentButtonContent(true),
                disabledContainerColor = actionPalette.harwAccentButtonContainer(false),
                disabledContentColor = actionPalette.harwAccentButtonContent(false),
            )
            if (showStop) {
                FilledIconButton(onClick = { onInterrupt?.invoke() }, enabled = onInterrupt != null, colors = actionColors) {
                    Icon(Icons.Filled.Stop, stringResource(
                        if (onInterrupt == null) R.string.chat_reply_waiting_stop else R.string.chat_reply_stop,
                    ))
                }
            } else {
                FilledIconButton(
                    colors = actionColors,
                    onClick = {
                        routeChatComposerSend(draft.text, onSend) { command ->
                            when (command) {
                                ChatSlashCommand.Export -> if (onExportTranscript != null) onExportTranscript()
                                    else slashNotice = R.string.chat_slash_export_needs_chat
                                ChatSlashCommand.Help -> if (onManagedSlash != null) onManagedSlash(draft.text)
                                    else slashNotice = R.string.chat_slash_help
                                ChatSlashCommand.Unknown -> if (onManagedSlash != null) onManagedSlash(draft.text)
                                    else slashNotice = if (ComposerManagedRead.parse(draft.text) == null) R.string.chat_slash_unknown
                                        else R.string.chat_slash_host_unavailable
                            }
                        }
                    },
                    enabled = preparingImages == 0 && enabled && !draft.uncertain &&
                        (if (slashCommand != null) !busy else canSend &&
                            (draft.text.isNotBlank() || draft.images.isNotEmpty() || hasDeferredAttachment)),
                ) {
                    if (busy) {
                        CircularProgressIndicator(Modifier.size(19.dp), strokeWidth = 2.dp)
                    } else {
                        Icon(Icons.AutoMirrored.Outlined.Send, "Nachricht senden")
                    }
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
        sessionStatus(state, session, view)?.let { status -> item { StatusMessage(status) } }
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

private fun sessionStatus(state: WorkspaceState, session: MobileSession?, view: WindowState?): String? = when {
    !state.online -> null
    view?.generation == null || view.generation != state.generation || view.snapshot?.online != true ||
        view.answerReloadPending -> null
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

/** Zweistufiger Modellwechsel fuer Cloud-Sessions: erst Provider, dann Modell aus dem Katalog; gilt ab der naechsten Nachricht. */
@Composable
internal fun ModelSwitchSheet(
    state: WorkspaceState,
    connection: HarwConnection,
    sessionId: String,
    active: Boolean,
    pending: String? = null,
    onPick: ((String) -> Unit)? = null,
    canChange: () -> Boolean = { true },
) {
    val manage = state.manage
    val catalog = manage.catalog
    val liveCatalog = catalog?.takeIf { sessionId.isEmpty() || manage.catalogSessionId == sessionId }
    val models = switchableModels(catalog?.models.orEmpty()) { it.lifecycle }
    val allowed = modelChipBlock(manage.support, active) == null
    var provider by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val providers = models.groupBy { it.provider }
    Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                if (provider == null) "Provider wählen" else "Modell wählen",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            if (provider != null) TextButton(onClick = { provider = null; error = null }) { Text("Zurück") }
        }
        Text(
            "Aktuell: " + (pending ?: modelChipLabel(null, liveCatalog?.liveModel, liveCatalog?.liveProvider)) + " · gilt ab der nächsten Nachricht.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        manage.catalogError?.let { StatusMessage(it, error = true) }
        error?.let { StatusMessage(it, error = true) }
        if (models.isEmpty()) {
            StatusMessage("Keine Modelle im Katalog.")
        } else if (provider == null) {
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
                items(providers.keys.toList(), key = { it }) { name ->
                    val live = liveCatalog?.liveProvider == name
                    Row(
                        Modifier.fillMaxWidth()
                            .selectable(selected = live, enabled = allowed && !busy, role = Role.Button) { provider = name }
                            .padding(vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        RadioButton(selected = live, onClick = null)
                        Column(Modifier.weight(1f)) {
                            Text(name, style = MaterialTheme.typography.bodyMedium)
                            Text(
                                "${providers[name]?.size ?: 0} Modell(e)",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        } else {
            val chosen = provider.orEmpty()
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
                items(providers[chosen].orEmpty(), key = { "${it.provider}/${it.id}" }) { model ->
                    val line = modelSwitchLine(model.provider, model.id)
                    val live = if (pending != null) pending == "${model.provider}/${model.id}"
                    else liveCatalog?.liveProvider == model.provider && liveCatalog.liveModel == model.id
                    Row(
                        Modifier.fillMaxWidth()
                            .selectable(selected = live, enabled = allowed && !busy && line != null, role = Role.RadioButton) {
                                if (!canChange()) return@selectable
                                if (onPick != null) {
                                    onPick("${model.provider}/${model.id}")
                                    return@selectable
                                }
                                busy = true
                                error = null
                                connection.setSessionModel(sessionId, "${model.provider}/${model.id}") { failure ->
                                    busy = false
                                    error = failure
                                }
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


/** Modell-Knopf des On-Device-Hosts: zeigt Provider/Modell, öffnet die Auswahl, Wechsel gilt ab der nächsten Antwort. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun LocalModelChip(hasImages: Boolean = false) {
    var open by rememberSaveable { mutableStateOf(false) }
    var refresh by remember { mutableIntStateOf(0) }
    val label by rememberLocalModelLabel(refresh)
    val hint = rememberLocalVisionHint(hasImages, refresh, onActivated = { refresh++ })
    AssistChip(
        onClick = { open = true },
        label = { Text(label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
    )
    hint()
    if (open) {
        ModalBottomSheet(
            onDismissRequest = { open = false },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            LocalModelSheet(onChanged = { refresh++ })
        }
    }
}

@Composable
internal fun rememberLocalModelLabel(refresh: Int): androidx.compose.runtime.State<String> {
    val context = LocalContext.current
    return produceState("Modell", refresh) {
        value = withContext(Dispatchers.IO) {
            try {
                val info = ProviderStore(context.filesDir.path, AndroidPlatformKeystore(context)).homeInfo()
                val base = localModelLabel(info.activeProvider, info.activeModel)
                if (info.activeProvider == "local-gguf" && localVisionLoaded(info.activeModel)) {
                    "$base · Bilder"
                } else {
                    base
                }
            } catch (_: Exception) {
                "Modell"
            }
        }
    }
}

private class VisionOffer(val modelId: String, val addon: uniffi.harw_mobile_ffi.VisionAddon?)

/** Importiertes Modell ohne Projektor: "Projektor wählen" (Dateiauswahl, Prüfung, Neuaktivierung). */
@Composable
private fun rememberImportedProjectorHint(
    modelId: String,
    hasImages: Boolean,
    onActivated: () -> Unit,
): @Composable () -> Unit {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var error by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val failure = localVisionFailure(modelId)
    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri: Uri? ->
        if (uri == null) return@rememberLauncherForActivityResult
        busy = true
        error = null
        scope.launch {
            try {
                val result = ModelImport(context.applicationContext, ModelStore(context.applicationContext))
                    .attachProjector(modelId, uri) {}
                if (result.info == null) {
                    error = result.message
                } else {
                    ModelManager(ModelStore(context.applicationContext)).useLocal(context.filesDir.path, modelId)
                    onActivated()
                }
            } catch (e: Exception) {
                error = e.message ?: "Projektor konnte nicht übernommen werden"
            } finally {
                busy = false
            }
        }
    }
    return {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            if (failure != null) {
                Text(
                    "Die Projektor-Datei ließ sich nicht laden ($failure). Bilder werden ignoriert.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            if (hasImages) {
                Text(
                    "Dieses Modell ignoriert Bilder, bis ein Projektor gewählt ist.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            OutlinedButton(enabled = !busy, onClick = { picker.launch(arrayOf("*/*")) }) { Text("Projektor wählen") }
            error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
        }
    }
}

/** Button "Bildverständnis laden" für das aktive On-Device-Modell ohne Projektor; lädt, zeigt Fortschritt, aktiviert neu. */
@Composable
internal fun rememberLocalVisionHint(
    hasImages: Boolean,
    refresh: Int,
    onActivated: () -> Unit,
): @Composable () -> Unit {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var tick by remember { mutableIntStateOf(0) }
    var error by remember { mutableStateOf<String?>(null) }
    val download by ModelDownloads.state.collectAsState()
    // Aktives Modell + Add-on; `null` = nichts anzubieten (kein Gerätemodell, kein Add-on, schon geladen).
    val offer by produceState<VisionOffer?>(null, refresh, tick, download) {
        value = withContext(Dispatchers.IO) {
            try {
                val info = ProviderStore(context.filesDir.path, AndroidPlatformKeystore(context)).homeInfo()
                val id = info.activeModel
                if (info.activeProvider != "local-gguf" || id.isNullOrBlank() || localModelSeesImages(context, id)) {
                    null
                } else {
                    val catalogAddon = uniffi.harw_mobile_ffi.visionCatalog().firstOrNull { id in it.modelIds }
                    if (catalogAddon != null || id.startsWith("own-")) VisionOffer(id, catalogAddon) else null
                }
            } catch (_: Exception) {
                null
            }
        }
    }
    val offered = offer ?: return {}
    val current = offered.addon
    if (current == null) {
        return rememberImportedProjectorHint(offered.modelId, hasImages, onActivated)
    }
    val fileId = current.file.id
    var lastFinished by remember { mutableStateOf<ModelDownloadState?>(null) }
    LaunchedEffect(download) {
        val d = download
        if (d is ModelDownloadState.Finished && d.modelId == fileId && d != lastFinished) {
            lastFinished = d
            when (val r = d.result) {
                is DownloadResult.Done -> try {
                    val modelId = current.modelIds.firstOrNull() ?: return@LaunchedEffect
                    val info = withContext(Dispatchers.IO) {
                        ProviderStore(context.filesDir.path, AndroidPlatformKeystore(context)).homeInfo()
                    }
                    val active = info.activeModel?.takeIf { it in current.modelIds } ?: modelId
                    ModelManager(ModelStore(context.applicationContext)).useLocal(context.filesDir.path, active)
                    error = null
                    onActivated()
                } catch (e: Exception) {
                    error = e.message ?: "Modell konnte nicht neu aktiviert werden"
                }
                is DownloadResult.Failed -> error = r.message
                is DownloadResult.NoSpace -> error = "Nicht genug freier Speicher"
                else -> Unit
            }
        }
    }
    fun start() {
        error = null
        when (ModelDownloads.start(context, fileId)) {
            StartResult.Started -> tick++
            StartResult.AlreadyRunning -> error = "Ein Download läuft bereits"
            StartResult.Refused -> error = "Das System hat den Download nicht gestartet"
            StartResult.UnknownModel -> error = "Unbekanntes Modell"
        }
    }
    return {
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            when {
                download is ModelDownloadState.Running && (download as ModelDownloadState.Running).modelId == fileId -> {
                    val run = download as ModelDownloadState.Running
                    if (run.phase == DownloadPhase.Verifying) {
                        LinearProgressIndicator(Modifier.fillMaxWidth())
                        Text("Bildverständnis: prüfe …", style = MaterialTheme.typography.bodySmall)
                    } else {
                        LinearProgressIndicator(progress = { downloadFraction(run.done, run.total) }, modifier = Modifier.fillMaxWidth())
                        Text(
                            "Bildverständnis: lade ${downloadPercent(run.done, run.total)}%",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                download is ModelDownloadState.WaitingForNetwork && (download as ModelDownloadState.WaitingForNetwork).modelId == fileId -> {
                    Text("Bildverständnis: warte auf WLAN", style = MaterialTheme.typography.bodySmall)
                    OutlinedButton(onClick = { start() }) { Text("Fortsetzen") }
                }
                else -> {
                    val failure = current.modelIds.firstNotNullOfOrNull { localVisionFailure(it) }
                    if (failure != null) {
                        Text(
                            "Die Datei für das Bildverständnis ist da, ließ sich aber nicht laden ($failure). Bilder werden ignoriert.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    if (hasImages) {
                        Text(
                            "Dieses Modell ignoriert Bilder, bis das Bildverständnis geladen ist.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    OutlinedButton(onClick = { start() }) {
                        Text("Bildverständnis laden (${gb(current.file.sizeBytes.toLong())})")
                    }
                }
            }
            error?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error) }
        }
    }
}

/** Zweistufige Auswahl für den On-Device-Host: erst Provider, dann dessen Modelle; gilt ab der nächsten Antwort. */
@Composable
internal fun LocalModelSheet(onChanged: () -> Unit, canChange: () -> Boolean = { true }) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val files = context.filesDir.path
    var reload by remember { mutableIntStateOf(0) }
    var error by remember { mutableStateOf<String?>(null) }
    var provider by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    val localId = "local-gguf"
    class Provider(val id: String, val title: String, val defaultModel: String)
    class Option(val id: String, val title: String)
    val overview by produceState<Triple<List<Provider>, String?, String?>?>(null, reload) {
        value = withContext(Dispatchers.IO) {
            try {
                val store = ProviderStore(files, AndroidPlatformKeystore(context))
                val info = store.homeInfo()
                val localModels = ModelManager(ModelStore(context.applicationContext)).local()
                    .filter { it.state == ModelFileState.COMPLETE }
                val cloud = store.routes().filter { it.kind != ProviderKind.LOCAL_GGUF }
                    .map { Provider(it.id, it.id, it.defaultModel) }
                val local = if (localModels.isNotEmpty()) listOf(Provider(localId, "Auf diesem Gerät", "")) else emptyList()
                Triple(local + cloud, info.activeProvider, info.activeModel)
            } catch (e: Exception) {
                error = e.message ?: "Provider konnten nicht gelesen werden"
                Triple(emptyList(), null, null)
            }
        }
    }
    val options by produceState<List<Option>?>(null, provider, reload) {
        val chosen = provider
        value = if (chosen == null) null else withContext(Dispatchers.IO) {
            try {
                if (chosen == localId) {
                    ModelManager(ModelStore(context.applicationContext)).local()
                        .filter { it.state == ModelFileState.COMPLETE }
                        .map { Option(it.id, it.label.ifBlank { it.id }) }
                } else {
                    val store = ProviderStore(files, AndroidPlatformKeystore(context))
                    val default = store.routes().firstOrNull { it.id == chosen }?.defaultModel.orEmpty()
                    val saved = try {
                        store.routeModels(chosen)
                    } catch (_: Exception) {
                        emptyList()
                    }
                    (saved + listOfNotNull(default.takeIf { it.isNotBlank() }))
                        .distinct().map { Option(it, it) }
                }
            } catch (e: Exception) {
                error = e.message ?: "Modelle konnten nicht gelesen werden"
                emptyList()
            }
        }
    }
    Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                if (provider == null) "Provider wählen" else "Modell wählen",
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            if (provider != null) TextButton(onClick = { provider = null; error = null }) { Text("Zurück") }
        }
        Text(
            "Gilt ab der nächsten Antwort.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        error?.let { StatusMessage(it, error = true) }
        val (providers, activeProvider, activeModel) = overview ?: Triple(emptyList(), null, null)
        if (provider == null) {
            if (overview == null) {
                CircularProgressIndicator()
            } else if (providers.isEmpty()) {
                StatusMessage("Keine Provider eingerichtet. Richte sie unter Einstellungen ein.")
            } else {
                LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
                    items(providers, key = { it.id }) { item ->
                        Row(
                            Modifier.fillMaxWidth().clickable { provider = item.id; error = null }.padding(vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(Modifier.weight(1f)) {
                                Text(item.title, style = MaterialTheme.typography.bodyMedium)
                                if (item.id == activeProvider) {
                                    Text(
                                        "aktiv: ${activeModel.orEmpty()}",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.primary,
                                    )
                                }
                            }
                            Text(">")
                        }
                    }
                }
            }
        } else {
            val list = options
            if (list == null || busy) {
                CircularProgressIndicator()
            } else if (list.isEmpty()) {
                StatusMessage("Keine Modelle hinterlegt. Trage sie unter Einstellungen > Provider > Modelle ein.")
            } else {
                val chosen = provider
                LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
                    items(list, key = { it.id }) { option ->
                        val active = chosen == activeProvider && option.id == activeModel
                        Row(
                            Modifier.fillMaxWidth()
                                .selectable(selected = active, role = Role.RadioButton) {
                                    if (!canChange()) return@selectable
                                    scope.launch {
                                        busy = true
                                        try {
                                            withContext(Dispatchers.IO) {
                                                if (chosen == localId) {
                                                    ModelManager(ModelStore(context.applicationContext)).useLocal(files, option.id)
                                                } else {
                                                    ProviderStore(files, AndroidPlatformKeystore(context)).activateRoute(chosen!!, option.id)
                                                }
                                            }
                                            error = null
                                            reload++
                                            onChanged()
                                        } catch (e: Exception) {
                                            error = e.message ?: "Modell konnte nicht gewählt werden"
                                        } finally {
                                            busy = false
                                        }
                                    }
                                }
                                .padding(vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            RadioButton(selected = active, onClick = null)
                            Text(option.title, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                        }
                    }
                }
            }
        }
    }
}
