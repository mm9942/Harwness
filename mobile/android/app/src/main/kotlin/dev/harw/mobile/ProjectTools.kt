package dev.harw.mobile

import android.content.Context
import org.json.JSONObject
import uniffi.harw_mobile_ffi.MobileProject
import uniffi.harw_mobile_ffi.MobileSessionRow

/** Eigene Anzeigenamen, Sortierung und Vorschläge für Projekte (nur auf diesem Handy). */
private const val NamesPref = "project_names"
private const val SortPref = "project_sort"
private const val DismissedPref = "project_suggestions_dismissed"

private fun prefs(context: Context) = context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE)

internal enum class ProjectSort(val label: String) {
    Recent("Zuletzt aktiv"), Name("Name A–Z"), Count("Meiste Chats");
}

internal fun readProjectSort(context: Context): ProjectSort =
    runCatching { ProjectSort.valueOf(prefs(context).getString(SortPref, null) ?: "") }.getOrDefault(ProjectSort.Recent)

internal fun writeProjectSort(context: Context, sort: ProjectSort) {
    prefs(context).edit().putString(SortPref, sort.name).apply()
}

/** Namen je Backend ("local"/"cloud"), damit gleiche Schlüssel nicht kollidieren. */
internal fun readProjectNames(context: Context, backend: String): Map<String, String> = runCatching {
    val root = JSONObject(prefs(context).getString("$NamesPref.$backend", "{}") ?: "{}")
    root.keys().asSequence().associateWith { root.getString(it) }
}.getOrDefault(emptyMap())

internal fun writeProjectName(context: Context, backend: String, key: String, name: String?): Map<String, String> {
    val next = readProjectNames(context, backend).toMutableMap()
    if (name.isNullOrBlank()) next.remove(key) else next[key] = name.trim().take(60)
    prefs(context).edit().putString("$NamesPref.$backend", JSONObject(next as Map<*, *>).toString()).apply()
    return next
}

internal fun orderProjects(projects: List<MobileProject>, sort: ProjectSort, names: Map<String, String>): List<MobileProject> {
    val (none, real) = projects.partition { it.projectKey == NoProjectKey }
    val sorted = when (sort) {
        ProjectSort.Recent -> return orderProjects(projects)
        ProjectSort.Name -> real.sortedBy { (names[it.projectKey] ?: it.displayLabel()).lowercase() }
        ProjectSort.Count -> real.sortedWith(compareByDescending<MobileProject> { it.sessionCount }.thenBy { it.projectKey })
    }
    return sorted + none
}

internal data class ProjectSuggestion(val name: String, val sessionIds: List<String>, val titles: List<String>)

private val Stop = setOf(
    "nicht", "oder", "eine", "einen", "einer", "einem", "dass", "wird", "wenn", "aber", "auch", "kann", "noch", "mein", "meine", "meinen",
    "dein", "sein", "hier", "dann", "wie", "was", "mit", "für", "fuer", "und", "der", "die", "das", "ich", "du", "ist", "sind", "habe",
    "haben", "this", "that", "with", "have", "from", "what", "your", "about", "there", "would", "could", "hello", "hallo", "hey", "kannst",
    "machen", "mal", "mir", "dir", "bitte", "danke", "chat", "neuer", "session", "jetzt", "heute", "gerade", "einfach", "wieder",
)

private fun tokens(text: String): Set<String> =
    text.lowercase().split(Regex("[^\\p{L}\\p{N}]+")).filter { it.length >= 4 && it !in Stop }.toSet()

/**
 * Erkennt Chats ohne Projekt, die zum selben Thema gehören: ein bedeutsames Wort, das in mindestens zwei Chats vorkommt
 * (Titel oder erste Nachricht). Gibt höchstens einen Vorschlag zurück, dazu nur solche, die nicht abgelehnt wurden.
 */
internal fun suggestProject(rows: List<MobileSessionRow>, dismissed: Set<String>): ProjectSuggestion? {
    val loose = rows.filter { it.projectKey.isNullOrBlank() }
    if (loose.size < 2) return null
    val perRow = loose.associateWith { tokens("${it.title.orEmpty()} ${it.firstMessage.orEmpty()}") }
    val counts = HashMap<String, Int>()
    perRow.values.forEach { set -> set.forEach { counts[it] = (counts[it] ?: 0) + 1 } }
    val best = counts.filter { it.value >= 2 && it.key !in dismissed }
        .maxWithOrNull(compareBy<Map.Entry<String, Int>> { it.value }.thenBy { it.key.length }) ?: return null
    val group = perRow.filterValues { best.key in it }.keys.toList()
    return ProjectSuggestion(
        name = best.key.replaceFirstChar { it.uppercase() },
        sessionIds = group.map { it.sessionId },
        titles = group.map { it.title?.takeIf { t -> t.isNotBlank() } ?: it.firstMessage.orEmpty().take(40) },
    )
}

internal fun readDismissedSuggestions(context: Context): Set<String> =
    prefs(context).getStringSet(DismissedPref, emptySet()) ?: emptySet()

internal fun dismissSuggestion(context: Context, name: String) {
    prefs(context).edit().putStringSet(DismissedPref, readDismissedSuggestions(context) + name.lowercase()).apply()
}
