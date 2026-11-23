package dev.harw.mobile

import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ProviderRoute
import uniffi.harw_mobile_ffi.ProviderStore
import uniffi.harw_mobile_ffi.ProviderValidation
import uniffi.harw_mobile_ffi.ValidationOutcome

/** Eintraege der Probe-Liste, die auf einmal angezeigt werden. */
private const val ProbeListLimit = 60

/**
 * Bearbeitet die Modellliste eines Providers: Eingabefeld je Modell mit Mülleimer,
 * "+" für ein weiteres Feld und "Beim Anbieter abrufen" (Probe mit dem gespeicherten Schlüssel).
 * Gewählte Probe-Modelle landen direkt in der Liste; das erste Modell ist der Standard.
 */
@Composable
internal fun ModelListDialog(store: ProviderStore, route: ProviderRoute, onDone: (saved: Boolean) -> Unit) {
    val scope = rememberCoroutineScope()
    var models by remember { mutableStateOf(listOf<String>()) }
    var probe by remember { mutableStateOf<ProviderValidation?>(null) }
    var probing by remember { mutableStateOf(false) }
    var saving by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var filter by remember { mutableStateOf("") }
    var contextWindow by remember { mutableStateOf("128000") }

    LaunchedEffect(route.id) {
        val stored = withContext(Dispatchers.IO) { runCatching { store.routeModels(route.id) }.getOrDefault(emptyList()) }
        models = stored.ifEmpty { listOf(route.defaultModel).filter { it.isNotEmpty() } }.ifEmpty { listOf("") }
        withContext(Dispatchers.IO) { runCatching { store.routeContextWindow(route.id) }.getOrNull() }?.let { contextWindow = it.toString() }
    }

    AlertDialog(
        onDismissRequest = { if (!saving) onDone(false) },
        title = { Text("Modelle von ${route.id}") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "Das erste Modell ist der Standard. Diese Liste erscheint später in der Modellauswahl.",
                    style = MaterialTheme.typography.bodySmall,
                )
                models.forEachIndexed { index, name ->
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        OutlinedTextField(
                            value = name,
                            onValueChange = { value -> models = models.toMutableList().also { it[index] = value } },
                            label = { Text(if (index == 0) "Modell (Standard)" else "Modell ${index + 1}") },
                            singleLine = true,
                            modifier = Modifier.weight(1f),
                        )
                        val source = remember { MutableInteractionSource() }
                        val pressed by source.collectIsPressedAsState()
                        IconButton(
                            onClick = { models = models.filterIndexed { i, _ -> i != index }.ifEmpty { listOf("") } },
                            interactionSource = source,
                            colors = IconButtonDefaults.iconButtonColors(
                                contentColor = if (pressed) Color(0xFFE53935) else MaterialTheme.colorScheme.onSurfaceVariant,
                            ),
                        ) {
                            Icon(Icons.Filled.Delete, contentDescription = "Modell entfernen")
                        }
                    }
                }
                OutlinedButton(onClick = { models = models + "" }) {
                    Icon(Icons.Filled.Add, contentDescription = null)
                    Text("  Modell hinzufügen")
                }
                OutlinedTextField(
                    value = contextWindow,
                    onValueChange = { contextWindow = it.filter { c -> c.isDigit() }.take(8) },
                    singleLine = true,
                    label = { Text("Kontextfenster (Token) aller Modelle") },
                    supportingText = { Text("Bestimmt, wann der Chat verdichtet wird. Ohne Angabe gilt nur 32k.") },
                    modifier = Modifier.fillMaxWidth(),
                )
                HorizontalDivider()
                OutlinedButton(
                    enabled = !probing && route.hasSecret,
                    onClick = {
                        probing = true
                        error = null
                        scope.launch {
                            try {
                                probe = withContext(Dispatchers.IO) { store.validateRoute(route.id) }
                            } catch (e: Exception) {
                                error = e.message ?: "Abruf fehlgeschlagen"
                            }
                            probing = false
                        }
                    },
                ) { Text(if (probing) "Frage Anbieter ab …" else "Beim Anbieter abrufen") }
                if (!route.hasSecret) {
                    Text(
                        "Zum Abrufen fehlt der Schlüssel. Erst „Schlüssel eintragen“.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
                probe?.let { result ->
                    if (result.outcome == ValidationOutcome.VALID || result.models.isNotEmpty()) {
                        Text("${result.modelCount} Modelle beim Anbieter gefunden.", style = MaterialTheme.typography.labelMedium)
                        OutlinedTextField(
                            value = filter,
                            onValueChange = { filter = it },
                            label = { Text("Filter") },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        val shown = result.models.filter { filter.isBlank() || it.contains(filter, ignoreCase = true) }
                        Row {
                            TextButton(onClick = {
                                models = (models.filter { it.isNotBlank() } + shown).distinct()
                            }) { Text("Alle ${minOf(shown.size, ProbeListLimit)} sichtbaren hinzufügen") }
                        }
                        shown.take(ProbeListLimit).forEach { id ->
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Checkbox(
                                    checked = id in models,
                                    onCheckedChange = { on ->
                                        models = if (on) (models.filter { it.isNotBlank() } + id).distinct() else models.filter { it != id }.ifEmpty { listOf("") }
                                    },
                                )
                                Text(id, style = MaterialTheme.typography.bodySmall)
                            }
                        }
                        if (shown.size > ProbeListLimit) {
                            Text("… weitere ${shown.size - ProbeListLimit}: Filter eingrenzen.", style = MaterialTheme.typography.bodySmall)
                        }
                    } else {
                        Text(
                            friendlyProviderError(result.detail ?: "Der Anbieter hat keine Modellliste geliefert."),
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Text(
                            "Manche Anbieter (z. B. Azure) listen nur eigene Bereitstellungen oder gar keine Modelle. Trage den Namen dann von Hand ein.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                error?.let { Text(friendlyProviderError(it), color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !saving && models.any { it.isNotBlank() },
                onClick = {
                    saving = true
                    error = null
                    scope.launch {
                        try {
                            withContext(Dispatchers.IO) {
                                store.setRouteModels(route.id, models.map { it.trim() }.filter { it.isNotEmpty() })
                                contextWindow.toULongOrNull()?.takeIf { it >= 4096uL }?.let { store.setRouteContextWindow(route.id, it) }
                            }
                            onDone(true)
                        } catch (e: Exception) {
                            error = e.message ?: "Speichern fehlgeschlagen"
                            saving = false
                        }
                    }
                },
            ) { Text(if (saving) "Speichere …" else "Speichern") }
        },
        dismissButton = { TextButton(enabled = !saving, onClick = { onDone(false) }) { Text("Abbrechen") } },
    )
}
