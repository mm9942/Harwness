package dev.harw.mobile

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
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
import uniffi.harw_mobile_ffi.CloudCatalog
import uniffi.harw_mobile_ffi.CloudProvider
import uniffi.harw_mobile_ffi.ProviderKind
import uniffi.harw_mobile_ffi.ProviderRoute
import uniffi.harw_mobile_ffi.ProviderStore

private const val IMPORTED_PREFS = "harw_imported_routes"
private const val IMPORTED_KEY = "ids"
private const val MAX_ID = 32

/** Ids lokaler Routen, die aus dem Cloud-Katalog kopiert wurden (Marker nur in der UI). */
internal fun readImportedRoutes(context: Context): Set<String> = try {
    context.getSharedPreferences(IMPORTED_PREFS, Context.MODE_PRIVATE)
        .getStringSet(IMPORTED_KEY, emptySet())?.toSet().orEmpty()
} catch (_: RuntimeException) {
    emptySet()
}

private fun writeImported(context: Context, ids: Set<String>) {
    try {
        context.getSharedPreferences(IMPORTED_PREFS, Context.MODE_PRIVATE).edit().putStringSet(IMPORTED_KEY, ids).apply()
    } catch (_: RuntimeException) {
    }
}

internal fun addImportedRoute(context: Context, id: String) = writeImported(context, readImportedRoutes(context) + id)

internal fun removeImportedRoute(context: Context, id: String) = writeImported(context, readImportedRoutes(context) - id)

/** Katalog-api -> lokale Protokollfamilie; `null` = nicht unterstuetzt. */
internal fun kindForCatalogApi(api: String): ProviderKind? = when (api) {
    "anthropic-messages" -> ProviderKind.ANTHROPIC
    "openai-responses" -> ProviderKind.OPEN_AI
    "openai-chat" -> ProviderKind.OPEN_AI_COMPATIBLE
    else -> null
}

/** Lokale Route-Id aus dem Cloud-Namen; bei Kollision Suffix `-cloud`, `-cloud-2`, ... */
internal fun freeRouteId(name: String, taken: Set<String>): String {
    val base = name.lowercase().map { if (it in 'a'..'z' || it in '0'..'9') it else '-' }
        .joinToString("").trim('-').ifEmpty { "cloud" }.take(MAX_ID).trim('-').ifEmpty { "cloud" }
    if (base !in taken) return base
    var n = 1
    while (true) {
        val suffix = if (n == 1) "-cloud" else "-cloud-$n"
        val candidate = base.take(MAX_ID - suffix.length).trim('-') + suffix
        if (candidate !in taken) return candidate
        n++
    }
}

private fun usableModel(id: String): Boolean = id.isNotEmpty() && id.length <= 256 && id.none { it.isWhitespace() || it.isISOControl() }

/** Modelle des Providers; Standard zuerst (Live-Modell oder Rollenzuordnung, sonst erstes). */
internal fun importModels(catalog: CloudCatalog, provider: CloudProvider): List<String> {
    val all = (provider.models + catalog.models.filter { it.provider == provider.name }.map { it.id })
        .filter(::usableModel).distinct()
    val preferred = listOfNotNull(
        catalog.liveModel.takeIf { catalog.liveProvider == provider.name },
        *catalog.roles.filter { it.provider == provider.name }.map { it.model }.toTypedArray(),
    ).firstOrNull { it in all }
    return if (preferred == null) all else listOf(preferred) + (all - preferred)
}

private fun importBlocker(provider: CloudProvider): String? = when {
    kindForCatalogApi(provider.api) == null -> "Protokoll „${provider.api}“ wird lokal nicht unterstützt"
    provider.baseUrl.isBlank() -> "Keine Basis-URL im Katalog"
    else -> null
}

