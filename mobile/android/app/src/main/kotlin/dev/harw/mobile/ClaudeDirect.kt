package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ClaudeLocal
import uniffi.harw_mobile_ffi.ClaudeLocalStatus
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ProviderStore

/** Id der Provider-Route, die `ClaudeLocal` im App-Home anlegt. */
private const val CLAUDE_ROUTE = "claude"
private const val CLAUDE_FAILED = "Die Claude-Anmeldung wurde nicht abgeschlossen. Versuche es erneut."

private fun claudeError(error: Exception): String = when (error) {
    is MobileException -> error.message ?: CLAUDE_FAILED
    else -> CLAUDE_FAILED
}

/**
 * "Claude" im Standalone-Modus: Anmeldung mit dem Claude-Abo auf dem Telefon (Code aus dem Browser
 * einfügen oder Setup-Token), Modellwahl und Abmelden. Das Token liegt im Android-Keystore.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ClaudeDirectCard(providers: ProviderStore, onOpenConnection: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val local = remember { ClaudeLocal(context.filesDir.path, AndroidPlatformKeystore(context.applicationContext)) }
    var status by remember { mutableStateOf<ClaudeLocalStatus?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var pastedCode by remember { mutableStateOf("") }
    var pastedToken by remember { mutableStateOf("") }
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var modelQuery by remember { mutableStateOf("") }
    var confirmSignOut by remember { mutableStateOf(false) }

    suspend fun reload() {
        status = withContext(Dispatchers.IO) { local.status() }
    }

    LaunchedEffect(Unit) {
        try {
            reload()
        } catch (e: Exception) {
            message = claudeError(e)
        }
    }

    fun launchOp(block: suspend () -> Unit) {
        if (busy) return
        busy = true
        message = null
        notice = null
        scope.launch {
            try {
                block()
                reload()
            } catch (e: Exception) {
                message = claudeError(e)
            }
            busy = false
        }
    }

    val linked = status?.linked == true
    Surface(tonalElevation = 2.dp, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Claude direkt", style = MaterialTheme.typography.titleMedium)
            Text(
                "Verbinde dein Claude-Abo direkt mit harw. Verfügbare Modelle und Nutzung richten sich nach deinem " +
                    "Abo. Das Token bleibt verschlüsselt auf diesem Telefon.",
                style = MaterialTheme.typography.bodyMedium,
            )
            message?.let { StatusMessage(it, error = true) }
            notice?.let { StatusMessage(it) }

            val current = status
            if (current != null && linked) {
                Text("Verbunden mit dem Claude-Abo.", style = MaterialTheme.typography.bodyMedium)
                if (current.active && current.model.isNotEmpty()) {
                    Text("Aktives Modell: ${current.model}", color = MaterialTheme.colorScheme.primary)
                } else {
                    Text("Noch kein Modell aktiv.", style = MaterialTheme.typography.labelMedium)
                }
            } else if (current != null) {
                Text("Nicht angemeldet.", style = MaterialTheme.typography.bodyMedium)
            }

            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    enabled = !busy && status != null,
                    onClick = {
                        launchOp {
                            val url = withContext(Dispatchers.IO) { local.beginSignIn() }
                            if (openInBrowser(context, url)) {
                                notice = "Browser geöffnet. Melde dich an und füge den angezeigten Code unten ein."
                            } else {
                                message = "Kein Browser gefunden, der die Claude-Anmeldung öffnen könnte."
                            }
                        }
                    },
                ) { Text(if (linked) "Neu anmelden" else "Mit Claude anmelden") }
                if (linked) {
                    OutlinedButton(
                        enabled = !busy,
                        onClick = {
                            launchOp {
                                models = withContext(Dispatchers.IO) { local.listModels() }
                                modelQuery = ""
                                if (models.isEmpty()) {
                                    notice = "Für dieses Konto sind aktuell keine Modelle verfügbar. " +
                                        "Du kannst eine Modell-ID von Hand eintragen."
                                }
                            }
                        },
                    ) { Text("Modelle laden") }
                    TextButton(enabled = !busy, onClick = { confirmSignOut = true }) { Text("Abmelden") }
                }
            }

            if (status?.signInPending == true) {
                OutlinedTextField(
                    value = pastedCode,
                    onValueChange = { pastedCode = it.trim().take(4096) },
                    label = { Text("Code aus dem Browser") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth(),
                )
                Button(
                    enabled = !busy && pastedCode.isNotEmpty(),
                    onClick = {
                        val code = pastedCode
                        launchOp {
                            withContext(Dispatchers.IO) { local.completeSignIn(code) }
                            pastedCode = ""
                            notice = "Claude ist verbunden. Wähle jetzt ein Modell."
                        }
                    },
                ) { Text("Anmeldung abschließen") }
            }

            if (!linked) {
                OutlinedTextField(
                    value = pastedToken,
                    onValueChange = { pastedToken = it.trim().take(8192) },
                    label = { Text("Oder Setup-Token einfügen (sk-ant-oat…)") },
                    singleLine = true,
                    visualTransformation = PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedButton(
                    enabled = !busy && pastedToken.isNotEmpty(),
                    onClick = {
                        val token = pastedToken
                        launchOp {
                            withContext(Dispatchers.IO) { local.linkToken(token) }
                            pastedToken = ""
                            notice = "Claude ist verbunden. Wähle jetzt ein Modell."
                        }
                    },
                ) { Text("Token speichern") }
            }

            if (linked) {
                SubscriptionModelPicker(
                    models = models,
                    query = modelQuery,
                    onQueryChange = { modelQuery = it },
                    activeModel = if (status?.active == true) status?.model else null,
                    enabled = !busy,
                    onPick = { id ->
                        launchOp {
                            withContext(Dispatchers.IO) { providers.activateRoute(CLAUDE_ROUTE, id) }
                            notice = "Modell $id ist aktiv."
                        }
                    },
                )
            }
            TextButton(onClick = onOpenConnection) { Text("Stattdessen mit Rechner verbinden") }
        }
    }

    if (confirmSignOut) {
        AlertDialog(
            onDismissRequest = { confirmSignOut = false },
            title = { Text("Von Claude abmelden?") },
            text = { Text("Das Token wird von diesem Telefon gelöscht. Widerrufen kannst du es in deinen Claude-Kontoeinstellungen.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmSignOut = false
                        launchOp {
                            withContext(Dispatchers.IO) { local.unlink() }
                            models = emptyList()
                            notice = "Auf diesem Telefon abgemeldet."
                        }
                    },
                ) { Text("Abmelden") }
            },
            dismissButton = { TextButton(onClick = { confirmSignOut = false }) { Text("Abbrechen") } },
        )
    }
}
