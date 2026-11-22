package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
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
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.UiaProfile
import uniffi.harw_mobile_ffi.UiaSetup
import uniffi.harw_mobile_ffi.UiaStore

private const val UIA_FAILED = "Der Vorgang ist fehlgeschlagen."
private const val DEFAULT_PERSONALITY_HINT = "Neutral ist die Standardpersönlichkeit."

private fun uiaFailure(error: Exception): String = when (error) {
    is MobileException.Session -> error.detail
    is MobileException -> error.message ?: UIA_FAILED
    else -> UIA_FAILED
}

/**
 * Agenten-Profile (UIA) im Standalone-Modus: vorhandene UIAs aktivieren, neue anlegen
 * (Name, Selbstbeschreibung, Persönlichkeit, Anrede, Kontext) mit Vorschau.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun UiaScreen(store: UiaStore, onBack: () -> Unit) {
    val scope = rememberCoroutineScope()
    var profiles by remember { mutableStateOf<List<UiaProfile>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var pendingDelete by remember { mutableStateOf<UiaProfile?>(null) }
    var preview by remember { mutableStateOf<String?>(null) }

    var name by rememberSaveable { mutableStateOf("") }
    var identity by rememberSaveable { mutableStateOf("") }
    var personality by rememberSaveable { mutableStateOf("") }
    var userName by rememberSaveable { mutableStateOf("") }
    var userContext by rememberSaveable { mutableStateOf("") }

    fun setup() = UiaSetup(name, identity, personality, userName, userContext)

    // Führt einen Store-Aufruf abseits des Main-Threads aus und lädt danach die Liste neu.
    fun run(block: suspend () -> Unit) {
        if (busy) return
        busy = true
        message = null
        scope.launch {
            try {
                block()
            } catch (e: Exception) {
                message = uiaFailure(e)
            }
            try {
                profiles = withContext(Dispatchers.IO) { store.listUias() }
            } catch (e: Exception) {
                message = uiaFailure(e)
            }
            busy = false
        }
    }

    LaunchedEffect(Unit) { run { } }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Agenten-Profile", onBack = onBack) {}
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            message?.let { StatusMessage(it, error = true) }
            notice?.let { StatusMessage(it) }
            Text(
                "Der UI-Agent ist dein Einstieg: Er nimmt deine Aufträge an und arbeitet im Arbeitsordner " +
                    "dieses Geräts. Eine neue Auswahl gilt ab der nächsten Sitzung.",
                style = MaterialTheme.typography.bodyMedium,
            )

            Text("Vorhandene UI-Agenten", style = MaterialTheme.typography.titleMedium)
            if (profiles.isEmpty()) {
                Text("Noch keine UI-Agenten.", style = MaterialTheme.typography.bodyMedium)
            }
            profiles.forEach { profile ->
                Surface(
                    tonalElevation = 2.dp,
                    shape = MaterialTheme.shapes.medium,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(profile.name, style = MaterialTheme.typography.titleMedium)
                        Text(
                            if (profile.active) "Aktiv" else "Vorhanden",
                            style = MaterialTheme.typography.labelMedium,
                            color = if (profile.active) MaterialTheme.colorScheme.primary
                            else MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (!profile.active) {
                                OutlinedButton(
                                    enabled = !busy,
                                    onClick = {
                                        run {
                                            withContext(Dispatchers.IO) { store.activateUia(profile.id) }
                                            notice = "„${profile.name}“ ist aktiv."
                                        }
                                    },
                                ) { Text("Aktivieren") }
                            }
                            if (profile.id != BUILTIN_UIA_ID) {
                                TextButton(enabled = !busy, onClick = { pendingDelete = profile }) { Text("Löschen") }
                            }
                        }
                    }
                }
            }

            Text("Neuen UI-Agenten anlegen", style = MaterialTheme.typography.titleMedium)
            OutlinedTextField(
                value = name,
                onValueChange = { name = it.take(60) },
                label = { Text("Name des UI-Agents (erforderlich)") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = identity,
                onValueChange = { identity = it.take(500) },
                label = { Text("Selbstbeschreibung (optional)") },
                supportingText = { Text("Leer: „Ich bin <Name>, eine lokale Benutzeroberflächen-Agentin.“") },
                minLines = 2,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = personality,
                onValueChange = { personality = it.take(2000) },
                label = { Text("Persönlichkeit") },
                supportingText = { Text(DEFAULT_PERSONALITY_HINT) },
                minLines = 2,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = userName,
                onValueChange = { userName = it.take(80) },
                label = { Text("Wie soll dich der Agent ansprechen? (optional)") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = userContext,
                onValueChange = { userContext = it.take(2000) },
                label = { Text("Kontext für den Agenten (optional)") },
                minLines = 3,
                modifier = Modifier.fillMaxWidth(),
            )
            StatusMessage("Keine Passwörter, API-Schlüssel oder anderen Geheimnisse eintragen.")
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(
                    enabled = !busy && name.isNotBlank(),
                    onClick = {
                        run { preview = withContext(Dispatchers.IO) { store.previewUia(setup()) } }
                    },
                ) { Text("Vorschau anzeigen") }
            }
        }
    }

    preview?.let { text ->
        AlertDialog(
            onDismissRequest = { preview = null },
            title = { Text("Vorschau") },
            text = {
                Column(Modifier.verticalScroll(rememberScrollState())) {
                    Text(text, style = MaterialTheme.typography.bodySmall)
                    Text(
                        "Der Agent wird erstellt und aktiviert. Trage in den Kontext keine Geheimnisse ein.",
                        style = MaterialTheme.typography.labelMedium,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
            },
            confirmButton = {
                Button(
                    enabled = !busy,
                    onClick = {
                        val request = setup()
                        preview = null
                        run {
                            val created = withContext(Dispatchers.IO) { store.createUia(request) }
                            notice = "„${created.name}“ wurde erstellt und aktiviert."
                            name = ""
                            identity = ""
                            personality = ""
                            userName = ""
                            userContext = ""
                        }
                    },
                ) { Text("Vorschau bestätigen und UI-Agent erstellen") }
            },
            dismissButton = { TextButton(onClick = { preview = null }) { Text("Vorschau schließen") } },
        )
    }

    pendingDelete?.let { profile ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("UI-Agent löschen?") },
            text = { Text("„${profile.name}“ wird von diesem Gerät entfernt.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingDelete = null
                        run {
                            withContext(Dispatchers.IO) { store.deleteUia(profile.id) }
                            notice = "„${profile.name}“ wurde gelöscht."
                        }
                    },
                ) { Text("Löschen") }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Abbrechen") } },
        )
    }
}

/** Id der eingebauten Telefon-UIA (`PHONE_UIA_ID` im Rust-Kern); sie ist nicht löschbar. */
private const val BUILTIN_UIA_ID = "harwness.agent.phone-uia@1"
