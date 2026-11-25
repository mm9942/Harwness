package dev.harw.mobile

import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.LocalSaveableStateRegistry
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.SecureFlagPolicy
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ApiVisionChoice
import uniffi.harw_mobile_ffi.CloudLibraryCaptionAccess
import uniffi.harw_mobile_ffi.CloudLibraryGeneratedCaption
import uniffi.harw_mobile_ffi.LocalVisionChoice
import uniffi.harw_mobile_ffi.MobileCloudLibraryAsset
import uniffi.harw_mobile_ffi.MobileCloudLibraryCommand
import uniffi.harw_mobile_ffi.MobileCloudLibraryReply
import uniffi.harw_mobile_ffi.MobileCloudLibraryScope
import uniffi.harw_mobile_ffi.MobileException

private enum class CloudLibraryChange { Rename, Describe, Remove }
private class CloudLibraryEdit(
    val revision: ULong,
    val item: MobileCloudLibraryAsset,
    val change: CloudLibraryChange,
)
private class CloudLibraryPreview(val label: String, val pixels: ImageBitmap)
private class CloudLibraryEditSelection(
    val source: CloudLibraryImageEditIdentity<MobileCloudLibraryScope>,
    val assetLabel: String,
    val stillLive: () -> Boolean,
)
private class CloudLibraryCaptionRequest(
    val revision: ULong,
    val asset: MobileCloudLibraryAsset,
    val access: CloudLibraryCaptionAccess,
    val local: List<LocalVisionChoice>,
    val api: AppApiVisionRouteSet?,
)

private sealed interface CloudLibraryVisionChoice {
    data class Local(val model: LocalVisionChoice) : CloudLibraryVisionChoice
    data class Api(val route: ApiVisionChoice) : CloudLibraryVisionChoice
}

private class CloudLibraryCaptionResult(
    val revision: ULong,
    val asset: MobileCloudLibraryAsset,
    val caption: CloudLibraryGeneratedCaption,
)

// The Activity applies the explicit screenshot preference, including the picker transition.
private val CloudSecureDialog = DialogProperties(securePolicy = SecureFlagPolicy.Inherit)

/** Separate authenticated gateway data surface. The Activity owns the authorized visit and photo picker. */
@Composable
internal fun CloudLibraryScreen(
    connection: HarwConnection,
    device: HarwConnection,
    scope: MobileCloudLibraryScope,
    stillOpen: () -> Boolean,
    server: String,
    photo: CloudLibraryPicked<Uri>?,
    pickerBusy: Boolean,
    onPick: (ULong) -> Unit,
    onConsumePhoto: (CloudLibraryPicked<Uri>) -> Boolean,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    CompositionLocalProvider(LocalSaveableStateRegistry provides null) {
        CloudLibraryAttached(connection, device, scope, stillOpen, server, photo, pickerBusy, onPick, onConsumePhoto, onBack)
    }
}

