package dev.harw.mobile

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileShareMode

internal const val AssistantSummaryLimit = 500
internal const val CloudAssistantSummaryLimit = AssistantSummaryLimit
private const val DisplayLimit = 50
private const val QueryLimit = 200
private const val RenamedTitleLimit = 200
private const val ProjectChoiceLimit = 40

internal data class AssistantProjectChoice(val key: String, val label: String, val pathHint: String?)

internal fun assistantProjectChoices(state: WorkspaceState, generation: Long): List<AssistantProjectChoice> {
    if (!state.online || state.generation != generation || state.projectSummariesGeneration != generation ||
        !state.existingProjectAssignment || state.projectsLegacy || state.projectsShare?.mode != MobileShareMode.CONTINUE
    ) return emptyList()
    return state.projects.asSequence()
        .take(ProjectChoiceLimit)
        .filter { project ->
            project.projectKey.isNotEmpty() && project.projectKey.length <= 128 &&
                project.projectKey.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' || it == '_' }
        }
        .distinctBy { it.projectKey }
        .map { AssistantProjectChoice(it.projectKey, it.label, it.pathHint) }
        .toList()
}

internal sealed interface AssistantSessionChange {
    val id: String

    data class Rename(override val id: String, val title: String) : AssistantSessionChange
    data class Archive(override val id: String) : AssistantSessionChange
    data class AssignProject(override val id: String, val destination: AssistantProjectChoice) : AssistantSessionChange
}

private fun cloudSummariesCurrent(state: WorkspaceState, owner: CloudAssistantTarget): Boolean =
    !state.local && state.online && state.profile == owner.profile && state.generation == owner.generation &&
        state.sessionSummariesGeneration == owner.generation

internal fun canManageCloudAssistantSessions(state: WorkspaceState, owner: CloudAssistantTarget): Boolean =
    cloudSummariesCurrent(state, owner) && state.projectsSupported && !state.projectsLegacy &&
        state.projectsShare?.mode == MobileShareMode.CONTINUE

/**
 * Local title filter over the ordinary gateway's current metadata. It never
 * invokes session.page (which includes first-message previews), reads history,
 * asks the model to discover sessions, or inserts search results in the room.
 */
@Composable
internal fun CloudAssistantSessionPicker(
    owner: CloudAssistantTarget,
    state: WorkspaceState,
    connection: HarwConnection,
    onOpen: (String) -> Boolean,
    onBack: () -> Unit,
) {
    AssistantSessionPicker(
        owner = owner,
        summaries = state.sessions,
        fresh = cloudSummariesCurrent(state, owner),
        manageable = canManageCloudAssistantSessions(state, owner),
        allowArchive = true,
        projectChoices = assistantProjectChoices(state, owner.generation),
        projectHostLabel = owner.profile.label,
        heading = "Cloud-Sessions",
        scopeNotice = "Nur geladene Titel von ${owner.profile.label}. Keine Suche in Nachrichten oder lokalen Sessions.",
        onOpen = { id -> connection.cloudAssistantTargetCurrent(owner) && onOpen(id) },
        onChange = { connection.changeCloudAssistantSession(owner, it) },
        onBack = onBack,
    )
}

