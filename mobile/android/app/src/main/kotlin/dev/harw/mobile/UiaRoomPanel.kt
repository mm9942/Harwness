package dev.harw.mobile

import android.graphics.Bitmap
import androidx.compose.foundation.Image

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Send
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.LocalSaveableStateRegistry
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import kotlinx.coroutines.CancellationException

/**
 * Local assistant view only. The owner supplies the sealed snapshot and commands that enforce
 * native access. No network, coding tools, routing, or storage from this composable.
 *
 * onSubmit must synchronously latch an accepted request or return false. True means admitted,
 * never committed. Do not add transcript text until native returns a durable snapshot.
 */
@Composable
internal fun UiaRoomPanel(
    state: UiaRoomState,
    onRequestUnlock: () -> Unit,
    onRetryOpen: () -> Unit,
    onSubmit: (UiaRoomSend) -> Boolean,
    onRequestCancel: (UiaRoomCancel) -> Unit,
    onDismissIssue: (UiaRoomUnlockLease, UiaRoomWork.Failed) -> Unit,
    modifier: Modifier = Modifier,
    draftState: MutableState<String>? = null,
    imageDraftState: MutableState<UiaRoomImageDraft?>? = null,
    submittedDraftState: MutableState<SubmittedRoomDraft?>? = null,
    loadRoomPhoto: (suspend (UiaRoomPhotoPreviewRequest) -> Bitmap?)? = null,
    roomPhotoAllowed: (UiaRoomPhotoPreviewRequest) -> Boolean = { false },
    transcriptPosition: LazyListState? = null,
    linkedWorkCard: @Composable () -> Unit = {},
    memoryPort: UiaManualMemoryPort? = null,
    memoryAllowed: () -> Boolean = { false },
) {
    val latestState by rememberUpdatedState(state)
    // No subtree widget or editable state can register a Bundle saver with Home/Activity.
    // The owner still must obscure screenshots and clear active state when the vault relocks.
    CompositionLocalProvider(LocalSaveableStateRegistry provides null) {
        Column(
            modifier = modifier.fillMaxSize().imePadding().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text("Assistent", style = MaterialTheme.typography.headlineSmall)
                Text(
                    "Auf diesem Gerät · kein Coding-Chat",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
            }
            when (state) {
                UiaRoomState.Locked -> RoomNotice(
                    title = "Assistent gesperrt",
                    body = "Entsperre Harw, um deinen lokalen Verlauf zu öffnen.",
                    action = "Harw entsperren",
                    onAction = onRequestUnlock,
                    modifier = Modifier.weight(1f),
                    showLock = true,
                )
                UiaRoomState.Opening -> RoomNotice(
                    title = "Lokaler Verlauf wird geöffnet",
                    body = null,
                    modifier = Modifier.weight(1f),
                    showProgress = true,
                )
                is UiaRoomState.Unavailable -> RoomNotice(
                    title = "Lokaler Assistent nicht verfügbar",
                    body = state.issue.label(),
                    action = "Erneut prüfen",
                    onAction = onRetryOpen,
                    modifier = Modifier.weight(1f),
                )
                is UiaRoomState.Open -> key(state.snapshot.lease) {
                    RoomConversation(
                        state = state,
                        send = { draft, image ->
                            val request = prepareUiaRoomSend(state, draft, image)
                            request != null && request.isFor(latestState) && onSubmit(request)
                        },
                        cancel = {
                            val request = prepareUiaRoomCancel(state)
                            if (request != null && request.isFor(latestState)) onRequestCancel(request)
                        },
                        dismissIssue = {
                            val latest = latestState
                            if (latest is UiaRoomState.Open &&
                                latest.work === state.work && latest.work is UiaRoomWork.Failed &&
                                latest.snapshot.lease === state.snapshot.lease
                            ) onDismissIssue(state.snapshot.lease, latest.work)
                        },
                        modifier = Modifier.weight(1f),
                        draftState = draftState,
                        imageDraftState = imageDraftState,
                        submittedDraftState = submittedDraftState,
                        loadRoomPhoto = loadRoomPhoto,
                        roomPhotoAllowed = roomPhotoAllowed,
                        transcriptPosition = transcriptPosition,
                        linkedWorkCard = linkedWorkCard,
                        memoryPort = memoryPort,
                        memoryAllowed = memoryAllowed,
                    )
                }
            }
        }
    }
}

