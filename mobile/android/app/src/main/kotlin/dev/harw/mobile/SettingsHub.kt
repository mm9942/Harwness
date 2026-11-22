package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.widget.Toast
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ManageConfigEntry
import uniffi.harw_mobile_ffi.ManageSupport

internal enum class SettingsRoute { Hub, Connection, Models, Commands, Providers, Agents, Jobs, LocalModels }

/** Eltern-Route fuer Zurueck; `null` = Einstellungen schliessen. */
internal fun settingsParent(route: SettingsRoute): SettingsRoute? = when (route) {
    SettingsRoute.Hub -> null
    SettingsRoute.Connection, SettingsRoute.Models, SettingsRoute.Commands,
    SettingsRoute.Providers, SettingsRoute.Agents, SettingsRoute.Jobs, SettingsRoute.LocalModels -> SettingsRoute.Hub
}

internal const val UI_PREFS = "harw_ui"
internal const val PREF_THEME = "theme" // "system" | "light" | "dark"
internal const val PREF_SHOW_REASONING = "show_reasoning"

private val THEME_CHOICES = listOf("system" to "Wie System", "light" to "Hell", "dark" to "Dunkel")
private const val NOTIFICATION_ERROR = "Benachrichtigungseinstellungen konnten nicht geöffnet werden"
private const val CONFIG_READONLY_HINT = "Ändern braucht Profil Voll und Step-up"

internal fun readThemePref(context: Context): String = try {
    val value = context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).getString(PREF_THEME, "system")
    if (THEME_CHOICES.any { it.first == value }) value ?: "system" else "system"
} catch (_: RuntimeException) {
    "system"
}

internal fun readShowReasoningPref(context: Context): Boolean = try {
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).getBoolean(PREF_SHOW_REASONING, true)
} catch (_: RuntimeException) {
    true
}

private fun writeThemePref(context: Context, value: String) {
    try {
        context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).edit().putString(PREF_THEME, value).apply()
    } catch (_: RuntimeException) {
    }
}

private fun writeShowReasoningPref(context: Context, value: Boolean) {
    try {
        context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).edit().putBoolean(PREF_SHOW_REASONING, value).apply()
    } catch (_: RuntimeException) {
    }
}

/** Grund, warum Verwalten-Zeilen gesperrt sind; `null` = frei. */
internal fun manageOpenBlock(state: WorkspaceState): String? = when {
    !state.online -> "offline"
    state.manage.support?.manage != true -> NO_MANAGE_TEXT
    else -> null
}

/** Server-Einstellungen sind nur mit Profil Voll und Step-up aenderbar. */
internal fun configWritable(support: ManageSupport?): Boolean =
    support != null && support.manage && support.stepup && support.control

internal fun configApplyLabel(apply: String): String = when (apply) {
    "now" -> "sofort"
    "next_session" -> "ab nächster Session"
    else -> apply
}

private fun phaseLabel(phase: ConnectionPhase): String = when (phase) {
    ConnectionPhase.DISCONNECTED -> "Offline"
    ConnectionPhase.CONNECTING -> "Verbindet"
    ConnectionPhase.CONNECTED -> "Verbunden"
    ConnectionPhase.RECONNECTING -> "Verbindet neu"
    ConnectionPhase.FAILED -> "Fehlgeschlagen"
    ConnectionPhase.REVOKED -> "Zugriff beendet"
}

/** Untertitel der Zeile "Verbindung": Phase und Route, nie Hostname oder Profilname. */
internal fun connectionSubtitle(state: WorkspaceState): String {
    val phase = phaseLabel(state.phase)
    val route = state.activeRoute ?: return phase
    return "$phase · ${route.name.lowercase()}"
}

