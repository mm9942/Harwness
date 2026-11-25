package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.CloudCatalog
import uniffi.harw_mobile_ffi.CloudModel
import uniffi.harw_mobile_ffi.CloudProvider
import uniffi.harw_mobile_ffi.CloudRole
import uniffi.harw_mobile_ffi.ManageAuthKind
import uniffi.harw_mobile_ffi.ManageKnownAuth
import uniffi.harw_mobile_ffi.ManageKnownProvider
import uniffi.harw_mobile_ffi.ManageSupport

// ---------------------------------------------------------------------------
// Reine Funktionen (testbar)
// ---------------------------------------------------------------------------

internal val EFFORT_LEVELS = listOf("minimal", "low", "medium", "high", "xhigh", "max")

/** Rollen mit eigenem Gewichtsfeld (harw-ops `role_effort_field`); andere bekommen keine Aufwand-Chips. */
internal val ROLES_WITH_EFFORT = setOf("uia", "orchestrator", "sub-orchestrator", "worker-simple", "worker-complex")
internal val CUSTOM_APIS = listOf("openai-chat", "openai-responses", "anthropic-messages")
internal val AUTH_HEADERS = listOf("bearer", "x-api-key", "api-key")

private const val MAX_LINE = 512
internal const val SECRET_NAME_DETAIL = "Schlüsselname: nur A-Z a-z 0-9 . _ -, höchstens 64 Zeichen"
internal const val PROVIDER_NAME_DETAIL = "Providername: nur A-Z a-z 0-9 _ -, höchstens 48 Zeichen"
private val TokenPattern = Regex("^[^\\s\\p{Cntrl}]{1,256}$")
private val ProviderNamePattern = Regex("^[A-Za-z0-9_-]{1,48}$")
private val SecretNamePattern = Regex("^[A-Za-z0-9._-]{1,64}$")
private val Ipv4Pattern = Regex("^[0-9]+(\\.[0-9]+){3}$")

private fun isToken(text: String): Boolean = TokenPattern.matches(text)

private fun lineOrNull(parts: List<String>): String? {
    val line = parts.joinToString(" ")
    return if (line.length > MAX_LINE) null else line
}

/** `/models set <role> <provider>/<model> [effort]`; null bei ungueltigem Token. */
internal fun modelsSetLine(role: String, provider: String, model: String, effort: String?): String? {
    if (!isToken(role) || !isToken(provider) || !isToken(model)) return null
    if (provider.contains('/')) return null
    if (effort != null && effort !in EFFORT_LEVELS) return null
    val parts = mutableListOf("/models", "set", role, "$provider/$model")
    if (effort != null) parts.add(effort)
    return lineOrNull(parts)
}

/** `/models reset <role>`. */
internal fun modelsResetLine(role: String): String? =
    if (isToken(role)) lineOrNull(listOf("/models", "reset", role)) else null

/** `/models scan [provider]`. */
internal fun modelsScanLine(provider: String?): String? {
    if (provider == null) return "/models scan"
    return if (isToken(provider)) lineOrNull(listOf("/models", "scan", provider)) else null
}

/** `/model switch <provider>/<model>`. */
internal fun modelSwitchLine(provider: String, model: String): String? {
    if (!isToken(provider) || !isToken(model) || provider.contains('/')) return null
    return lineOrNull(listOf("/model", "switch", "$provider/$model"))
}

/** `/provider enable|disable <name>`. */
internal fun providerToggleLine(name: String, enable: Boolean): String? =
    if (isToken(name)) lineOrNull(listOf("/provider", if (enable) "enable" else "disable", name)) else null

/** `/provider remove <name>`. */
internal fun providerRemoveLine(name: String): String? =
    if (isToken(name)) lineOrNull(listOf("/provider", "remove", name)) else null

internal data class ProviderAddRequest(
    val known: Boolean,
    /** Bekannt. */
    val catalogId: String?,
    /** Bekannt: API_KEY|OAUTH; eigen: API_KEY|NONE. */
    val auth: ManageKnownAuth,
    val name: String,
    val keyName: String,
    /** Eigen. */
    val api: String?,
    /** Eigen. */
    val baseUrl: String?,
    /** Eigen, optional. */
    val authHeader: String?,
)