@Composable
private fun RoomNotice(
    title: String,
    body: String?,
    modifier: Modifier = Modifier,
    action: String? = null,
    onAction: () -> Unit = {},
    showLock: Boolean = false,
    showProgress: Boolean = false,
) {
    Box(modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
        Column(
            verticalArrangement = Arrangement.spacedBy(10.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        ) {
            if (showLock) Icon(Icons.Outlined.Lock, contentDescription = null)
            Text(title, style = MaterialTheme.typography.titleMedium)
            body?.let { Text(it, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            if (showProgress) LinearProgressIndicator(Modifier.fillMaxWidth())
            if (action != null) Button(onClick = onAction) { Text(action) }
        }
    }
}

/** Volatile per-unlock submission marker, not a retry queue or saved draft. */
internal class SubmittedRoomDraft(
    snapshot: UiaRoomSnapshot,
    val text: String,
    val image: UiaRoomImageDraft?,
) {
    val lease = snapshot.lease
    val revision = snapshot.revision

    fun wasCommitted(next: UiaRoomSnapshot): Boolean =
        next.lease === lease && next.revision == revision + 1 &&
            next.pairs.lastOrNull()?.let {
                it.user == text.trim() && it.hasLibraryImage == (image != null)
            } == true
}

@Composable
private fun RoomConversation(
    state: UiaRoomState.Open,
    send: (String, UiaRoomImageDraft?) -> Boolean,
    cancel: () -> Unit,
    dismissIssue: () -> Unit,
    modifier: Modifier,
    draftState: MutableState<String>?,
    imageDraftState: MutableState<UiaRoomImageDraft?>?,
    submittedDraftState: MutableState<SubmittedRoomDraft?>?,
    loadRoomPhoto: (suspend (UiaRoomPhotoPreviewRequest) -> Bitmap?)?,
    roomPhotoAllowed: (UiaRoomPhotoPreviewRequest) -> Boolean,
    transcriptPosition: LazyListState?,
    linkedWorkCard: @Composable () -> Unit,
    memoryPort: UiaManualMemoryPort?,
    memoryAllowed: () -> Boolean,
) {
    // Nested under a per-unlock key and no SaveableStateRegistry. Never save this
    // draft to Android state, preferences, clipboard, files, diagnostics, or a ViewModel.
    val privateDraft = remember { mutableStateOf<String>("") }
    val activeDraft: MutableState<String> = draftState ?: privateDraft
    var draft by activeDraft
    val image = imageDraftState?.value
    val privateSubmission = remember { mutableStateOf<SubmittedRoomDraft?>(null) }
    var submitted by (submittedDraftState ?: privateSubmission)
    var photoPreview by remember { mutableStateOf<UiaRoomPhotoPreviewRequest?>(null) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val showingState by rememberUpdatedState(state)
    val photoGate by rememberUpdatedState(roomPhotoAllowed)
    val loadPhoto by rememberUpdatedState(loadRoomPhoto)
    DisposableEffect(lifecycle, state.snapshot.lease) {
        val observer = LifecycleEventObserver { _, _ ->
            if (lifecycle.currentState != Lifecycle.State.RESUMED) photoPreview = null
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }
    fun photoStillAllowed(expected: UiaRoomPhotoPreviewRequest): Boolean =
        photoPreview === expected && lifecycle.currentState == Lifecycle.State.RESUMED &&
            expected.isFor(showingState) && photoGate(expected)

    photoPreview?.let { expected ->
        if (expected.isFor(state) && roomPhotoAllowed(expected)) {
            key(expected) {
                RoomPhotoDialog(
                    load = { if (photoStillAllowed(expected)) loadPhoto?.invoke(expected) else null },
                    stillAllowed = { photoStillAllowed(expected) },
                    onClose = { if (photoPreview === expected) photoPreview = null },
                )
            }
        } else LaunchedEffect(expected) { if (photoPreview === expected) photoPreview = null }
    }
    // Queue admission can precede Android's foreground grant. Preserve the
    // visible draft on denial, Stop or model failure, with no automatic retry.
    val settledCommit = submitted?.wasCommitted(state.snapshot) == true
    LaunchedEffect(state.snapshot, state.work, submitted) {
        val previous = submitted ?: return@LaunchedEffect
        if (previous.wasCommitted(state.snapshot)) {
            imageDraftState?.let { holder ->
                if (previous.image != null && holder.value === previous.image) holder.value = null
            }
            if (draft == previous.text) draft = ""
        }
        if (state.work is UiaRoomWork.Failed || state.snapshot.revision != previous.revision ||
            state.snapshot.lease !== previous.lease
        ) submitted = null
    }
    Column(modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        val pairs = state.snapshot.pairs
        var savedOnly by remember { mutableStateOf(false) }
        var selections by remember(memoryPort) { mutableStateOf<UiaMemorySelections?>(null) }
        var memoryIssue by remember(memoryPort) { mutableStateOf(false) }
        var pending by remember(memoryPort) { mutableStateOf(false) }
        var reload by remember(memoryPort) { mutableStateOf(0) }
        val memoryScope = rememberCoroutineScope()
        val latestSnapshot by rememberUpdatedState(state.snapshot)
        val allowed by rememberUpdatedState(memoryAllowed)
        LaunchedEffect(memoryPort, state.snapshot.revision, reload) {
            if (memoryPort == null || !allowed()) return@LaunchedEffect
            val result = memoryPort.read(state.snapshot.revision)
            if (!allowed()) return@LaunchedEffect
            selections = (result as? UiaMemoryResult.Current)?.selections
            memoryIssue = result !is UiaMemoryResult.Current
        }
        if (memoryPort != null) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                TextButton(onClick = { savedOnly = !savedOnly }, enabled = selections != null) {
                    Text(if (savedOnly) "Alle Nachrichten" else "Gemerkte Stellen")
                }
                if (memoryIssue) TextButton(onClick = { reload += 1 }) { Text("Merkliste neu laden") }
            }
        }
        val visiblePairs = pairs.asReversed().mapIndexedNotNull { index, pair ->
            val ordinal = pairs.size - index
            if (!savedOnly || ordinal in selections?.marked.orEmpty()) ordinal to pair else null
        }
        if (savedOnly && selections != null && visiblePairs.isEmpty()) {
            RoomNotice("Keine gemerkten Stellen", "Markiere einen Gesprächsabschnitt, um ihn hier wiederzufinden.", Modifier.weight(1f))
        } else if (pairs.isEmpty()) {
            RoomNotice(
                title = "Dein persönlicher Chat",
                body = "Schreib eine Nachricht an das lokale Modell.",
                modifier = Modifier.weight(1f),
            )
        } else {
            LazyColumn(
                modifier = Modifier.fillMaxWidth().weight(1f),
                reverseLayout = true,
                state = transcriptPosition ?: rememberLazyListState(),
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                itemsIndexed(visiblePairs, key = { _, entry -> entry.first }) { _, (ordinal, pair) ->
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        RoomBubble(
                            "Du", pair.user, fromUser = true, hasLibraryImage = pair.hasLibraryImage,
                            onOpenPhoto = if (pair.canPreviewLibraryImage && loadRoomPhoto != null) ({
                                val expected = UiaRoomPhotoPreviewRequest(
                                    state.snapshot.lease, state.snapshot.revision, ordinal,
                                )
                                if (lifecycle.currentState == Lifecycle.State.RESUMED &&
                                    expected.isFor(showingState) && photoGate(expected)
                                ) photoPreview = expected
                            }) else null,
                        )
                        RoomBubble("Harw · lokal", pair.reply, fromUser = false)
                        selections?.let { current ->
                            if (memoryPort != null) {
                                val marked = ordinal in current.marked
                                TextButton(
                                    enabled = !pending,
                                    onClick = {
                                        if (pending || !allowed() || latestSnapshot !== state.snapshot) return@TextButton
                                        pending = true
                                        memoryScope.launch {
                                            try {
                                                val result = memoryPort.set(state.snapshot.revision, current, ordinal, !marked)
                                                if (!allowed() || latestSnapshot !== state.snapshot) return@launch
                                                when (result) {
                                                    is UiaMemoryResult.Current -> {
                                                        selections = result.selections
                                                        memoryIssue = false
                                                    }
                                                    UiaMemoryResult.Changed -> reload += 1
                                                    UiaMemoryResult.Unavailable -> memoryIssue = true
                                                }
                                            } finally { pending = false }
                                        }
                                    },
                                ) { Text(if (marked) "Aus Merkliste entfernen" else "Lokal merken") }
                            }
                        }
                    }
                }
            }
        }
        when (val work = state.work) {
            UiaRoomWork.Idle -> Unit
            is UiaRoomWork.Processing -> Column(
                verticalArrangement = Arrangement.spacedBy(3.dp),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Lokales Modell arbeitet …",
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.labelMedium,
                    )
                    TextButton(onClick = cancel) { Text("Abbrechen") }
                }
                LinearProgressIndicator(Modifier.fillMaxWidth())
            }
            is UiaRoomWork.Failed -> Surface(
                shape = RoundedCornerShape(14.dp),
                color = MaterialTheme.colorScheme.errorContainer,
                contentColor = MaterialTheme.colorScheme.onErrorContainer,
                modifier = Modifier.fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.padding(horizontal = 8.dp),
                ) {
                    Text(work.issue.label(), modifier = Modifier.weight(1f))
                    TextButton(onClick = dismissIssue) { Text("Schließen") }
                }
            }
        }

        linkedWorkCard()
        if (image != null) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (image.isFor(state)) stringResource(R.string.private_library_assistant_photo_draft, image.selection.name)
                    else stringResource(R.string.private_library_assistant_photo_changed),
                    modifier = Modifier.weight(1f),
                    style = MaterialTheme.typography.labelMedium,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                TextButton(enabled = state.permitsSend && !settledCommit, onClick = {
                    if (!settledCommit && imageDraftState?.value === image) {
                        submitted = null
                        imageDraftState?.value = null
                    }
                }) { Text(stringResource(R.string.private_library_assistant_remove_photo)) }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedTextField(
                value = draft,
                onValueChange = { if (it.length <= 8 * 1024) draft = it },
                enabled = state.permitsSend && !settledCommit,
                label = { Text("Nachricht an Harw") },
                keyboardOptions = KeyboardOptions(autoCorrectEnabled = false),
                maxLines = 5,
                modifier = Modifier.weight(1f),
            )
            FilledIconButton(
                onClick = {
                    val typed = draft
                    if (!settledCommit && send(typed, image)) {
                        submitted = SubmittedRoomDraft(state.snapshot, typed, image)
                    }
                },
                enabled = state.permitsSend && !settledCommit && uiaRoomAcceptsDraft(draft) &&
                    (image == null || image.isFor(state)),
            ) {
                Icon(Icons.AutoMirrored.Outlined.Send, contentDescription = "Nachricht senden")
            }
        }
    }
}

