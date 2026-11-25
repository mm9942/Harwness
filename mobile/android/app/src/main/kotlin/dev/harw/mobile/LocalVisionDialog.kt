package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.LocalVisionChoice
import uniffi.harw_mobile_ffi.LocalVisionImage
import uniffi.harw_mobile_ffi.LocalVisionOutcome
import uniffi.harw_mobile_ffi.LocalVisionProgress
import uniffi.harw_mobile_ffi.MobileException

/** Uses only the currently loaded phone model; no session ID, transcript, provider or save port. */
@Composable
internal fun LocalVisionDialog(
    connection: HarwConnection,
    onClose: () -> Unit,
    onUseApi: () -> Unit,
    onConfiguredApi: (ConfiguredVisionFallbackInvite) -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val requested = remember(context) { readLocalImageConcurrency(context) }
    val preference = remember(context) {
        readLocalVisionPreference(context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE))
    }
    val modelLabels = remember(context) {
        ModelManager(ModelStore(context.applicationContext)).catalog().associate { it.id to it.label }
    }
    var choices by remember(connection) { mutableStateOf<List<LocalVisionChoice>?>(null) }
    var selectedModel by remember(connection) { mutableStateOf<LocalVisionChoice?>(null) }
    var loadFailed by remember(connection) { mutableStateOf(false) }
    var refs by remember { mutableStateOf<List<LocalVisionRef<Uri>>>(emptyList()) }
    var nextId by remember { mutableStateOf(1UL) }
    var pickerNotice by remember { mutableStateOf<String?>(null) }
    var startedIds by remember { mutableStateOf<List<ULong>?>(null) }
    var batchNotice by remember { mutableStateOf<String?>(null) }
    var stopped by remember { mutableStateOf(false) }
    var finished by remember { mutableStateOf(false) }
    var job by remember { mutableStateOf<Job?>(null) }
    val results = remember { mutableStateMapOf<ULong, LocalVisionItemState>() }
    val running = startedIds != null && !stopped && !finished
    val eligible = choices?.let { eligibleLoadedVisionModels(preference, it) }
    val modelFailed = loadFailed || (choices != null && eligible.isNullOrEmpty()) ||
        (finished && !stopped && (batchNotice != null || results.values.any {
            it == LocalVisionItemState.Failed || it is LocalVisionItemState.Partial
        }))
    fun label(id: String): String = modelLabels[id] ?: id

    // Unlike the media picker, request local *openable* documents rather than cloud-only photos.
    // No persisted URI permission, thumbnail cache, or path outside this dialog.
    val contract = remember {
        object : ActivityResultContracts.OpenMultipleDocuments() {
            override fun createIntent(context: Context, input: Array<String>): Intent =
                super.createIntent(context, input)
                    .addCategory(Intent.CATEGORY_OPENABLE)
                    .putExtra(Intent.EXTRA_LOCAL_ONLY, true)
        }
    }
    val picker = rememberLauncherForActivityResult(contract) { picked ->
        if (startedIds == null && picked.isNotEmpty()) {
            val combined = appendLocalVisionRefs(refs, picked, nextId)
            if (combined == null) {
                pickerNotice = "Höchstens $LocalVisionMaxRefs Bilder. Die Auswahl blieb unverändert."
            } else {
                nextId += (combined.size - refs.size).toULong()
                refs = combined
                pickerNotice = null
            }
        }
    }

    LaunchedEffect(connection) {
        try {
            val found = connection.localVisionChoices()
            choices = found
            selectedModel = eligibleLoadedVisionModels(preference, found).singleOrNull()
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            loadFailed = true
        }
    }

    fun stop() {
        stopped = true
        job?.cancel()
    }

    fun close() {
        if (running) stop()
        onClose()
    }

    val displayIds = startedIds ?: refs.map { it.id }
    AlertDialog(
        onDismissRequest = ::close,
        title = { Text("Lokale Modellanalyse") },
        text = {
            LazyColumn(
                modifier = Modifier.heightIn(max = 460.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                item {
                    Text("Ein Ergebnis pro Bild, nur mit einem Modell auf diesem Gerät. Harw sendet dabei weder die Bilder noch Chatdaten an einen Server und fügt die Ergebnisse nicht zum Chat hinzu.")
                }
                when {
                    loadFailed -> item { Text("Das lokale Modell konnte nicht abgefragt werden.") }
                    choices == null -> item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                    choices.isNullOrEmpty() -> item {
                        Text("Kein kompatibles Bildmodell geladen. Die lokale Analyse lädt oder wechselt kein Modell.")
                    }
                    eligible.isNullOrEmpty() -> item {
                        Text("Das bevorzugte Bildmodell ist nicht geladen. Eine lokale Alternative ist in den Einstellungen nicht freigegeben.")
                    }
                }
                if (modelFailed && !running) {
                    item { LocalVisionApiAlternative(onConfiguredApi, onUseApi) }
                }
                if (startedIds == null) {
                    if (!eligible.isNullOrEmpty()) {
                        item {
                            Column(Modifier.selectableGroup()) {
                                Text("Geladenes Vision-Modell", style = MaterialTheme.typography.titleSmall)
                                preference.modelId?.let {
                                    Text("Bevorzugt: " + label(it), style = MaterialTheme.typography.bodySmall)
                                }
                                for (option in eligible.orEmpty()) {
                                    Row(
                                        Modifier.fillMaxWidth().selectable(
                                            selected = selectedModel == option,
                                            onClick = { selectedModel = option },
                                            role = Role.RadioButton,
                                        ).padding(vertical = 6.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        RadioButton(selected = selectedModel == option, onClick = null)
                                        val prefix = if (isLoadedVisionAlternative(preference, option)) "Lokale Alternative: " else ""
                                        Text(prefix + label(option.modelId), Modifier.padding(start = 8.dp))
                                    }
                                }
                            }
                        }
                    }
                    item {
                        Text(stringResource(R.string.harw_local_vision_parallel_requested, requested))
                        selectedModel?.let {
                            Text(stringResource(R.string.harw_local_vision_parallel_estimate, it.effectiveParallelism.toInt()))
                        }
                        TextButton(
                            enabled = refs.size < LocalVisionMaxRefs && !eligible.isNullOrEmpty(),
                            onClick = { picker.launch(arrayOf("image/*")) },
                        ) { Text("Lokale Bilder auswählen (${refs.size}/$LocalVisionMaxRefs)") }
                        pickerNotice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    }
                } else {
                    item {
                        selectedModel?.let {
                            Text(it.modelId)
                        }
                        val states = results.values.toList()
                        val complete = states.count { it is LocalVisionItemState.Caption }
                        val partial = states.count { it is LocalVisionItemState.Partial }
                        val failed = states.count {
                            it == LocalVisionItemState.Unreadable || it == LocalVisionItemState.SizeLimit ||
                                it == LocalVisionItemState.InvalidInput || it == LocalVisionItemState.Failed
                        }
                        val settled = complete + partial + failed
                        Text("$settled von ${displayIds.size} ausgewertet · $complete vollständig · $partial teilweise · $failed fehlgeschlagen")
                        batchNotice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        if (running) {
                            LinearProgressIndicator(
                                progress = { settled.toFloat() / displayIds.size.coerceAtLeast(1) },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                        if (stopped) Text("Gestoppt. Bereits angezeigte Ergebnisse bleiben erhalten.")
                    }
                }
                itemsIndexed(displayIds, key = { _, id -> id.toLong() }) { index, id ->
                    val number = index + 1
                    Column {
                        Text("Bild $number", style = MaterialTheme.typography.titleSmall)
                        if (startedIds == null) {
                            TextButton(onClick = {
                                refs = refs.filterNot { it.id == id }
                                pickerNotice = null
                            }) { Text("Bild $number entfernen") }
                        } else {
                            when (val status = results[id]) {
                                is LocalVisionItemState.Caption -> LocalVisionCaption(context, index, status.text)
                                is LocalVisionItemState.Partial -> {
                                    Text(when (status.cause) {
                                        LocalVisionPartialCause.OutputLimit -> "Teilbeschreibung · Ausgabelimit"
                                        LocalVisionPartialCause.ContextLimit -> "Teilbeschreibung · Kontextlimit"
                                    })
                                    if (status.text.isNotBlank()) LocalVisionCaption(context, index, status.text)
                                }
                                LocalVisionItemState.Unreadable -> Text("Bild nicht lesbar.")
                                LocalVisionItemState.SizeLimit -> Text("Bild überschreitet das Größenlimit.")
                                LocalVisionItemState.InvalidInput -> Text("Bild kann vom Modell nicht verarbeitet werden.")
                                LocalVisionItemState.Failed -> Text("Keine Beschreibung vom lokalen Modell erhalten.")
                                LocalVisionItemState.Preparing ->
                                    Text(if (stopped) "Nicht abgeschlossen." else "Bild wird vorbereitet …")
                                LocalVisionItemState.Admitted ->
                                    Text(if (stopped) "Nicht abgeschlossen." else "Für die lokale Analyse vorbereitet …")
                                null -> Text(if (stopped) "Nicht gestartet." else "Wartet …")
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            if (startedIds == null) {
                TextButton(enabled = refs.isNotEmpty() && selectedModel != null, onClick = {
                    val choice = selectedModel?.takeIf { it in eligible.orEmpty() } ?: return@TextButton
                    val approved = refs
                    if (startedIds != null || approved.isEmpty()) return@TextButton
                    startedIds = approved.map { it.id }
                    val labels = approved.mapIndexed { index, ref -> ref.id to "Bild ${index + 1}" }.toMap()
                    refs = emptyList()
                    job = scope.launch {
                        try {
                            runLocalVisionBatch(
                                refs = approved,
                                prepare = { ref ->
                                    withContext(Dispatchers.IO) {
                                        prepareImage(context, ref.value, labels.getValue(ref.id))?.image?.let {
                                            LocalVisionPrepared(it, it.dataB64.length)
                                        }
                                    }
                                },
                                describe = { images, receive ->
                                    try {
                                        connection.localVisionDescribe(
                                            choice,
                                            images.map { LocalVisionImage(it.id, it.value) },
                                            object : LocalVisionProgress {
                                                override fun onResult(id: ULong, outcome: LocalVisionOutcome) {
                                                    receive(id, outcome.toLocalVisionItemState())
                                                }
                                            },
                                        )
                                    } catch (error: MobileException.Busy) {
                                        if (!stopped) batchNotice = "Das lokale Modell arbeitet noch. Schließe den Dialog und versuche es später erneut."
                                        throw error
                                    } catch (error: MobileException.Session) {
                                        if (!stopped) batchNotice = "Das ausgewählte Modell ist nicht mehr verfügbar. Öffne die lokale Analyse erneut."
                                        throw error
                                    }
                                },
                                update = { id, result -> if (!stopped) results[id] = result },
                            )
                        } finally {
                            finished = true
                        }
                    }
                }) {
                    val chosen = selectedModel
                    Text(refs.size.toString() + " Bilder " +
                        (if (chosen != null && isLoadedVisionAlternative(preference, chosen)) "mit lokaler Alternative" else "lokal") +
                        " analysieren")
                }
            } else if (running) {
                TextButton(onClick = ::stop) { Text("Stoppen") }
            }
        },
        dismissButton = {
            TextButton(onClick = ::close) { Text(if (running) "Schließen und stoppen" else "Schließen") }
        },
    )
}

@Composable
private fun LocalVisionCaption(context: Context, index: Int, text: String) {
    Text(text)
    TextButton(onClick = { copyVisionCaption(context, index, text) }) {
        Text("Beschreibung für Bild ${index + 1} kopieren")
    }
}

private fun LocalVisionOutcome.toLocalVisionItemState(): LocalVisionItemState = when (this) {
    is LocalVisionOutcome.Caption -> if (text.isBlank()) LocalVisionItemState.Failed else LocalVisionItemState.Caption(text)
    is LocalVisionOutcome.OutputLimit -> LocalVisionItemState.Partial(text, LocalVisionPartialCause.OutputLimit)
    is LocalVisionOutcome.ContextLimit -> LocalVisionItemState.Partial(text, LocalVisionPartialCause.ContextLimit)
    LocalVisionOutcome.InvalidInput -> LocalVisionItemState.InvalidInput
    LocalVisionOutcome.Failed -> LocalVisionItemState.Failed
}