/** Both Assistant variants browse summaries already present on their own host. */
@Composable
internal fun AssistantSessionPicker(
    owner: Any,
    summaries: List<MobileSession>,
    fresh: Boolean,
    manageable: Boolean,
    allowArchive: Boolean,
    projectChoices: List<AssistantProjectChoice>,
    projectHostLabel: String,
    heading: String,
    scopeNotice: String,
    onOpen: suspend (String) -> Boolean,
    onChange: suspend (AssistantSessionChange) -> Unit,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    val scope = rememberCoroutineScope()
    var query by remember(owner) { mutableStateOf("") }
    var archived by remember(owner) { mutableStateOf(setOf<String>()) }
    var renamed by remember(owner) { mutableStateOf(mapOf<String, String>()) }
    var running by remember(owner) { mutableStateOf(false) }
    var notice by remember(owner) { mutableStateOf<String?>(null) }
    var selection by remember(owner) { mutableStateOf<MobileSession?>(null) }
    var renameDraft by remember(owner) { mutableStateOf<String?>(null) }
    var assigning by remember(owner) { mutableStateOf<MobileSession?>(null) }
    val showManagementMenu = allowArchive || projectChoices.isNotEmpty()

    val entries = remember(summaries, fresh, query, archived, renamed) {
        val needle = query.trim()
        if (!fresh) emptyList()
        else summaries.asSequence()
            .take(AssistantSummaryLimit)
            .filterNot { it.sessionId in archived }
            .distinctBy { it.sessionId }
            .filter { row ->
                needle.isEmpty() || (renamed[row.sessionId] ?: row.title)?.contains(needle, ignoreCase = true) == true
            }
            .take(DisplayLimit + 1)
            .toList()
    }

    fun change(request: AssistantSessionChange) {
        if (running || !manageable) return
        running = true
        notice = null
        selection = null
        renameDraft = null
        assigning = null
        scope.launch {
            try {
                onChange(request)
                when (request) {
                    is AssistantSessionChange.Rename -> renamed = renamed + (request.id to request.title)
                    is AssistantSessionChange.Archive -> archived = archived + request.id
                    is AssistantSessionChange.AssignProject -> Unit
                }
                notice = when (request) {
                    is AssistantSessionChange.Rename -> "Titel geändert."
                    is AssistantSessionChange.Archive -> "Session archiviert."
                    is AssistantSessionChange.AssignProject -> "Session dem gewählten Projekt zugewiesen."
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                notice = if (error is MobileException.Denied) "Der Server erlaubt diese Änderung nicht."
                else "Die Änderung ist nicht bestätigt. Harw wiederholt sie nicht automatisch."
            } finally {
                running = false
            }
        }
    }

    Column(Modifier.fillMaxSize().imePadding()) {
        HarwHeader(title = heading, onBack = onBack) { }
        if (running) LinearProgressIndicator(Modifier.fillMaxWidth())
        OutlinedTextField(
            value = query,
            onValueChange = { if (it.length <= QueryLimit) query = it },
            enabled = fresh && !running,
            label = { Text("Geladene Sessiontitel filtern") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        )
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            item {
                Text(scopeNotice)
            }
            notice?.let { message -> item { Text(message) } }
            if (!fresh) item { Text("Für diese Verbindung ist noch keine Sessionliste bestätigt.") }
            else if (entries.isEmpty()) item { Text("Keine Treffer in den geladenen Sessiontiteln.") }
            if (fresh && (summaries.size > AssistantSummaryLimit || entries.size > DisplayLimit)) item {
                Text("Die Liste ist begrenzt. Angezeigt werden bis zu $DisplayLimit Treffer aus den ersten $AssistantSummaryLimit geladenen Titel-Einträgen.")
            }
            items(entries.take(DisplayLimit)) { row ->
                val title = renamed[row.sessionId] ?: row.title?.takeUnless(String::isBlank) ?: "Session ohne Titel"
                Column {
                    Text(title, style = MaterialTheme.typography.titleSmall)
                    Row {
                        TextButton(
                            enabled = !running,
                            onClick = {
                                if (!running) {
                                    running = true
                                    notice = null
                                    scope.launch {
                                        try {
                                            if (!onOpen(row.sessionId)) {
                                                notice = "Diese Session kann hier nicht sicher geöffnet werden."
                                            }
                                        } catch (error: Exception) {
                                            if (error is CancellationException) throw error
                                            notice = "Diese Session kann hier nicht sicher geöffnet werden."
                                        } finally {
                                            running = false
                                        }
                                    }
                                }
                            },
                        ) { Text("Öffnen") }
                        if (manageable) {
                            TextButton(
                                enabled = !running,
                                onClick = {
                                    selection = row
                                    if (!showManagementMenu) renameDraft = (renamed[row.sessionId] ?: row.title.orEmpty()).take(RenamedTitleLimit)
                                },
                            ) { Text(if (showManagementMenu) "Verwalten" else "Umbenennen") }
                        }
                    }
                }
            }
        }
    }

    val moving = assigning
    if (moving != null && manageable && !running) {
        AssistantProjectDialog(
            owner = owner,
            rowTitle = renamed[moving.sessionId] ?: moving.title.orEmpty(),
            hostLabel = projectHostLabel,
            choices = projectChoices,
            onDismiss = { assigning = null },
            onConfirm = { destination ->
                if (destination in projectChoices) {
                    change(AssistantSessionChange.AssignProject(moving.sessionId, destination))
                }
            },
        )
    }

    val row = selection?.takeIf { manageable && !running }
    if (row != null) {
        val edit = renameDraft
        AlertDialog(
            onDismissRequest = { selection = null; renameDraft = null },
            title = { Text(if (edit == null) "Session verwalten" else "Session umbenennen") },
            text = {
                if (edit == null) {
                    Column {
                        if (projectChoices.isNotEmpty()) {
                            TextButton(onClick = { selection = null; assigning = row }) {
                                Text("Projekt neu zuweisen")
                            }
                        }
                        if (allowArchive) {
                            Text("Archivieren ändert die Sichtbarkeit auf diesem Server. Es löscht die Session nicht.")
                        }
                    }
                } else {
                    OutlinedTextField(
                        value = edit,
                        onValueChange = { if (it.length <= RenamedTitleLimit) renameDraft = it },
                        label = { Text("Neuer Titel") },
                        singleLine = true,
                    )
                }
            },
            confirmButton = {
                if (edit == null && allowArchive) {
                    TextButton(onClick = { change(AssistantSessionChange.Archive(row.sessionId)) }) {
                        Text("Archivieren")
                    }
                } else if (edit != null) {
                    TextButton(
                        enabled = edit.isNotBlank() && edit.none(Char::isISOControl),
                        onClick = { change(AssistantSessionChange.Rename(row.sessionId, edit.trim())) },
                    ) { Text("Speichern") }
                }
            },
            dismissButton = {
                Row {
                    if (edit == null) {
                        TextButton(onClick = { renameDraft = (renamed[row.sessionId] ?: row.title.orEmpty()).take(RenamedTitleLimit) }) {
                            Text("Umbenennen")
                        }
                    }
                    TextButton(onClick = { selection = null; renameDraft = null }) { Text("Abbrechen") }
                }
            },
        )
    }
}


@Composable
private fun AssistantProjectDialog(
    owner: Any,
    rowTitle: String,
    hostLabel: String,
    choices: List<AssistantProjectChoice>,
    onDismiss: () -> Unit,
    onConfirm: (AssistantProjectChoice) -> Unit,
) {
    var picked by remember(owner) { mutableStateOf<AssistantProjectChoice?>(null) }
    val destination = picked
    if (destination == null) {
        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Bestehendes Projekt wählen") },
            text = {
                if (choices.isEmpty()) Text("Für diese Verbindung sind keine Ziele bestätigt.")
                else LazyColumn(Modifier.heightIn(max = 320.dp)) {
                    items(choices) { choice ->
                        TextButton(onClick = { picked = choice }, modifier = Modifier.fillMaxWidth()) {
                            Column {
                                Text(choice.label)
                                choice.pathHint?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                            }
                        }
                    }
                }
            },
            confirmButton = { TextButton(onClick = onDismiss) { Text("Abbrechen") } },
        )
    } else {
        AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Session diesem Projekt zuweisen?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Session: ${rowTitle.ifBlank { "Ohne Titel" }}")
                    Text("Host: $hostLabel")
                    Text("Ziel: ${destination.label}")
                    destination.pathHint?.let { Text(it) }
                    Text("Die Zuweisung setzt auch den Arbeitsordner für künftige Arbeit in dieser Session. Es wird kein Projektordner angelegt und nichts ans Modell gesendet.")
                }
            },
            confirmButton = {
                TextButton(enabled = destination in choices, onClick = { onConfirm(destination) }) {
                    Text("Zuweisung bestätigen")
                }
            },
            dismissButton = { TextButton(onClick = { picked = null }) { Text("Anderes Projekt") } },
        )
    }
}