@Composable
private fun RoomPhotoDialog(
    load: suspend () -> Bitmap?,
    stillAllowed: () -> Boolean,
    onClose: () -> Unit,
) {
    var image by remember { mutableStateOf<Bitmap?>(null) }
    var loaded by remember { mutableStateOf(false) }
    val allowed by rememberUpdatedState(stillAllowed)
    LaunchedEffect(Unit) {
        if (!allowed()) return@LaunchedEffect
        val preview = try {
            load()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: PrivateLibraryException) {
            null
        } catch (_: IllegalStateException) {
            null
        }
        if (!allowed()) {
            preview?.recycle()
            return@LaunchedEffect
        }
        image = preview
        loaded = true
    }
    AlertDialog(
        onDismissRequest = onClose,
        properties = DialogProperties(securePolicy = SecureFlagPolicy.Inherit),
        title = { Text(stringResource(R.string.private_library_assistant_open_photo)) },
        text = {
            val bitmap = image
            when {
                bitmap != null -> Image(
                    bitmap.asImageBitmap(),
                    stringResource(R.string.private_library_assistant_photo_in_turn),
                    Modifier.fillMaxWidth().heightIn(max = 320.dp),
                    contentScale = ContentScale.Fit,
                )
                !loaded -> LinearProgressIndicator(Modifier.fillMaxWidth())
                else -> Text(stringResource(R.string.private_library_assistant_photo_unavailable))
            }
        },
        confirmButton = {
            TextButton(onClick = onClose) { Text(stringResource(R.string.private_library_assistant_close_photo)) }
        },
    )
}

