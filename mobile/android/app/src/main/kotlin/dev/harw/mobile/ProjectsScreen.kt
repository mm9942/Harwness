package dev.harw.mobile

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Forum
import androidx.compose.material.icons.outlined.Refresh
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeParseException
import uniffi.harw_mobile_ffi.MobileProject

/** Schluessel des Pseudo-Projekts "Ohne Projekt" (Vertrag K-W1: leerer `project_key`). */
internal const val NoProjectKey: String = ""

private const val NoProjectLabel = "Ohne Projekt"

private val MonthNames = listOf(
    "Januar", "Februar", "März", "April", "Mai", "Juni",
    "Juli", "August", "September", "Oktober", "November", "Dezember",
)

/** Anzeigename; das Pseudo-Projekt heisst immer "Ohne Projekt", leere Labels fallen auf den Schluessel zurueck. */
internal fun MobileProject.displayLabel(): String = when {
    projectKey == NoProjectKey -> NoProjectLabel
    label.isNotBlank() -> label
    else -> projectKey
}

/** RFC-3339-Zeit in UTC-Millisekunden, `null` bei unlesbarem Wert. */
internal fun parseActivityMillis(rfc3339: String): Long? =
    try {
        OffsetDateTime.parse(rfc3339.trim()).toInstant().toEpochMilli()
    } catch (_: DateTimeParseException) {
        null
    }

/** Monatsschluessel `JJJJ-MM` (in `zone`), `null` bei unlesbarer Zeit. Fuer "nach Monat" gruppierte Listen. */
internal fun monthKey(rfc3339: String, zone: ZoneId = ZoneOffset.UTC): String? {
    val millis = parseActivityMillis(rfc3339) ?: return null
    val date = Instant.ofEpochMilli(millis).atZone(zone)
    return "%04d-%02d".format(date.year, date.monthValue)
}

/** `2026-11` -> `November 2026`; unlesbare Schluessel kommen unveraendert zurueck, `null` -> "Unbekannt". */
internal fun monthHeading(key: String?): String {
    if (key == null) return "Unbekannt"
    val parts = key.split('-')
    val year = parts.getOrNull(0)?.toIntOrNull()
    val month = parts.getOrNull(1)?.toIntOrNull()
    if (parts.size != 2 || year == null || month == null || month !in 1..12) return key
    return "${MonthNames[month - 1]} $year"
}

/** Gruppiert nach Monat, neuester Monat zuerst, Reihenfolge innerhalb eines Monats bleibt; Unlesbares ans Ende. */
internal fun <T> groupByMonth(
    items: List<T>,
    activity: (T) -> String,
    zone: ZoneId = ZoneOffset.UTC,
): List<Pair<String?, List<T>>> {
    val groups = LinkedHashMap<String?, MutableList<T>>()
    for (item in items) groups.getOrPut(monthKey(activity(item), zone)) { mutableListOf() }.add(item)
    val dated = groups.filterKeys { it != null }.toSortedMap(compareByDescending { it!! })
    val result = dated.map { (k, v) -> k to v.toList() }.toMutableList()
    groups[null]?.let { result.add(null to it.toList()) }
    return result
}

/**
 * Suche ueber Label, Pfadhinweis und Schluessel (ohne Gross-/Kleinschreibung, getrimmt; leer = alles).
 * Das Pseudo-Projekt "Ohne Projekt" matcht ebenfalls ueber seinen Anzeigenamen.
 */
internal fun filterProjects(projects: List<MobileProject>, query: String): List<MobileProject> {
    val needle = query.trim().lowercase()
    if (needle.isEmpty()) return projects
    return projects.filter { p ->
        p.displayLabel().lowercase().contains(needle) ||
            p.label.lowercase().contains(needle) ||
            (p.pathHint?.lowercase()?.contains(needle) == true) ||
            (p.projectKey != NoProjectKey && p.projectKey.lowercase().contains(needle))
    }
}

/** Reihenfolge der Liste: echte Projekte nach letzter Aktivitaet (neueste zuerst), "Ohne Projekt" immer zuletzt. */
internal fun orderProjects(projects: List<MobileProject>): List<MobileProject> {
    val (none, real) = projects.partition { it.projectKey == NoProjectKey }
    val sorted = real.sortedWith(
        compareByDescending<MobileProject> { parseActivityMillis(it.lastActivity) ?: Long.MIN_VALUE }
            .thenBy { it.projectKey },
    )
    return sorted + none
}

