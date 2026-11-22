package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ProviderKind
import uniffi.harw_mobile_ffi.ProviderRoute
import uniffi.harw_mobile_ffi.ProviderStore
import uniffi.harw_mobile_ffi.ProviderValidation
import uniffi.harw_mobile_ffi.ValidationOutcome
import uniffi.harw_mobile_ffi.providerDefaults

private const val MAX_SHOWN_MODELS = 50

private val KIND_CHOICES = listOf(
    ProviderKind.ANTHROPIC to "Claude (Anthropic)",
    ProviderKind.OPEN_AI to "OpenAI",
    ProviderKind.DASH_SCOPE to "Qwen Cloud (DashScope)",
    ProviderKind.OPEN_ROUTER to "OpenRouter",
    ProviderKind.OPEN_AI_COMPATIBLE to "Custom (OpenAI-compatible)",
)

private fun kindLabel(kind: ProviderKind): String =
    if (kind == ProviderKind.LOCAL_GGUF) "On this device" else KIND_CHOICES.firstOrNull { it.first == kind }?.second ?: kind.name

private fun failureText(error: Exception): String =
    if (error is MobileException) error.message ?: "Der Vorgang ist fehlgeschlagen." else "Der Vorgang ist fehlgeschlagen."

private fun validationText(v: ProviderValidation): String =
    when (v.outcome) {
        ValidationOutcome.VALID -> "Schlüssel funktioniert – ${v.modelCount} Modelle"
        ValidationOutcome.DISCOVERY_UNSUPPORTED -> "Gespeichert – dieser Endpunkt listet keine Modelle"
        ValidationOutcome.REJECTED -> "Der Anbieter hat den Schlüssel abgelehnt (HTTP ${v.httpStatus?.toString() ?: "?"})"
        ValidationOutcome.UNREACHABLE -> "Anbieter nicht erreichbar"
        ValidationOutcome.FAILED -> "Anbieterfehler (HTTP ${v.httpStatus?.toString() ?: "?"})"
        ValidationOutcome.INVALID_ROUTE -> v.detail ?: "Ungültige Anbieter-Route"
        ValidationOutcome.NOT_CHECKED -> "Ohne Prüfung gespeichert (Prüfung in diesem Build nicht verfügbar)"
    }

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ProvidersScreen(
    store: ProviderStore,
    onBack: () -> Unit,
    onOpenConnection: () -> Unit = {},
    onOpenLocalModels: () -> Unit = {},
) {
    var mode by rememberSaveable { mutableStateOf(ProviderMode.Api) }
    var presetQuery by remember { mutableStateOf("") }
    var presetGroup by remember { mutableStateOf("Alle") }
    var presetId by remember { mutableStateOf<String?>(null) }
    var modelQuery by remember { mutableStateOf("") }
    val scope = rememberCoroutineScope()
    var routes by remember { mutableStateOf<List<ProviderRoute>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var result by remember { mutableStateOf<ProviderValidation?>(null) }
    var resultRouteId by remember { mutableStateOf("") }
    var pendingDelete by remember { mutableStateOf<ProviderRoute?>(null) }

    var kind by remember { mutableStateOf(ProviderKind.ANTHROPIC) }
    var id by remember { mutableStateOf("") }
    var baseUrl by remember { mutableStateOf(providerDefaults(ProviderKind.ANTHROPIC).baseUrl) }
    var model by remember { mutableStateOf(providerDefaults(ProviderKind.ANTHROPIC).suggestedModel) }
    var key by remember { mutableStateOf("") }
    val defaults = providerDefaults(kind)

    // Runs one store call off the main thread and reloads the route list afterwards.
    fun run(block: suspend () -> Unit) {
        if (busy) return
        busy = true
        message = null
        scope.launch {
            try {
                block()
            } catch (e: Exception) {
                message = failureText(e)
            }
            try {
                routes = withContext(Dispatchers.IO) { store.routes() }
            } catch (e: Exception) {
                message = failureText(e)
            }
            busy = false
        }
    }

    LaunchedEffect(Unit) {
        busy = true
        try {
            routes = withContext(Dispatchers.IO) { store.routes() }
        } catch (e: Exception) {
            message = failureText(e)
        }
        busy = false
    }

    Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                IconButton(onClick = onBack) {
                    Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "Zurück")
                }
                Text("Provider", style = MaterialTheme.typography.titleLarge)
            }
            message?.let { Text(friendlyProviderError(it), color = MaterialTheme.colorScheme.error) }
            Text(
                "Wähle eine Verbindung. Cloud-Anfragen gehen an den gewählten Anbieter; lokale Modelle verarbeiten Text auf diesem Gerät.",
                style = MaterialTheme.typography.bodyMedium,
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                ProviderMode.entries.forEach { m ->
                    FilterChip(selected = mode == m, onClick = { mode = m }, label = { Text(m.label) })
                }
            }
            if (mode == ProviderMode.ChatGpt) {
                ChatgptDirectCard(store)
            } else if (mode != ProviderMode.Api) {
                ModeInfo(mode, onOpenConnection, onOpenLocalModels)
            }
            if (mode == ProviderMode.Api) {
            if (routes.isEmpty()) {
                Text("Noch keine Provider gespeichert.", style = MaterialTheme.typography.bodyMedium)
            }
            routes.forEach { route ->
                Surface(
                    tonalElevation = 2.dp,
                    shape = MaterialTheme.shapes.medium,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(route.id, style = MaterialTheme.typography.titleMedium)
                        Text(kindLabel(route.kind), style = MaterialTheme.typography.bodySmall)
                        Text(route.baseUrl, style = MaterialTheme.typography.bodySmall)
                        if (route.defaultModel.isNotEmpty()) {
                            Text(route.defaultModel, style = MaterialTheme.typography.bodySmall)
                        }
                        if (route.active) {
                            Text("Aktiv", color = MaterialTheme.colorScheme.primary)
                        } else if (route.hasSecret) {
                            Text("Schlüssel gespeichert", style = MaterialTheme.typography.labelMedium)
                        }
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(
                                enabled = !busy,
                                onClick = {
                                    run {
                                        val v = withContext(Dispatchers.IO) { store.validateRoute(route.id) }
                                        result = v
                                        resultRouteId = route.id
                                    }
                                },
                            ) { Text("Prüfen") }
                            if (route.defaultModel.isNotEmpty()) {
                                OutlinedButton(
                                    enabled = !busy,
                                    onClick = {
                                        run {
                                            withContext(Dispatchers.IO) {
                                                store.activateRoute(route.id, route.defaultModel)
                                            }
                                        }
                                    },
                                ) { Text("Aktivieren") }
                            }
                            TextButton(enabled = !busy, onClick = { pendingDelete = route }) { Text("Löschen") }
                        }
                    }
                }
            }

            HorizontalDivider()
            Text("Provider hinzufügen", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(
                value = presetQuery,
                onValueChange = { presetQuery = it },
                label = { Text("Anbieter oder Plattform suchen") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PROVIDER_GROUPS.forEach { g ->
                    FilterChip(selected = presetGroup == g, onClick = { presetGroup = g }, label = { Text(g) })
                }
            }
            val presets = searchPresets(presetQuery, presetGroup)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                presets.forEach { p ->
                    FilterChip(
                        selected = presetId == p.id,
                        enabled = !busy,
                        onClick = {
                            presetId = p.id
                            p.kind?.let { kind = it }
                            baseUrl = p.endpoint
                            model = p.model
                            if (id.isBlank() && !p.id.startsWith("custom-")) id = p.id
                        },
                        label = { Text(p.label) },
                    )
                }
            }
            if (presets.isEmpty()) Text("Keine passende Vorlage.", style = MaterialTheme.typography.bodySmall)
            val chosen = presetId?.let(::presetById)
            chosen?.let { p ->
                Text(p.description, style = MaterialTheme.typography.bodySmall)
                Text(p.endpointHint, style = MaterialTheme.typography.bodySmall)
                Text(p.modelHint, style = MaterialTheme.typography.bodySmall)
                p.blocker?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
            Text("Protokollfamilie", style = MaterialTheme.typography.labelMedium)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                KIND_CHOICES.forEach { (k, label) ->
                    FilterChip(
                        selected = kind == k,
                        enabled = !busy,
                        onClick = {
                            kind = k
                            val d = providerDefaults(k)
                            baseUrl = d.baseUrl
                            model = d.suggestedModel
                        },
                        label = { Text(label) },
                    )
                }
            }
            OutlinedTextField(
                value = id,
                onValueChange = { id = it },
                label = { Text("Id (Kleinbuchstaben, Ziffern, Bindestriche)") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = baseUrl,
                onValueChange = { baseUrl = it },
                label = { Text("Basis-URL des API-Endpunkts") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = model,
                onValueChange = { model = it },
                label = { Text("Modell-ID oder Deployment-Name") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = key,
                onValueChange = { key = it },
                label = { Text(chosen?.credentialLabel ?: if (defaults.secretRequired) "API-Schlüssel" else "API-Schlüssel (optional)") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(
                    keyboardType = KeyboardType.Password,
                    autoCorrectEnabled = false,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                enabled = !busy && id.isNotBlank() && chosen?.blocker == null,
                onClick = {
                    val problem = chosen?.let { validatePreset(it, baseUrl, model) }
                    if (problem != null) {
                        message = problem
                    } else {
                    val bytes = key.toByteArray(Charsets.UTF_8)
                    key = ""
                    val newRoute = ProviderRoute(
                        id = id.trim(),
                        kind = kind,
                        baseUrl = baseUrl.trim(),
                        defaultModel = model.trim(),
                        hasSecret = false,
                        active = false,
                    )
                    run {
                        try {
                            val v = withContext(Dispatchers.IO) { store.saveRoute(newRoute, bytes) }
                            result = v
                            resultRouteId = newRoute.id
                        } finally {
                            bytes.fill(0)
                        }
                    }
                    }
                },
            ) { Text("Prüfen und speichern") }

            result?.let { v ->
                Surface(
                    tonalElevation = 2.dp,
                    shape = MaterialTheme.shapes.medium,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(friendlyProviderError(validationText(v)))
                        Text(if (v.saved) "Gespeichert" else "Nicht gespeichert", style = MaterialTheme.typography.labelMedium)
                        if (v.models.isNotEmpty()) {
                            Text("Modell antippen, um es zu aktivieren", style = MaterialTheme.typography.labelMedium)
                            OutlinedTextField(
                                value = modelQuery,
                                onValueChange = { modelQuery = it },
                                label = { Text("Modelle suchen") },
                                singleLine = true,
                                modifier = Modifier.fillMaxWidth(),
                            )
                            val shown = v.models.filter { it.contains(modelQuery.trim(), ignoreCase = true) }
                            Text("${shown.size} verfügbare Modelle", style = MaterialTheme.typography.labelMedium)
                            shown.take(MAX_SHOWN_MODELS).forEach { m ->
                                TextButton(
                                    enabled = !busy,
                                    onClick = {
                                        run { withContext(Dispatchers.IO) { store.activateRoute(resultRouteId, m) } }
                                    },
                                ) { Text(m) }
                            }
                        }
                    }
                }
            }
            } // Ende Modus API
        }
    }

    pendingDelete?.let { target ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("Provider löschen?") },
            text = { Text("${target.id} und den gespeicherten Schlüssel entfernen.") },
            confirmButton = {
                TextButton(onClick = {
                    pendingDelete = null
                    run { withContext(Dispatchers.IO) { store.deleteRoute(target.id) } }
                }) { Text("Löschen") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Abbrechen") } },
        )
    }
}

private enum class ProviderMode(val label: String) {
    ChatGpt("ChatGPT direkt"),
    Api("API"),
    Device("Auf diesem Gerät"),
    Runtime("Codex / Claude"),
}

@Composable
private fun ModeInfo(mode: ProviderMode, onOpenConnection: () -> Unit, onOpenLocalModels: () -> Unit) {
    Surface(tonalElevation = 2.dp, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            when (mode) {
                ProviderMode.ChatGpt -> {
                    Text("ChatGPT direkt", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Verbinde dein ChatGPT-Konto direkt mit harw. Verfügbare Modelle und Nutzung richten sich nach " +
                            "deinem verbundenen Konto und Abo. Die Anmeldung läuft über den verbundenen harw-Server " +
                            "(Karte „ChatGPT-Konto“ unter Verbindung); sie läuft nach fünf Minuten ab.",
                    )
                    Button(onClick = onOpenConnection) { Text("Verbindung öffnen") }
                }
                ProviderMode.Device -> {
                    Text("Auf diesem Gerät", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Lokale Modelle verarbeiten Text auf diesem Gerät. Lade ein GGUF-Modell herunter oder " +
                            "importiere eine Datei. Die lokale Ausführung folgt in einer späteren Version.",
                    )
                    Button(onClick = onOpenLocalModels) { Text("Lokale Modelle öffnen") }
                }
                ProviderMode.Runtime -> {
                    Text("Codex / Claude auf dem Rechner", style = MaterialTheme.typography.titleMedium)
                    Text(
                        "Verbinde die App mit der harw-Instanz auf deinem Rechner, auf der Codex oder Claude " +
                            "laufen. Anbieter und Modelle des Rechners verwaltest du danach unter " +
                            "„Modelle & Provider“.",
                    )
                    Button(onClick = onOpenConnection) { Text("Mit Rechner verbinden") }
                }
                ProviderMode.Api -> Unit
            }
        }
    }
}
