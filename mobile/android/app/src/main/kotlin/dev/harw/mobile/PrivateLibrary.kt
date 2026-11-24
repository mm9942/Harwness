package dev.harw.mobile

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.net.Uri
import android.view.WindowManager
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.LocalSaveableStateRegistry
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** An opaque authorisation epoch, never constructed from a path, Intent, or saved UI state. */
internal class PrivateLibraryLease

internal class PrivateLibraryId private constructor(private val opaque: String) {
    override fun equals(other: Any?): Boolean = other is PrivateLibraryId && opaque == other.opaque
    override fun hashCode(): Int = opaque.hashCode()
    internal fun nativeValue(): String = opaque
    companion object {
        internal fun fromNative(value: String): PrivateLibraryId? = value.takeIf {
            it.length == 32 && it.any { ch -> ch != '0' } && it.all { ch -> ch in '0'..'9' || ch in 'a'..'f' }
        }?.let(::PrivateLibraryId)
    }
}

internal sealed interface PrivateLibraryDescription {
    data object Missing : PrivateLibraryDescription
    class Ready(val text: String) : PrivateLibraryDescription
}

internal class PrivateLibraryEntry(
    val id: PrivateLibraryId,
    val name: String,
    val description: PrivateLibraryDescription,
)

internal class PrivateLibrarySnapshot(val revision: ULong, val entries: List<PrivateLibraryEntry>)

internal enum class PrivateLibraryIssue {
    Locked, Changed, Full, InvalidPhoto, Interrupted, Unavailable, LocalModelUnavailable, CaptionIncomplete,
    DescriptionMissing, AssistantDraftUnavailable,
}

internal class PrivateLibraryCaptionChoice(
    val modelId: String,
    val loadedRevision: ULong,
)
internal class PrivateLibraryException(val issue: PrivateLibraryIssue) : Exception()

/** Private RAM only; no ordinary session, raw media, locator URL, or send capability. */
internal class PrivateLibraryAssistantCaption(
    val lease: PrivateLibraryLease,
    val vault: VaultPinScope,
    val text: String,
)

internal fun interface PrivateLibraryCaptionReceiver {
    fun stage(caption: PrivateLibraryAssistantCaption): Boolean
}

/** A scoped RAM-only task input, not a grant to read or export raw media. */
internal class PrivateLibraryAssistantImage(
    val lease: PrivateLibraryLease,
    val vault: VaultPinScope,
    val catalogueRevision: ULong,
    val id: PrivateLibraryId,
    val name: String,
)

internal fun interface PrivateLibraryImageReceiver {
    fun stage(image: PrivateLibraryAssistantImage): Boolean
}

/** Native-issued, single-use opaque asset selection. No raw prompt/media/URL export. */
internal interface PrivateLibraryDraft : AutoCloseable {
    val lease: PrivateLibraryLease
}

/**
 * Only an authenticated native owner's live, local port may implement this.
 * Every operation revalidates the precise lease, profile, key generation and
 * applicable expected revision. No regular Host, cloud, files or plaintext fallback.
 */
internal interface PrivateLibraryPort {
    val lease: PrivateLibraryLease
    val active: StateFlow<Boolean>
    val boundVault: VaultPinScope? get() = null
    val localDraftSupported: Boolean
    val localCaptionSupported: Boolean get() = false
    val privateAssistantCaptionSupported: Boolean get() = false
    /** This exact authenticated port permits explicit read-for-preview and encrypted derivative import. */
    val explicitApiEditSupported: Boolean get() = false