@Composable
internal fun CloudImportCard(
    state: WorkspaceState,
    connection: HarwConnection,
    sessionId: String?,
    store: ProviderStore,
    existing: List<ProviderRoute>,
    onImported: () -> Unit,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    LaunchedEffect(state.online, state.generation) {
        connection.refreshManageSupport()
        connection.refreshCatalog(sessionId)
    }
    val catalog = state.manage.catalog
    var selected by remember { mutableStateOf(setOf<String>()) }
    var busy by remember { mutableStateOf(false) }
    var report by remember { mutableStateOf<List<String>>(emptyList()) }
    val taken = existing.map { it.id }.toSet()

    Surface(tonalElevation = 2.dp, shape = MaterialTheme.shapes.medium, modifier = Modifier.fillMaxWidth()) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("Aus Cloud importieren", style = MaterialTheme.typography.titleMedium)
            Text(
                "Kopiert Name, Protokoll, Basis-URL und Modelle des Servers in die lokalen Provider. " +
                    "Schlüssel überträgt der Server nie: importierte Routen zeigen „Schlüssel fehlt“; " +
                    "trage den Schlüssel lokal ein oder nutze den Login (ChatGPT direkt, Codex, Claude). " +
                    "Vorhandene lokale Routen werden nie überschrieben, der Import legt dann einen Namen mit Suffix an.",
                style = MaterialTheme.typography.bodySmall,
            )
            state.manage.catalogError?.let { StatusMessage(it, error = true) }
            if (catalog == null) {
                if (state.manage.catalogError == null) StatusMessage("Provider-Katalog wird geladen …")
            } else {
                if (catalog.providers.isEmpty()) StatusMessage("Keine Provider auf dem Server.")
                catalog.providers.forEach { provider ->
                    val blocker = importBlocker(provider)
                    val target = freeRouteId(provider.name, taken)
                    val models = importModels(catalog, provider)
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = provider.name in selected,
                            enabled = blocker == null && !busy,
                            onCheckedChange = { on -> selected = if (on) selected + provider.name else selected - provider.name },
                        )
                        Column(Modifier.weight(1f)) {
                            Text(provider.name, style = MaterialTheme.typography.titleSmall)
                            Text("${provider.api} · ${provider.baseUrl.ifBlank { "keine URL" }}", style = MaterialTheme.typography.bodySmall)
                            Text(
                                if (models.isEmpty()) "Keine Modelle im Katalog" else "${models.size} Modelle, Standard ${models.first()}",
                                style = MaterialTheme.typography.bodySmall,
                            )
                            if (blocker != null) {
                                Text(blocker, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                            } else if (target != provider.name) {
                                Text("Name lokal vergeben – wird als „$target“ angelegt", style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    }
                }
                Button(
                    enabled = !busy && selected.isNotEmpty(),
                    onClick = {
                        val picks = catalog.providers.filter { it.name in selected }
                        busy = true
                        scope.launch {
                            val used = taken.toMutableSet()
                            val lines = mutableListOf<String>()
                            for (provider in picks) {
                                val kind = kindForCatalogApi(provider.api) ?: continue
                                val id = freeRouteId(provider.name, used)
                                val models = importModels(catalog, provider)
                                val route = ProviderRoute(
                                    id = id,
                                    kind = kind,
                                    baseUrl = provider.baseUrl,
                                    defaultModel = models.firstOrNull().orEmpty(),
                                    hasSecret = false,
                                    active = false,
                                )
                                try {
                                    withContext(Dispatchers.IO) { store.importRoute(route, models) }
                                    addImportedRoute(context, id)
                                    used += id
                                    lines += "„${provider.name}“ importiert als $id – Schlüssel fehlt"
                                } catch (e: Exception) {
                                    lines += "„${provider.name}“ nicht importiert: " +
                                        friendlyProviderError((e as? uniffi.harw_mobile_ffi.MobileException)?.message ?: "Fehler")
                                }
                            }
                            report = lines
                            selected = emptySet()
                            busy = false
                            onImported()
                        }
                    },
                ) { Text("Ausgewählte importieren") }
            }
            report.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
        }
    }
}