/** Diagnose OHNE Geheimnisse, Hostnamen, Ids: nur App-Version, Phase, Route-Art, Tor-Zustand, Features/Caps, Zaehler. */
internal fun diagnosticReport(state: WorkspaceState, appVersion: String): String = buildString {
    appendLine("Harw Diagnose")
    appendLine("app: $appVersion")
    appendLine("phase: ${state.phase.name.lowercase()}")
    appendLine("route: ${state.activeRoute?.name?.lowercase() ?: "-"}")
    appendLine("route-wahl: ${state.routePreference.name.lowercase()}")
    state.tor?.let { appendLine("tor: ${it.percent}% bereit=${it.ready}") }
    val support = state.manage.support
    if (support == null) {
        appendLine("verwalten: unbekannt")
    } else {
        appendLine(
            "verwalten: manage=${support.manage} stepup=${support.stepup} observe=${support.observe} " +
                "steer=${support.steer} approve=${support.approve} control=${support.control}",
        )
    }
    appendLine("sessions: ${state.sessions.size}")
    appendLine("projekte: ${state.projects.size}")
    appendLine("befehle: ${state.manage.commands.size}")
}

private fun appVersionOf(context: Context): String = try {
    context.packageManager.getPackageInfo(context.packageName, 0).versionName ?: "?"
} catch (_: Exception) {
    "?"
}

@Composable
internal fun SettingsHub(
    state: WorkspaceState,
    connection: HarwConnection,
    onBack: () -> Unit,
    onOpen: (SettingsRoute) -> Unit,
) {
    val context = LocalContext.current
    val manage = state.manage
    val support = manage.support
    LaunchedEffect(state.online, state.generation) {
        if (state.online) {
            connection.refreshManageSupport()
            connection.refreshConfig()
        }
    }
    var theme by remember { mutableStateOf(readThemePref(context)) }
    var showReasoning by remember { mutableStateOf(readShowReasoningPref(context)) }
    var editing by remember { mutableStateOf<ManageConfigEntry?>(null) }
    val block = manageOpenBlock(state)

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Einstellungen", onBack = onBack) {}
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(vertical = 4.dp)) {
            HubRow("Verbindung", connectionSubtitle(state), null) { onOpen(SettingsRoute.Connection) }
            HubRow("Modelle & Provider", "Rollen, Provider, Modelle laden (wie /models)", block) {
                onOpen(SettingsRoute.Models)
            }
            HubRow("Befehle", "Alle Slash-Befehle des Servers", block) { onOpen(SettingsRoute.Commands) }
            HubRow("Provider (auf diesem Gerät)", "Schlüssel und Routen für den Standalone-Betrieb", null) { onOpen(SettingsRoute.Providers) }
            HubRow("Agents", "Agents der Sitzung", null) { onOpen(SettingsRoute.Agents) }
            HubRow("Jobs", "Hintergrund-Jobs der Sitzung", null) { onOpen(SettingsRoute.Jobs) }
            HubRow("Lokale Modelle", "GGUF-Modelle laden oder importieren", null) { onOpen(SettingsRoute.LocalModels) }

            if (support?.manage == true) {
                HorizontalDivider(Modifier.padding(vertical = 8.dp), color = MaterialTheme.colorScheme.outlineVariant)
                SectionTitle("Server-Einstellungen")
                val writable = configWritable(support)
                if (!writable) {
                    StatusMessage(CONFIG_READONLY_HINT, Modifier.padding(horizontal = 16.dp, vertical = 4.dp))
                }
                manage.configError?.let {
                    StatusMessage(it, Modifier.padding(horizontal = 16.dp, vertical = 4.dp), error = true)
                }
                manage.config.forEach { entry ->
                    HubRow(
                        title = "${entry.key} = ${entry.value}",
                        subtitle = configApplyLabel(entry.apply),
                        block = if (writable) null else CONFIG_READONLY_HINT,
                        showBlock = false,
                    ) { editing = entry }
                }
            }

            HorizontalDivider(Modifier.padding(vertical = 8.dp), color = MaterialTheme.colorScheme.outlineVariant)
            SectionTitle("Darstellung")
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                THEME_CHOICES.forEach { (key, label) ->
                    FilterChip(
                        selected = theme == key,
                        onClick = {
                            theme = key
                            writeThemePref(context, key)
                        },
                        label = { Text(label) },
                    )
                }
            }
            Row(
                Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("Denkzusammenfassungen anzeigen", Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
                Switch(
                    checked = showReasoning,
                    onCheckedChange = {
                        showReasoning = it
                        writeShowReasoningPref(context, it)
                    },
                )
            }

            HorizontalDivider(Modifier.padding(vertical = 8.dp), color = MaterialTheme.colorScheme.outlineVariant)
            HubRow("Benachrichtigungen", "Systemeinstellungen der App öffnen", null) { openNotificationSettings(context) }
            HubRow("Diagnose teilen", "Bericht ohne Geheimnisse und Adressen", null) {
                shareDiagnostic(context, diagnosticReport(state, appVersionOf(context)))
            }
        }
    }

    editing?.let { entry ->
        ConfigEditDialog(
            entry = entry,
            enabled = manage.running == null && configWritable(support),
            onDismiss = { editing = null },
            onSet = { value ->
                connection.setConfig(entry.key, value)
                editing = null
            },
            onReset = {
                connection.resetConfig(entry.key)
                editing = null
            },
        )
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
    )
}

