package dev.harw.mobile

import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.ManageCommand
import uniffi.harw_mobile_ffi.ManageEffect

internal const val NO_MANAGE_TEXT = "Dieser Server kennt die Verwaltung noch nicht."
internal const val MAX_LINE_CHARS = 512

/** Feste Reihenfolge und Beschriftung der Gruppen; unbekannte Domaene -> "Sonstiges". */
internal val COMMAND_GROUPS: List<Pair<String, String>> = listOf(
    "session" to "Unterhaltung",
    "agents" to "Agenten",
    "execution" to "Ausführung & Jobs",
    "catalog_config" to "Modelle & Einstellungen",
    "knowledge" to "Wissen & Notizen",
    "identity" to "Identität",
    "network" to "Netzwerk",
    "security" to "Sicherheit",
    "crypto" to "Kryptografie",
    "misc" to "Sonstiges",
)

private const val MISC_DOMAIN = "misc"

internal fun groupLabel(domain: String): String =
    COMMAND_GROUPS.firstOrNull { it.first == domain }?.second
        ?: COMMAND_GROUPS.first { it.first == MISC_DOMAIN }.second

private fun groupKey(domain: String): String =
    if (COMMAND_GROUPS.any { it.first == domain }) domain else MISC_DOMAIN

/** Suche ueber name, aliases, summary, usage (ohne Gross/Klein, fuehrendes `/` ignoriert). Leere Suche = alle. */
internal fun filterCommands(commands: List<ManageCommand>, query: String): List<ManageCommand> {
    val needle = query.trim().trimStart('/').trim().lowercase()
    if (needle.isEmpty()) return commands
    return commands.filter { command ->
        command.name.lowercase().contains(needle) ||
            command.aliases.any { it.lowercase().trimStart('/').contains(needle) } ||
            command.summary.lowercase().contains(needle) ||
            command.usage.lowercase().contains(needle)
    }
}

/** Gruppiert in COMMAND_GROUPS-Reihenfolge; in der Gruppe: verfuegbare zuerst, dann nach name. Leere Gruppen fehlen. */
internal fun groupCommands(commands: List<ManageCommand>): List<Pair<String, List<ManageCommand>>> {
    val byGroup = commands.groupBy { groupKey(it.domain) }
    return COMMAND_GROUPS.mapNotNull { (key, label) ->
        val members = byGroup[key] ?: return@mapNotNull null
        label to members.sortedWith(compareByDescending<ManageCommand> { it.available }.thenBy { it.name })
    }
}

private fun hasControlChar(text: String): Boolean = text.any { Character.isISOControl(it) }

/** `/name [sub] [args]`, Leerraum normalisiert; `null` wenn Ergebnis > MAX_LINE_CHARS oder Steuerzeichen enthaelt. */
internal fun buildCommandLine(name: String, sub: String?, args: String): String? {
    if (hasControlChar(name) || (sub != null && hasControlChar(sub)) || hasControlChar(args)) return null
    val cleanName = name.trim().trimStart('/').trim()
    if (cleanName.isEmpty() || cleanName.any { it.isWhitespace() }) return null
    val parts = buildList {
        add("/$cleanName")
        sub?.trim()?.takeIf { it.isNotEmpty() }?.let { add(it) }
        args.trim().takeIf { it.isNotEmpty() }?.let { add(it) }
    }
    val line = parts.joinToString(" ").split(Regex("\\s+")).filter { it.isNotEmpty() }.joinToString(" ")
    return if (line.length > MAX_LINE_CHARS) null else line
}

/** Text nach `/` im Chat-Entwurf, wenn der Entwurf mit `/` beginnt und keinen Zeilenumbruch hat; sonst null. */
internal fun slashQuery(draft: String): String? {
    if (!draft.startsWith("/")) return null
    if (draft.contains('\n') || draft.contains('\r')) return null
    return draft.substring(1)
}

/** Deutsche Wirkung fuer die Karte. */
internal fun effectLabel(effect: ManageEffect): String = when (effect) {
    ManageEffect.APPLIED -> "Ausgeführt"
    ManageEffect.STAGED -> "Gilt ab dem nächsten Turn"
    ManageEffect.NEXT_SESSION -> "Gilt ab nächster Session"
    ManageEffect.REFUSED -> "Abgelehnt"
    ManageEffect.STEPUP_REQUIRED -> "Bestätigung nötig"
    ManageEffect.UNKNOWN -> "Unbekannte Wirkung"
}

