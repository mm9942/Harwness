package dev.harw.mobile

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.CatalogModel
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ModelFileState
import uniffi.harw_mobile_ffi.ModelOrigin
import uniffi.harw_mobile_ffi.ModelVerification

private const val BytesPerGb = 1_000_000_000.0
private const val BytesPerMb = 1_000_000L
private const val ShaPreviewChars = 16
private const val LegacyStorageMaxSdk = 29
private const val NotificationPermissionMinSdk = 33

private const val TextNotDownloaded = "Nicht heruntergeladen"
private const val TextVerifiedModel = "Feste Version, Prüfsumme geprüft"
private const val TextNotChecked = "Heruntergeladen, nicht geprüft"
private const val TextInvalid = "Ungültige Datei, löschen und erneut herunterladen"
private const val TextUnreadable = "Stammt aus einer früheren Installation, erneut importieren"
private const val TextInUse = "Aktiver Provider auf diesem Gerät"
private const val TextUseHint = "Läuft offline im Standalone-Modus"
private const val TextImportCancelled = "Import abgebrochen"
private const val TextOldFileKept =
    "Die alte Datei konnte nicht entfernt werden und belegt noch Speicher. Lösche sie in einem Dateimanager."

private fun gb(bytes: Long): String = String.format(Locale.US, "%.1f GB", bytes / BytesPerGb)

private fun downloadFraction(done: Long, total: Long): Float =
    if (total <= 0L) 0f else (done.toFloat() / total.toFloat()).coerceIn(0f, 1f)

private fun noSpaceText(neededBytes: Long, freeBytes: Long): String =
    "Nicht genug Speicher: benötigt ${neededBytes / BytesPerMb} MB, frei sind ${freeBytes / BytesPerMb} MB"

private fun stateText(model: CatalogModel, local: LocalModel?): String =
    when (local?.state ?: ModelFileState.MISSING) {
        ModelFileState.MISSING -> TextNotDownloaded
        ModelFileState.PARTIAL -> {
            val done = local?.bytesOnDisk?.toLong() ?: 0L
            "Teilweise, ${downloadPercent(done, model.sizeBytes.toLong())}%"
        }
        ModelFileState.COMPLETE ->
            if (local?.verification == ModelVerification.VERIFIED) TextVerifiedModel else TextNotChecked
        ModelFileState.INVALID -> TextInvalid
        ModelFileState.UNREADABLE -> TextUnreadable
    }

private fun ownStateText(local: LocalModel): String = when (local.state) {
    ModelFileState.MISSING -> TextNotDownloaded
    ModelFileState.PARTIAL -> "Teilweise, ${downloadPercent(local.bytesOnDisk.toLong(), local.sizeBytes.toLong())}%"
    ModelFileState.COMPLETE -> "Bereit"
    ModelFileState.INVALID -> TextInvalid
    ModelFileState.UNREADABLE -> TextUnreadable
}

private fun importPhaseText(phase: ImportPhase): String = when (phase) {
    ImportPhase.Copying -> "Kopiere …"
    ImportPhase.Verifying -> "Prüfe …"
    ImportPhase.Replacing -> "Ersetze …"
}

/** Message text and error flag for one import result. */
private fun importResultText(result: ImportResult, fromReimport: Boolean): Pair<String, Boolean> = when (result) {
    is ImportResult.Imported -> {
        val base = "${result.model.label} importiert"
        if (fromReimport && !result.originalDeleted) "$base\n$TextOldFileKept" to false else base to false
    }
    is ImportResult.AlreadyPresent -> "Dieses Modell ist bereits auf dem Gerät" to false
    is ImportResult.Mismatch -> String.format(
        Locale.US,
        "Datei passt nicht zum Katalogeintrag %1\$s (Größe %2\$s statt %3\$s)",
        result.label,
        gb(result.actualSize),
        gb(result.expectedSize),
    ) to true
    is ImportResult.NoSpace -> noSpaceText(result.neededBytes, result.freeBytes) to true
    ImportResult.NotGguf -> "Keine GGUF-Datei" to true
    ImportResult.Busy -> "Ein Download läuft, versuche es danach erneut" to false
    ImportResult.Cancelled -> TextImportCancelled to false
    is ImportResult.Failed -> result.message to true
}

private fun needsStoragePermission(context: Context): Boolean =
    Build.VERSION.SDK_INT <= LegacyStorageMaxSdk &&
        ContextCompat.checkSelfPermission(context, Manifest.permission.WRITE_EXTERNAL_STORAGE) !=
        PackageManager.PERMISSION_GRANTED

