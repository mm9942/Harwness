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
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import java.util.UUID
import kotlinx.coroutines.CancellationException
import uniffi.harw_mobile_ffi.CloudAssistantAccess
import uniffi.harw_mobile_ffi.MobileCloudAssistantSnapshot
import uniffi.harw_mobile_ffi.MobileCloudAssistantStatus
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ServerProfile

/** Identity object: never reusable for a different profile or link generation. */
internal class CloudAssistantTarget(
    val profile: ServerProfile,
    val generation: Long,
    val access: CloudAssistantAccess,
    val recallSupported: Boolean = false,
    val sharedSupported: Boolean = false,
    val sharedDeviceFingerprint: String? = null,
    val modelsSupported: Boolean = false,
)

internal sealed interface CloudAssistantCommand {
    data object Status : CloudAssistantCommand
    data object Provision : CloudAssistantCommand
    data object Revoke : CloudAssistantCommand
    // Immutable in-memory delivery attempt. Retrying uses the exact same request, not
    // a new read of composer/selection state. The default remains the legacy send.
    class Submit(val id: String, val text: String, recallIds: List<String> = emptyList(), val model: String? = null) : CloudAssistantCommand {
        val recallMessageIds: List<String> = recallIds.toList()
    }
}

private const val AssistantPromptBytes = 8 * 1024
private const val AssistantRecallBytes = 8 * 1024
private const val AssistantRecallTurns = 4