@Composable
private fun CloudLibraryAttached(
    connection: HarwConnection,
    device: HarwConnection,
    scope: MobileCloudLibraryScope,
    stillOpen: () -> Boolean,
    server: String,
    photo: CloudLibraryPicked<Uri>?,
    pickerBusy: Boolean,
    onPick: (ULong) -> Unit,
    onConsumePhoto: (CloudLibraryPicked<Uri>) -> Boolean,
    onBack: () -> Unit,
) {
    val context = LocalContext.current.applicationContext
    val work = rememberCoroutineScope()
    var snapshot by remember { mutableStateOf<MobileCloudLibraryReply.Snapshot?>(null) }
    var busy by remember { mutableStateOf(false) }
    var issue by remember { mutableStateOf<String?>(null) }
    var edit by remember { mutableStateOf<CloudLibraryEdit?>(null) }
    var imageEdit by remember(scope) { mutableStateOf<CloudLibraryEditSelection?>(null) }
    var preview by remember { mutableStateOf<CloudLibraryPreview?>(null) }
    var captionRequest by remember { mutableStateOf<CloudLibraryCaptionRequest?>(null) }
    var captionResult by remember { mutableStateOf<CloudLibraryCaptionResult?>(null) }

    // These opaque native handles are never saved in Android instance state.
    val accessToRelease = captionRequest?.access
    DisposableEffect(accessToRelease) { onDispose { accessToRelease?.destroy() } }
    val captionToRelease = captionResult?.caption
    DisposableEffect(captionToRelease) { onDispose { captionToRelease?.destroy() } }

    fun captionCurrent(revision: ULong, assetId: String): Boolean =
        stillOpen() && snapshot?.let { data ->
            data.revision == revision && data.assets.any { it.assetId == assetId }
        } == true

    suspend fun requireCaptionCurrent(revision: ULong, assetId: String) {
        currentCoroutineContext().ensureActive()
        if (!captionCurrent(revision, assetId)) throw MobileException.Disconnected()
    }

    suspend fun invoke(command: MobileCloudLibraryCommand): MobileCloudLibraryReply =
        connection.cloudLibrary(scope, command)

    suspend fun refresh() {
        snapshot = invoke(MobileCloudLibraryCommand.List) as? MobileCloudLibraryReply.Snapshot
            ?: throw IllegalStateException("Unexpected Cloud Library result")
    }

    fun perform(workItem: suspend () -> Unit) {
        if (busy) return
        busy = true
        issue = null
        work.launch {
            try {
                workItem()
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: MobileException.Ambiguous) {
                snapshot = null
                issue = "Der Speicherstand ist unklar. Lade diese Library neu. Es wird nichts automatisch wiederholt."
            } catch (_: MobileException.Busy) {
                snapshot = null
                issue = "Die Library wurde geändert oder ist belegt. Lade sie neu und wähle erneut."
            } catch (_: MobileException.Revoked) {
                snapshot = null
                issue = "Der Zugriff auf diese Cloud Library wurde entzogen."
            } catch (_: Exception) {
                issue = "Die Aktion in der Cloud Library ist fehlgeschlagen."
            } finally {
                busy = false
            }
        }
    }

    LaunchedEffect(scope) { perform { refresh() } }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Cloud Library", onBack = onBack) {}
        Text(
            "Gateway: ${server}. Getrennt von deiner lokalen Library. Der Gateway kann hochgeladene Bilder verarbeiten und speichert sie verschlüsselt.",
            Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
            style = MaterialTheme.typography.bodySmall,
        )
        FlowRow(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(enabled = !busy && snapshot != null && !pickerBusy && photo == null, onClick = {
                snapshot?.revision?.let(onPick)
            }) { Text("Bild auswählen") }
            TextButton(enabled = !busy, onClick = { perform { refresh() } }) { Text("Neu laden") }
            if (busy) CircularProgressIndicator(Modifier.padding(8.dp))
        }
        issue?.let { Text(it, Modifier.padding(horizontal = 16.dp)) }
        snapshot?.let { data ->
            if (data.assets.isEmpty()) Text("Noch keine Cloud-Bilder.", Modifier.padding(16.dp))
            LazyColumn(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
                items(data.assets) { asset ->
                    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                        Text(asset.name, style = MaterialTheme.typography.titleMedium)
                        Text(asset.description ?: "Keine Beschreibung", style = MaterialTheme.typography.bodySmall)
                        FlowRow {
                            TextButton(enabled = !busy, onClick = {
                                perform {
                                    val reply = invoke(MobileCloudLibraryCommand.Preview(data.revision, asset.assetId))
                                        as? MobileCloudLibraryReply.Preview ?: error("Wrong preview")
                                    val decoded = withContext(Dispatchers.Default) {
                                        try {
                                            BitmapFactory.decodeByteArray(reply.jpeg, 0, reply.jpeg.size)?.asImageBitmap()
                                        } finally { reply.jpeg.fill(0) }
                                    } ?: error("Invalid preview")
                                    preview = CloudLibraryPreview(asset.name, decoded)
                                }
                            }) { Text("Vorschau") }
                            TextButton(enabled = !busy, onClick = { edit = CloudLibraryEdit(data.revision, asset, CloudLibraryChange.Rename) }) { Text("Umbenennen") }
                            TextButton(enabled = !busy, onClick = { edit = CloudLibraryEdit(data.revision, asset, CloudLibraryChange.Describe) }) { Text("Beschreibung") }
                            TextButton(enabled = !busy, onClick = {
                                perform {
                                    requireCaptionCurrent(data.revision, asset.assetId)
                                    val access = connection.cloudLibraryCaptionAccess(scope, data.revision, asset.assetId)
                                    var retained = false
                                    try {
                                        // Manual selection is explicit; the preferred automatic model
                                        // only affects ordering, never substitutes another model.
                                        val installed = optionalVisionChoices { device.libraryVisionChoices() }.orEmpty()
                                        val prefs = context.getSharedPreferences(UI_PREFS, android.content.Context.MODE_PRIVATE)
                                        val preferred = readLocalVisionPreference(prefs).modelId
                                        val local = installed.sortedByDescending { it.modelId == preferred }
                                        val api = optionalVisionChoices { appApiVisionRoutes(context) }
                                        requireCaptionCurrent(data.revision, asset.assetId)
                                        captionRequest = CloudLibraryCaptionRequest(data.revision, asset, access, local, api)
                                        retained = true
                                    } finally {
                                        if (!retained) access.destroy()
                                    }
                                }
                            }) { Text("Bild beschreiben") }
                            TextButton(enabled = !busy && !pickerBusy && photo == null, onClick = {
                                val started = connection.state.value
                                if (!stillOpen() || !started.online || started.generation == null) return@TextButton
                                imageEdit = CloudLibraryEditSelection(
                                    CloudLibraryImageEditIdentity(scope, data.revision, asset.assetId),
                                    asset.name,
                                ) {
                                    val current = connection.state.value
                                    stillOpen() && current.online && current.profile == started.profile &&
                                        current.generation == started.generation
                                }
                            }) { Text("Mit API bearbeiten") }
                            TextButton(enabled = !busy, onClick = { edit = CloudLibraryEdit(data.revision, asset, CloudLibraryChange.Remove) }) { Text("Entfernen") }
                        }
                    }
                }
            }
        }
    }

    imageEdit?.let { selection ->
        val identity = selection.source
        suspend fun requireExactOrigin() {
            currentCoroutineContext().ensureActive()
            if (!selection.stillLive()) throw MobileException.Disconnected()
            val now = invoke(MobileCloudLibraryCommand.List) as? MobileCloudLibraryReply.Snapshot
                ?: throw MobileException.Disconnected()
            if (!identity.permits(scope, now.revision, now.assets.map { it.assetId }, selection.stillLive())) {
                throw MobileException.Busy("Cloud Library source revision changed")
            }
        }
        val access = remember(selection) {
            LibraryApiImageEdit(
                label = selection.assetLabel,
                destination = LibraryImageEditDestination.Cloud(server),
                readExactJpeg = { accept ->
                    if (!selection.stillLive()) throw MobileException.Disconnected()
                    val reply = invoke(MobileCloudLibraryCommand.Preview(identity.revision, identity.assetId))
                        as? MobileCloudLibraryReply.Preview ?: throw MobileException.Disconnected()
                    if (!selection.stillLive() || reply.assetId != identity.assetId) {
                        reply.jpeg.fill(0)
                        throw MobileException.Disconnected()
                    }
                    accept(reply.jpeg)
                },
                requireExactOrigin = ::requireExactOrigin,
                saveExactDerivative = { digest, jpeg ->
                    // Compare-and-swap against the original source revision, not the current UI selection.
                    if (!selection.stillLive()) throw MobileException.Disconnected()
                    val reply = invoke(MobileCloudLibraryCommand.Import(identity.revision, identity.derivativeName(digest), jpeg))
                    check(reply is MobileCloudLibraryReply.Updated)
                    // The write has already been acknowledged. Refresh only on closing the editor.
                },
            )
        }
        ApiImageEditDialog(
            isCurrentChat = selection.stillLive,
            onClose = {
                imageEdit = null
                if (selection.stillLive()) perform { refresh() }
            },
            library = access,
            dialogProperties = CloudSecureDialog,
        )
    }

    photo?.let { selected ->
        var name by remember(selected) { mutableStateOf("Bild") }
        AlertDialog(
            onDismissRequest = { onConsumePhoto(selected) },
            properties = CloudSecureDialog,
            title = { Text("Bild an ${server} senden?") },
            text = {
                Column {
                    Text(
                        if (snapshot != null && snapshot?.revision != selected.revision) {
                            "Die Cloud Library wurde geändert. Wähle das Bild erneut aus."
                        } else {
                            "Dieses Bild wird an deinen Gateway übertragen und dort verschlüsselt gespeichert. Es wird nicht an einen Chat oder ein KI-Modell gesendet."
                        },
                    )
                    OutlinedTextField(name, { if (it.length <= 160) name = it }, label = { Text("Name") }, singleLine = true)
                }
            },
            confirmButton = {
                TextButton(enabled = !busy && name.isNotBlank() && snapshot?.revision == selected.revision, onClick = {
                    val acceptedName = name.trim()
                    if (onConsumePhoto(selected)) perform {
                        withContext(Dispatchers.IO) {
                            val jpeg = privateLibraryPhoto(context, selected.selected) ?: error("Invalid photo")
                            try {
                                withContext(Dispatchers.Main.immediate) {
                                    invoke(MobileCloudLibraryCommand.Import(selected.revision, acceptedName, jpeg))
                                }
                            } finally { jpeg.fill(0) }
                        }
                        refresh()
                    }
                }) { Text("An Gateway senden") }
            },
            dismissButton = { TextButton(onClick = { onConsumePhoto(selected) }) { Text("Abbrechen") } },
        )
    }

    edit?.let { request ->
        var entered by remember(request) {
            mutableStateOf(if (request.change == CloudLibraryChange.Rename) request.item.name else request.item.description.orEmpty())
        }
        val title = when (request.change) {
            CloudLibraryChange.Rename -> "Cloud-Bild umbenennen"
            CloudLibraryChange.Describe -> "Cloud-Beschreibung ändern"
            CloudLibraryChange.Remove -> "Cloud-Bild entfernen?"
        }
        AlertDialog(
            onDismissRequest = { edit = null },
            properties = CloudSecureDialog,
            title = { Text(title) },
            text = {
                if (request.change == CloudLibraryChange.Remove) Text("Entfernt das Bild nur aus dieser Cloud Library.")
                else {
                    val max = if (request.change == CloudLibraryChange.Rename) 160 else 4096
                    OutlinedTextField(entered, { if (it.length <= max) entered = it }, label = { Text(title) })
                }
            },
            confirmButton = {
                TextButton(
                    enabled = !busy && snapshot?.revision == request.revision && (request.change != CloudLibraryChange.Rename || entered.isNotBlank()),
                    onClick = {
                        val change = when (request.change) {
                            CloudLibraryChange.Rename -> MobileCloudLibraryCommand.Rename(request.revision, request.item.assetId, entered.trim())
                            CloudLibraryChange.Describe -> MobileCloudLibraryCommand.Describe(request.revision, request.item.assetId, entered.trim().ifEmpty { null })
                            CloudLibraryChange.Remove -> MobileCloudLibraryCommand.Remove(request.revision, request.item.assetId)
                        }
                        edit = null
                        perform { invoke(change); refresh() }
                    },
                ) { Text("Bestätigen") }
            },
            dismissButton = { TextButton(onClick = { edit = null }) { Text("Abbrechen") } },
        )
    }

    captionRequest?.let { request ->
        CloudLibraryCaptionChoiceDialog(
            request, busy, canConfirm = captionCurrent(request.revision, request.asset.assetId),
            onClose = { if (!busy) captionRequest = null },
            onConfirm = { chosen ->
                perform {
                    try {
                        requireCaptionCurrent(request.revision, request.asset.assetId)
                        val result = when (chosen) {
                            is CloudLibraryVisionChoice.Local ->
                                connection.cloudLibraryCaptionLocal(request.access, device, chosen.model)
                            is CloudLibraryVisionChoice.Api -> {
                                val fresh = appApiVisionRoutes(context)
                                try {
                                    if (fresh.profileKey != request.api?.profileKey ||
                                        chosen.route !in fresh.choices
                                    ) error("The approved image route changed")
                                    requireCaptionCurrent(request.revision, request.asset.assetId)
                                    connection.cloudLibraryCaptionApi(request.access, fresh.store, chosen.route)
                                } finally {
                                    fresh.store.destroy()
                                }
                            }
                        }
                        var retained = false
                        try {
                            requireCaptionCurrent(request.revision, request.asset.assetId)
                            captionResult = CloudLibraryCaptionResult(request.revision, request.asset, result)
                            retained = true
                        } finally {
                            if (!retained) result.destroy()
                        }
                    } finally {
                        captionRequest = null
                    }
                }
            },
        )
    }
    captionResult?.let { result ->
        val generated = remember(result) { result.caption.text() }
        AlertDialog(
            onDismissRequest = { if (!busy) captionResult = null },
            properties = CloudSecureDialog,
            title = { Text("Beschreibung für ${result.asset.name} speichern?") },
            text = {
                Column(Modifier.heightIn(max = 430.dp).verticalScroll(rememberScrollState())) {
                    Text("Der Text ersetzt erst nach dem Speichern die bisherige Beschreibung in deiner Cloud Library auf ${server}.")
                    Text(generated, Modifier.padding(top = 12.dp))
                }
            },
            confirmButton = {
                TextButton(enabled = !busy && captionCurrent(result.revision, result.asset.assetId), onClick = {
                    perform {
                        try {
                            requireCaptionCurrent(result.revision, result.asset.assetId)
                            connection.saveCloudLibraryCaption(result.caption)
                            if (stillOpen()) refresh()
                        } finally {
                            // Never retry a potentially ambiguous save automatically.
                            captionResult = null
                        }
                    }
                }) { Text("In Cloud Library speichern") }
            },
            dismissButton = { TextButton(enabled = !busy, onClick = { captionResult = null }) { Text("Verwerfen") } },
        )
    }

    preview?.let { shown ->
        AlertDialog(
            onDismissRequest = { preview = null },
            properties = CloudSecureDialog,
            title = { Text(shown.label) },
            text = {
                Image(shown.pixels, "Cloud-Bild", Modifier.fillMaxWidth().heightIn(max = 360.dp), contentScale = ContentScale.Fit)
            },
            confirmButton = { TextButton(onClick = { preview = null }) { Text("Schließen") } },
        )
    }
}


