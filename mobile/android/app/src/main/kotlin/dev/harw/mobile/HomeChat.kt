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
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AssistChip
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
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

/** Entwurf eines noch nicht gesendeten neuen Chats; die Session entsteht erst mit der ersten Nachricht. */
internal data class NewChatDraft(
    val local: Boolean,
    /** `null` = neues Projekt zum Thema der ersten Nachricht. */
    val projectKey: String? = null,
    /** Gewaehltes Cloud-Modell `<provider>/<modell>`; `null` = Standard des Servers. */
    val model: String? = null,
    val text: String = "",
    val images: List<DraftImage> = emptyList(),
    val error: String? = null,
)

/**
 * Neuer Chat: die normale Unterhaltungsansicht als Entwurf. Oben Lokal|Cloud und Modell, in der Mitte die
 * Projektwahl, unten der gewohnte Eingabebereich. Die Session wird erst beim ersten Senden angelegt.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun NewChatScreen(
    draft: NewChatDraft,
    targetState: WorkspaceState,
    cloudState: WorkspaceState,
    connection: HarwConnection,
    onBack: () -> Unit,
    onSwitch: (Boolean) -> Unit,
    onProject: (String?) -> Unit,
    onModel: (String) -> Unit,
    onEdit: (String) -> Unit,
    onAttach: (DraftImage) -> Unit,
    onDetach: (Int) -> Unit,
    onImageError: (String) -> Unit,
    onSend: () -> Unit,
) {
    var menu by rememberSaveable { mutableStateOf(false) }
    var modelsOpen by rememberSaveable { mutableStateOf(false) }
    val busy = targetState.creating
    val real = targetState.projects.filter { it.projectKey != NoProjectKey }
    val choice = draft.projectKey
    val topicMode = choice == null
    val imagesSupported by produceState(false, targetState.online, targetState.generation, draft.local) {
        value = targetState.online && connection.supportsImages()
    }
    val manage = cloudState.manage
    Column(Modifier.fillMaxSize().imePadding()) {
        HarwHeader(title = "Neuer Chat", onBack = onBack) { ConnectionBadge(targetState) }
        BackendSwitch(local = draft.local, enabled = !busy, onSelect = onSwitch)
        Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 2.dp)) {
            if (draft.local) {
                LocalModelChip()
            } else {
                val block = modelChipBlock(manage.support, true)
                AssistChip(
                    onClick = { modelsOpen = true },
                    enabled = block == null && !busy,
                    label = {
                        Text(
                            draft.model ?: modelChipLabel(null, manage.catalog?.liveModel, manage.catalog?.liveProvider),
                            maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    },
                )
                block?.let {
                    Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (modelsOpen && !draft.local) {
            ModalBottomSheet(
                onDismissRequest = { modelsOpen = false },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            ) {
                ModelSwitchSheet(
                    cloudState, connection, "", true,
                    pending = draft.model,
                    onPick = { onModel(it); modelsOpen = false },
                )
            }
        }
        Column(
            Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 24.dp, vertical = 12.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterVertically),
        ) {
            Text("Projekt", style = MaterialTheme.typography.labelLarge)
            Box {
                OutlinedButton(onClick = { menu = true }, enabled = !busy) {
                    Text(real.firstOrNull { it.projectKey == choice }?.displayLabel() ?: "Kein Projekt – neues zum Thema anlegen")
                }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(
                        text = { Text("Kein Projekt – neues zum Thema anlegen") },
                        onClick = { onProject(null); menu = false },
                    )
                    for (project in real) {
                        DropdownMenuItem(
                            text = { Text(project.displayLabel()) },
                            onClick = { onProject(project.projectKey); menu = false },
                        )
                    }
                }
            }
            Text(
                if (topicMode) {
                    "Aus der ersten Nachricht wird ein Projektname abgeleitet und die Session dem Projekt zugewiesen."
                } else {
                    "Die Session wird mit deiner ersten Nachricht in diesem Projekt angelegt."
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            if (targetState.projectsLoaded && !targetState.projectsSupported) {
                Text(
                    NoProjectsSupportText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            if (!targetState.online) {
                StatusMessage(if (draft.local) "Lokal ist noch nicht verbunden." else "Cloud ist noch nicht verbunden.", error = true)
            }
        }
        PromptComposer(
            draft = SessionDraft(text = draft.text, images = draft.images, error = draft.error, sending = busy),
            enabled = !busy,
            canSend = targetState.online && targetState.canCreate && !busy,
            busy = busy,
            hint = "Erste Nachricht an Harw",
            onEdit = onEdit,
            onReview = {},
            onSend = onSend,
            onDiscard = {},
            onAttachImage = if (imagesSupported) onAttach else null,
            onDetachImage = onDetach,
            onImageError = onImageError,
        )
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