/** A separate route. No session composer, saved input, import, attachment, tool, or local fallback. */
@Composable
internal fun CloudAssistantScreen(
    owner: CloudAssistantTarget,
    connection: HarwConnection,
    cloudState: WorkspaceState,
    onOpenSession: (String) -> Boolean,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    var operation by remember(owner) { mutableStateOf<CloudAssistantCommand?>(CloudAssistantCommand.Status) }
    var snapshot by remember(owner) { mutableStateOf<MobileCloudAssistantSnapshot?>(null) }
    var draft by remember(owner) { mutableStateOf("") }
    var unresolved by remember(owner) { mutableStateOf<CloudAssistantCommand.Submit?>(null) }
    var retryConfirmedByStatus by remember(owner) { mutableStateOf(false) }
    var notice by remember(owner) { mutableStateOf<String?>(null) }
    var confirmation by remember(owner) { mutableStateOf<CloudAssistantCommand?>(null) }
    var searching by remember(owner) { mutableStateOf(false) }
    var recallMode by remember(owner) { mutableStateOf(false) }
    var selectedRecall by remember(owner) { mutableStateOf(emptySet<String>()) }
    var openSharedRoom by remember(owner) { mutableStateOf(false) }
    var selectedModel by remember(owner) { mutableStateOf<String?>(null) }
    var availableModels by remember(owner) { mutableStateOf<List<String>?>(null) }
    var choosingModel by remember(owner) { mutableStateOf(false) }
    var modelError by remember(owner) { mutableStateOf<String?>(null) }
    var modelReload by remember(owner) { mutableStateOf(0) }
    LaunchedEffect(owner, snapshot?.status, modelReload) {
        if (owner.modelsSupported && snapshot?.status == MobileCloudAssistantStatus.READY) {
            try {
                availableModels = connection.cloudAssistantModels(owner)
                modelError = null
            } catch (e: Exception) {
                if (e is CancellationException) throw e
                availableModels = null
                modelError = "Modellliste konnte nicht geladen werden."
            }
        }
    }
    if (openSharedRoom && owner.sharedSupported) {
        CloudSharedAssistantScreen(owner, connection, onBack = { openSharedRoom = false })
        return
    }

    fun enqueue(next: CloudAssistantCommand) {
        if (operation != null) return
        if (next is CloudAssistantCommand.Submit) {
            unresolved = next
            retryConfirmedByStatus = false
        }
        operation = next
    }

    LaunchedEffect(owner, operation) {
        val request = operation ?: return@LaunchedEffect
        val wasMutation = request != CloudAssistantCommand.Status && request !is CloudAssistantCommand.Submit
        notice = null
        try {
            val result = connection.cloudAssistantCall(owner, request)
            snapshot = result
            val pendingMessage = unresolved
            if (result.status != MobileCloudAssistantStatus.READY ||
                (pendingMessage != null && result.turns.any { it.clientMessageId == pendingMessage.id })
            ) {
                unresolved = null
                draft = ""
                recallMode = false
                selectedRecall = emptySet()
            }
            if (request == CloudAssistantCommand.Status && unresolved != null) {
                retryConfirmedByStatus = true
                notice = "Die Nachricht steht noch nicht im Verlauf. Beim erneuten Senden bleiben Nachricht, ID und Auswahl unverändert."
            } else if (request is CloudAssistantCommand.Submit && unresolved != null) {
                notice = "Die Nachricht ist noch nicht bestätigt. Lade den Verlauf neu, bevor du erneut sendest."
            }
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            if (wasMutation || error is MobileException.Disconnected || error is MobileException.Revoked ||
                error is MobileException.Denied
            ) snapshot = null
            notice = when (error) {
                is MobileException.Disconnected, is MobileException.Revoked ->
                    "Diese Cloud-Verbindung ist nicht mehr aktiv."
                is MobileException.Denied ->
                    "Der gewählte Server hat die Aktion abgelehnt."
                is MobileException.Busy ->
                    "Der Assistent ist beschäftigt. Lade den Verlauf neu, um den Stand zu prüfen."
                else -> if (unresolved != null) {
                    "Die Zustellung ist unklar. Lade den Verlauf neu; Harw sendet nicht automatisch erneut."
                } else {
                    "Die Aktion konnte nicht bestätigt werden. Lade den Status neu."
                }
            }
        } finally {
            operation = null
        }
    }

    val result = snapshot
    val turns = result?.turns.orEmpty()
    val idle = operation == null
    val pendingMessage = unresolved
    val recallBytes = remember(result) {
        turns.associate { it.clientMessageId to
            (it.text.toByteArray(Charsets.UTF_8).size + it.answer.toByteArray(Charsets.UTF_8).size) }
    }
    val selectedBytes = selectedRecall.sumOf { recallBytes[it] ?: 0 }
    val readyToSelectRecall = owner.recallSupported && idle && pendingMessage == null
    val canSubmitNew = draft.isNotBlank() &&
        (selectedModel == null || availableModels?.contains(selectedModel) == true) && (!recallMode ||
        (owner.recallSupported && selectedRecall.size in 1..AssistantRecallTurns &&
            selectedRecall.all(recallBytes::containsKey) && selectedBytes <= AssistantRecallBytes))
    if (searching && result?.status == MobileCloudAssistantStatus.READY) {
        CloudAssistantSessionPicker(owner, cloudState, connection, onOpenSession, onBack = { searching = false })
        return
    }
    Column(Modifier.fillMaxSize().imePadding()) {
        HarwHeader(title = "Cloud-Assistent", onBack = onBack) {
            TextButton(enabled = idle, onClick = { enqueue(CloudAssistantCommand.Status) }) {
                Text("Neu laden")
            }
        }
        if (!idle) LinearProgressIndicator(Modifier.fillMaxWidth())
        if (result?.status == MobileCloudAssistantStatus.READY && owner.modelsSupported) {
            TextButton(enabled = idle && pendingMessage == null, onClick = { choosingModel = true }) {
                Text(selectedModel ?: "Gateway-Standardmodell")
            }
            if (selectedModel != null && availableModels?.contains(selectedModel) != true) {
                Text("Das gewählte Modell ist nicht bestätigt. Bitte Modell erneut wählen.",
                    color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(horizontal = 16.dp))
            }
        }
        if (choosingModel) {
            AlertDialog(
                onDismissRequest = { choosingModel = false },
                title = { Text("Assistant-Modell wählen") },
                text = {
                    LazyColumn(Modifier.heightIn(max = 420.dp)) {
                        item {
                            Text("Nur vom Gateway freigegebene Modelle. Der Cloud-Verlauf wird an den gewählten Anbieter gesendet.")
                            TextButton(onClick = { selectedModel = null; choosingModel = false }) { Text("Gateway-Standardmodell") }
                            modelError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                            TextButton(onClick = { modelReload++ }) { Text("Modellliste aktualisieren") }
                        }
                        items(availableModels.orEmpty()) { model ->
                            TextButton(onClick = { selectedModel = model; choosingModel = false }) { Text(model) }
                        }
                    }
                },
                confirmButton = { TextButton(onClick = { choosingModel = false }) { Text("Schließen") } },
            )
        }
        LazyColumn(
            modifier = Modifier.weight(1f).fillMaxWidth(),
            contentPadding = PaddingValues(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            item {
                Text("Server: ${owner.profile.label}", style = MaterialTheme.typography.labelLarge)
                Text("Dieser Verlauf gehört nur zum Cloud-Assistenten. Lokale Inhalte werden nicht übernommen.")
                if (owner.sharedSupported) {
                    OutlinedButton(
                        enabled = idle && pendingMessage == null && draft.isBlank() && !recallMode,
                        onClick = { openSharedRoom = true },
                    ) { Text(stringResource(R.string.cloud_shared_assistant_open)) }
                }
            }
            when (result?.status) {
                MobileCloudAssistantStatus.UNPROVISIONED -> item {
                    Text("Für dieses Gerät ist auf dem Server noch kein Cloud-Assistent eingerichtet.")
                    Button(enabled = idle, onClick = { confirmation = CloudAssistantCommand.Provision }) {
                        Text("Cloud-Assistent einrichten")
                    }
                }
                MobileCloudAssistantStatus.REVOKED -> item {
                    Text("Der Cloud-Assistent wurde auf diesem Server für dieses Gerät deaktiviert.")
                }
                MobileCloudAssistantStatus.READY -> {
                    item {
                        OutlinedButton(
                            enabled = idle && pendingMessage == null && draft.isBlank() && !recallMode,
                            onClick = { searching = true },
                        ) { Text("Cloud-Sessions suchen und verwalten") }
                    }
                    if (owner.recallSupported && turns.isNotEmpty()) item {
                        TextButton(enabled = readyToSelectRecall, onClick = {
                            recallMode = !recallMode
                            if (!recallMode) selectedRecall = emptySet()
                        }) {
                            Text(if (recallMode) stringResource(R.string.cloud_assistant_recall_clear) else stringResource(R.string.cloud_assistant_recall_choose))
                        }
                        if (recallMode) {
                            Text(
                                stringResource(R.string.cloud_assistant_recall_disclosure),
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                    items(turns) { turn ->
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("Du", style = MaterialTheme.typography.labelLarge)
                            Text(turn.text)
                            Text("Cloud-Assistent", style = MaterialTheme.typography.labelLarge)
                            Text(turn.answer)
                            if (recallMode && owner.recallSupported) {
                                val selected = turn.clientMessageId in selectedRecall
                                val fits = selectedRecall.size < AssistantRecallTurns &&
                                    selectedBytes + (recallBytes[turn.clientMessageId] ?: AssistantRecallBytes + 1) <= AssistantRecallBytes
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Checkbox(
                                        checked = selected,
                                        enabled = readyToSelectRecall && (selected || fits),
                                        onCheckedChange = { include ->
                                            if (readyToSelectRecall && (!include || fits)) {
                                                selectedRecall = if (include) selectedRecall + turn.clientMessageId
                                                    else selectedRecall - turn.clientMessageId
                                            }
                                        },
                                    )
                                    Text(stringResource(R.string.cloud_assistant_recall_include))
                                }
                            }
                        }
                    }
                    item {
                        OutlinedButton(enabled = idle && pendingMessage == null, onClick = { confirmation = CloudAssistantCommand.Revoke }) {
                            Text("Cloud-Assistent deaktivieren")
                        }
                    }
                }
                null -> if (idle) item { Text("Der Serverstatus ist nicht bestätigt.") }
            }
        }
        notice?.let {
            Text(it, Modifier.padding(horizontal = 16.dp, vertical = 6.dp), color = MaterialTheme.colorScheme.error)
        }
        if (result?.status == MobileCloudAssistantStatus.READY) {
            Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
                OutlinedTextField(
                    value = draft,
                    onValueChange = { if (it.toByteArray(Charsets.UTF_8).size <= AssistantPromptBytes) draft = it },
                    enabled = idle && pendingMessage == null,
                    label = { Text("Nachricht an den Cloud-Assistenten") },
                    maxLines = 4,
                    modifier = Modifier.fillMaxWidth(),
                )
                if (recallMode) Text(
                    stringResource(R.string.cloud_assistant_recall_count, selectedRecall.size, AssistantRecallTurns),
                    style = MaterialTheme.typography.bodySmall,
                )
                Button(
                    enabled = idle && if (pendingMessage != null) retryConfirmedByStatus else canSubmitNew,
                    onClick = {
                        if (pendingMessage != null) {
                            if (retryConfirmedByStatus) enqueue(pendingMessage)
                        } else if (canSubmitNew) {
                            val selection = if (recallMode) turns.mapNotNull {
                                it.clientMessageId.takeIf(selectedRecall::contains)
                            } else emptyList()
                            // Selection mode can never silently send a plain prompt.
                            if (!recallMode || selection.isNotEmpty()) {
                                enqueue(CloudAssistantCommand.Submit(UUID.randomUUID().toString(), draft, selection, selectedModel))
                            }
                        }
                    },
                ) {
                    Text(when {
                        pendingMessage != null -> "Dieselbe Nachricht erneut senden"
                        recallMode -> stringResource(R.string.cloud_assistant_recall_send)
                        else -> "An Cloud-Assistent senden"
                    })
                }
            }
        }
    }

    confirmation?.let { action ->
        val provisioning = action == CloudAssistantCommand.Provision
        AlertDialog(
            onDismissRequest = { confirmation = null },
            title = { Text(if (provisioning) "Cloud-Assistent einrichten?" else "Cloud-Assistent deaktivieren?") },
            text = {
                Text(
                    if (provisioning) {
                        "Der gewählte Server richtet einen eigenen Verlauf ein. Nachrichten, die du hier sendest, " +
                            "werden dort verarbeitet und gespeichert. Es werden keine lokalen Daten übernommen."
                    } else {
                        "Der bisherige Verlauf ist danach nicht mehr sichtbar. Die Daten werden dadurch nicht gelöscht. " +
                            "Das lässt sich hier nicht rückgängig machen."
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = { confirmation = null; enqueue(action) }) {
                    Text(if (provisioning) "Einrichten" else "Deaktivieren")
                }
            },
            dismissButton = { TextButton(onClick = { confirmation = null }) { Text("Abbrechen") } },
        )
    }
}
