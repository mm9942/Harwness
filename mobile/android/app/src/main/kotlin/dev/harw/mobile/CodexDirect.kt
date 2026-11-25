package dev.harw.mobile

import android.content.Context
import android.system.Os
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
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.CodexLocal
import uniffi.harw_mobile_ffi.CodexLocalStatus
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ProviderStore

/** Id der Provider-Route, die `CodexLocal` im App-Home anlegt. */
private const val CODEX_ROUTE = "codex"
private const val CODEX_FAILED = "Die Codex-Anmeldung wurde nicht abgeschlossen. Versuche es erneut."

private fun codexError(error: Exception): String = when (error) {
    is MobileException -> error.message ?: CODEX_FAILED
    else -> CODEX_FAILED
}

/**
 * Der Codex-Provider liest `$HOME/.codex/auth.json`; auf Android zeigt `HOME` nicht auf das App-Verzeichnis.
 * Setzt `HOME` auf `filesDir` (idempotent). Wird beim App-Start und vor der Codex-Anmeldung aufgerufen.
 */
internal fun ensureCodexHome(context: Context) {
    runCatching { Os.setenv("HOME", context.filesDir.path, true) }
}

/** Wartet auf die Loopback-Rückleitung des Codex-Logins (127.0.0.1:[port]) und liefert ihr Request-Target. */
private suspend fun waitForCodexCallback(port: Int, path: String, openBrowser: suspend () -> Unit): String =
    withContext(Dispatchers.IO) {
        ServerSocket().use { server ->
            server.reuseAddress = true
            server.bind(InetSocketAddress(InetAddress.getByAddress(byteArrayOf(127, 0, 0, 1)), port), 4)
            server.soTimeout = 350
            openBrowser()
            val deadline = System.nanoTime() + TimeUnit.MINUTES.toNanos(5)
            while (System.nanoTime() < deadline) {
                currentCoroutineContext().ensureActive()
                val socket = try {
                    server.accept()
                } catch (_: SocketTimeoutException) {
                    continue
                }
                val target = socket.use { handleCodexCallback(it, path) }
                if (target != null) return@withContext target
            }
            throw MobileException.Denied("Die Codex-Anmeldung ist abgelaufen. Starte sie erneut.")
        }
    }

private fun handleCodexCallback(socket: Socket, path: String): String? {
    socket.soTimeout = 1_500
    val target = runCatching {
        val line = socket.getInputStream().bufferedReader(Charsets.US_ASCII).readLine().orEmpty()
        val parts = line.split(' ')
        parts.takeIf { it.size == 3 && it[0] == "GET" && it[1].substringBefore('?') == path }
            ?.get(1)
    }.getOrNull()
    respondToCodexCallback(socket, target != null)
    return target
}

private fun respondToCodexCallback(socket: Socket, accepted: Boolean) {
    val message = if (accepted) {
        "Zurück zu harw, um die Codex-Anmeldung abzuschließen."
    } else {
        "Diese Anfrage gehört nicht zur harw-Anmeldung."
    }
    val body = "<!doctype html><meta charset=utf-8><title>harw</title><p>$message</p>".encodeToByteArray()
    val head = "HTTP/1.1 ${if (accepted) "200 OK" else "404 Not Found"}\r\n" +
        "Content-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n" +
        "Referrer-Policy: no-referrer\r\nConnection: close\r\nContent-Length: ${body.size}\r\n\r\n"
    runCatching {
        socket.getOutputStream().apply {
            write(head.encodeToByteArray())
            write(body)
            flush()
        }
    }
}

/**
 * "Codex" im Standalone-Modus: Anmeldung mit dem ChatGPT-/Codex-Abo auf dem Telefon, Modellwahl und
 * Abmelden. Die Zugangsdaten liegen im Android-Keystore (Slot `codex-direct`) und in `files/.codex/auth.json`.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun CodexDirectCard(providers: ProviderStore, onOpenConnection: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val local = remember {
        ensureCodexHome(context)
        CodexLocal(context.filesDir.path, AndroidPlatformKeystore(context.applicationContext))
    }
    var status by remember { mutableStateOf<CodexLocalStatus?>(null) }
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
            message = codexError(e)
        }
    }

    fun signIn() {
        if (busy) return
        busy = true
        message = null
        notice = null
        signInJob = scope.launch {
            try {
                ensureCodexHome(context)
                val start = withContext(Dispatchers.IO) { local.beginSignIn() }
                val target = waitForCodexCallback(start.port.toInt(), start.path) {
                    withContext(Dispatchers.Main.immediate) {
                        if (openInBrowser(context, start.url)) {
                            phase = "Browser geöffnet. Die Anmeldung läuft nach fünf Minuten ab."
                        } else {
                            throw MobileException.Denied("Kein Browser gefunden, der die Codex-Anmeldung öffnen könnte.")
                        }
                    }
                }
                phase = "Konto wird verbunden …"
                status = withContext(Dispatchers.IO) { local.completeSignIn(target) }
                notice = "Codex ist verbunden. Wähle jetzt ein Modell."
            } catch (e: CancellationException) {
                notice = "Anmeldung abgebrochen."
                throw e
            } catch (e: Exception) {
                message = codexError(e)
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
                message = codexError(e)
            }
            busy = false
        }
    }

    val linked = status?.linked == true
    Surface(tonalElevation = 2.dp, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Codex direkt", style = MaterialTheme.typography.titleMedium)
            Text(
                "Melde dich mit deinem ChatGPT-Abo wie in der Codex-CLI an. Verfügbare Modelle und Nutzung richten " +
                    "sich nach deinem Abo. Die Zugangsdaten bleiben verschlüsselt auf diesem Telefon.",
                style = MaterialTheme.typography.bodyMedium,
            )
            message?.let { StatusMessage(it, error = true) }
            notice?.let { StatusMessage(it) }
            phase?.let { StatusMessage(it) }

            val current = status
            if (current != null && linked) {
                Text("Verbunden: ${current.email ?: "Codex-Konto"}", style = MaterialTheme.typography.bodyMedium)
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
                    Button(enabled = !busy && status != null, onClick = { signIn() }) {
                        Text(if (linked) "Neu anmelden" else "Mit Codex anmelden")
                    }
                    if (linked) {
                        OutlinedButton(
                            enabled = !busy,
                            onClick = {
                                launchOp {
                                    ensureCodexHome(context)
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
                            ensureCodexHome(context)
                            withContext(Dispatchers.IO) { providers.activateRoute(CODEX_ROUTE, id) }
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
            title = { Text("Von Codex abmelden?") },
            text = { Text("Die Zugangsdaten werden von diesem Telefon gelöscht. Das Konto kannst du in den ChatGPT-Einstellungen trennen.") },
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