    suspend fun snapshot(): PrivateLibrarySnapshot
    /** Return an owned JPEG for a bounded preview, wiped by the caller. No disk/cache writes. */
    suspend fun preview(revision: ULong, id: PrivateLibraryId): ByteArray?
    /** Borrow bytes only for this call. The Android caller will wipe them. */
    suspend fun add(revision: ULong, name: String, jpeg: ByteArray)
    suspend fun rename(revision: ULong, id: PrivateLibraryId, name: String)
    suspend fun describe(revision: ULong, id: PrivateLibraryId, description: String?)
    suspend fun remove(revision: ULong, id: PrivateLibraryId)
    suspend fun captionChoice(): PrivateLibraryCaptionChoice =
        throw PrivateLibraryException(PrivateLibraryIssue.LocalModelUnavailable)
    /** Image stays native. Returns a confirmed complete caption, never a partial. */
    suspend fun describeWithLocalModel(
        revision: ULong, id: PrivateLibraryId, choice: PrivateLibraryCaptionChoice,
    ): String = throw PrivateLibraryException(PrivateLibraryIssue.LocalModelUnavailable)
    /** One saved text description, resolved by the exact encrypted Library lease and revision. */
    suspend fun readSavedDescriptionForAssistant(
        revision: ULong, id: PrivateLibraryId,
    ): PrivateLibraryAssistantCaption = throw PrivateLibraryException(PrivateLibraryIssue.Unavailable)

    /** Selection only; no send, inference, cloud conversion, or hydration into a normal chat. */
    suspend fun prepareLocalDraft(revision: ULong, ids: List<PrivateLibraryId>): PrivateLibraryDraft
}

/**
 * On success the native vault-local draft surface takes ownership of the grant.
 * False always leaves ownership with the Library. The receiver verifies that
 * the live profile/lease is the same immediately before accepting.
 */
internal fun interface PrivateLibraryDraftReceiver {
    fun adopt(draft: PrivateLibraryDraft): Boolean
}

private const val MaxLibraryJpeg = 180_000
private const val MaxLibraryPickerBytes = 12 * 1024 * 1024
private const val LimitDraftItems = 4

private class LibraryFieldEdit(val revision: ULong, val entry: PrivateLibraryEntry, val rename: Boolean)
private class LibraryRemoval(val revision: ULong, val entry: PrivateLibraryEntry)
private class LibraryAssistantDescription(val revision: ULong, val entry: PrivateLibraryEntry)
private class LibraryAssistantPhoto(val revision: ULong, val entry: PrivateLibraryEntry)
private class PrivateLibraryImageEdit(
    val lease: PrivateLibraryLease,
    val vault: VaultPinScope,
    val revision: ULong,
    val id: PrivateLibraryId,
    val sourceLabel: String,
)
private class LibraryCaptionRequest(
    val revision: ULong,
    val entry: PrivateLibraryEntry,
    val model: PrivateLibraryCaptionChoice,
)
private class PreviewTile(val bitmap: Bitmap)

@Composable
internal fun PrivateLibraryScreen(
    port: PrivateLibraryPort,
    onBack: () -> Unit,
    draftReceiver: PrivateLibraryDraftReceiver?,
    captionReceiver: PrivateLibraryCaptionReceiver? = null,
    assistantImageReceiver: PrivateLibraryImageReceiver? = null,
) {
    val context = LocalContext.current
    val window = remember(context) { context.findActivity()?.window }
    val active by port.active.collectAsState()
    val protected = readScreenshotProtection(context)
    var hasWindow by remember(port.lease, window) { mutableStateOf(false) }
    // The authenticated lease is required either way. Capture opt-out only
    // changes the window flag and does not unlock or share this library.
    DisposableEffect(port.lease, window, protected) {
        val flag = WindowManager.LayoutParams.FLAG_SECURE
        if (protected) window?.addFlags(flag) else window?.clearFlags(flag)
        hasWindow = window != null
        onDispose {}
    }
    if (!active || !hasWindow) {
        Column(Modifier.fillMaxSize()) {
            HarwHeader(title = "Library", onBack = onBack) {}
            Text("Die private Bibliothek ist gesperrt.", Modifier.padding(16.dp))
        }
        return
    }
    CompositionLocalProvider(LocalSaveableStateRegistry provides null) {
        PrivateLibraryUnlocked(port, onBack, draftReceiver, captionReceiver, assistantImageReceiver)
    }
}

