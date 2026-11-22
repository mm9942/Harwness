package dev.harw.mobile

import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
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

private const val PREVIEW_MAX_BYTES = 262144UL
private const val TRANSFER_MAX_BYTES = 8L * 1024 * 1024

/** Offener Texteditor; [sha] ist null bei einer neuen Datei. */
private class EditorFile(val name: String, val path: String, val text: String, val sha: String?, val writable: Boolean)

private fun filesFailure(e: Exception): String = when (e) {
    is DeviceException.TooLarge -> "Die Datei ist zu groß."
    is DeviceException.Exists -> "Eine Datei mit diesem Namen existiert bereits."
    is DeviceException.Failed -> if (e.detail == "conflict") {
        "Die Datei wurde zwischenzeitlich geändert. Schließen und neu öffnen, um nichts zu überschreiben."
    } else {
        "Zugriff fehlgeschlagen. Der Ordner ist eventuell nicht mehr freigegeben."
    }
    is DeviceException -> "Zugriff fehlgeschlagen. Der Ordner ist eventuell nicht mehr freigegeben."
    else -> "Der Vorgang ist fehlgeschlagen."
}

/** Liest höchstens [limit] Bytes (kein readNBytes: erst ab API 33). */
private fun readLimited(stream: java.io.InputStream, limit: Long): ByteArray {
    val out = java.io.ByteArrayOutputStream()
    val buffer = ByteArray(64 * 1024)
    var total = 0L
    while (total < limit) {
        val n = stream.read(buffer, 0, minOf(buffer.size.toLong(), limit - total).toInt())
        if (n < 0) break
        out.write(buffer, 0, n)
        total += n
    }
    return out.toByteArray()
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
    var editor by remember { mutableStateOf<EditorFile?>(null) }
    var editText by remember { mutableStateOf("") }
    var newFileDialog by remember { mutableStateOf(false) }
    var newFileName by remember { mutableStateOf("") }
    var exportEntry by remember { mutableStateOf<SafEntry?>(null) }
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

    val resolver = context.contentResolver

    val importPicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri: Uri? ->
        val target = root
        if (uri != null && target != null) {
            scope.launch {
                try {
                    withContext(Dispatchers.IO) {
                        val name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                            if (it.moveToFirst()) it.getString(0) else null
                        }?.replace('/', '_') ?: "import"
                        val bytes = resolver.openInputStream(uri)?.use { readLimited(it, TRANSFER_MAX_BYTES + 1) }
                            ?: throw DeviceException.Failed("i/o error")
                        if (bytes.size > TRANSFER_MAX_BYTES) throw DeviceException.TooLarge()
                        store.write(target.id, if (path.isEmpty()) name else "$path/$name", bytes)
                    }
                    message = null
                    openDir(target, path)
                } catch (e: Exception) {
                    message = filesFailure(e)
                }
            }
        }
    }

    val exportPicker = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri: Uri? ->
        val target = root
        val entry = exportEntry
        exportEntry = null
        if (uri != null && target != null && entry != null) {
            scope.launch {
                try {
                    withContext(Dispatchers.IO) {
                        val bytes = store.read(target.id, entry.path, TRANSFER_MAX_BYTES.toULong())
                        val out = resolver.openOutputStream(uri, "w") ?: throw DeviceException.Failed("i/o error")
                        out.use { it.write(bytes) }
                    }
                    message = null
                } catch (e: Exception) {
                    message = filesFailure(e)
                }
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
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { openDir(current, path) }) { Text("Aktualisieren") }
                    if (current.writable) {
                        OutlinedButton(onClick = { importPicker.launch(arrayOf("*/*")) }) { Text("Importieren") }
                        OutlinedButton(onClick = {
                            newFileName = ""
                            newFileDialog = true
                        }) { Text("Neue Textdatei") }
                    }
                }
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
                                        val text = bytes.toString(Charsets.UTF_8)
                                        editText = text
                                        editor = EditorFile(e.name, e.path, text, sha256Hex(bytes), current.writable)
                                    } catch (x: Exception) {
                                        message = filesFailure(x)
                                    }
                                }
                            }
                        }.padding(vertical = 8.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                    ) {
                        Text(if (e.isDir) "${e.name}/" else e.name, Modifier.weight(1f))
                        if (!e.isDir) {
                            Text(humanSize(e.sizeBytes), style = MaterialTheme.typography.bodySmall)
                            TextButton(onClick = {
                                exportEntry = e
                                exportPicker.launch(e.name)
                            }) { Text("Exportieren") }
                        }
                    }
                    HorizontalDivider()
                }
            }
        }
    }

    editor?.let { file ->
        val canSave = file.writable && editText.toByteArray(Charsets.UTF_8).size <= PREVIEW_MAX_BYTES.toInt()
        AlertDialog(
            onDismissRequest = { editor = null },
            title = { Text(file.name) },
            text = {
                OutlinedTextField(
                    value = editText,
                    onValueChange = { editText = it },
                    readOnly = !file.writable,
                    label = { Text(if (file.writable) "Text bearbeiten (bis 256 KB)" else "Nur lesen") },
                    modifier = Modifier.fillMaxWidth().heightIn(min = 160.dp, max = 360.dp),
                )
            },
            confirmButton = {
                if (file.writable) {
                    TextButton(
                        enabled = canSave,
                        onClick = {
                            val current = root
                            val bytes = editText.toByteArray(Charsets.UTF_8)
                            if (current != null) {
                                scope.launch {
                                    try {
                                        withContext(Dispatchers.IO) {
                                            if (file.sha == null) {
                                                store.write(current.id, file.path, bytes)
                                            } else {
                                                store.replace(current.id, file.path, bytes, file.sha)
                                            }
                                        }
                                        editor = null
                                        message = null
                                        openDir(current, path)
                                    } catch (x: Exception) {
                                        editor = null
                                        message = filesFailure(x)
                                    }
                                }
                            }
                        },
                    ) { Text("Speichern") }
                }
            },
            dismissButton = { TextButton(onClick = { editor = null }) { Text("Schließen") } },
        )
    }
    if (newFileDialog) {
        val trimmed = newFileName.trim()
        val valid = trimmed.isNotEmpty() && '/' !in trimmed && trimmed != "." && trimmed != ".."
        AlertDialog(
            onDismissRequest = { newFileDialog = false },
            title = { Text("Neue Textdatei") },
            text = {
                OutlinedTextField(
                    value = newFileName,
                    onValueChange = { newFileName = it },
                    label = { Text("Dateiname") },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(
                    enabled = valid,
                    onClick = {
                        newFileDialog = false
                        editText = ""
                        editor = EditorFile(trimmed, if (path.isEmpty()) trimmed else "$path/$trimmed", "", null, true)
                    },
                ) { Text("Anlegen") }
            },
            dismissButton = { TextButton(onClick = { newFileDialog = false }) { Text("Abbrechen") } },
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