private fun needsNotificationPermission(context: Context): Boolean =
    Build.VERSION.SDK_INT >= NotificationPermissionMinSdk &&
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
        PackageManager.PERMISSION_GRANTED

@Composable
internal fun ModelsScreen(manager: ModelManager, onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val catalog = remember(manager) { manager.catalog() }
    val store = remember(context) { ModelStore(context.applicationContext) }
    val modelImport = remember(store) { ModelImport(context.applicationContext, store) }
    val totalRam = remember(context) {
        val info = ActivityManager.MemoryInfo()
        (context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager)?.getMemoryInfo(info)
        info.totalMem
    }
    val downloads by ModelDownloads.state.collectAsState()

    var ready by remember { mutableStateOf(false) }
    var locals by remember { mutableStateOf<List<LocalModel>>(emptyList()) }
    var message by remember { mutableStateOf<String?>(null) }
    var messageIsError by remember { mutableStateOf(false) }
    var pendingDelete by remember { mutableStateOf<LocalModel?>(null) }
    var importJob by remember { mutableStateOf<Job?>(null) }
    var importProgress by remember { mutableStateOf<ImportProgress?>(null) }
    var pendingStartId by remember { mutableStateOf<String?>(null) }
    var showDocumentsFallback by remember { mutableStateOf(false) }
    var expandedSha by remember { mutableStateOf<String?>(null) }
    var wifiOnly by remember { mutableStateOf(true) }
    var activeLocalId by remember { mutableStateOf<String?>(null) }

    fun show(text: String?, error: Boolean = false) {
        message = text
        messageIsError = error
    }

    suspend fun reload() {
        if (!ready) return
        try {
            locals = manager.local()
            activeLocalId = manager.activeLocal(context.filesDir.path)
        } catch (e: MobileException) {
            show(e.message ?: "Modelle konnten nicht gelesen werden", error = true)
        }
    }

    // E5: on API 28/29 the storage permission is settled before the store picks a location.
    val storagePermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        ready = true
    }
    LaunchedEffect(Unit) {
        if (needsStoragePermission(context)) {
            storagePermission.launch(Manifest.permission.WRITE_EXTERNAL_STORAGE)
        } else {
            ready = true
        }
    }

    fun useModel(modelId: String) {
        scope.launch {
            try {
                manager.useLocal(context.filesDir.path, modelId)
                activeLocalId = modelId
                show("Aktiver Provider ist jetzt das Modell auf diesem Gerät")
            } catch (e: MobileException) {
                show(e.message ?: "Modell konnte nicht ausgewählt werden", error = true)
            }
        }
    }

    fun startDownload(modelId: String) {
        when (ModelDownloads.start(context, modelId)) {
            StartResult.Started -> show(null)
            StartResult.AlreadyRunning -> show("Ein Download läuft bereits", error = true)
            StartResult.Refused -> show("Das System hat den Download nicht gestartet", error = true)
            StartResult.UnknownModel -> show("Unbekanntes Modell", error = true)
        }
    }

    // E6: the download starts after the notification prompt, whatever the answer.
    val notificationPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        val id = pendingStartId
        pendingStartId = null
        if (id != null) startDownload(id)
    }

    fun requestDownload(modelId: String) {
        if (needsNotificationPermission(context)) {
            pendingStartId = modelId
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        } else {
            startDownload(modelId)
        }
    }

    fun runImport(block: suspend () -> Unit) {
        if (importJob != null) return
        show(null)
        importProgress = null
        importJob = scope.launch {
            try {
                block()
            } catch (_: CancellationException) {
                show(TextImportCancelled)
            } catch (e: MobileException) {
                show(e.message ?: "Import fehlgeschlagen", error = true)
            } finally {
                importJob = null
                importProgress = null
                scope.launch { reload() }
            }
        }
    }

    fun reimportFrom(treeUri: Uri) {
        runImport {
            val found = modelImport.findForeignModels(treeUri)
            if (found.isEmpty()) {
                show("In diesem Ordner wurden keine Modelle gefunden")
                showDocumentsFallback = true
                return@runImport
            }
            showDocumentsFallback = false
            val lines = mutableListOf<String>()
            var anyError = false
            for (foreign in found) {
                val (text, error) = importResultText(modelImport.reimport(foreign) { importProgress = it }, fromReimport = true)
                lines += text
                anyError = anyError || error
            }
            show(lines.joinToString("\n"), anyError)
        }
    }

    val treeLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val uri = result.data?.data
        if (result.resultCode == Activity.RESULT_OK && uri != null && ready) reimportFrom(uri)
    }
    val fileLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val uri = result.data?.data
        if (result.resultCode == Activity.RESULT_OK && uri != null && ready) {
            runImport {
                val (text, error) = importResultText(
                    modelImport.importDocument(uri) { importProgress = it },
                    fromReimport = false,
                )
                show(text, error)
            }
        }
    }

    LaunchedEffect(ready) {
        if (!ready) return@LaunchedEffect
        wifiOnly = store.wifiOnly()
        reload()
        try {
            modelImport.cleanStaging()
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            // leftovers are retried on the next visit
        }
    }

    LaunchedEffect(downloads, ready) {
        if (!ready) return@LaunchedEffect
        when (val state = downloads) {
            is ModelDownloadState.Finished -> {
                when (val result = state.result) {
                    is DownloadResult.Done -> show("${result.model.label} heruntergeladen. $TextVerifiedModel")
                    is DownloadResult.NoSpace -> show(noSpaceText(result.neededBytes, result.freeBytes), error = true)
                    is DownloadResult.Failed -> show(result.message, error = true)
                    DownloadResult.Cancelled -> show("Download pausiert")
                    DownloadResult.WaitingForNetwork -> show("Warte auf WLAN")
                }
                reload()
            }
            is ModelDownloadState.Paused, is ModelDownloadState.WaitingForNetwork -> reload()
            else -> Unit
        }
    }

    pendingDelete?.let { target ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("Modell löschen?") },
            text = { Text("${target.label} wird von diesem Gerät entfernt.") },
            confirmButton = {
                TextButton(onClick = {
                    pendingDelete = null
                    scope.launch {
                        try {
                            manager.delete(target.id)
                            show(null)
                        } catch (e: MobileException) {
                            show(e.message ?: "Modell konnte nicht gelöscht werden", error = true)
                        }
                        reload()
                    }
                }) { Text("Löschen") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Behalten") } },
        )
    }

    val downloadRunning = downloads is ModelDownloadState.Running
    val downloadActive = downloadRunning || downloads is ModelDownloadState.WaitingForNetwork
    val importing = importJob != null
    val busy = downloadRunning || importing
    val ownModels = locals.filter { it.origin == ModelOrigin.IMPORTED }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Modelle", onBack = onBack) {}
        if (!ready) return@Column
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text("Nur im WLAN", style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
                        Switch(
                            checked = wifiOnly,
                            enabled = !downloadActive,
                            onCheckedChange = {
                                wifiOnly = it
                                store.setWifiOnly(it)
                            },
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        OutlinedButton(enabled = !busy, onClick = { treeLauncher.launch(modelImport.reimportTreeIntent()) }) {
                            Text("Modelle erneut importieren")
                        }
                        OutlinedButton(enabled = !busy, onClick = { fileLauncher.launch(modelImport.importFileIntent()) }) {
                            Text("GGUF importieren")
                        }
                    }
                    if (showDocumentsFallback) {
                        OutlinedButton(enabled = !busy, onClick = { treeLauncher.launch(modelImport.documentsTreeIntent()) }) {
                            Text("Ordner „Dokumente“ wählen")
                        }
                    }
                }
            }
            if (importing) {
                item {
                    Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            val progress = importProgress
                            Text(progress?.let { importPhaseText(it.phase) } ?: "Kopiere …", style = MaterialTheme.typography.bodyMedium)
                            if (progress != null && progress.phase == ImportPhase.Copying && progress.total > 0L) {
                                LinearProgressIndicator(
                                    progress = { downloadFraction(progress.done, progress.total) },
                                    modifier = Modifier.fillMaxWidth(),
                                )
                            } else {
                                LinearProgressIndicator(Modifier.fillMaxWidth())
                            }
                            OutlinedButton(onClick = { importJob?.cancel() }) { Text("Abbrechen") }
                        }
                    }
                }
            }
            message?.let { text ->
                item { StatusMessage(text, error = messageIsError) }
            }
            items(catalog, key = { it.id }) { model ->
                val local = locals.firstOrNull { it.id == model.id }
                val state = local?.state ?: ModelFileState.MISSING
                val current = downloads
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(model.label, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                            if (model.recommended) {
                                Surface(shape = MaterialTheme.shapes.small, color = MaterialTheme.colorScheme.primaryContainer) {
                                    Text(
                                        "Empfohlen",
                                        Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                                    )
                                }
                            }
                        }
                        Text(gb(model.sizeBytes.toLong()), style = MaterialTheme.typography.bodyMedium)
                        if (model.note.isNotEmpty()) {
                            Text(
                                model.note,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Text(stateText(model, local), style = MaterialTheme.typography.bodyMedium)
                        if (state == ModelFileState.COMPLETE) {
                            Text(
                                if (activeLocalId == model.id) TextInUse else TextUseHint,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Text(
                            "Benötigt ${gb(model.minRamBytes.toLong())} RAM, Gerät hat ${gb(totalRam)}",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (totalRam in 1 until model.minRamBytes.toLong()) {
                                MaterialTheme.colorScheme.error
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                        when {
                            current is ModelDownloadState.Running && current.modelId == model.id -> {
                                if (current.phase == DownloadPhase.Verifying) {
                                    LinearProgressIndicator(Modifier.fillMaxWidth())
                                    Text("Prüfe …", style = MaterialTheme.typography.bodySmall)
                                } else {
                                    LinearProgressIndicator(
                                        progress = { downloadFraction(current.done, current.total) },
                                        modifier = Modifier.fillMaxWidth(),
                                    )
                                    Text(
                                        "Lade herunter ${downloadPercent(current.done, current.total)}%",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                }
                                OutlinedButton(onClick = { ModelDownloads.pause(context) }) { Text("Pausieren") }
                            }
                            current is ModelDownloadState.WaitingForNetwork && current.modelId == model.id -> {
                                Text("Warte auf WLAN", style = MaterialTheme.typography.bodySmall)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(enabled = !importing, onClick = { requestDownload(model.id) }) { Text("Fortsetzen") }
                                    OutlinedButton(onClick = { ModelDownloads.pause(context) }) { Text("Pausieren") }
                                }
                            }
                            current is ModelDownloadState.Paused && current.modelId == model.id -> {
                                Text("Pausiert", style = MaterialTheme.typography.bodySmall)
                                Button(enabled = !busy, onClick = { requestDownload(model.id) }) { Text("Fortsetzen") }
                            }
                            else -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                if (state == ModelFileState.UNREADABLE) {
                                    Button(enabled = !busy, onClick = { treeLauncher.launch(modelImport.reimportTreeIntent()) }) {
                                        Text("Erneut importieren")
                                    }
                                } else if (state != ModelFileState.COMPLETE) {
                                    Button(enabled = !busy, onClick = { requestDownload(model.id) }) {
                                        Text(if (state == ModelFileState.PARTIAL) "Fortsetzen" else "Herunterladen")
                                    }
                                } else if (activeLocalId != model.id) {
                                    Button(enabled = !busy, onClick = { useModel(model.id) }) { Text("Verwenden") }
                                }
                                if (state != ModelFileState.MISSING && local != null) {
                                    OutlinedButton(enabled = !busy, onClick = { pendingDelete = local }) { Text("Löschen") }
                                }
                            }
                        }
                    }
                }
            }
            items(ownModels, key = { "own:" + it.id }) { local ->
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(local.label, style = MaterialTheme.typography.titleMedium)
                        Text(gb(local.sizeBytes.toLong()), style = MaterialTheme.typography.bodyMedium)
                        Text("Eigenes Modell, nicht geprüft", style = MaterialTheme.typography.bodyMedium)
                        Text(ownStateText(local), style = MaterialTheme.typography.bodyMedium)
                        local.sha256?.let { sha ->
                            val expanded = expandedSha == local.id
                            Text(
                                "SHA-256 " + if (expanded) sha else sha.take(ShaPreviewChars) + "…",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.clickable { expandedSha = if (expanded) null else local.id },
                            )
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (local.state == ModelFileState.COMPLETE) {
                                if (activeLocalId == local.id) {
                                    Text(TextInUse, style = MaterialTheme.typography.bodySmall)
                                } else {
                                    Button(enabled = !busy, onClick = { useModel(local.id) }) { Text("Verwenden") }
                                }
                            }
                            OutlinedButton(enabled = !busy, onClick = { pendingDelete = local }) { Text("Löschen") }
                        }
                    }
                }
            }
        }
    }
}