/**
 * Bekannt: `/provider add <catalogId> --auth api-key|oauth --name <name> [--key <handle>]`
 * Eigen:   `/provider add-custom <name> --api <api> --base-url <url> (--key <handle> | --no-auth) [--auth-header <h>]`
 * null bei ungueltigem Token, fehlendem Pflichtfeld oder Zeile > 512 Zeichen.
 */
internal fun providerAddLine(request: ProviderAddRequest, handle: String?): String? {
    if (!ProviderNamePattern.matches(request.name)) return null
    if (handle != null && !isToken(handle)) return null
    return if (request.known) knownProviderAddLine(request, handle) else customProviderAddLine(request, handle)
}

private fun knownProviderAddLine(request: ProviderAddRequest, handle: String?): String? {
    val id = request.catalogId ?: return null
    if (!isToken(id)) return null
    val auth = when (request.auth) {
        ManageKnownAuth.API_KEY -> "api-key"
        ManageKnownAuth.OAUTH -> "oauth"
        else -> return null
    }
    val parts = mutableListOf("/provider", "add", id, "--auth", auth, "--name", request.name)
    if (handle != null) {
        parts.add("--key")
        parts.add(handle)
    }
    return lineOrNull(parts)
}

private fun customProviderAddLine(request: ProviderAddRequest, handle: String?): String? {
    val api = request.api ?: return null
    if (api !in CUSTOM_APIS) return null
    val url = request.baseUrl ?: return null
    if (customBaseUrlProblem(url) != null) return null
    val header = request.authHeader
    if (header != null && header !in AUTH_HEADERS) return null
    val parts = mutableListOf("/provider", "add-custom", request.name, "--api", api, "--base-url", url)
    when (request.auth) {
        ManageKnownAuth.API_KEY -> {
            if (handle == null) return null
            parts.add("--key")
            parts.add(handle)
        }
        ManageKnownAuth.NONE -> parts.add("--no-auth")
        else -> return null
    }
    if (header != null) {
        parts.add("--auth-header")
        parts.add(header)
    }
    return lineOrNull(parts)
}

/**
 * Lokale Vorpruefung einer eigenen Basis-URL (Server prueft erneut). null = ok, sonst deutscher Grund.
 */
internal fun customBaseUrlProblem(url: String): String? {
    val scheme = "https://"
    if (!url.startsWith(scheme, ignoreCase = true)) return "nur https"
    val rest = url.substring(scheme.length)
    val authority = rest.substringBefore('/')
    if (authority.contains('@')) return "keine Zugangsdaten in der URL"
    if (url.contains('?') || url.contains('#')) return "keine Query oder Fragment"
    if (url.any { it.isWhitespace() || Character.isISOControl(it) || it == '<' || it == '>' }) return "ungültige Zeichen"
    if (authority.startsWith("[")) return "keine lokale oder IP-Adresse"
    val host = authority.substringBefore(':')
    if (host.isEmpty()) return "Host fehlt"
    if (host.trimEnd('.').equals("localhost", ignoreCase = true) || Ipv4Pattern.matches(host)) {
        return "keine lokale oder IP-Adresse"
    }
    return null
}

internal fun providerNameProblem(name: String): String? =
    if (ProviderNamePattern.matches(name)) null else PROVIDER_NAME_DETAIL

internal fun secretNameProblem(name: String): String? =
    if (SecretNamePattern.matches(name)) null else SECRET_NAME_DETAIL

internal fun authLabel(kind: ManageAuthKind, name: String?, hasSecret: Boolean): String {
    val base = when (kind) {
        ManageAuthKind.NONE -> "keine"
        ManageAuthKind.ENV -> "Umgebung ${name.orEmpty()}".trim()
        ManageAuthKind.KEYRING -> "Schlüsselbund ${name.orEmpty()}".trim()
        ManageAuthKind.SECRETS -> "Secret-Store ${name.orEmpty()}".trim()
        ManageAuthKind.FILE, ManageAuthKind.FILE_JSON -> "Datei"
        ManageAuthKind.INLINE_KEY -> "Schlüssel in Konfiguration"
        ManageAuthKind.UNKNOWN -> "unbekannt"
    }
    return if (!hasSecret && kind != ManageAuthKind.NONE) "$base (fehlt)" else base
}

