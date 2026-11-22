package dev.harw.mobile

import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.DeviceException
import uniffi.harw_mobile_ffi.ShellRequest

private const val TERMUX_PKG = "com.termux"
private const val TERMUX_RUN_PERMISSION = "com.termux.permission.RUN_COMMAND"
private const val TERMUX_SH = "/data/data/com.termux/files/usr/bin/sh"

private fun termuxFailure(e: Exception): String = when (e) {
    is DeviceException.Disabled -> "Termux ist in der App ausgeschaltet."
    is DeviceException.Unavailable -> "Termux ist nicht bereit: Installation, Berechtigung und allow-external-apps prüfen."
    is DeviceException.Declined -> "Der Befehl wurde nicht freigegeben."
    is DeviceException -> "Der Befehl wurde abgelehnt oder ist fehlgeschlagen."
    else -> "Der Vorgang ist fehlgeschlagen."
}

/**
 * Termux: Befehle über Termux RUN_COMMAND ausführen (Terminal des Kits, TerminalPane).
 * Der Schalter, die Berechtigung und der sichtbare Befehl sind die Freigabe; der Agent-Pfad
 * nutzt dieselbe Brücke mit eigener Freigabe-Rückfrage.
 */
@Composable
internal fun TermuxScreen(onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    // Der Nutzer tippt den Befehl hier selbst und sieht ihn: keine zweite Rückfrage.
    val bridge = remember { TermuxBridge(context.applicationContext, CommandApprover { _, _ -> true }) }
    var enabled by remember { mutableStateOf(bridge.isEnabled()) }
    var granted by remember {
        mutableStateOf(context.checkSelfPermission(TERMUX_RUN_PERMISSION) == PackageManager.PERMISSION_GRANTED)
    }
    val installed = remember {
        try {
            context.packageManager.getPackageInfo(TERMUX_PKG, 0)
            true
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }
    }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted = it }
    var command by remember { mutableStateOf("") }
    var running by remember { mutableStateOf(false) }
    var output by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Termux", onBack = onBack) {}
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(
                "Führt Befehle in Termux auf diesem Gerät aus. Voraussetzungen: Termux ist installiert und in " +
                    "~/.termux/termux.properties steht allow-external-apps=true.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                if (installed) "Termux: installiert" else "Termux: nicht gefunden (aus F-Droid installieren)",
                color = if (installed) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.error,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Termux-Befehle in harw erlauben", Modifier.weight(1f))
                Switch(checked = enabled, onCheckedChange = {
                    enabled = it
                    bridge.setEnabled(it)
                })
            }
            if (!granted) {
                Text(
                    "Berechtigung „Befehle in Termux ausführen“ fehlt.",
                    color = MaterialTheme.colorScheme.error,
                )
                OutlinedButton(onClick = { permission.launch(TERMUX_RUN_PERMISSION) }) { Text("Berechtigung anfragen") }
            }
            OutlinedTextField(
                value = command,
                onValueChange = { command = it },
                label = { Text("Befehl (sh -c)") },
                modifier = Modifier.fillMaxWidth(),
            )
            Button(
                enabled = enabled && installed && granted && !running && command.isNotBlank(),
                onClick = {
                    val text = command
                    running = true
                    error = null
                    output = null
                    scope.launch {
                        try {
                            val result = withContext(Dispatchers.IO) {
                                bridge.run(
                                    ShellRequest(
                                        argv = listOf(TERMUX_SH, "-c", text),
                                        cwd = null,
                                        timeoutMs = SHELL_DEFAULT_TIMEOUT_MS.toULong(),
                                        maxOutputBytes = SHELL_MAX_OUTPUT_BYTES.toUInt(),
                                    ),
                                )
                            }
                            output = buildString {
                                if (result.timedOut) appendLine("Zeitüberschreitung.")
                                result.exitCode?.let { appendLine("Exit-Code: $it") }
                                if (result.stdout.isNotEmpty()) appendLine(result.stdout)
                                if (result.stderr.isNotEmpty()) appendLine(result.stderr)
                                if (result.truncated) appendLine("(Ausgabe gekürzt)")
                            }
                        } catch (e: Exception) {
                            error = termuxFailure(e)
                        }
                        running = false
                    }
                },
            ) { Text(if (running) "Läuft …" else "Ausführen") }
            error?.let { StatusMessage(it, error = true) }
            output?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
    }
}
