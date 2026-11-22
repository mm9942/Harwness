package dev.harw.mobile

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.DeviceException
import uniffi.harw_mobile_ffi.SafEntry
import uniffi.harw_mobile_ffi.SafRoot

private const val PREVIEW_MAX_BYTES = 256UL * 1024UL

private fun filesFailure(e: Exception): String = when (e) {
    is DeviceException.TooLarge -> "Die Datei ist zu groß für die Vorschau."
    is DeviceException -> "Zugriff fehlgeschlagen. Der Ordner ist eventuell nicht mehr freigegeben."
    else -> "Der Vorgang ist fehlgeschlagen."
}

private fun humanSize(bytes: ULong): String {
    val b = bytes.toLong()
    return when {
        b < 1024 -> "$b B"
        b < 1024 * 1024 -> "${b / 1024} KB"
        else -> "${b / (1024 * 1024)} MB"
    }
}

/**
 * Dateien: vom Gerät freigegebene Ordner (Storage Access Framework) durchsuchen und Textdateien
 * ansehen. Nichts verlässt das Gerät; der Kern liest nur über [SafStore] in freigegebenen Wurzeln.
 */
@Composable
internal fun FilesScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { SafStore(context.applicationContext) }
    val scope = rememberCoroutineScope()
    var roots by remember { mutableStateOf<List<SafRoot>>(emptyList()) }
    var root by remember { mutableStateOf<SafRoot?>(null) }
    var path by remember { mutableStateOf("") }
    var entries by remember { mutableStateOf<List<SafEntry>>(emptyList()) }
    var message by remember { mutableStateOf<String?>(null) }
    var preview by remember { mutableStateOf<Pair<String, String>?>(null) }
    var pendingRemove by remember { mutableStateOf<SafRoot?>(null) }

    fun reloadRoots() {
        scope.launch {
            try {
                roots = withContext(Dispatchers.IO) { store.roots() }
            } catch (e: Exception) {
                message = filesFailure(e)
            }
        }
    }

    fun openDir(target: SafRoot, dir: String) {
        scope.launch {
            try {
                val listed = withContext(Dispatchers.IO) { store.list(target.id, dir) }
                entries = listed.sortedWith(compareByDescending<SafEntry> { it.isDir }.thenBy { it.name.lowercase() })
                root = target
                path = dir
                message = null
            } catch (e: Exception) {
                message = filesFailure(e)
            }
        }
    }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri: Uri? ->
        if (uri != null) {
            scope.launch {
                try {
                    val label = uri.lastPathSegment?.substringAfterLast(':')?.ifBlank { null } ?: "Ordner"
                    withContext(Dispatchers.IO) { store.addRoot(uri, label) }
                    message = null
                } catch (e: Exception) {
                    message = filesFailure(e)
                }
                reloadRoots()
            }
        }
    }

    LaunchedEffect(Unit) { reloadRoots() }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(
            title = "Dateien",
            onBack = {
                val current = root
                if (current != null && path.isNotEmpty()) {
                    openDir(current, path.substringBeforeLast('/', ""))
                } else if (current != null) {
                    root = null
                } else {
                    onBack()
                }
            },
        ) {}
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            message?.let { StatusMessage(it, error = true) }
            val current = root
            if (current == null) {
                Text(
                    "Gib Ordner dieses Geräts frei, damit harw dort Dateien lesen (und, wenn erlaubt, schreiben) kann. " +
                        "Die Freigabe gilt nur für den gewählten Ordner und lässt sich jederzeit widerrufen.",
                    style = MaterialTheme.typography.bodyMedium,
                )
                Button(onClick = { picker.launch(null) }) { Text("Ordner freigeben") }
                if (roots.isEmpty()) {
                    Text("Noch keine Ordner freigegeben.", style = MaterialTheme.typography.bodySmall)
                }
                roots.forEach { r ->
                    Row(
                        Modifier.fillMaxWidth().clickable { openDir(r, "") }.padding(vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f)) {
                            Text(r.label, style = MaterialTheme.typography.titleSmall)
                            Text(
                                if (r.writable) "Lesen und Schreiben" else "Nur lesen",
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                        TextButton(onClick = { pendingRemove = r }) { Text("Freigabe entfernen") }
                    }
                    HorizontalDivider()
                }
            } else {
                Text("${current.label}/${path}", style = MaterialTheme.typography.titleSmall)
                OutlinedButton(onClick = { openDir(current, path) }) { Text("Aktualisieren") }
                if (path.isNotEmpty()) {
                    TextButton(onClick = { openDir(current, path.substringBeforeLast('/', "")) }) { Text("Übergeordnet") }
                }
                if (entries.isEmpty()) Text("Dieser Ordner ist leer.", style = MaterialTheme.typography.bodySmall)
                entries.forEach { e ->
                    Row(
                        Modifier.fillMaxWidth().clickable {
                            if (e.isDir) {
                                openDir(current, e.path)
                            } else {
                                scope.launch {
                                    try {
                                        val bytes = withContext(Dispatchers.IO) { store.read(current.id, e.path, PREVIEW_MAX_BYTES) }
                                        preview = e.name to bytes.toString(Charsets.UTF_8)
                                    } catch (x: Exception) {
                                        message = filesFailure(x)
                                    }
                                }
                            }
                        }.padding(vertical = 8.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(if (e.isDir) "${e.name}/" else e.name, Modifier.weight(1f))
                        if (!e.isDir) Text(humanSize(e.sizeBytes), style = MaterialTheme.typography.bodySmall)
                    }
                    HorizontalDivider()
                }
            }
        }
    }

    preview?.let { (name, text) ->
        AlertDialog(
            onDismissRequest = { preview = null },
            title = { Text(name) },
            text = { Column(Modifier.verticalScroll(rememberScrollState())) { Text(text) } },
            confirmButton = { TextButton(onClick = { preview = null }) { Text("Schließen") } },
        )
    }
    pendingRemove?.let { target ->
        AlertDialog(
            onDismissRequest = { pendingRemove = null },
            title = { Text("Freigabe entfernen?") },
            text = { Text("${target.label} wird nicht mehr für harw lesbar sein. Die Dateien selbst bleiben unverändert.") },
            confirmButton = {
                TextButton(onClick = {
                    pendingRemove = null
                    store.removeRoot(target.id)
                    if (root?.id == target.id) root = null
                    reloadRoots()
                }) { Text("Entfernen") }
            },
            dismissButton = { TextButton(onClick = { pendingRemove = null }) { Text("Abbrechen") } },
        )
    }
}
