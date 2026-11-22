package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import java.text.Normalizer
import uniffi.harw_mobile_ffi.MobileProject

internal const val NoProjectsSupportText =
    "Dieser Host kennt keine Projekte. Die Session bleibt ohne Projekt, der Titel wird aus der Nachricht abgeleitet."

internal const val ServerCreatesNoProjectsText =
    "Der Server legt Projekte nicht selbst an: Ein Projekt entsteht aus einem vorhandenen Ordner mit " +
        "Projektwurzel auf dem Server. Die Session bleibt ohne Projekt."

/** Ordner-/Projektname aus freiem Text: Kleinbuchstaben, Umlaute aufgeloest, Rest als `-`, max. 40 Zeichen. */
internal fun slugify(text: String): String {
    val base = text.trim().lowercase()
        .replace("ä", "ae").replace("ö", "oe").replace("ü", "ue").replace("ß", "ss")
    val plain = Normalizer.normalize(base, Normalizer.Form.NFD).replace(Regex("\\p{M}+"), "")
    return plain.replace(Regex("[^a-z0-9]+"), "-").trim('-').take(40).trim('-')
}

/** Kurzes Thema aus der ersten Nachricht: erste Zeile, Leerraum zusammengezogen, hoechstens 6 Woerter / 50 Zeichen. */
internal fun topicTitle(message: String): String {
    val line = message.lineSequence().map { it.trim() }.firstOrNull { it.isNotEmpty() } ?: return ""
    val words = line.split(Regex("\\s+")).take(6).joinToString(" ")
    return words.take(50).trim().trimEnd('.', ',', ':', ';', '!', '?')
}

/** Umschalter "Lokal | Cloud" am Kopf der Startliste. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BackendSwitch(local: Boolean, enabled: Boolean, onSelect: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    SingleChoiceSegmentedButtonRow(modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp)) {
        SegmentedButton(
            selected = local,
            onClick = { onSelect(true) },
            enabled = enabled,
            shape = SegmentedButtonDefaults.itemShape(0, 2),
        ) { Text("Lokal") }
        SegmentedButton(
            selected = !local,
            onClick = { onSelect(false) },
            enabled = enabled,
            shape = SegmentedButtonDefaults.itemShape(1, 2),
        ) { Text("Cloud") }
    }
}

@Composable
internal fun NewChatFab(enabled: Boolean, onClick: () -> Unit, modifier: Modifier = Modifier) {
    ExtendedFloatingActionButton(
        onClick = { if (enabled) onClick() },
        modifier = modifier,
        icon = { Icon(Icons.Outlined.Add, contentDescription = null) },
        text = { Text("Neuer Chat") },
    )
}

/** Bottom Sheet "Neuer Chat": Ort, Projekt, optionale erste Nachricht. `projectKey == null` = neues Projekt zum Thema. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun NewChatSheet(
    local: Boolean,
    projects: List<MobileProject>,
    initialProjectKey: String?,
    canStart: Boolean,
    busy: Boolean,
    onSwitch: (Boolean) -> Unit,
    onDismiss: () -> Unit,
    onStart: (projectKey: String?, message: String) -> Unit,
) {
    var picked by rememberSaveable { mutableStateOf(initialProjectKey) }
    var message by rememberSaveable { mutableStateOf("") }
    var menu by rememberSaveable { mutableStateOf(false) }
    val real = projects.filter { it.projectKey != NoProjectKey }
    val choice = picked
    val topicMode = choice == null
    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            Modifier.fillMaxWidth().padding(horizontal = 16.dp).padding(bottom = 16.dp).navigationBarsPadding(),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text("Neuer Chat", style = MaterialTheme.typography.titleLarge)
            Text("Ort", style = MaterialTheme.typography.labelLarge)
            BackendSwitch(local = local, enabled = !busy, onSelect = onSwitch, modifier = Modifier.padding(horizontal = 0.dp))
            if (local) {
                Text("Modell", style = MaterialTheme.typography.labelLarge)
                LocalModelChip()
            }
            Text("Projekt", style = MaterialTheme.typography.labelLarge)
            OutlinedButton(onClick = { menu = true }, modifier = Modifier.fillMaxWidth()) {
                Text(real.firstOrNull { it.projectKey == choice }?.displayLabel() ?: "Kein Projekt – neues zum Thema anlegen")
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text("Kein Projekt – neues zum Thema anlegen") },
                    onClick = { picked = null; menu = false },
                )
                for (project in real) {
                    DropdownMenuItem(
                        text = { Text(project.displayLabel()) },
                        onClick = { picked = project.projectKey; menu = false },
                    )
                }
            }
            OutlinedTextField(
                value = message,
                onValueChange = { message = it },
                modifier = Modifier.fillMaxWidth(),
                minLines = 2,
                maxLines = 6,
                label = { Text(if (topicMode) "Erste Nachricht (bestimmt das Thema)" else "Erste Nachricht (optional)") },
            )
            if (topicMode) {
                Text(
                    "Aus der ersten Nachricht wird ein Projektname abgeleitet und die Session dem Projekt zugewiesen.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Button(
                onClick = { onStart(choice, message.trim()) },
                enabled = canStart && !busy && (!topicMode || message.isNotBlank()),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (busy) "Startet …" else "Starten") }
        }
    }
}

/** Dialog "Neues Projekt": fragt einen Namen, legt ihn an oder erklaert, warum der Server das nicht kann. */
@Composable
internal fun NewProjectDialog(
    busy: Boolean,
    onDismiss: () -> Unit,
    onCreate: (String, (String?) -> Unit) -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    var working by rememberSaveable { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Neues Projekt") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it; error = null },
                    singleLine = true,
                    label = { Text("Projektname") },
                    supportingText = { Text("Schlüssel: ${slugify(name).ifBlank { "–" }}") },
                    modifier = Modifier.fillMaxWidth(),
                )
                error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Abbrechen") } },
        confirmButton = {
            Button(
                enabled = !busy && !working && slugify(name).isNotBlank(),
                onClick = {
                    working = true
                    onCreate(name) { failure ->
                        working = false
                        if (failure == null) onDismiss() else error = failure
                    }
                },
            ) { Text("Anlegen") }
        },
    )
}