@Composable
private fun HubRow(
    title: String,
    subtitle: String,
    block: String?,
    showBlock: Boolean = true,
    onClick: () -> Unit,
) {
    val locked = block != null && showBlock
    Column(
        Modifier
            .fillMaxWidth()
            .clickable(enabled = !locked, onClick = onClick)
            .alpha(if (locked) 0.5f else 1f)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        Text(title, style = MaterialTheme.typography.bodyLarge)
        Text(
            if (locked) "$subtitle · $block" else subtitle,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ConfigEditDialog(
    entry: ManageConfigEntry,
    enabled: Boolean,
    onDismiss: () -> Unit,
    onSet: (String) -> Unit,
    onReset: () -> Unit,
) {
    var text by remember(entry.key) { mutableStateOf(entry.value) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(entry.key) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "Gilt ${configApplyLabel(entry.apply)}. Änderungen brauchen Bestätigung per Biometrie.",
                    style = MaterialTheme.typography.bodySmall,
                )
                if (entry.choices.isNotEmpty()) {
                    entry.choices.forEach { choice ->
                        FilterChip(
                            selected = text == choice,
                            onClick = { text = choice },
                            label = { Text(choice) },
                            enabled = enabled,
                        )
                    }
                } else {
                    OutlinedTextField(
                        value = text,
                        onValueChange = { text = it.replace("\n", "").replace("\r", "") },
                        singleLine = true,
                        enabled = enabled,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                if (!enabled) Text(CONFIG_READONLY_HINT, style = MaterialTheme.typography.bodySmall)
            }
        },
        confirmButton = {
            TextButton(onClick = { onSet(text) }, enabled = enabled && text != entry.value && text.isNotBlank()) {
                Text("Speichern")
            }
        },
        dismissButton = {
            Row {
                TextButton(onClick = onReset, enabled = enabled) { Text("Zurücksetzen") }
                TextButton(onClick = onDismiss) { Text("Abbrechen") }
            }
        },
    )
}

private fun openNotificationSettings(context: Context) {
    try {
        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
            .putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
    } catch (_: Exception) {
        Toast.makeText(context, NOTIFICATION_ERROR, Toast.LENGTH_SHORT).show()
    }
}

private fun shareDiagnostic(context: Context, report: String) {
    try {
        val send = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_SUBJECT, "Harw Diagnose")
            .putExtra(Intent.EXTRA_TEXT, report)
        context.startActivity(Intent.createChooser(send, "Diagnose teilen").addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: Exception) {
        Toast.makeText(context, "Teilen nicht möglich", Toast.LENGTH_SHORT).show()
    }
}