@Composable
private fun PrivateLibraryUnlocked(
    port: PrivateLibraryPort,
    onBack: () -> Unit,
    draftReceiver: PrivateLibraryDraftReceiver?,
    captionReceiver: PrivateLibraryCaptionReceiver?,
    assistantImageReceiver: PrivateLibraryImageReceiver?,
) {
    val context = LocalContext.current
    val app = context.applicationContext
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    var snapshot by remember(port.lease) { mutableStateOf<PrivateLibrarySnapshot?>(null) }
    var selected by remember(port.lease) { mutableStateOf(emptySet<PrivateLibraryId>()) }
    var busy by remember(port.lease) { mutableStateOf(false) }
    var issue by remember(port.lease) { mutableStateOf<PrivateLibraryIssue?>(null) }
    var edit by remember(port.lease) { mutableStateOf<LibraryFieldEdit?>(null) }
    var imageEdit by remember(port.lease) { mutableStateOf<PrivateLibraryImageEdit?>(null) }
    var remove by remember(port.lease) { mutableStateOf<LibraryRemoval?>(null) }
    var caption by remember(port.lease) { mutableStateOf<LibraryCaptionRequest?>(null) }
    var captionActive by remember(port.lease) { mutableStateOf<PrivateLibraryId?>(null) }
    var assistantDescription by remember(port.lease) { mutableStateOf<LibraryAssistantDescription?>(null) }
    var assistantPhoto by remember(port.lease) { mutableStateOf<LibraryAssistantPhoto?>(null) }
    val gallery = LocalPrivateLibraryGallery.current
    val pending = gallery?.pending
    val vault = port.boundVault
    val imageEditSupported = port.explicitApiEditSupported && vault != null
    // A protected asset may only be handed to an actual, same-lease private
    // composer. Normal Local chats and Cloud are not fallback receivers.
    val draftSupported = port.localDraftSupported && draftReceiver != null
    val captionToAssistant = port.privateAssistantCaptionSupported && captionReceiver != null && vault != null

    DisposableEffect(lifecycle, port.lease) {
        val observer = LifecycleEventObserver { _, _ ->
            if (lifecycle.currentState != Lifecycle.State.RESUMED) {
                imageEdit = null
                assistantDescription = null
                assistantPhoto = null
            }
        }
        lifecycle.addObserver(observer)
        onDispose { lifecycle.removeObserver(observer) }
    }

    suspend fun synchronize() {
        val next = port.snapshot()
        if (!port.active.value) return
        snapshot = next
        selected = selected.intersect(next.entries.mapTo(HashSet()) { it.id })
    }

    fun perform(block: suspend () -> Unit) {
        if (busy || !port.active.value) return
        busy = true
        issue = null
        scope.launch {
            try {
                block()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: PrivateLibraryException) {
                if (port.active.value) {
                    issue = failure.issue
                    if (failure.issue in setOf(PrivateLibraryIssue.Changed, PrivateLibraryIssue.Interrupted)) {
                        try { synchronize() } catch (cancelled: CancellationException) {
                            throw cancelled
                        } catch (_: Exception) {
                            snapshot = null
                            selected = emptySet()
                        }
                    }
                }
            } catch (_: Exception) {
                if (port.active.value) issue = PrivateLibraryIssue.Unavailable
            } finally {
                busy = false
            }
        }
    }

    LaunchedEffect(port.lease) { perform { synchronize() } }

    // The system picker is upstream of this route. A returned URI reaches
    // this screen only after a fresh same-profile unlock; it is not read until
    // the user accepts and the prior vault generation/revision still match.
    val photo = pending?.takeIf { it.request.vault == vault }
    if (pending != null && photo == null) {
        LaunchedEffect(pending) { gallery?.consume?.invoke(pending) }
    }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Library", onBack = onBack) {}
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(
                enabled = !busy && photo == null && snapshot != null && gallery != null && vault != null,
                onClick = {
                    val revision = snapshot?.revision
                    if (revision != null && vault != null) gallery?.request?.invoke(vault, revision)
                },
            ) { Text("Aus Galerie") }
            if (draftSupported) {
                TextButton(
                    enabled = !busy && photo == null && selected.isNotEmpty(),
                    onClick = {
                        val current = snapshot
                        if (current != null) perform {
                            val ids = current.entries.map { it.id }.filter { it in selected }
                            val draft = port.prepareLocalDraft(current.revision, ids)
                            var adopted = false
                            try {
                                if (port.active.value && draft.lease === port.lease) {
                                    adopted = draftReceiver?.adopt(draft) == true
                                }
                                if (!adopted) throw PrivateLibraryException(PrivateLibraryIssue.Unavailable)
                            } finally {
                                if (!adopted) draft.close()
                            }
                        }
                    },
                ) { Text("In neuen privaten Chat (${selected.size})") }
            }
            TextButton(enabled = !busy && photo == null, onClick = { perform { synchronize() } }) { Text("Aktualisieren") }
        }
        if (!draftSupported && snapshot?.entries?.isNotEmpty() == true) Text(
            if (assistantImageReceiver != null) stringResource(R.string.private_library_assistant_photo_intro)
            else if (captionToAssistant) stringResource(R.string.private_library_assistant_text_only)
            else "Bilder bleiben in der privaten Library. Aktuell ist kein Anhang für den privaten Assistenten möglich.",
            Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            style = MaterialTheme.typography.bodySmall,
        )
        if (!imageEditSupported) Text(
            "API-Bildbearbeitung ist für diese private Geräte-Library nicht verfügbar.",
            Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
            style = MaterialTheme.typography.bodySmall,
        )
        if (busy) CircularProgressIndicator(Modifier.padding(12.dp).size(24.dp))
        issue?.let { Text(issueText(it), Modifier.padding(horizontal = 16.dp, vertical = 4.dp)) }
        val current = snapshot
        if (current?.entries?.isEmpty() == true) {
            Text("Noch keine Bilder in dieser privaten Bibliothek.", Modifier.padding(16.dp))
        }
        if (current != null) {
            LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
                items(current.entries) { entry ->
                    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            if (draftSupported) {
                                val checked = entry.id in selected
                                Checkbox(
                                    checked = checked,
                                    enabled = !busy && photo == null && (checked || selected.size < LimitDraftItems),
                                    onCheckedChange = { wanted ->
                                        selected = if (wanted) selected + entry.id else selected - entry.id
                                    },
                                )
                            }
                            PrivateLibraryThumbnail(port, current.revision, entry.id)
                            Column(Modifier.padding(start = 8.dp)) {
                                Text(entry.name, style = MaterialTheme.typography.titleMedium)
                                when (val caption = entry.description) {
                                    PrivateLibraryDescription.Missing -> Text("Keine Beschreibung")
                                    is PrivateLibraryDescription.Ready -> {
                                        Text("Beschreibung vorhanden", style = MaterialTheme.typography.labelSmall)
                                        Text(caption.text)
                                    }
                                }
                            }
                        }
                        if (captionActive == entry.id) Text("Das lokale Modell erstellt eine Beschreibung …")
                        Row {
                            TextButton(enabled = !busy && photo == null, onClick = { edit = LibraryFieldEdit(current.revision, entry, true) }) {
                                Text("Umbenennen")
                            }
                            TextButton(enabled = !busy && photo == null, onClick = { edit = LibraryFieldEdit(current.revision, entry, false) }) {
                                Text("Beschreibung")
                            }
                            if (port.localCaptionSupported) {
                                TextButton(
                                    enabled = !busy && photo == null,
                                    onClick = {
                                        perform {
                                            val choice = port.captionChoice()
                                            if (!port.active.value) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                                            if (snapshot?.revision != current.revision) {
                                                throw PrivateLibraryException(PrivateLibraryIssue.Changed)
                                            }
                                            caption = LibraryCaptionRequest(current.revision, entry, choice)
                                        }
                                    },
                                ) { Text("Lokal beschreiben") }
                            }
                            TextButton(enabled = !busy && photo == null, onClick = { remove = LibraryRemoval(current.revision, entry) }) {
                                Text("Entfernen")
                            }
                        }
                        if (assistantImageReceiver != null && vault != null) {
                            TextButton(
                                enabled = !busy && photo == null && port.active.value,
                                onClick = {
                                    if (port.active.value && lifecycle.currentState == Lifecycle.State.RESUMED) {
                                        assistantPhoto = LibraryAssistantPhoto(current.revision, entry)
                                    }
                                },
                            ) { Text(stringResource(R.string.private_library_assistant_attach_photo)) }
                        }
                        if (captionToAssistant && entry.description is PrivateLibraryDescription.Ready) {
                            TextButton(
                                enabled = !busy && photo == null && port.active.value,
                                onClick = {
                                    if (port.active.value && lifecycle.currentState == Lifecycle.State.RESUMED) {
                                        assistantDescription = LibraryAssistantDescription(current.revision, entry)
                                    }
                                },
                            ) { Text(stringResource(R.string.private_library_assistant_picture_description)) }
                        }
                        if (imageEditSupported && vault != null) {
                            TextButton(enabled = !busy && photo == null, onClick = {
                                if (port.active.value && lifecycle.currentState == Lifecycle.State.RESUMED) {
                                    imageEdit = PrivateLibraryImageEdit(port.lease, vault, current.revision, entry.id, entry.name)
                                }
                            }) { Text("Mit API bearbeiten") }
                        }
                    }
                }
            }
        }
    }

    assistantPhoto?.let { selection ->
        val receiver = assistantImageReceiver
        if (receiver != null && vault != null) {
            AlertDialog(
                onDismissRequest = { assistantPhoto = null },
                properties = DialogProperties(securePolicy = SecureFlagPolicy.Inherit),
                title = { Text(stringResource(R.string.private_library_assistant_attach_photo)) },
                text = { Text(stringResource(R.string.private_library_assistant_photo_consent, selection.entry.name)) },
                confirmButton = {
                    TextButton(enabled = !busy && snapshot?.revision == selection.revision, onClick = {
                        if (assistantPhoto !== selection || !port.active.value || port.boundVault != vault ||
                            lifecycle.currentState != Lifecycle.State.RESUMED ||
                            snapshot?.revision != selection.revision
                        ) return@TextButton
                        assistantPhoto = null
                        issue = null
                        // A draft carries only this selection; native decrypts only if Send
                        // re-authorizes it against the same catalogue, room and active profile.
                        if (!receiver.stage(PrivateLibraryAssistantImage(
                                port.lease, vault, selection.revision, selection.entry.id, selection.entry.name,
                            ))
                        ) issue = PrivateLibraryIssue.AssistantDraftUnavailable
                    }) { Text(stringResource(R.string.private_library_assistant_stage_photo)) }
                },
                dismissButton = { TextButton(onClick = { assistantPhoto = null }) { Text("Abbrechen") } },
            )
        }
    }

    assistantDescription?.let { selection ->
        fun stillAllowed() =
            assistantDescription === selection && port.active.value &&
                lifecycle.currentState == Lifecycle.State.RESUMED && snapshot?.revision == selection.revision
        AlertDialog(
            onDismissRequest = { if (!busy) assistantDescription = null },
            properties = DialogProperties(securePolicy = SecureFlagPolicy.Inherit),
            title = { Text(stringResource(R.string.private_library_assistant_picture_description)) },
            text = {
                Text(stringResource(R.string.private_library_assistant_description_consent, selection.entry.name))
            },
            confirmButton = {
                TextButton(
                    enabled = !busy,
                    onClick = {
                        if (!stillAllowed()) return@TextButton
                        perform {
                            try {
                                val saved = port.readSavedDescriptionForAssistant(selection.revision, selection.entry.id)
                                currentCoroutineContext().ensureActive()
                                if (!stillAllowed() || saved.lease !== port.lease || saved.vault != port.boundVault) {
                                    throw PrivateLibraryException(PrivateLibraryIssue.Changed)
                                }
                                if (captionReceiver?.stage(saved) != true) {
                                    throw PrivateLibraryException(PrivateLibraryIssue.AssistantDraftUnavailable)
                                }
                            } finally {
                                if (assistantDescription === selection) assistantDescription = null
                            }
                        }
                    },
                ) { Text(stringResource(R.string.private_library_assistant_stage_description)) }
            },
            dismissButton = {
                TextButton(enabled = !busy, onClick = { assistantDescription = null }) { Text("Abbrechen") }
            },
        )
    }

    imageEdit?.let { selection ->
        fun stillAuthorized(): Boolean =
            imageEdit === selection && lifecycle.currentState == Lifecycle.State.RESUMED &&
                port.active.value && port.lease === selection.lease && port.boundVault == selection.vault

        suspend fun requireExactOrigin() {
            currentCoroutineContext().ensureActive()
            if (!stillAuthorized()) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
            val now = port.snapshot()
            if (!stillAuthorized()) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
            if (now.revision != selection.revision || now.entries.none { it.id == selection.id }) {
                throw PrivateLibraryException(PrivateLibraryIssue.Changed)
            }
        }

        val access = remember(selection) {
            LibraryApiImageEdit(
                label = selection.sourceLabel,
                destination = LibraryImageEditDestination.PrivateDevice,
                readExactJpeg = { accept ->
                    if (!stillAuthorized()) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                    withContext(Dispatchers.IO) {
                        // Keep this PNG input's cleanup handle across a cancelled Main handoff.
                        // The native read revalidates the lease, profile/key generation, revision and ID.
                        val jpeg = port.preview(selection.revision, selection.id)
                            ?: throw PrivateLibraryException(PrivateLibraryIssue.InvalidPhoto)
                        accept(jpeg)
                    }
                    if (!stillAuthorized()) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                },
                requireExactOrigin = ::requireExactOrigin,
                saveExactDerivative = { digest, jpeg ->
                    currentCoroutineContext().ensureActive()
                    if (!stillAuthorized()) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                    // Existing native CAS: a fresh asset in this vault, never a replace or fallback.
                    port.add(selection.revision, libraryDerivativeName(selection.id.nativeValue(), digest), jpeg)
                },
            )
        }
        ApiImageEditDialog(
            isCurrentChat = ::stillAuthorized,
            onClose = {
                val refresh = stillAuthorized()
                imageEdit = null
                if (refresh) perform { synchronize() }
            },
            library = access,
            dialogProperties = DialogProperties(securePolicy = SecureFlagPolicy.Inherit),
        )
    }

    if (photo != null && gallery != null) {
        val revisionMatches = snapshot?.revision == photo.request.revision
        AlertDialog(
            onDismissRequest = { gallery.consume(photo) },
            properties = DialogProperties(securePolicy = if (readScreenshotProtection(context)) SecureFlagPolicy.SecureOn else SecureFlagPolicy.SecureOff),
            title = { Text("Ausgewähltes Bild privat speichern?") },
            text = {
                Text(if (snapshot == null || revisionMatches) {
                    "Das Bild wird verschlüsselt in der lokalen Library gespeichert."
                } else "Die Library hat sich geändert. Wähle das Bild erneut aus.")
            },
            confirmButton = {
                TextButton(enabled = !busy && revisionMatches, onClick = {
                    if (port.active.value && gallery.consume(photo)) perform {
                        withContext(Dispatchers.IO) {
                            val jpeg = privateLibraryPhoto(app, photo.uri)
                                ?: throw PrivateLibraryException(PrivateLibraryIssue.InvalidPhoto)
                            try {
                                if (!port.active.value) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                                port.add(photo.request.revision, "Bild", jpeg)
                            } finally { jpeg.fill(0) }
                        }
                        synchronize()
                    }
                }) { Text("Verschlüsselt speichern") }
            },
            dismissButton = { TextButton(onClick = { gallery.consume(photo) }) { Text("Abbrechen") } },
        )
    }

    caption?.let { request ->
        val replaces = request.entry.description is PrivateLibraryDescription.Ready
        AlertDialog(
            onDismissRequest = { caption = null },
            properties = DialogProperties(securePolicy = if (readScreenshotProtection(context)) SecureFlagPolicy.SecureOn else SecureFlagPolicy.SecureOff),
            title = { Text("Bild lokal beschreiben?") },
            text = {
                Text(
                    if (replaces) "Eine vollständige Antwort von ${request.model.modelId} ersetzt deine bisherige Beschreibung."
                    else "Nur ${request.model.modelId} auf diesem Gerät analysiert das Bild. Gespeichert wird nur eine vollständige Beschreibung.",
                )
            },
            confirmButton = {
                TextButton(
                    enabled = !busy && snapshot?.revision == request.revision,
                    onClick = {
                        caption = null
                        perform {
                            captionActive = request.entry.id
                            try {
                                val verified = port.describeWithLocalModel(request.revision, request.entry.id, request.model)
                                currentCoroutineContext().ensureActive()
                                if (!port.active.value) throw PrivateLibraryException(PrivateLibraryIssue.Locked)
                                if (snapshot?.revision != request.revision) {
                                    throw PrivateLibraryException(PrivateLibraryIssue.Changed)
                                }
                                // Same owner token and optimistic revision CAS;
                                // no completion, old value replacement, or retry on partial results.
                                port.describe(request.revision, request.entry.id, verified)
                                synchronize()
                            } finally { captionActive = null }
                        }
                    },
                ) { Text("Lokal beschreiben & speichern") }
            },
            dismissButton = { TextButton(onClick = { caption = null }) { Text("Abbrechen") } },
        )
    }

    edit?.let { change ->
        var text by remember(port.lease, change) {
            mutableStateOf(
                if (change.rename) change.entry.name else
                    (change.entry.description as? PrivateLibraryDescription.Ready)?.text.orEmpty(),
            )
        }
        val limit = if (change.rename) 160 else 4096
        AlertDialog(
            onDismissRequest = { edit = null },
            properties = DialogProperties(securePolicy = if (readScreenshotProtection(context)) SecureFlagPolicy.SecureOn else SecureFlagPolicy.SecureOff),
            title = { Text(if (change.rename) "Bild umbenennen" else "Beschreibung bearbeiten") },
            text = {
                OutlinedTextField(
                    value = text, onValueChange = { if (it.length <= limit) text = it },
                    singleLine = change.rename,
                    supportingText = if (change.rename) null else ({ Text("Leer lassen, um die Beschreibung zu entfernen.") }),
                )
            },
            confirmButton = {
                TextButton(
                    enabled = !busy && (!change.rename || text.isNotBlank()),
                    onClick = {
                        val entered = text.trim()
                        edit = null
                        perform {
                            if (change.rename) port.rename(change.revision, change.entry.id, entered)
                            else port.describe(change.revision, change.entry.id, entered.takeIf(String::isNotEmpty))
                            synchronize()
                        }
                    },
                ) { Text("Speichern") }
            },
            dismissButton = { TextButton(onClick = { edit = null }) { Text("Abbrechen") } },
        )
    }

    remove?.let { removal ->
        AlertDialog(
            onDismissRequest = { remove = null },
            properties = DialogProperties(securePolicy = if (readScreenshotProtection(context)) SecureFlagPolicy.SecureOn else SecureFlagPolicy.SecureOff),
            title = { Text("Bild aus der Library entfernen?") },
            text = { Text("Das Original in deiner Galerie bleibt erhalten.") },
            confirmButton = {
                TextButton(
                    enabled = !busy,
                    onClick = {
                        remove = null
                        perform {
                            port.remove(removal.revision, removal.entry.id)
                            synchronize()
                        }
                    },
                ) { Text("Entfernen") }
            },
            dismissButton = { TextButton(onClick = { remove = null }) { Text("Abbrechen") } },
        )
    }
}

