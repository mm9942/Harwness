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

private const val TextNotDownloaded = "Not downloaded"
private const val TextVerifiedModel = "Pinned version, checksum verified"
private const val TextNotChecked = "Downloaded, not checked"
private const val TextInvalid = "Invalid file, delete and download again"
private const val TextUnreadable = "Belongs to an earlier install, re-import"
private const val TextLaterUpdate = "Local execution follows in a later update"
private const val TextImportCancelled = "Import cancelled"
private const val TextOldFileKept =
    "The old file could not be removed and still uses space. Delete it in a file manager."

private fun gb(bytes: Long): String = String.format(Locale.US, "%.1f GB", bytes / BytesPerGb)

private fun downloadFraction(done: Long, total: Long): Float =
    if (total <= 0L) 0f else (done.toFloat() / total.toFloat()).coerceIn(0f, 1f)

private fun noSpaceText(neededBytes: Long, freeBytes: Long): String =
    "Not enough storage: needs ${neededBytes / BytesPerMb} MB, ${freeBytes / BytesPerMb} MB free"

private fun stateText(model: CatalogModel, local: LocalModel?): String =
    when (local?.state ?: ModelFileState.MISSING) {
        ModelFileState.MISSING -> TextNotDownloaded
        ModelFileState.PARTIAL -> {
            val done = local?.bytesOnDisk?.toLong() ?: 0L
            "Partial, ${downloadPercent(done, model.sizeBytes.toLong())}%"
        }
        ModelFileState.COMPLETE ->
            if (local?.verification == ModelVerification.VERIFIED) TextVerifiedModel else TextNotChecked
        ModelFileState.INVALID -> TextInvalid
        ModelFileState.UNREADABLE -> TextUnreadable
    }

private fun ownStateText(local: LocalModel): String = when (local.state) {
    ModelFileState.MISSING -> TextNotDownloaded
    ModelFileState.PARTIAL -> "Partial, ${downloadPercent(local.bytesOnDisk.toLong(), local.sizeBytes.toLong())}%"
    ModelFileState.COMPLETE -> "Ready"
    ModelFileState.INVALID -> TextInvalid
    ModelFileState.UNREADABLE -> TextUnreadable
}

private fun importPhaseText(phase: ImportPhase): String = when (phase) {
    ImportPhase.Copying -> "Copying…"
    ImportPhase.Verifying -> "Verifying…"
    ImportPhase.Replacing -> "Replacing…"
}

