package dev.harw.mobile

import android.app.ActivityManager
import android.content.Context
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
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import java.util.Locale
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.CatalogModel
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ModelFileState

private const val BytesPerGb = 1_000_000_000.0
private const val BytesPerMb = 1_000_000L

private fun gb(bytes: Long): String = String.format(Locale.US, "%.1f GB", bytes / BytesPerGb)

private fun downloadFraction(done: Long, total: Long): Float =
    if (total <= 0L) 0f else (done.toFloat() / total.toFloat()).coerceIn(0f, 1f)

private fun stateText(model: CatalogModel, local: LocalModel?): String {
    val size = model.sizeBytes.toLong()
    return when (local?.state ?: ModelFileState.MISSING) {
        ModelFileState.MISSING -> "Not downloaded"
        ModelFileState.PARTIAL -> {
            val percent = (downloadFraction(local?.bytesOnDisk?.toLong() ?: 0L, size) * 100f).toInt()
            "Partial, $percent%"
        }
        ModelFileState.COMPLETE -> "Downloaded"
        ModelFileState.INVALID -> "Invalid file, delete and download again"
    }
}

@Composable
internal fun ModelsScreen(manager: ModelManager, onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val catalog = remember(manager) { manager.catalog() }
    val totalRam = remember(context) {
        val info = ActivityManager.MemoryInfo()
        (context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager)?.getMemoryInfo(info)
        info.totalMem
    }
    var locals by remember { mutableStateOf<List<LocalModel>>(emptyList()) }
    var message by remember { mutableStateOf<String?>(null) }
    var messageIsError by remember { mutableStateOf(false) }
    var activeId by remember { mutableStateOf<String?>(null) }
    var progress by remember { mutableStateOf(0f) }
    var job by remember { mutableStateOf<Job?>(null) }
    var pendingDelete by remember { mutableStateOf<CatalogModel?>(null) }

    suspend fun reload() {
        try {
            locals = manager.local()
        } catch (e: MobileException) {
            message = e.message ?: "Could not read models"
            messageIsError = true
        }
    }

    LaunchedEffect(manager) { reload() }

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
                            manager.delete(target)
                            message = null
                        } catch (e: MobileException) {
                            message = e.message ?: "Could not delete model"
                            messageIsError = true
                        }
                        reload()
                    }
                }) { Text("Delete") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Keep") } },
        )
    }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Models", onBack = onBack) {}
        LazyColumn(
            Modifier.fillMaxSize(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            message?.let { text ->
                item { StatusMessage(text, error = messageIsError) }
            }
            items(catalog, key = { it.id }) { model ->
                val local = locals.firstOrNull { it.id == model.id }
                val downloading = activeId == model.id
                val state = local?.state ?: ModelFileState.MISSING
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
                        Text(
                            "Needs ${gb(model.minRamBytes.toLong())} RAM, device has ${gb(totalRam)}",
                            style = MaterialTheme.typography.bodySmall,
                            color = if (totalRam in 1 until model.minRamBytes.toLong()) {
                                MaterialTheme.colorScheme.error
                            } else {
                                MaterialTheme.colorScheme.onSurfaceVariant
                            },
                        )
                        if (downloading) {
                            LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (downloading) {
                                OutlinedButton(onClick = { job?.cancel() }) { Text("Cancel") }
                            } else {
                                if (state != ModelFileState.COMPLETE) {
                                    Button(
                                        enabled = activeId == null,
                                        onClick = {
                                            activeId = model.id
                                            progress = downloadFraction(local?.bytesOnDisk?.toLong() ?: 0L, model.sizeBytes.toLong())
                                            message = null
                                            job = scope.launch {
                                                try {
                                                    val result = manager.download(model) { done, total ->
                                                        progress = downloadFraction(done, total)
                                                    }
                                                    when (result) {
                                                        is DownloadResult.Done -> {
                                                            message = "${model.label} downloaded"
                                                            messageIsError = false
                                                        }
                                                        is DownloadResult.NoSpace -> {
                                                            message = "Not enough storage: needs ${result.neededBytes / BytesPerMb} MB, " +
                                                                "${result.freeBytes / BytesPerMb} MB free"
                                                            messageIsError = true
                                                        }
                                                        is DownloadResult.Failed -> {
                                                            message = result.message
                                                            messageIsError = true
                                                        }
                                                        DownloadResult.Cancelled -> {
                                                            message = "Download cancelled"
                                                            messageIsError = false
                                                        }
                                                    }
                                                } catch (e: CancellationException) {
                                                    message = "Download cancelled"
                                                    messageIsError = false
                                                } finally {
                                                    activeId = null
                                                    job = null
                                                    scope.launch { reload() }
                                                }
                                            }
                                        },
                                    ) { Text(if (state == ModelFileState.PARTIAL) "Resume" else "Download") }
                                }
                                if (state != ModelFileState.MISSING) {
                                    OutlinedButton(enabled = activeId == null, onClick = { pendingDelete = model }) { Text("Delete") }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