/** Grund, warum Schreib-Bedienelemente aus sind; null = erlaubt. */
internal fun manageWriteBlock(support: ManageSupport?): String? = when {
    support == null || !support.manage -> NO_MANAGE_TEXT
    !support.control -> "Ändern braucht Profil Voll"
    !support.stepup -> "Server erlaubt noch keine Änderungen vom Handy"
    else -> null
}

/** Waehlbare Modelle eines Providers: `retired` ausgeblendet. */
internal fun selectableModels(models: List<CloudModel>, provider: String?): List<CloudModel> =
    if (provider == null) emptyList() else models.filter { it.provider == provider && it.lifecycle != "retired" }

internal fun roleSummary(role: CloudRole): String {
    val target = if (role.provider != null && role.model != null) "${role.provider}/${role.model}" else "Standard"
    return if (role.effort != null) "$target · ${role.effort}" else target
}

// ---------------------------------------------------------------------------
// Bildschirm
// ---------------------------------------------------------------------------

private sealed interface CloudView {
    data object Overview : CloudView
    data class Role(val role: String) : CloudView
    data object AddProvider : CloudView
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun CloudModelsScreen(
    state: WorkspaceState,
    connection: HarwConnection,
    sessionId: String?,
    onBack: () -> Unit,
    tabs: @Composable () -> Unit = {},
) {
    val manage = state.manage
    LaunchedEffect(state.online, state.generation) {
        connection.refreshManageSupport()
        connection.refreshCatalog(sessionId)
    }
    var view by remember { mutableStateOf<CloudView>(CloudView.Overview) }
    val running = manage.running != null
    val block = manageWriteBlock(manage.support)
        ?: if (manage.stepUp != null) "Erst die offene Bestätigung abschließen" else null
    val catalog = manage.catalog

    Column(Modifier.fillMaxSize()) {
        HarwHeader(
            title = "Modelle & Provider",
            onBack = { if (view == CloudView.Overview) onBack() else view = CloudView.Overview },
        ) {}
        tabs()
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
            )
        }
        manage.catalogError?.let { StatusMessage(it, Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
        if (catalog == null) {
            if (manage.catalogError == null) StatusMessage("Modelle werden geladen …", Modifier.padding(12.dp))
            return@Column
        }
        when (val current = view) {
            is CloudView.Role -> {
                val role = catalog.roles.firstOrNull { it.role == current.role }
                if (role == null) {
                    view = CloudView.Overview
                } else {
                    RoleEditor(
                        role = role,
                        catalog = catalog,
                        running = running,
                        block = block,
                        onSave = { provider, model, effort ->
                            val line = modelsSetLine(role.role, provider, model, effort)
                            if (line != null) connection.runCommand(null, line) { if (it != null) view = CloudView.Overview }
                        },
                        onReset = {
                            val line = modelsResetLine(role.role)
                            if (line != null) connection.runCommand(null, line) { if (it != null) view = CloudView.Overview }
                        },
                        onClose = { view = CloudView.Overview },
                    )
                }
            }
            CloudView.AddProvider -> ProviderAddForm(
                state = state,
                connection = connection,
                running = running,
                block = block,
                onClose = { view = CloudView.Overview },
            )
            CloudView.Overview -> Column(
                Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (block != null) StatusMessage(block)
                SectionTitle("Rollen")
                if (catalog.roles.isEmpty()) StatusMessage("Keine Rollen vom Server.")
                catalog.roles.forEach { role ->
                    RoleRow(role, onClick = { view = CloudView.Role(role.role) })
                }
                SectionTitle("Provider")
                if (catalog.providers.isEmpty()) StatusMessage("Keine Provider eingerichtet.")
                catalog.providers.forEach { provider ->
                    ProviderCard(
                        provider = provider,
                        writeBlock = block,
                        running = running,
                        onToggle = { enable ->
                            providerToggleLine(provider.name, enable)?.let { connection.runCommand(null, it) }
                        },
                        onScan = { modelsScanLine(provider.name)?.let { connection.runCommand(null, it) } },
                        onRemove = { providerRemoveLine(provider.name)?.let { connection.runCommand(null, it) } },
                    )
                }
                Button(enabled = block == null && !running, onClick = { view = CloudView.AddProvider }) {
                    Text("Provider hinzufügen")
                }
                if (sessionId != null) {
                    SectionTitle("Live-Modell")
                    val live = if (catalog.liveProvider != null && catalog.liveModel != null) {
                        "${catalog.liveProvider}/${catalog.liveModel}"
                    } else {
                        "unbekannt"
                    }
                    Text(live, style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
                }
                if (catalog.note.isNotBlank()) {
                    Text(
                        catalog.note,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
}

@Composable
private fun RoleRow(role: CloudRole, onClick: () -> Unit) {
    Surface(
        Modifier.fillMaxWidth().clickable(onClick = onClick),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(role.label, style = MaterialTheme.typography.titleSmall)
            Text(roleSummary(role), style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
            if (role.source.isNotBlank()) {
                Text(
                    role.source,
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun ProviderCard(
    provider: CloudProvider,
    writeBlock: String?,
    running: Boolean,
    onToggle: (Boolean) -> Unit,
    onScan: () -> Unit,
    onRemove: () -> Unit,
) {
    var menu by remember { mutableStateOf(false) }
    var confirmRemove by remember { mutableStateOf(false) }
    val canWrite = writeBlock == null && !running
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(3.dp)) {
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text(provider.name, style = MaterialTheme.typography.titleSmall)
                    Text(provider.api, style = MaterialTheme.typography.labelMedium)
                }
                Switch(checked = provider.enabled, enabled = canWrite, onCheckedChange = onToggle)
                androidx.compose.foundation.layout.Box {
                    TextButton(onClick = { menu = true }) { Text("Mehr") }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(
                            text = { Text("Modelle laden") },
                            enabled = canWrite,
                            onClick = {
                                menu = false
                                onScan()
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("Entfernen") },
                            enabled = canWrite,
                            onClick = {
                                menu = false
                                confirmRemove = true
                            },
                        )
                    }
                }
            }
            SelectionContainer {
                Text(provider.baseUrl, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
            }
            Text(
                "Anmeldung: ${authLabel(provider.authKind, provider.authName, provider.hasSecret)}",
                style = MaterialTheme.typography.bodySmall,
            )
            Text("Modelle: ${provider.models.size}", style = MaterialTheme.typography.bodySmall)
            if (writeBlock != null) {
                Text(writeBlock, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
    if (confirmRemove) {
        AlertDialog(
            onDismissRequest = { confirmRemove = false },
            title = { Text("Provider entfernen?") },
            text = { Text("${provider.name} wird aus der Server-Konfiguration entfernt. Der Server verlangt danach eine Bestätigung.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmRemove = false
                    onRemove()
                }) { Text("Entfernen") }
            },
            dismissButton = { TextButton(onClick = { confirmRemove = false }) { Text("Abbrechen") } },
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun RoleEditor(
    role: CloudRole,
    catalog: CloudCatalog,
    running: Boolean,
    block: String?,
    onSave: (provider: String, model: String, effort: String?) -> Unit,
    onReset: () -> Unit,
    onClose: () -> Unit,
) {
    var provider by rememberSaveable(role.role) { mutableStateOf(role.provider) }
    var model by rememberSaveable(role.role) { mutableStateOf(role.model) }
    var effort by rememberSaveable(role.role) { mutableStateOf(role.effort) }
    val providers = catalog.providers.filter { it.enabled }
    val models = selectableModels(catalog.models, provider)
    val chosen = models.firstOrNull { it.id == model }
    val canSave = block == null && !running && provider != null && chosen != null
    Column(
        Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(role.label, style = MaterialTheme.typography.titleMedium)
        Text("Aktuell: ${roleSummary(role)}", style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
        if (block != null) StatusMessage(block)
        Text("Provider", style = MaterialTheme.typography.labelMedium)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            providers.forEach { entry ->
                FilterChip(
                    selected = provider == entry.name,
                    onClick = {
                        if (provider != entry.name) {
                            provider = entry.name
                            model = null
                        }
                    },
                    label = { Text(entry.name) },
                )
            }
        }
        if (providers.isEmpty()) Text("Kein aktiver Provider.", style = MaterialTheme.typography.bodySmall)
        if (provider != null) {
            Text("Modell", style = MaterialTheme.typography.labelMedium)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                models.forEach { entry ->
                    FilterChip(
                        selected = model == entry.id,
                        onClick = { model = entry.id },
                        label = { Text(entry.name ?: entry.id) },
                    )
                }
            }
            if (models.isEmpty()) Text("Keine Modelle. Zuerst \"Modelle laden\".", style = MaterialTheme.typography.bodySmall)
            if (chosen != null && chosen.lifecycle != "active") {
                Text(
                    if (chosen.lifecycle == "unknown") "Lebenszyklus unbekannt" else "Hinweis: ${chosen.lifecycle.replace('_', ' ')}",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
        if (role.role in ROLES_WITH_EFFORT) {
            Text("Aufwand", style = MaterialTheme.typography.labelMedium)
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                FilterChip(selected = effort == null, onClick = { effort = null }, label = { Text("—") })
                EFFORT_LEVELS.forEach { level ->
                    FilterChip(selected = effort == level, onClick = { effort = level }, label = { Text(level) })
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                enabled = canSave,
                onClick = {
                    val p = provider
                    val m = model
                    if (p != null && m != null) onSave(p, m, effort?.takeIf { role.role in ROLES_WITH_EFFORT })
                },
            ) { Text("Speichern") }
            OutlinedButton(enabled = block == null && !running, onClick = onReset) { Text("Zurücksetzen") }
            TextButton(onClick = onClose) { Text("Zurück") }
        }
        Text(
            catalog.note.ifBlank { "Gilt ab nächster Session" },
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

// ---------------------------------------------------------------------------
// Provider anlegen
// ---------------------------------------------------------------------------

private fun knownAuthLabel(auth: ManageKnownAuth): String = when (auth) {
    ManageKnownAuth.API_KEY -> "API-Schlüssel"
    ManageKnownAuth.OAUTH -> "OAuth"
    ManageKnownAuth.NONE -> "Keine"
}

private fun headerLabel(header: String?): String = header ?: "Standard"

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun ProviderAddForm(
    state: WorkspaceState,
    connection: HarwConnection,
    running: Boolean,
    block: String?,
    onClose: () -> Unit,
) {
    val manage = state.manage
    var tab by rememberSaveable { mutableStateOf(0) }
    LaunchedEffect(state.online, state.generation) { connection.refreshKnownProviders() }
    Column(Modifier.fillMaxSize()) {
        TabRow(selectedTabIndex = tab) {
            Tab(selected = tab == 0, onClick = { tab = 0 }, text = { Text("Bekannt") })
            Tab(selected = tab == 1, onClick = { tab = 1 }, text = { Text("Eigener") })
        }
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            if (block != null) StatusMessage(block)
            if (tab == 0) {
                KnownProviderForm(manage.known, manage.knownError, running, block, connection, onClose)
            } else {
                CustomProviderForm(running, block, connection, onClose)
            }
            TextButton(onClick = onClose) { Text("Zurück") }
        }
    }
}

@Composable
private fun SecretField(label: String, value: String, onChange: (String) -> Unit) {
    OutlinedTextField(
        value = value,
        onValueChange = { onChange(it.replace("\n", "").replace("\r", "")) },
        label = { Text(label) },
        singleLine = true,
        visualTransformation = PasswordVisualTransformation(),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
        modifier = Modifier.fillMaxWidth(),
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun KnownProviderForm(
    known: List<ManageKnownProvider>,
    knownError: String?,
    running: Boolean,
    block: String?,
    connection: HarwConnection,
    onClose: () -> Unit,
) {
    var selectedId by rememberSaveable { mutableStateOf<String?>(null) }
    var authName by rememberSaveable { mutableStateOf<String?>(null) }
    var name by rememberSaveable { mutableStateOf("") }
    var keyName by rememberSaveable { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    DisposableEffect(Unit) { onDispose { secret = "" } }

    knownError?.let { StatusMessage(it, error = true) }
    if (known.isEmpty() && knownError == null) StatusMessage("Bekannte Provider werden geladen …")
    val entry = known.firstOrNull { it.id == selectedId }
    val auth = entry?.authKinds?.firstOrNull { it.name == authName } ?: entry?.authKinds?.firstOrNull { it != ManageKnownAuth.NONE }
    if (entry == null) {
        known.forEach { item ->
            Surface(
                Modifier.fillMaxWidth().clickable {
                    selectedId = item.id
                    val first = item.authKinds.firstOrNull { it != ManageKnownAuth.NONE }
                    authName = first?.name
                    name = if (first == ManageKnownAuth.OAUTH) item.defaultNameOauth ?: item.defaultNameApiKey else item.defaultNameApiKey
                    keyName = name
                    secret = ""
                    error = null
                },
                shape = RoundedCornerShape(12.dp),
                color = MaterialTheme.colorScheme.surfaceContainer,
            ) {
                Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(item.displayName, style = MaterialTheme.typography.titleSmall)
                    Text(item.baseUrl, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                    Text("Modelle: ${item.models.size}", style = MaterialTheme.typography.labelMedium)
                }
            }
        }
        return
    }
    if (auth == null) {
        StatusMessage("Dieser Provider bietet keine passende Anmeldeart.")
        OutlinedButton(onClick = { selectedId = null }) { Text("Anderen wählen") }
        return
    }
    val needsKey = auth == ManageKnownAuth.API_KEY || (auth == ManageKnownAuth.OAUTH && entry.oauthTokenPrefix != null)
    val prefix = if (auth == ManageKnownAuth.OAUTH) entry.oauthTokenPrefix else null
    val defaultName = if (auth == ManageKnownAuth.OAUTH) entry.defaultNameOauth else entry.defaultNameApiKey
    val taken = if (auth == ManageKnownAuth.OAUTH) entry.oauthTaken else entry.apiKeyTaken
    val nameTaken = taken && name == defaultName

    Text(entry.displayName, style = MaterialTheme.typography.titleMedium)
    SelectionContainer {
        Text(entry.baseUrl, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
    }
    Text("Anmeldung", style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        entry.authKinds.filter { it != ManageKnownAuth.NONE }.forEach { kind ->
            FilterChip(
                selected = auth == kind,
                onClick = {
                    authName = kind.name
                    name = (if (kind == ManageKnownAuth.OAUTH) entry.defaultNameOauth else entry.defaultNameApiKey)
                        ?: entry.defaultNameApiKey
                    keyName = name
                    secret = ""
                },
                label = { Text(knownAuthLabel(kind)) },
            )
        }
    }
    OutlinedTextField(
        value = name,
        onValueChange = { name = it.replace("\n", "") },
        label = { Text("Name") },
        singleLine = true,
        isError = name.isNotEmpty() && providerNameProblem(name) != null,
        supportingText = {
            Text(providerNameProblem(name).takeIf { name.isNotEmpty() } ?: if (nameTaken) "existiert schon – anderen Namen wählen" else "")
        },
        modifier = Modifier.fillMaxWidth(),
    )
    if (needsKey) {
        SecretField(if (prefix != null) "Setup-Token (beginnt mit $prefix)" else "API-Schlüssel", secret) { secret = it }
        OutlinedTextField(
            value = keyName,
            onValueChange = { keyName = it.replace("\n", "") },
            label = { Text("Schlüsselname") },
            singleLine = true,
            isError = keyName.isNotEmpty() && secretNameProblem(keyName) != null,
            supportingText = { Text(secretNameProblem(keyName).takeIf { keyName.isNotEmpty() } ?: "") },
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            "Der Schlüssel wird nur an den Server gesendet und nie angezeigt.",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    } else {
        Text("Nutzt das am Server verknüpfte Konto.", style = MaterialTheme.typography.bodySmall)
    }
    val prefixOk = prefix == null || secret.startsWith(prefix)
    if (needsKey && prefix != null && secret.isNotEmpty() && !prefixOk) {
        Text("Das Token beginnt nicht mit $prefix.", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.error)
    }
    val request = ProviderAddRequest(
        known = true, catalogId = entry.id, auth = auth, name = name, keyName = keyName,
        api = null, baseUrl = null, authHeader = null,
    )
    val valid = providerNameProblem(name) == null && !nameTaken &&
        (!needsKey || (secret.isNotEmpty() && prefixOk && secretNameProblem(keyName) == null)) &&
        providerAddLine(request, if (needsKey) "0" else null) != null
    error?.let { StatusMessage(it, error = true) }
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Button(
            enabled = valid && block == null && !running,
            onClick = {
                val value = secret
                error = null
                connection.addProvider(request, value.takeIf { needsKey }) { problem ->
                    secret = ""
                    if (problem == null) onClose() else error = problem
                }
                secret = ""
            },
        ) { Text("Anlegen") }
        OutlinedButton(onClick = { selectedId = null; secret = "" }) { Text("Anderen wählen") }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun CustomProviderForm(
    running: Boolean,
    block: String?,
    connection: HarwConnection,
    onClose: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var api by rememberSaveable { mutableStateOf(CUSTOM_APIS.first()) }
    var baseUrl by rememberSaveable { mutableStateOf("https://") }
    var withKey by rememberSaveable { mutableStateOf(true) }
    var header by rememberSaveable { mutableStateOf<String?>(null) }
    var keyName by rememberSaveable { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    DisposableEffect(Unit) { onDispose { secret = "" } }

    OutlinedTextField(
        value = name,
        onValueChange = {
            val same = keyName == name
            name = it.replace("\n", "")
            if (same) keyName = name
        },
        label = { Text("Name") },
        singleLine = true,
        isError = name.isNotEmpty() && providerNameProblem(name) != null,
        supportingText = { Text(providerNameProblem(name).takeIf { name.isNotEmpty() } ?: "") },
        modifier = Modifier.fillMaxWidth(),
    )
    Text("API", style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        CUSTOM_APIS.forEach { option ->
            FilterChip(selected = api == option, onClick = { api = option }, label = { Text(option) })
        }
    }
    val urlProblem = customBaseUrlProblem(baseUrl)
    OutlinedTextField(
        value = baseUrl,
        onValueChange = { baseUrl = it.replace("\n", "").trim() },
        label = { Text("Basis-URL") },
        singleLine = true,
        isError = baseUrl.length > 8 && urlProblem != null,
        supportingText = { Text(urlProblem.takeIf { baseUrl.length > 8 } ?: "nur https, keine lokale Adresse") },
        modifier = Modifier.fillMaxWidth(),
    )
    Text("Anmeldung", style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        FilterChip(selected = withKey, onClick = { withKey = true }, label = { Text("API-Schlüssel") })
        FilterChip(selected = !withKey, onClick = { withKey = false; secret = "" }, label = { Text("Keine") })
    }
    Text("Header", style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        FilterChip(selected = header == null, onClick = { header = null }, label = { Text(headerLabel(null)) })
        AUTH_HEADERS.forEach { option ->
            FilterChip(selected = header == option, onClick = { header = option }, label = { Text(option) })
        }
    }
    if (withKey) {
        SecretField("API-Schlüssel", secret) { secret = it }
        OutlinedTextField(
            value = keyName,
            onValueChange = { keyName = it.replace("\n", "") },
            label = { Text("Schlüsselname") },
            singleLine = true,
            isError = keyName.isNotEmpty() && secretNameProblem(keyName) != null,
            supportingText = { Text(secretNameProblem(keyName).takeIf { keyName.isNotEmpty() } ?: "") },
            modifier = Modifier.fillMaxWidth(),
        )
    }
    val request = ProviderAddRequest(
        known = false, catalogId = null,
        auth = if (withKey) ManageKnownAuth.API_KEY else ManageKnownAuth.NONE,
        name = name, keyName = keyName, api = api, baseUrl = baseUrl, authHeader = header,
    )
    val valid = providerAddLine(request, if (withKey) "0" else null) != null &&
        (!withKey || (secret.isNotEmpty() && secretNameProblem(keyName) == null))
    error?.let { StatusMessage(it, error = true) }
    Button(enabled = valid && block == null && !running, onClick = { confirm = true }) { Text("Anlegen") }

    if (confirm) {
        AlertDialog(
            onDismissRequest = { confirm = false },
            title = { Text("Eigenen Provider anlegen?") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Der Server sendet Anfragen an diese Adresse:")
                    SelectionContainer {
                        Text(baseUrl, style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
                    }
                    if (withKey) {
                        Text("Schlüsselname:")
                        SelectionContainer {
                            Text(keyName, style = MaterialTheme.typography.bodyMedium, fontFamily = FontFamily.Monospace)
                        }
                    } else {
                        Text("Ohne Anmeldung.")
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = {
                    confirm = false
                    error = null
                    val value = secret
                    connection.addProvider(request, value.takeIf { withKey }) { problem ->
                        secret = ""
                        if (problem == null) onClose() else error = problem
                    }
                    secret = ""
                }) { Text("Weiter") }
            },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text("Abbrechen") } },
        )
    }
}