@Composable
private fun RoomBubble(
    author: String,
    text: String,
    fromUser: Boolean,
    hasLibraryImage: Boolean = false,
    onOpenPhoto: (() -> Unit)? = null,
) {
    Box(
        modifier = Modifier.fillMaxWidth(),
        contentAlignment = if (fromUser) Alignment.CenterEnd else Alignment.CenterStart,
    ) {
        Surface(
            color = if (fromUser) MaterialTheme.colorScheme.primaryContainer
            else MaterialTheme.colorScheme.surfaceContainer,
            contentColor = if (fromUser) MaterialTheme.colorScheme.onPrimaryContainer
            else MaterialTheme.colorScheme.onSurface,
            shape = RoundedCornerShape(16.dp),
            modifier = Modifier.fillMaxWidth(.92f),
        ) {
            Column(Modifier.padding(horizontal = 14.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(author, style = MaterialTheme.typography.labelMedium)
                if (hasLibraryImage) Text(
                    stringResource(R.string.private_library_assistant_photo_in_turn),
                    style = MaterialTheme.typography.labelMedium,
                )
                Text(text, style = MaterialTheme.typography.bodyLarge)
                if (onOpenPhoto != null) TextButton(onClick = onOpenPhoto) {
                    Text(stringResource(R.string.private_library_assistant_open_photo))
                }
            }
        }
    }
}

private fun UiaRoomIssue.label(): String = when (this) {
    UiaRoomIssue.Vault -> "Der verschlüsselte Verlauf konnte nicht sicher geöffnet werden."
    UiaRoomIssue.LocalModel -> "Wähle in den Einstellungen ein verfügbares lokales Modell."
    UiaRoomIssue.Reopen -> "Öffne den lokalen Verlauf erneut, damit er auf dem richtigen Stand ist."
    UiaRoomIssue.Reconciliation -> "Der Speicherstatus ist unklar. Prüfe den Verlauf, bevor du erneut sendest."
}

@Composable
private fun UiaRoomRetryable.label(): String = when (this) {
    UiaRoomRetryable.Incomplete -> "Keine vollständige Antwort. Diese Nachricht wurde nicht im Verlauf gespeichert."
    UiaRoomRetryable.Inference -> "Das lokale Modell konnte nicht antworten."
    UiaRoomRetryable.Busy -> "Das lokale Modell ist gerade ausgelastet."
    UiaRoomRetryable.Cancelled -> "Antwort abgebrochen. Die Nachricht wurde nicht im Verlauf gespeichert."
    UiaRoomRetryable.PhotoModel -> stringResource(R.string.private_library_assistant_photo_model_required)
    UiaRoomRetryable.PhotoChanged -> stringResource(R.string.private_library_assistant_photo_changed)
    UiaRoomRetryable.PhotoInvalid -> stringResource(R.string.private_library_assistant_photo_invalid)
    UiaRoomRetryable.Interrupted -> stringResource(R.string.private_library_assistant_turn_interrupted)
    UiaRoomRetryable.ForegroundDenied -> stringResource(R.string.private_assistant_attempt_not_sent)
    UiaRoomRetryable.AttemptUnknown -> stringResource(R.string.private_library_assistant_turn_attempt_unknown)
}