internal fun sessionCountText(count: UInt, active: UInt): String {
    val base = if (count == 1u) "1 Session" else "$count Sessions"
    return if (active > 0u) "$base · $active aktiv" else base
}

/** Relative Angabe der letzten Aktivitaet ("gerade eben", "vor 5 Min.", "vor 3 Std.", "vor 2 Tg.", sonst Datum). */
internal fun lastActivityText(rfc3339: String, nowMillis: Long, zone: ZoneId = ZoneOffset.UTC): String {
    val millis = parseActivityMillis(rfc3339) ?: return "Aktivität unbekannt"
    val diff = (nowMillis - millis).coerceAtLeast(0L)
    val minutes = diff / 60_000L
    val text = when {
        minutes < 1L -> "gerade eben"
        minutes < 60L -> "vor $minutes Min."
        minutes < 24L * 60L -> "vor ${minutes / 60L} Std."
        minutes < 7L * 24L * 60L -> "vor ${minutes / (24L * 60L)} Tg."
        else -> {
            val d = Instant.ofEpochMilli(millis).atZone(zone)
            "am %02d.%02d.%04d".format(d.dayOfMonth, d.monthValue, d.year)
        }
    }
    return text
}

/**
 * Projektliste: Suche, Anzahl und letzte Aktivitaet je Projekt, "+ Session" je Zeile, "Ohne Projekt" am Ende.
 * Zustandslos bis auf das Suchfeld; Daten und Aktionen kommen vom Aufrufer (A5/A6).
 */
