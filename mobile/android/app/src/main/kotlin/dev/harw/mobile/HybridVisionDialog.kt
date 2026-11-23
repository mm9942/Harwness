package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.RadioButton
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
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.HybridVisionChoice
import uniffi.harw_mobile_ffi.HybridVisionProgress

/** A fresh, immutable draft snapshot. No remembered consent, auto-retry or transcript replay. */
@Composable
internal fun HybridVisionDialog(
    connection: HarwConnection,
    images: List<DraftImage>,
    question: String,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var choices by remember { mutableStateOf<List<HybridVisionChoice>?>(null) }
    var route by remember { mutableStateOf<HybridVisionChoice?>(null) }
    var selected by remember { mutableStateOf<Set<Int>>(emptySet()) }
    var job by remember { mutableStateOf<Job?>(null) }
    var started by remember { mutableStateOf(false) }
    var finalText by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    val phase = remember { MutableStateFlow(false) }
    val text = remember { MutableStateFlow("") }
    val localStarted by phase.collectAsState()
    val partial by text.collectAsState()
    val busy = job != null
    fun close() {
        job?.cancel()
        onClose()
    }
    LaunchedEffect(connection) {
        try {
            choices = connection.hybridVisionChoices()
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            error = "Die Vision-Modellauswahl konnte nicht gelesen werden."
        }
    }

    AlertDialog(
        onDismissRequest = ::close,
        title = { Text("Bilder einmalig in der Cloud analysieren") },
        text = {
            Column(
                Modifier.heightIn(max = 460.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text("Nur die ausgewählten neuen Bilder und eine neutrale Bildfrage gehen an den angegebenen Anbieter. Dein Entwurfstext, der Chatverlauf und die endgültige Antwort bleiben auf dem Gerät.")
                Text("Der Anbieter verarbeitet Bilder nach seinen Regeln. Abbrechen holt bereits gesendete Bilder nicht zurück. Die Antwort wird nicht als Chatnachricht gespeichert.")
                val available = choices
                if (available == null && error == null) LinearProgressIndicator()
                if (available?.isEmpty() == true) {
                    Text("Wähle ein lokales Qwen und hinterlege einen offiziellen OpenAI- oder Anthropic-API-Anbieter mit einem bildfähigen Modell.")
                }
                if (!started && !available.isNullOrEmpty()) {
                    Text("Anbieter und Modell")
                    available.forEach { option ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            RadioButton(selected = route == option, onClick = { route = option })
                            Column {
                                Text("${option.routeId} · ${option.modelId}")
                                Text(option.endpoint)
                            }
                        }
                    }
                    route?.let { Text("Antwort lokal: ${it.localModelId}") }
                    Text("Ein bis drei neue Bilder auswählen")
                    images.forEachIndexed { index, image ->
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Checkbox(
                                checked = index in selected,
                                enabled = index in selected || selected.size < 3,
                                onCheckedChange = { checked ->
                                    selected = if (checked) selected + index else selected - index
                                },
                            )
                            Text(image.label)
                            if (index in selected) {
                                DraftThumbnail(image, enabled = false, onRemove = {})
                            }
                        }
                    }
                }
                if (busy) {
                    LinearProgressIndicator()
                    Text(if (localStarted) "Qwen schreibt die Antwort auf diesem Gerät …" else "Bilder werden beim gewählten Anbieter analysiert …")
                }
                error?.let { Text(it) }
                (finalText ?: partial.takeIf { it.isNotEmpty() })?.let { answer ->
                    SelectionContainer { Text(answer) }
                }
            }
        },
        confirmButton = {
            if (!started) {
                TextButton(
                    enabled = route != null && selected.isNotEmpty() && !busy,
                    onClick = {
                        val chosen = route ?: return@TextButton
                        val approved = images.filterIndexed { index, _ -> index in selected }.map { it.image }
                        if (approved.isEmpty() || started) return@TextButton
                        started = true
                        val progress = object : HybridVisionProgress {
                            override fun onLocalStart() { phase.value = true }
                            override fun onLocalText(delta: String) {
                                text.update { (it + delta).take(32_768) }
                            }
                        }
                        job = scope.launch {
                            try {
                                finalText = connection.hybridVisionReply(chosen, approved, question, progress)
                            } catch (e: CancellationException) {
                                throw e
                            } catch (_: Exception) {
                                error = "Bildanalyse fehlgeschlagen oder nicht mehr verfügbar. Nichts wird automatisch erneut gesendet."
                            } finally {
                                job = null
                            }
                        }
                    },
                ) { Text("Ausgewählte Bilder senden (${selected.size})") }
            }
        },
        dismissButton = { TextButton(onClick = ::close) { Text(if (busy) "Abbrechen" else "Schließen") } },
    )
}
