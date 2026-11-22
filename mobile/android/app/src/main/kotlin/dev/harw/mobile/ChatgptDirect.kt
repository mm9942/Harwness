package dev.harw.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
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
import androidx.compose.ui.unit.dp
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ChatgptLocal
import uniffi.harw_mobile_ffi.ChatgptLocalStatus
import uniffi.harw_mobile_ffi.ChatgptProviderCredential
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ProviderStore

/** Id der Provider-Route, die `ChatgptLocal.link` im App-Home anlegt. */
private const val CHATGPT_ROUTE = "chatgpt"
private const val CHATGPT_MODEL_LIMIT = 40
private const val CHATGPT_FAILED = "Die ChatGPT-Anmeldung wurde nicht abgeschlossen. Versuche es erneut oder wähle ein anderes Konto."

private fun chatgptHostId(context: Context): String {
    val prefs = context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE)
    prefs.getString("chatgpt_local_host", null)?.let { return it }
    val created = "urn:uuid:" + UUID.randomUUID()
    prefs.edit().putString("chatgpt_local_host", created).apply()
    return created
}

private fun chatgptError(error: Exception): String = when (error) {
    is SiwcFailure -> CHATGPT_FAILED
    is MobileException.Denied -> "OpenAI hat unzureichende oder ungültige Zugangsdaten geliefert. Melde dich erneut an."
    is MobileException -> error.message ?: CHATGPT_FAILED
    else -> CHATGPT_FAILED
}