internal suspend fun <T> optionalVisionChoices(load: suspend () -> T): T? =
    try { load() } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }

@Composable
private fun CloudLibraryCaptionChoiceDialog(
    request: CloudLibraryCaptionRequest,
    busy: Boolean,
    canConfirm: Boolean,
    onClose: () -> Unit,
    onConfirm: (CloudLibraryVisionChoice) -> Unit,
) {
    var choice by remember(request) { mutableStateOf<CloudLibraryVisionChoice?>(null) }
    val selected = choice
    val apiChoices = request.api?.choices.orEmpty()
    val apiStore = request.api?.store
    DisposableEffect(apiStore) { onDispose { apiStore?.destroy() } }
    AlertDialog(
        onDismissRequest = onClose,
        properties = CloudSecureDialog,
        title = { Text("Bild beschreiben: ${request.asset.name}") },
        text = {
            LazyColumn(Modifier.heightIn(max = 430.dp)) {
                item {
                    Text("Nur dieses Bild wird von deinem Gateway geladen. Kein Chat und keine anderen Bilder werden verwendet.")
                }
                if (request.local.isEmpty() && apiChoices.isEmpty()) item {
                    Text("Kein passendes lokales Vision-Modell installiert und keine geeignete Bild-API eingerichtet.")
                }
                items(request.local) { model ->
                    val candidate = CloudLibraryVisionChoice.Local(model)
                    TextButton(enabled = !busy, onClick = { choice = candidate }) {
                        Text((if (selected == candidate) "✓ " else "") + "Auf diesem Gerät · ${model.modelId}" +
                            if (model.loadedRevision == 0uL) " · wird geladen" else " · geladen")
                    }
                }
                items(apiChoices) { route ->
                    val candidate = CloudLibraryVisionChoice.Api(route)
                    TextButton(enabled = !busy, onClick = { choice = candidate }) {
                        Text((if (selected == candidate) "✓ " else "") + "Externe API · ${route.routeId} · ${route.modelId}\n${route.endpoint}")
                    }
                }
                if (selected is CloudLibraryVisionChoice.Api) item {
                    Text("Beim Bestätigen wird nur dieses Bild an ${selected.route.endpoint} für das Modell ${selected.route.modelId} gesendet.")
                }
                if (busy) item { CircularProgressIndicator(Modifier.padding(8.dp)) }
            }
        },
        confirmButton = {
            TextButton(enabled = !busy && canConfirm && selected != null, onClick = {
                if (selected != null) onConfirm(selected)
            }) { Text(if (selected is CloudLibraryVisionChoice.Api) "Bild an API senden" else "Lokal beschreiben") }
        },
        dismissButton = { TextButton(enabled = !busy, onClick = onClose) { Text("Abbrechen") } },
    )
}