@Composable
private fun PrivateLibraryThumbnail(port: PrivateLibraryPort, revision: ULong, id: PrivateLibraryId) {
    val preview by produceState<PreviewTile?>(null, port.lease, revision, id) {
        try {
            value = withContext(Dispatchers.IO) {
                val bytes = port.preview(revision, id) ?: return@withContext null
                try {
                    if (!port.active.value) null
                    else decodePrivateLibraryPreview(bytes, sampleSize = 8)?.let(::PreviewTile)
                } finally {
                    bytes.fill(0)
                }
            }?.takeIf { port.active.value }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            value = null
        }
    }
    preview?.let { item ->
        Image(
            item.bitmap.asImageBitmap(), "Private Bildvorschau", Modifier.size(64.dp),
            contentScale = ContentScale.Crop,
        )
    }
}

/** Only for authenticated, in-memory private previews. Callers own and wipe bytes. */
internal fun decodePrivateLibraryPreview(jpeg: ByteArray, sampleSize: Int): Bitmap? {
    if (jpeg.size !in 4..MaxLibraryJpeg) return null
    val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
    BitmapFactory.decodeByteArray(jpeg, 0, jpeg.size, bounds)
    if (bounds.outMimeType != "image/jpeg" || bounds.outWidth !in 1..4096 || bounds.outHeight !in 1..4096) return null
    return BitmapFactory.decodeByteArray(
        jpeg, 0, jpeg.size, BitmapFactory.Options().apply { inSampleSize = sampleSize.coerceAtLeast(1) },
    )
}

private fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.takeUnless { it === this }?.findActivity()
    else -> null
}

/** Import image pixels only. No lasting URI permission, EXIF, filename, or original metadata. */
internal fun privateLibraryPhoto(context: Context, uri: Uri): ByteArray? {
    if (uri.scheme != "content") return null
    val raw = try {
        context.contentResolver.openInputStream(uri)?.use { input ->
            val buffer = ByteArray(MaxLibraryPickerBytes + 1)
            var size = 0
            try {
                while (size < buffer.size) {
                    val count = input.read(buffer, size, buffer.size - size)
                    if (count == 0) return null
                    if (count == -1) break
                    size += count
                }
                if (size in 1..MaxLibraryPickerBytes) buffer.copyOf(size) else null
            } finally { buffer.fill(0) }
        }
    } catch (_: Exception) { null } ?: return null
    try {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(raw, 0, raw.size, bounds)
        if (bounds.outWidth !in 1..16_384 || bounds.outHeight !in 1..16_384 ||
            bounds.outWidth.toLong() * bounds.outHeight > 64L * 1024 * 1024
        ) return null
        val longEdge = maxOf(bounds.outWidth, bounds.outHeight)
        var sample = 1
        while (longEdge / sample > 1280) sample *= 2
        val bitmap = BitmapFactory.decodeByteArray(raw, 0, raw.size, BitmapFactory.Options().apply { inSampleSize = sample })
            ?: return null
        try {
            for (quality in intArrayOf(84, 72, 62, 50, 40)) {
                val out = SensitiveLibraryJpeg()
                try {
                    if (bitmap.compress(Bitmap.CompressFormat.JPEG, quality, out) && out.size() in 4..MaxLibraryJpeg) {
                        return out.toByteArray()
                    }
                } finally { out.wipe() }
            }
            return null
        } finally { bitmap.recycle() }
    } finally { raw.fill(0) }
}

