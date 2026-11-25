package dev.harw.mobile

import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ApiImageEditAsset
import uniffi.harw_mobile_ffi.ApiImageEditChoice
import uniffi.harw_mobile_ffi.ApiImageEditDraft
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ProviderStore

private const val MaxOriginalBytes = 20 * 1024 * 1024
private const val MaxSourcePngBytes = 2 * 1024 * 1024
private const val MaxResultPngBytes = 6 * 1024 * 1024
private const val MaxInstructionBytes = 4 * 1024
private const val ResultEdge = 1024

private class EditPreview(
    val draft: ApiImageEditDraft,
    val bitmap: Bitmap,
    val instruction: String,
    val route: ApiImageEditChoice,
)

private class EditResult(val asset: ApiImageEditAsset, val bitmap: Bitmap, val route: ApiImageEditChoice)
private enum class EditPhase { Ready, Preparing, Sending, Saving }

/** One fresh picker result or one explicitly selected, scope-pinned Library row.
 * Does not read chat media, persist a URI grant, or send to a chat. */
@Composable
internal fun ApiImageEditDialog(
    isCurrentChat: () -> Boolean,
    onClose: () -> Unit,
    library: LibraryApiImageEdit? = null,
    dialogProperties: DialogProperties = DialogProperties(),
) {
    val context = LocalContext.current.applicationContext
    val scope = rememberCoroutineScope()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val isCurrent by rememberUpdatedState(isCurrentChat)
    var store by remember { mutableStateOf<ProviderStore?>(null) }
    var choices by remember { mutableStateOf<List<ApiImageEditChoice>?>(null) }
    var choice by remember { mutableStateOf<ApiImageEditChoice?>(null) }
    var source by remember { mutableStateOf<Uri?>(null) }
    var instruction by remember { mutableStateOf("") }
    var preview by remember { mutableStateOf<EditPreview?>(null) }
    var result by remember { mutableStateOf<EditResult?>(null) }
    var syncApproved by remember { mutableStateOf(false) }
    var attempted by remember { mutableStateOf(false) }
    var phase by remember { mutableStateOf(EditPhase.Ready) }
    var notice by remember { mutableStateOf<String?>(null) }
    var saved by remember { mutableStateOf(false) }
    var librarySaveStarted by remember { mutableStateOf(false) }
    var confirmLibrarySave by remember { mutableStateOf(false) }
    var work by remember { mutableStateOf<Job?>(null) }
    val idle = phase == EditPhase.Ready
    val destination = library?.destination

    // Neither a PIN-vault editor nor a Cloud Library editor may continue via a background UI.
    // Cancel at the lifecycle event, not just after Compose disposes the protected route.
    if (library != null) DisposableEffect(lifecycle, library) {
        val observer = LifecycleEventObserver { _, _ ->
            if (lifecycle.currentState != Lifecycle.State.RESUMED) work?.cancel()
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            work?.cancel()
        }
    }

    val handle = preview?.draft
    DisposableEffect(handle) {
        onDispose { handle?.destroy() }
    }
    val displayedResult = result
    DisposableEffect(displayedResult) {
        onDispose {
            displayedResult?.asset?.png?.fill(0)
        }
    }
    val openedStore = store
    DisposableEffect(openedStore) { onDispose { openedStore?.destroy() } }
    LaunchedEffect(context) {
        var unownedStore: ProviderStore? = null
        try {
            val verified = withContext(Dispatchers.IO) {
                val opened = ProviderStore(context.filesDir.path, AndroidPlatformKeystore(context))
                unownedStore = opened
                opened.apiImageEditChoices()
            }
            if (!isCurrent()) return@LaunchedEffect
            store = unownedStore
            unownedStore = null
            choices = verified
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            choices = emptyList()
            notice = "Die konfigurierten Bildbearbeitungsrouten konnten nicht gelesen werden."
        } finally {
            unownedStore?.destroy()
        }
    }

    val picker = if (library == null) rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri != null && uri.scheme == "content" && phase == EditPhase.Ready &&
            !attempted && preview == null && isCurrent()
        ) {
            source = uri
            notice = null
        }
    } else null
    val export = if (library == null) rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("image/png")) { uri ->
        val availablePng = result?.asset?.png
        if (uri != null && availablePng != null && phase == EditPhase.Ready && isCurrent()) {
            // Keep the writer's copy independent from the preview lifecycle.
            val png = availablePng.copyOf()
            phase = EditPhase.Saving
            notice = null
            saved = false
            work = scope.launch {
                try {
                    withContext(Dispatchers.IO) {
                        checkNotNull(context.contentResolver.openOutputStream(uri, "wt")).use { it.write(png) }
                    }
                    saved = true
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    notice = "Speichern fehlgeschlagen. Die gewählte Datei kann unvollständig sein."
                } finally {
                    png.fill(0)
                    phase = EditPhase.Ready
                }
            }
        }
    } else null

    fun close() {
        if (phase == EditPhase.Saving) return
        work?.cancel()
        onClose()
    }

    fun review() {
        val selected = choice ?: return
        val reviewedStore = store ?: return
        val chosenUri = source
        if (chosenUri == null && library == null) return
        if (phase != EditPhase.Ready || attempted || instruction.isBlank() || !isCurrent()) return
        val approvedInstruction = instruction
        phase = EditPhase.Preparing
        notice = null
        syncApproved = false
        work = scope.launch {
            var unowned: ApiImageEditDraft? = null
            var unownedBitmap: Bitmap? = null
            var originalJpeg: ByteArray? = null
            try {
                library?.readExactJpeg?.invoke { originalJpeg = it }
                if (!isCurrent()) return@launch
                val jpeg = originalJpeg
                val ready = withContext(Dispatchers.IO) {
                    val input = if (library != null) jpeg?.let(::librarySourcePng)
                        else chosenUri?.let { prepareSelectedPng(context, it) }
                    if (input == null) return@withContext null
                    val draft = try {
                        reviewedStore.prepareApiImageEdit(selected, input, approvedInstruction)
                    } finally {
                        input.fill(0)
                    }
                    unowned = draft
                    val previewBytes = draft.previewPng()
                    val bitmap = try {
                        BitmapFactory.decodeByteArray(previewBytes, 0, previewBytes.size)
                    } finally {
                        previewBytes.fill(0)
                    } ?: return@withContext null
                    unownedBitmap = bitmap
                    EditPreview(draft, bitmap, draft.instruction(), selected)
                }
                if (!isCurrent()) return@launch
                if (ready == null) {
                    notice = "Dieses Foto konnte nicht als begrenztes PNG vorbereitet werden."
                } else {
                    preview = ready
                    unowned = null
                    unownedBitmap = null
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                notice = editFailure(error, "Die Bildvorschau konnte nicht vorbereitet werden.")
            } finally {
                unowned?.destroy()
                unownedBitmap?.recycle()
                originalJpeg?.fill(0)
                phase = EditPhase.Ready
            }
        }
    }

    fun send() {
        val approved = preview ?: return
        val reviewedStore = store ?: return
        if (phase != EditPhase.Ready || !syncApproved || attempted || !isCurrent()) return
        phase = EditPhase.Sending
        attempted = true
        notice = null
        work = scope.launch {
            var unownedAsset: ApiImageEditAsset? = null
            var unownedDecoded: EditResult? = null
            try {
                // Recheck source membership at the last in-app boundary before sending the immutable preview.
                library?.requireExactOrigin?.invoke()
                currentCoroutineContext().ensureActive()
                if (!isCurrent()) return@launch
                val asset = reviewedStore.confirmApiImageEdit(approved.draft, explicitlySynchronous = true)
                unownedAsset = asset
                val decoded = withContext(Dispatchers.IO) {
                    decodeEditedAsset(asset, approved.route).also { unownedDecoded = it }
                }
                if (isCurrent()) {
                    result = decoded
                    if (decoded == null) notice = "Das gelieferte Bild konnte nicht angezeigt werden."
                    else {
                        unownedAsset = null
                        unownedDecoded = null
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                notice = editFailure(error, "Bildbearbeitung wurde nicht abgeschlossen. Harw sendet nichts erneut.")
            } finally {
                unownedDecoded?.bitmap?.recycle()
                unownedAsset?.png?.fill(0)
                preview = null
                phase = EditPhase.Ready
            }
        }
    }

    fun saveToLibrary() {
        val destination = library ?: return
        val edited = result ?: return
        if (!confirmLibrarySave || librarySaveStarted || phase != EditPhase.Ready || !isCurrent()) return
        confirmLibrarySave = false
        librarySaveStarted = true
        phase = EditPhase.Saving
        notice = null
        work = scope.launch {
            var jpeg: ByteArray? = null
            try {
                destination.requireExactOrigin()
                val encoded = withContext(Dispatchers.IO) {
                    encodeLibraryEditJpeg(edited.bitmap).also { jpeg = it }
                } ?: error("Result did not fit")
                currentCoroutineContext().ensureActive()
                if (!isCurrent()) return@launch
                destination.saveExactDerivative(edited.asset.contentDigest, encoded)
                if (isCurrent()) saved = true
            } catch (error: CancellationException) {
                throw error
            } catch (_: MobileException.Ambiguous) {
                notice = "Der Speicherstand ist unklar. Lade die Library neu. Harw wiederholt das Speichern nicht."
            } catch (error: PrivateLibraryException) {
                notice = when (error.issue) {
                    PrivateLibraryIssue.Interrupted -> "Der Speicherstand ist unklar. Öffne die private Library erneut. Harw wiederholt das Speichern nicht."
                    PrivateLibraryIssue.Full -> "Die private Library ist voll. Harw wiederholt das Speichern nicht."
                    else -> "Speichern fehlgeschlagen oder die private Library wurde geändert oder gesperrt. Harw wiederholt es nicht."
                }
            } catch (_: Exception) {
                notice = "Speichern fehlgeschlagen oder die gewählte Library wurde geändert. Harw wiederholt es nicht."
            } finally {
                jpeg?.fill(0)
                phase = EditPhase.Ready
            }
        }
    }

    AlertDialog(
        onDismissRequest = ::close,
        properties = dialogProperties,
        title = { Text("Bild bearbeiten") },
        text = {
            Column(
                Modifier.heightIn(max = 520.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                val edited = result
                val review = preview
                when {
                    edited != null -> {
                        Text("Bearbeitetes Bild", style = MaterialTheme.typography.titleSmall)
                        EditBitmap(edited.bitmap, "Bearbeitetes Bild")
                        Text("1024 × 1024 · PNG · neues Ergebnis. Das Original wurde nicht geändert.")
                        Text(
                            "Quelle der Bearbeitung: ${edited.route.routeId} · ${edited.route.modelId} · BLAKE3 ${edited.asset.contentDigest.take(16)}…",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Text("Das Bild bleibt in dieser Vorschau, wenn du es nicht ausdrücklich speicherst.")
                        if (library == null) {
                            TextButton(enabled = idle, onClick = {
                                try {
                                    export?.launch("harw-bild.png")
                                } catch (_: Exception) {
                                    notice = "Android konnte keinen Speicherort öffnen."
                                }
                            }) { Text("PNG speichern unter …") }
                            if (saved) Text("PNG gespeichert.")
                        } else {
                            TextButton(enabled = idle && !librarySaveStarted && isCurrent(), onClick = {
                                confirmLibrarySave = true
                            }) {
                                Text(if (destination == LibraryImageEditDestination.PrivateDevice)
                                    "Als neues Bild privat speichern" else "Als neues Bild in Cloud Library speichern")
                            }
                            if (saved) Text(if (destination == LibraryImageEditDestination.PrivateDevice)
                                "Die Kopie wurde verschlüsselt in dieser privaten Geräte-Library gespeichert."
                            else "Neu gespeichert. Wähle die Kopie bei Bedarf im Chat aus der gleichen Cloud Library.")
                        }
                    }
                    review != null -> {
                        Text("Genau dieses Bild wird gesendet.", style = MaterialTheme.typography.titleSmall)
                        EditBitmap(review.bitmap, "Vorschau des Bilds, das gesendet wird")
                        Text("Auftrag: ${review.instruction}")
                        Text("Anbieter: ${review.route.routeId} · ${review.route.modelId}")
                        Text(review.route.endpoint, style = MaterialTheme.typography.bodySmall)
                        Text(when (destination) {
                            null -> "Nur das Bild und dieser Auftrag gehen über den Provider dieses Geräts an den gewählten Anbieter. Weder Chatinhalte noch Library- oder Gateway-Zugangsdaten werden verwendet."
                            LibraryImageEditDestination.PrivateDevice -> "Bei deiner Bestätigung verlassen nur dieses private Bild und der Auftrag das Gerät über den oben genannten API-Anbieter. Es werden weder PIN, Vault-Schlüssel, andere Bilder noch Chatinhalte geteilt. Harw lädt nichts in eine Cloud Library."
                            is LibraryImageEditDestination.Cloud -> "Nur dieses zuvor aus deiner Cloud Library geladene Bild und der Auftrag gehen über den API-Schlüssel dieses Geräts an den Anbieter. Keine Chatinhalte, weiteren Library-Bilder oder Gateway-Zugangsdaten werden gesendet."
                        })
                        Text("Die Anfrage kann Kosten verursachen. Stoppen holt bereits übertragene Daten nicht zurück.")
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(checked = syncApproved, enabled = idle && !attempted,
                                onCheckedChange = { syncApproved = it })
                            Text("Eine synchrone Einzelanfrage ohne Streaming bestätigen")
                        }
                    }
                    attempted -> Text("Keine automatische Wiederholung und kein Ersatzanbieter.")
                    choices == null -> Text("Bildbearbeitungsrouten werden geprüft …")
                    choices.isNullOrEmpty() -> Text(
                        "Kein passender Anbieter. Hinterlege auf diesem Gerät einen offiziellen OpenAI-API-Schlüssel und füge gpt-image-1.5 zu dessen Modellen hinzu. Diese Funktion nutzt keine benutzerdefinierten Anbieter-Header.",
                    )
                    else -> {
                        Text(when (destination) {
                            null -> "Wähle eine eingerichtete API-Route und genau ein Foto."
                            LibraryImageEditDestination.PrivateDevice ->
                                "Quelle: ${library?.label}. Die Vorschau entsteht in der entsperrten Geräte-Library. Zum Anbieter geht das Bild erst nach deiner gesonderten Bestätigung."
                            is LibraryImageEditDestination.Cloud ->
                                "Quelle: ${library?.label}. Erst wird das gewählte Bild vom Gateway für die Vorschau geladen. Zum Anbieter geht es erst nach deiner gesonderten Bestätigung."
                        })
                        Column(Modifier.selectableGroup()) {
                            choices.orEmpty().forEach { option ->
                                Row(
                                    Modifier.fillMaxWidth().selectable(
                                        selected = choice == option,
                                        onClick = { choice = option; notice = null },
                                        enabled = idle,
                                        role = Role.RadioButton,
                                    ),
                                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    RadioButton(selected = choice == option, onClick = null, enabled = idle)
                                    Column {
                                        Text("${option.routeId} · ${option.modelId}")
                                        Text(option.endpoint, style = MaterialTheme.typography.bodySmall)
                                    }
                                }
                            }
                        }
                        if (library == null) TextButton(enabled = idle, onClick = {
                            try {
                                picker?.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                            } catch (_: Exception) {
                                notice = "Die Android-Fotoauswahl konnte nicht geöffnet werden."
                            }
                        }) { Text(if (source == null) "Foto auswählen" else "Anderes Foto auswählen") }
                        if (source != null) Text("1 Foto ausgewählt.")
                        OutlinedTextField(
                            value = instruction,
                            onValueChange = {
                                if (it.length <= MaxInstructionBytes && it.toByteArray().size <= MaxInstructionBytes) {
                                    instruction = it
                                    notice = null
                                }
                            },
                            enabled = idle,
                            maxLines = 5,
                            label = { Text("Was soll im Bild geändert werden?") },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                }
                if (!idle || choices == null) LinearProgressIndicator(Modifier.fillMaxWidth())
                when (phase) {
                    EditPhase.Preparing -> Text("Vorschau wird nur auf diesem Gerät vorbereitet …")
                    EditPhase.Sending -> Text("Bearbeitung läuft beim gewählten Anbieter …")
                    EditPhase.Saving -> Text(when (destination) {
                        null -> "PNG wird am gewählten Ort gespeichert …"
                        LibraryImageEditDestination.PrivateDevice -> "Die neue Bildkopie wird auf diesem Gerät verschlüsselt gespeichert …"
                        is LibraryImageEditDestination.Cloud -> "Die neue Bildkopie wird in dieser Cloud Library gespeichert …"
                    })
                    EditPhase.Ready -> Unit
                }
                notice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            when {
                phase == EditPhase.Sending -> TextButton(onClick = ::close) { Text("Stoppen") }
                preview != null && !attempted -> TextButton(
                    enabled = idle && syncApproved,
                    onClick = ::send,
                ) { Text("Bild und Auftrag einmal senden") }
                !attempted && !choices.isNullOrEmpty() -> TextButton(
                    enabled = idle && (source != null || library != null) && choice != null && instruction.isNotBlank(),
                    onClick = ::review,
                ) { Text("Vorschau prüfen") }
            }
        },
        dismissButton = {
            if (preview != null && idle && !attempted) {
                TextButton(onClick = {
                    preview = null
                    syncApproved = false
                }) { Text("Zur Auswahl") }
            } else {
                TextButton(onClick = ::close, enabled = phase != EditPhase.Saving) { Text("Schließen") }
            }
        },
    )
    if (confirmLibrarySave && destination != null) AlertDialog(
        onDismissRequest = { confirmLibrarySave = false },
        properties = dialogProperties,
        title = { Text(when (destination) {
            LibraryImageEditDestination.PrivateDevice -> "Neue Bildkopie privat speichern?"
            is LibraryImageEditDestination.Cloud -> "Neue Bildkopie an ${destination.gateway} senden?"
        }) },
        text = { Text(when (destination) {
            LibraryImageEditDestination.PrivateDevice ->
                "Die bearbeitete Kopie wird als JPEG (ohne Transparenz, bei Bedarf verkleinert) getrennt vom Original in dieser Geräte-Library verschlüsselt gespeichert. Beim Speichern wird nichts ins Netz oder an einen Chat gesendet."
            is LibraryImageEditDestination.Cloud ->
                "Die bearbeitete Kopie wird als JPEG (ohne Transparenz, bei Bedarf verkleinert) getrennt vom Original verschlüsselt in dieser Cloud Library gespeichert. Kein Chat wird gesendet."
        }) },
        confirmButton = {
            TextButton(enabled = idle && isCurrent(), onClick = ::saveToLibrary) {
                Text(if (destination == LibraryImageEditDestination.PrivateDevice)
                    "Verschlüsselt speichern" else "Kopie an Gateway senden")
            }
        },
        dismissButton = { TextButton(onClick = { confirmLibrarySave = false }) { Text("Abbrechen") } },
    )
}

@Composable
private fun EditBitmap(bitmap: Bitmap, description: String) {
    val image = remember(bitmap) { bitmap.asImageBitmap() }
    Image(image, description, Modifier.fillMaxWidth().heightIn(max = 220.dp), contentScale = ContentScale.Fit)
}

private fun editFailure(error: Exception, fallback: String): String = when (error) {
    is MobileException.Session -> error.detail
    is MobileException.Busy -> "Die Quelle wurde geändert oder ist belegt. Wähle das Bild erneut. Harw sendet nichts erneut."
    is PrivateLibraryException -> when (error.issue) {
        PrivateLibraryIssue.Locked -> "Die private Library ist gesperrt. Harw sendet nichts erneut."
        PrivateLibraryIssue.Changed, PrivateLibraryIssue.Interrupted ->
            "Die private Library wurde geändert oder unterbrochen. Wähle das Bild erneut."
        else -> fallback
    }
    is MobileException.Disconnected -> "Die Bearbeitung wurde unterbrochen. Harw sendet nichts erneut."
    else -> fallback
}

private fun decodeEditedAsset(asset: ApiImageEditAsset, route: ApiImageEditChoice): EditResult? {
    if (asset.mime != "image/png" || asset.width != ResultEdge.toUInt() || asset.height != ResultEdge.toUInt() ||
        asset.png.isEmpty() || asset.png.size > MaxResultPngBytes
    ) return null
    val bitmap = BitmapFactory.decodeByteArray(asset.png, 0, asset.png.size) ?: return null
    if (bitmap.width != ResultEdge || bitmap.height != ResultEdge) {
        bitmap.recycle()
        return null
    }
    return EditResult(asset, bitmap, route)
}

/** Same Android picker permission as prompt attachments, but preserves PNG transparency and
 * physically bounds provider reads even for open document providers with unknown length. */
private fun prepareSelectedPng(context: Context, uri: Uri): ByteArray? {
    if (uri.scheme != "content") return null
    val original = context.contentResolver.openInputStream(uri)?.use { stream ->
        val out = ByteArrayOutputStream()
        val buffer = ByteArray(16 * 1024)
        while (true) {
            val count = stream.read(buffer)
            if (count < 0) break
            if (count == 0 || count > MaxOriginalBytes - out.size()) return null
            out.write(buffer, 0, count)
        }
        out.toByteArray()
    } ?: return null
    return try { boundedImagePng(original) } finally { original.fill(0) }
}

private fun librarySourcePng(jpeg: ByteArray): ByteArray? =
    jpeg.takeIf { it.size in 4..MaxLibraryJpegBytes }?.let(::boundedImagePng)

private fun boundedImagePng(original: ByteArray): ByteArray? {
    val decoded = run {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(original, 0, original.size, bounds)
        if (bounds.outWidth !in 1..16_384 || bounds.outHeight !in 1..16_384 ||
            bounds.outWidth.toLong() * bounds.outHeight > 80_000_000L ||
            bounds.outMimeType !in setOf("image/jpeg", "image/png")
        ) return null
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / sample > 1536) sample *= 2
        val options = BitmapFactory.Options().apply {
            inSampleSize = sample
            inPreferredConfig = Bitmap.Config.ARGB_8888
        }
        BitmapFactory.decodeByteArray(original, 0, original.size, options)
    } ?: return null
    try {
        val longest = maxOf(decoded.width, decoded.height)
        for (edge in intArrayOf(1280, 1024, 896, 768, 640, 512)) {
            val scaled = if (longest <= edge) decoded else Bitmap.createScaledBitmap(
                decoded,
                (decoded.width.toLong() * edge / longest).toInt().coerceAtLeast(1),
                (decoded.height.toLong() * edge / longest).toInt().coerceAtLeast(1),
                true,
            )
            try {
                val out = EditEncodingBuffer()
                try {
                    if (scaled.compress(Bitmap.CompressFormat.PNG, 100, out) && out.size() in 1..MaxSourcePngBytes) {
                        return out.toByteArray()
                    }
                } finally { out.wipe() }
            } finally {
                if (scaled !== decoded) scaled.recycle()
            }
        }
        return null
    } finally {
        decoded.recycle()
    }
}

private const val MaxLibraryJpegBytes = 180_000

private class EditEncodingBuffer : ByteArrayOutputStream() {
    fun wipe() { buf.fill(0); reset() }
}

private fun encodeLibraryEditJpeg(source: Bitmap): ByteArray? {
    // The native output and the preview are both checked as exact 1024-square PNGs.
    // Keep quality when possible, but high-entropy output must not inflate the Library limit.
    val paint = Paint(Paint.FILTER_BITMAP_FLAG)
    for (edge in intArrayOf(ResultEdge, 768, 512)) {
        val pixels = Bitmap.createBitmap(edge, edge, Bitmap.Config.ARGB_8888)
        try {
            Canvas(pixels).apply {
                drawColor(Color.WHITE)
                drawBitmap(source, null, Rect(0, 0, edge, edge), paint)
            }
            for (quality in intArrayOf(88, 76, 64, 52)) {
                val out = EditEncodingBuffer()
                try {
                    if (pixels.compress(Bitmap.CompressFormat.JPEG, quality, out) &&
                        out.size() in 4..MaxLibraryJpegBytes) return out.toByteArray()
                } finally { out.wipe() }
            }
        } finally { pixels.recycle() }
    }
    return null
}