@Composable
internal fun ProjectsScreen(
    projects: List<MobileProject>,
    loaded: Boolean,
    online: Boolean,
    canCreate: Boolean,
    error: String?,
    onSelect: (String) -> Unit,
    onNewSession: (String) -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
    onNewProject: (() -> Unit)? = null,
    nowMillis: Long = System.currentTimeMillis(),
    names: Map<String, String> = emptyMap(),
    sort: ProjectSort = ProjectSort.Recent,
    onSort: (ProjectSort) -> Unit = {},
    onRename: (String, String?) -> Unit = { _, _ -> },
    suggestion: ProjectSuggestion? = null,
    onAcceptSuggestion: (ProjectSuggestion) -> Unit = {},
    onDismissSuggestion: (ProjectSuggestion) -> Unit = {},
) {
    var query by rememberSaveable { mutableStateOf("") }
    var sortMenu by remember { mutableStateOf(false) }
    var renaming by remember { mutableStateOf<MobileProject?>(null) }
    val visible = remember(projects, query, sort, names) { filterProjects(orderProjects(projects, sort, names), query) }
    renaming?.let { project ->
        var text by remember(project.projectKey) { mutableStateOf(names[project.projectKey] ?: project.displayLabel()) }
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Projekt umbenennen") },
            text = {
                OutlinedTextField(value = text, onValueChange = { text = it }, singleLine = true, label = { Text("Name") })
            },
            confirmButton = { TextButton(onClick = { onRename(project.projectKey, text); renaming = null }, enabled = text.isNotBlank()) { Text("Speichern") } },
            dismissButton = {
                Row {
                    if (names.containsKey(project.projectKey)) TextButton(onClick = { onRename(project.projectKey, null); renaming = null }) { Text("Zurücksetzen") }
                    TextButton(onClick = { renaming = null }) { Text("Abbrechen") }
                }
            },
        )
    }
    Column(modifier.fillMaxSize().padding(top = 9.dp)) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = 12.dp),
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                "Projekte",
                modifier = Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            androidx.compose.foundation.layout.Box {
                TextButton(onClick = { sortMenu = true }) { Text(sort.label) }
                DropdownMenu(expanded = sortMenu, onDismissRequest = { sortMenu = false }) {
                    ProjectSort.values().forEach { option ->
                        DropdownMenuItem(
                            text = { Text(if (option == sort) "✓ ${option.label}" else option.label) },
                            onClick = { sortMenu = false; onSort(option) },
                        )
                    }
                }
            }
            if (onNewProject != null) {
                TextButton(onClick = onNewProject, enabled = online) { Text("Neues Projekt") }
            }
            IconButton(onClick = onRefresh, enabled = online) { Icon(Icons.Outlined.Refresh, "Projekte aktualisieren") }
        }
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
            singleLine = true,
            label = { Text("Projekte suchen") },
        )
        when {
            projects.isEmpty() -> Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                when {
                    online && !loaded && error == null -> Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        CircularProgressIndicator(strokeWidth = 2.dp, modifier = Modifier.size(22.dp))
                        Text(
                            "Projekte werden geladen…",
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            style = MaterialTheme.typography.bodyMedium,
                        )
                    }
                    error != null -> Column(
                        horizontalAlignment = Alignment.CenterHorizontally,
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        StatusMessage("Projekte konnten nicht geladen werden: $error", error = true)
                        if (online) TextButton(onClick = onRefresh) { Text("Erneut versuchen") }
                    }
                    !online -> DirectoryEmpty("Keine Verbindung. Projekte erscheinen, sobald das Handy verbunden ist.")
                    else -> DirectoryEmpty("Noch keine Projekte. Sessions erscheinen hier, sobald sie geteilt werden.")
                }
            }
            visible.isEmpty() -> Box(Modifier.fillMaxSize().padding(12.dp), contentAlignment = Alignment.Center) {
                DirectoryEmpty("Kein Projekt passt zu „${query.trim()}“.")
            }
            else -> LazyColumn(
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 9.dp, bottom = 14.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                error?.let { message -> item { StatusMessage("Aktualisieren fehlgeschlagen: $message", error = true) } }
                if (online && !canCreate) {
                    item { StatusMessage("Dieses Gerät darf keine Sessions anlegen.") }
                }
                if (suggestion != null) {
                    item(key = "suggestion") {
                        Surface(
                            shape = RoundedCornerShape(15.dp),
                            border = BorderStroke(1.dp, MaterialTheme.colorScheme.primary),
                            color = MaterialTheme.colorScheme.surfaceContainerLow,
                        ) {
                            Column(Modifier.fillMaxWidth().padding(13.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                Text("Diese Chats scheinen zusammenzugehören", style = MaterialTheme.typography.titleSmall)
                                suggestion.titles.take(4).forEach {
                                    Text("• $it", style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
                                }
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    TextButton(onClick = { onAcceptSuggestion(suggestion) }) { Text("Projekt „${suggestion.name}“ anlegen") }
                                    TextButton(onClick = { onDismissSuggestion(suggestion) }) { Text("Nicht jetzt") }
                                }
                            }
                        }
                    }
                }
                items(visible, key = { it.projectKey }) { project ->
                    ProjectRow(
                        label = names[project.projectKey] ?: project.displayLabel(),
                        onRename = if (project.projectKey == NoProjectKey) null else ({ renaming = project }),
                        project = project,
                        nowMillis = nowMillis,
                        canCreate = online && canCreate,
                        onSelect = { onSelect(project.projectKey) },
                        onNewSession = { onNewSession(project.projectKey) },
                    )
                }
            }
        }
    }
}

@Composable
private fun ProjectRow(
    label: String,
    onRename: (() -> Unit)?,
    project: MobileProject,
    nowMillis: Long,
    canCreate: Boolean,
    onSelect: () -> Unit,
    onNewSession: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        shape = RoundedCornerShape(15.dp),
        border = BorderStroke(1.dp, colors.outlineVariant),
        color = colors.surfaceContainerLowest,
    ) {
        Row(
            Modifier.fillMaxWidth().clickable(onClick = onSelect).padding(start = 13.dp, end = 4.dp, top = 10.dp, bottom = 10.dp),
            horizontalArrangement = Arrangement.spacedBy(9.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(Icons.Outlined.Forum, contentDescription = null, Modifier.size(18.dp), tint = colors.primary)
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(
                    label, style = MaterialTheme.typography.titleSmall,
                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                )
                project.pathHint?.takeIf { it.isNotBlank() }?.let {
                    Text(it, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
                }
                val detail = sessionCountText(project.sessionCount, project.activeCount) +
                    " · " + lastActivityText(project.lastActivity, nowMillis)
                Text(detail, style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            if (onRename != null) {
                IconButton(onClick = onRename) { Icon(Icons.Outlined.Edit, "Projekt umbenennen", Modifier.size(18.dp)) }
            }
            IconButton(onClick = onNewSession, enabled = canCreate) {
                Icon(Icons.Outlined.Add, "Neue Session in ${project.displayLabel()}", Modifier.size(20.dp))
            }
        }
    }
}