/** Same-process transient encoder buffer, never written to a file or normal chat cache. */
private class SensitiveLibraryJpeg : ByteArrayOutputStream(512 * 1024) {
    fun wipe() { buf.fill(0); reset() }
}

@Composable
private fun issueText(issue: PrivateLibraryIssue): String = when (issue) {
    PrivateLibraryIssue.Locked -> "Die Library ist gesperrt."
    PrivateLibraryIssue.Changed -> "Der Inhalt wurde geändert. Bitte wähle erneut."
    PrivateLibraryIssue.Full -> "Die private Library ist voll."
    PrivateLibraryIssue.InvalidPhoto -> "Dieses Bild konnte nicht privat importiert werden."
    PrivateLibraryIssue.Interrupted -> "Der Speichervorgang war unterbrochen. Prüfe den aktualisierten Stand."
    PrivateLibraryIssue.Unavailable -> "Die private Library ist gerade nicht verfügbar."
    PrivateLibraryIssue.LocalModelUnavailable ->
        "Kein passendes geladenes Bildmodell. Lade ein lokales Vision-Modell in den Einstellungen."
    PrivateLibraryIssue.CaptionIncomplete -> "Keine vollständige Beschreibung. Es wurde nichts geändert."
    PrivateLibraryIssue.DescriptionMissing -> stringResource(R.string.private_library_assistant_description_missing)
    PrivateLibraryIssue.AssistantDraftUnavailable -> stringResource(R.string.private_library_assistant_draft_unavailable)
}