/** Message text and error flag for one import result. */
private fun importResultText(result: ImportResult, fromReimport: Boolean): Pair<String, Boolean> = when (result) {
    is ImportResult.Imported -> {
        val base = "${result.model.label} imported"
        if (fromReimport && !result.originalDeleted) "$base\n$TextOldFileKept" to false else base to false
    }
    is ImportResult.AlreadyPresent -> "This model is already on the device" to false
    is ImportResult.Mismatch -> String.format(
        Locale.US,
        "File does not match catalog entry %1\$s (size %2\$s instead of %3\$s)",
        result.label,
        gb(result.actualSize),
        gb(result.expectedSize),
    ) to true
    is ImportResult.NoSpace -> noSpaceText(result.neededBytes, result.freeBytes) to true
    ImportResult.NotGguf -> "Not a GGUF file" to true
    ImportResult.Busy -> "A download is running, try again when it is done" to false
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

    fun show(text: String?, error: Boolean = false) {
        message = text
        messageIsError = error
    }

    suspend fun reload() {
        if (!ready) return
        try {
            locals = manager.local()
        } catch (e: MobileException) {
            show(e.message ?: "Could not read models", error = true)
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

    fun startDownload(modelId: String) {
        when (ModelDownloads.start(context, modelId)) {
            StartResult.Started -> show(null)
            StartResult.AlreadyRunning -> show("A download is already running", error = true)
            StartResult.Refused -> show("The system did not allow the download to start", error = true)
            StartResult.UnknownModel -> show("Unknown model", error = true)
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
                show(e.message ?: "Import failed", error = true)
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
                show("No models found in this folder")
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
                    is DownloadResult.Done -> show("${result.model.label} downloaded. $TextVerifiedModel")
                    is DownloadResult.NoSpace -> show(noSpaceText(result.neededBytes, result.freeBytes), error = true)
                    is DownloadResult.Failed -> show(result.message, error = true)
                    DownloadResult.Cancelled -> show("Download paused")
                    DownloadResult.WaitingForNetwork -> show("Waiting for Wi-Fi")
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
            title = { Text("Delete model?") },
            text = { Text("${target.label} will be removed from this device.") },
            confirmButton = {
                TextButton(onClick = {
                    pendingDelete = null
                    scope.launch {
                        try {
                            manager.delete(target.id)
                            show(null)
                        } catch (e: MobileException) {
                            show(e.message ?: "Could not delete model", error = true)
                        }
                        reload()
                    }
                }) { Text("Delete") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Keep") } },
        )
    }

    val downloadRunning = downloads is ModelDownloadState.Running
    val downloadActive = downloadRunning || downloads is ModelDownloadState.WaitingForNetwork
    val importing = importJob != null
    val busy = downloadRunning || importing
    val ownModels = locals.filter { it.origin == ModelOrigin.IMPORTED }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Models", onBack = onBack) {}
        if (!ready) return@Column
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text("Wi-Fi only", style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
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
                            Text("Re-import models")
                        }
                        OutlinedButton(enabled = !busy, onClick = { fileLauncher.launch(modelImport.importFileIntent()) }) {
                            Text("Import GGUF")
                        }
                    }
                    if (showDocumentsFallback) {
                        OutlinedButton(enabled = !busy, onClick = { treeLauncher.launch(modelImport.documentsTreeIntent()) }) {
                            Text("Choose Documents folder")
                        }
                    }
                }
            }
            if (importing) {
                item {
                    Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            val progress = importProgress
                            Text(progress?.let { importPhaseText(it.phase) } ?: "Copying…", style = MaterialTheme.typography.bodyMedium)
                            if (progress != null && progress.phase == ImportPhase.Copying && progress.total > 0L) {
                                LinearProgressIndicator(
                                    progress = { downloadFraction(progress.done, progress.total) },
                                    modifier = Modifier.fillMaxWidth(),
                                )
                            } else {
                                LinearProgressIndicator(Modifier.fillMaxWidth())
                            }
                            OutlinedButton(onClick = { importJob?.cancel() }) { Text("Cancel") }
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
                                        "Recommended",
                                        Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                                    )
                                }
                            }
                        }
                        Text(gb(model.sizeBytes.toLong()), style = MaterialTheme.typography.bodyMedium)
                        Text(stateText(model, local), style = MaterialTheme.typography.bodyMedium)
                        if (state == ModelFileState.COMPLETE) {
                            Text(
                                TextLaterUpdate,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        Text(
                            "Needs ${gb(model.minRamBytes.toLong())} RAM, device has ${gb(totalRam)}",
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
                                    Text("Verifying…", style = MaterialTheme.typography.bodySmall)
                                } else {
                                    LinearProgressIndicator(
                                        progress = { downloadFraction(current.done, current.total) },
                                        modifier = Modifier.fillMaxWidth(),
                                    )
                                    Text(
                                        "Downloading ${downloadPercent(current.done, current.total)}%",
                                        style = MaterialTheme.typography.bodySmall,
                                    )
                                }
                                OutlinedButton(onClick = { ModelDownloads.pause(context) }) { Text("Pause") }
                            }
                            current is ModelDownloadState.WaitingForNetwork && current.modelId == model.id -> {
                                Text("Waiting for Wi-Fi", style = MaterialTheme.typography.bodySmall)
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(enabled = !importing, onClick = { requestDownload(model.id) }) { Text("Resume") }
                                    OutlinedButton(onClick = { ModelDownloads.pause(context) }) { Text("Pause") }
                                }
                            }
                            current is ModelDownloadState.Paused && current.modelId == model.id -> {
                                Text("Paused", style = MaterialTheme.typography.bodySmall)
                                Button(enabled = !busy, onClick = { requestDownload(model.id) }) { Text("Resume") }
                            }
                            else -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                if (state == ModelFileState.UNREADABLE) {
                                    Button(enabled = !busy, onClick = { treeLauncher.launch(modelImport.reimportTreeIntent()) }) {
                                        Text("Re-import")
                                    }
                                } else if (state != ModelFileState.COMPLETE) {
                                    Button(enabled = !busy, onClick = { requestDownload(model.id) }) {
                                        Text(if (state == ModelFileState.PARTIAL) "Resume" else "Download")
                                    }
                                }
                                if (state != ModelFileState.MISSING && local != null) {
                                    OutlinedButton(enabled = !busy, onClick = { pendingDelete = local }) { Text("Delete") }
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
                        Text("Own model, not verified", style = MaterialTheme.typography.bodyMedium)
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
                        OutlinedButton(enabled = !busy, onClick = { pendingDelete = local }) { Text("Delete") }
                    }
                }
            }
        }
    }
}