/**
 * "ChatGPT direkt" im Standalone-Modus: Anmeldung mit dem ChatGPT-Abo auf dem Telefon,
 * Modellwahl und Abmelden. Die Zugangsdaten liegen im Android-Keystore (Slot `chatgpt-direct`).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun ChatgptDirectCard(providers: ProviderStore) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val local = remember { ChatgptLocal(context.filesDir.path, AndroidPlatformKeystore(context.applicationContext)) }
    var status by remember { mutableStateOf<ChatgptLocalStatus?>(null) }
    var busy by remember { mutableStateOf(false) }
    var phase by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(null) }
    var notice by remember { mutableStateOf<String?>(null) }
    var models by remember { mutableStateOf<List<String>>(emptyList()) }
    var modelQuery by remember { mutableStateOf("") }
    var confirmSignOut by remember { mutableStateOf(false) }
    var signInJob by remember { mutableStateOf<Job?>(null) }

    suspend fun reload() {
        status = withContext(Dispatchers.IO) { local.status() }
    }

    LaunchedEffect(Unit) {
        try {
            reload()
        } catch (e: Exception) {
            message = chatgptError(e)
        }
    }

    fun signIn(newAccount: Boolean) {
        if (busy) return
        busy = true
        message = null
        notice = null
        signInJob = scope.launch {
            try {
                val current = status
                val returning = if (newAccount || current?.linked != true) null else current
                val tokens = signInWithChatgpt(
                    hostId = chatgptHostId(context),
                    returningClient = returning?.clientId,
                    expectedSubject = returning?.subject,
                    openBrowser = { url ->
                        try {
                            context.startActivity(
                                Intent(Intent.ACTION_VIEW, Uri.parse(url))
                                    .addCategory(Intent.CATEGORY_BROWSABLE)
                                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                            )
                            phase = "Browser geöffnet. Die Anmeldung läuft nach fünf Minuten ab."
                        } catch (_: ActivityNotFoundException) {
                            throw SiwcFailure("No web browser is available to sign in to ChatGPT.")
                        }
                    },
                    onVerifying = { phase = "Anmeldung wird geprüft …" },
                )
                phase = "Konto wird verbunden …"
                status = withContext(Dispatchers.IO) {
                    local.link(
                        ChatgptProviderCredential(
                            tokens.clientId, tokens.accessToken, tokens.refreshToken, tokens.idToken,
                            tokens.scope, tokens.expiresAt, tokens.earliestRefreshAt,
                            tokens.identity.subject, tokens.identity.email,
                        ),
                    )
                }
                notice = "ChatGPT ist verbunden. Wähle jetzt ein Modell."
            } catch (e: CancellationException) {
                notice = "Anmeldung abgebrochen."
                throw e
            } catch (e: Exception) {
                message = if (e is SiwcFailure && e.message?.contains("No web browser") == true) {
                    "Kein Browser gefunden, der die ChatGPT-Anmeldung öffnen könnte."
                } else {
                    chatgptError(e)
                }
            } finally {
                phase = null
                busy = false
                signInJob = null
            }
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
                message = chatgptError(e)
            }
            busy = false
        }
    }

    val linked = status?.linked == true
    Surface(tonalElevation = 2.dp, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("ChatGPT direkt", style = MaterialTheme.typography.titleMedium)
            Text(
                "Verbinde dein ChatGPT-Konto direkt mit harw. Verfügbare Modelle und Nutzung richten sich nach " +
                    "deinem verbundenen Konto und Abo. Die Zugangsdaten bleiben verschlüsselt auf diesem Telefon.",
                style = MaterialTheme.typography.bodyMedium,
            )
            message?.let { StatusMessage(it, error = true) }
            notice?.let { StatusMessage(it) }
            phase?.let { StatusMessage(it) }

            val current = status
            if (current != null && linked) {
                val account = current.email ?: "ChatGPT-Konto"
                Text(
                    if (current.needsReauth) "$account · Neuanmeldung nötig" else "Verbunden: $account",
                    style = MaterialTheme.typography.bodyMedium,
                )
                if (current.active && current.model.isNotEmpty()) {
                    Text("Aktives Modell: ${current.model}", color = MaterialTheme.colorScheme.primary)
                } else {
                    Text("Noch kein Modell aktiv.", style = MaterialTheme.typography.labelMedium)
                }
            } else if (current != null) {
                Text("Nicht angemeldet.", style = MaterialTheme.typography.bodyMedium)
            }

            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (signInJob != null) {
                    OutlinedButton(onClick = { signInJob?.cancel() }) { Text("Abbrechen") }
                } else {
                    Button(enabled = !busy && status != null, onClick = { signIn(newAccount = false) }) {
                        Text(if (!linked) "Mit ChatGPT anmelden" else if (status?.needsReauth == true) "Erneut anmelden" else "Neu anmelden")
                    }
                    if (linked) {
                        OutlinedButton(enabled = !busy, onClick = { signIn(newAccount = true) }) { Text("Anderes Konto") }
                        OutlinedButton(
                            enabled = !busy && status?.needsReauth != true,
                            onClick = {
                                launchOp {
                                    models = withContext(Dispatchers.IO) { local.listModels() }
                                    modelQuery = ""
                                    if (models.isEmpty()) {
                                        notice = "Für dieses ChatGPT-Konto sind aktuell keine Modelle verfügbar. " +
                                            "Du kannst eine Modell-ID von Hand eintragen."
                                    }
                                }
                            },
                        ) { Text("Modelle laden") }
                        TextButton(enabled = !busy, onClick = { confirmSignOut = true }) { Text("Abmelden") }
                    }
                }
            }

            if (linked && status?.needsReauth != true) {
                OutlinedTextField(
                    value = modelQuery,
                    onValueChange = { modelQuery = it.trim().take(256) },
                    label = { Text(if (models.isEmpty()) "Modell-ID" else "Modelle suchen oder Modell-ID eintragen") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                val shown = models.filter { it.contains(modelQuery, ignoreCase = true) }
                if (models.isNotEmpty()) {
                    Text("${shown.size} von ${models.size} Modellen", style = MaterialTheme.typography.labelMedium)
                }
                FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    shown.take(CHATGPT_MODEL_LIMIT).forEach { id ->
                        FilterChip(
                            selected = status?.active == true && status?.model == id,
                            enabled = !busy,
                            onClick = {
                                launchOp {
                                    withContext(Dispatchers.IO) { providers.activateRoute(CHATGPT_ROUTE, id) }
                                    notice = "Modell $id ist aktiv."
                                }
                            },
                            label = { Text(id) },
                        )
                    }
                }
                if (modelQuery.isNotEmpty() && modelQuery !in models) {
                    OutlinedButton(
                        enabled = !busy,
                        onClick = {
                            val id = modelQuery
                            launchOp {
                                withContext(Dispatchers.IO) { providers.activateRoute(CHATGPT_ROUTE, id) }
                                notice = "Modell $id ist aktiv."
                            }
                        },
                    ) { Text("Modell „$modelQuery“ verwenden") }
                }
            }
        }
    }

    if (confirmSignOut) {
        AlertDialog(
            onDismissRequest = { confirmSignOut = false },
            title = { Text("Von ChatGPT abmelden?") },
            text = {
                Text(
                    "Die Zugangsdaten werden von diesem Telefon gelöscht. Eine Abmeldung bei OpenAI wird nicht " +
                        "bestätigt; trenne harw bei Bedarf in den ChatGPT-Einstellungen.",
                )
            },
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