/** Expertenzeile: beginnt mit `/`, hoechstens MAX_LINE_CHARS, keine Steuerzeichen. Sonst `null`. */
internal fun expertLine(raw: String): String? {
    val line = raw.trim()
    if (!line.startsWith("/") || line.length < 2 || line.length > MAX_LINE_CHARS) return null
    return if (hasControlChar(line)) null else line
}

/** Volle Befehlsliste (aus dem Hub, ohne Session) oder als Blatt aus dem Chat (mit Session). */
@OptIn(ExperimentalFoundationApi::class)
@Composable
internal fun CommandsScreen(
    state: WorkspaceState,
    connection: HarwConnection,
    sessionId: String?,
    initialQuery: String = "",
    onBack: () -> Unit,
    /** Im Chat: fuegt die Zeile in den Entwurf ein (zusaetzlich zu "Ausführen"). */
    onInsert: ((String) -> Unit)? = null,
    /** Fuer `/models show`: oeffnet die Rollen-Tabelle. */
    onOpenModels: (() -> Unit)? = null,
) {
    val manage = state.manage
    LaunchedEffect(state.online, state.generation, sessionId) {
        connection.refreshManageSupport()
        connection.refreshCommands(sessionId)
    }
    var query by rememberSaveable(sessionId, initialQuery) { mutableStateOf(initialQuery) }
    var expert by rememberSaveable { mutableStateOf(false) }
    var expertText by rememberSaveable { mutableStateOf("/") }
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    val running = manage.running != null

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Befehle", onBack = onBack) {}
        val support = manage.support
        if (support != null && !support.manage) {
            StatusMessage(NO_MANAGE_TEXT, Modifier.padding(12.dp))
            return@Column
        }
        manage.card?.let { card ->
            CommandResultCard(
                card = card,
                onClose = connection::clearCommandCard,
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 6.dp),
                onOpenModels = onOpenModels,
            )
        }
        manage.commandsError?.let { StatusMessage(it, Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }

        val chosen = selected?.let { name -> manage.commands.firstOrNull { it.name == name } }
        if (chosen != null) {
            CommandForm(
                command = chosen,
                running = running,
                onClose = { selected = null },
                onRun = { line -> connection.runCommand(sessionId, line) },
                onInsert = onInsert,
            )
            return@Column
        }

        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text("Expertenmodus", Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
            Switch(checked = expert, onCheckedChange = { expert = it })
        }
        if (expert) {
            val line = expertLine(expertText)
            Column(Modifier.fillMaxWidth().padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = expertText,
                    onValueChange = { expertText = it.replace("\n", "").replace("\r", "") },
                    label = { Text("/zeile") },
                    singleLine = true,
                    isError = expertText.length > 1 && line == null,
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(enabled = line != null && !running, onClick = { line?.let { connection.runCommand(sessionId, it) } }) {
                        Text("Ausführen")
                    }
                    if (onInsert != null) {
                        OutlinedButton(enabled = line != null, onClick = { line?.let(onInsert) }) { Text("Einfügen") }
                    }
                }
                Text(
                    "Der Server entscheidet, ob die Zeile erlaubt ist.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            return@Column
        }

        OutlinedTextField(
            value = query,
            onValueChange = { query = it.replace("\n", "") },
            placeholder = { Text("Befehl suchen") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
        )
        if (!manage.commandsLoaded && manage.commandsError == null) {
            StatusMessage("Befehle werden geladen …", Modifier.padding(12.dp))
            return@Column
        }
        val groups = remember(manage.commands, query) { groupCommands(filterCommands(manage.commands, query)) }
        if (groups.isEmpty()) {
            StatusMessage(
                if (manage.commands.isEmpty()) "Keine Befehle vom Server." else "Kein Befehl passt zur Suche.",
                Modifier.padding(12.dp),
            )
            return@Column
        }
        LazyColumn(
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 12.dp, vertical = 6.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            groups.forEach { (label, members) ->
                stickyHeader(key = "group-$label") {
                    Text(
                        label,
                        Modifier.fillMaxWidth().background(MaterialTheme.colorScheme.background).padding(vertical = 6.dp),
                        style = MaterialTheme.typography.titleSmall,
                        color = MaterialTheme.colorScheme.primary,
                    )
                }
                items(members, key = { "cmd-${it.name}" }) { command ->
                    CommandRow(command, onClick = { if (command.available) selected = command.name })
                }
            }
        }
    }
}

@Composable
private fun CommandRow(command: ManageCommand, onClick: () -> Unit) {
    Surface(
        Modifier.fillMaxWidth().alpha(if (command.available) 1f else 0.45f).clickable(enabled = command.available, onClick = onClick),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text("/${command.name}", style = MaterialTheme.typography.titleSmall, fontFamily = FontFamily.Monospace)
            if (command.summary.isNotBlank()) Text(command.summary, style = MaterialTheme.typography.bodyMedium)
            if (!command.available) {
                Text(
                    command.reason ?: "nicht verfügbar",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class, androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun CommandForm(
    command: ManageCommand,
    running: Boolean,
    onClose: () -> Unit,
    onRun: (String) -> Unit,
    onInsert: ((String) -> Unit)?,
) {
    var sub by rememberSaveable(command.name) { mutableStateOf<String?>(null) }
    var args by rememberSaveable(command.name) { mutableStateOf("") }
    val line = buildCommandLine(command.name, sub, args)
    val hint = command.subcommands.firstOrNull { it.name == sub }
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("/${command.name}", style = MaterialTheme.typography.titleMedium, fontFamily = FontFamily.Monospace)
        if (command.summary.isNotBlank()) Text(command.summary, style = MaterialTheme.typography.bodyMedium)
        if (command.usage.isNotBlank()) {
            Text(command.usage, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
        }
        if (command.subcommands.isNotEmpty()) {
            Text("Unterbefehl", style = MaterialTheme.typography.labelMedium)
            androidx.compose.foundation.layout.FlowRow(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                command.subcommands.forEach { entry ->
                    FilterChip(
                        selected = sub == entry.name,
                        onClick = { sub = if (sub == entry.name) null else entry.name },
                        label = { Text(entry.name) },
                    )
                }
            }
            hint?.let {
                if (it.summary.isNotBlank()) Text(it.summary, style = MaterialTheme.typography.bodySmall)
            }
        }
        OutlinedTextField(
            value = args,
            onValueChange = { args = it.replace("\n", " ") },
            label = { Text("Argumente") },
            supportingText = hint?.args?.takeIf { it.isNotBlank() }?.let { { Text(it) } },
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            line ?: "Zeile ungültig oder zu lang.",
            style = MaterialTheme.typography.bodySmall,
            fontFamily = FontFamily.Monospace,
            color = if (line == null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(enabled = line != null && !running, onClick = { line?.let(onRun) }) { Text("Ausführen") }
            if (onInsert != null) {
                OutlinedButton(enabled = line != null, onClick = { line?.let(onInsert) }) { Text("Einfügen") }
            }
            TextButton(onClick = onClose) { Text("Zurück") }
        }
    }
}

/** Ergebniskarte: Text, Wirkung, Grund, "Technische Details" (JSON einklappbar). */
@Composable
internal fun CommandResultCard(
    card: ManageCard,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    onOpenModels: (() -> Unit)? = null,
) {
    val result = card.result
    var details by rememberSaveable(card.line) { mutableStateOf(false) }
    val isModelsShow = card.line.trim().replace(Regex("\\s+"), " ").equals("/models show", ignoreCase = true)
    Surface(
        modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
        color = if (result.ok) MaterialTheme.colorScheme.surfaceContainerHigh else MaterialTheme.colorScheme.errorContainer,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(effectLabel(result.effect), style = MaterialTheme.typography.titleSmall)
                    Text(card.line, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
                }
                TextButton(onClick = onClose) { Text("Schließen") }
            }
            if (result.text.isNotBlank()) {
                SelectionContainer {
                    Text(
                        result.text,
                        style = MaterialTheme.typography.bodyMedium,
                        fontFamily = if (result.text.contains('\n')) FontFamily.Monospace else FontFamily.Default,
                    )
                }
            }
            if (result.effect == ManageEffect.REFUSED) {
                result.reasonCode?.let {
                    Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
            if (isModelsShow && onOpenModels != null) {
                OutlinedButton(onClick = onOpenModels) { Text("Als Rollen-Tabelle öffnen") }
            }
            val json = result.dataJson
            if (json != null) {
                TextButton(onClick = { details = !details }) { Text("Technische Details") }
                if (details) {
                    SelectionContainer {
                        Text(json, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                    }
                }
            }
            if (result.dataTruncated) {
                Text("Details zu groß, gekürzt", style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}
