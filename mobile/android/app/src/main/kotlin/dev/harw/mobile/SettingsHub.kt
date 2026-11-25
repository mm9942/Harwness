package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.provider.Settings
import android.widget.Toast
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
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
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.ManageConfigEntry
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.setLocalImageConcurrency

internal enum class SettingsRoute { Hub, Connection, Models, Commands, Providers, Agents, Jobs, LocalModels, Files, Termux, Uias, Knowledge, PinBiometrics, Vpn }

/** Eltern-Route fuer Zurueck; `null` = Einstellungen schliessen. */
internal fun settingsParent(route: SettingsRoute): SettingsRoute? = when (route) {
    SettingsRoute.Hub -> null
    SettingsRoute.Connection, SettingsRoute.Models, SettingsRoute.Commands,
    SettingsRoute.Providers, SettingsRoute.Agents, SettingsRoute.Jobs, SettingsRoute.LocalModels,
    SettingsRoute.Files, SettingsRoute.Termux, SettingsRoute.Uias, SettingsRoute.Knowledge,
    SettingsRoute.PinBiometrics, SettingsRoute.Vpn -> SettingsRoute.Hub
}

internal const val UI_PREFS = "harw_ui"
internal const val PREF_THEME = "theme" // "system" | "light" | "dark"
internal const val PREF_SHOW_REASONING = "show_reasoning"
internal const val PREF_SCREENSHOT_PROTECTION = "screenshot_protection"

internal fun readScreenshotProtection(context: Context): Boolean =
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE)
        .getBoolean(PREF_SCREENSHOT_PROTECTION, true)

internal fun writeScreenshotProtection(context: Context, enabled: Boolean) {
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE)
        .edit().putBoolean(PREF_SCREENSHOT_PROTECTION, enabled).apply()
}
private const val PREF_LEGACY_IMAGE_PARALLEL = "image_parallel"
internal const val PREF_LOCAL_IMAGE_CONCURRENCY = "image_concurrency"
private val visionSettingsLock = Mutex()

internal fun restoredImageConcurrency(value: Any?, legacyParallel: Boolean?): Int = when {
    value == null -> if (legacyParallel == false) 1 else DefaultLocalVisionConcurrency
    value is Int && value in LocalVisionConcurrencyRange -> value
    else -> 1
}

internal fun readLocalImageConcurrency(context: Context): Int = try {
    val prefs = context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).all
    restoredImageConcurrency(
        prefs[PREF_LOCAL_IMAGE_CONCURRENCY],
        prefs[PREF_LEGACY_IMAGE_PARALLEL] as? Boolean,
    )
} catch (_: RuntimeException) {
    1
}

internal fun applyLocalImageConcurrency(value: Int): Boolean =
    value in LocalVisionConcurrencyRange && runCatching { setLocalImageConcurrency(value.toUByte()) }.getOrDefault(false)

private fun writeLocalImageConcurrency(context: Context, value: Int): Boolean = try {
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).edit()
        .putInt(PREF_LOCAL_IMAGE_CONCURRENCY, value)
        .remove(PREF_LEGACY_IMAGE_PARALLEL)
        .commit()
} catch (_: RuntimeException) {
    false
}

private val THEME_CHOICES = listOf(
    "system" to R.string.settings_hub_theme_system,
    "light" to R.string.settings_hub_theme_light,
    "dark" to R.string.settings_hub_theme_dark,
)
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

/** Grund, warum Cloud-Verwaltung nicht verfügbar ist; `null` = Lesen ist freigegeben. */
internal fun manageOpenBlock(state: WorkspaceState): Int? {
    val support = state.manage.support
    return when {
        state.local -> R.string.settings_hub_access_cloud_only
        state.setupPending -> R.string.settings_hub_access_loading
        state.profile == null -> R.string.settings_hub_access_no_profile
        state.phase == ConnectionPhase.CONNECTING || state.phase == ConnectionPhase.RECONNECTING ->
            R.string.settings_hub_access_connecting
        state.phase == ConnectionPhase.REVOKED -> R.string.settings_hub_access_revoked
        !state.online -> R.string.settings_hub_access_offline
        support == null -> R.string.settings_hub_access_checking
        !support.manage -> R.string.settings_hub_access_unavailable
        !support.observe -> R.string.settings_hub_access_read_denied
        else -> null
    }
}

/** Rohwerte lesen braucht Observe; Änderungen brauchen außerdem Control, Approve und Step-up. */
internal fun configWritable(support: ManageSupport?): Boolean =
    support != null && support.manage && support.observe && support.control && support.approve && support.stepup

internal fun configApplyLabel(apply: String): String = when (apply) {
    "now" -> "sofort"
    "next_session" -> "ab nächster Session"
    else -> apply
}

/** Phase and route only; never disclose the profile label or endpoint. */
@Composable
private fun connectionSubtitle(state: WorkspaceState): String {
    val phase = stringResource(when (state.phase) {
        ConnectionPhase.DISCONNECTED -> R.string.settings_hub_phase_offline
        ConnectionPhase.CONNECTING -> R.string.settings_hub_phase_connecting
        ConnectionPhase.CONNECTED -> R.string.settings_hub_phase_connected
        ConnectionPhase.RECONNECTING -> R.string.settings_hub_phase_reconnecting
        ConnectionPhase.FAILED -> R.string.settings_hub_phase_failed
        ConnectionPhase.REVOKED -> R.string.settings_hub_access_revoked
    })
    val route = when (state.activeRoute) {
        ConnectionRoute.DIRECT -> stringResource(R.string.settings_hub_route_direct)
        ConnectionRoute.ONION -> stringResource(R.string.settings_hub_route_tor)
        null -> return phase
    }
    return stringResource(R.string.settings_hub_phase_route, phase, route)
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
    diagnosticsConnection: HarwConnection = connection,
    diagnosticsLocal: Boolean = false,
    localDisconnected: Boolean,
    disconnectLocal: () -> Unit,
) {
    val context = LocalContext.current
    val cloudBlock = manageOpenBlock(state)?.let { stringResource(it) }
    LaunchedEffect(connection, state.online, state.generation) {
        if (state.online) connection.refreshManageSupport()
    }
    var modelsFolderOpen by remember { mutableStateOf(false) }
    var diagnosticsOpen by remember(diagnosticsConnection) { mutableStateOf(false) }
    var modelsFolderRevision by remember { mutableStateOf(0) }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = stringResource(R.string.settings_hub_title), onBack = onBack) {}
        Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(vertical = 4.dp)) {
            HubSection(stringResource(R.string.settings_hub_profile_section), first = true) {
                HubRow(stringResource(R.string.settings_hub_connection), connectionSubtitle(state), null) { onOpen(SettingsRoute.Connection) }
                if (android.os.Build.VERSION.SDK_INT >= 29) {
                    val vpnPhase by HarwVpnService.phase.collectAsState()
                    HubRow(stringResource(R.string.vpn_title), stringResource(vpnPhase.text), null) {
                        onOpen(SettingsRoute.Vpn)
                    }
                }
            }
            HubSection(stringResource(R.string.settings_hub_models_section)) {
                HubRow(stringResource(R.string.settings_hub_cloud_models), stringResource(R.string.settings_hub_cloud_models_detail), cloudBlock) {
                    onOpen(SettingsRoute.Models)
                }
                HubRow(stringResource(R.string.settings_hub_device_providers), stringResource(R.string.settings_hub_device_providers_detail), null) {
                    onOpen(SettingsRoute.Providers)
                }
                HubRow(stringResource(R.string.settings_hub_local_models), stringResource(R.string.settings_hub_local_models_detail), null) {
                    onOpen(SettingsRoute.LocalModels)
                }
                HubRow(stringResource(R.string.settings_hub_model_folder), modelsFolderLabel(context, modelsFolderRevision), null) {
                    modelsFolderOpen = true
                }
            }
            HubSection(stringResource(R.string.settings_hub_vision_section)) {
                VisionDescriptionSettings(context, onConfigureProviders = { onOpen(SettingsRoute.Providers) }) {
                    LocalImageSettings(context)
                }
                ChatTitleSettings(context, loadCatalog = { loadChatTitleModelCatalog(context).forSettings() })
            }
            HubSection(stringResource(R.string.harw_ui_appearance_title)) {
                HarwLanguageSetting()
                DisplaySettings(context)
            }
            HubSection(stringResource(R.string.settings_hub_files_section)) {
                HubRow(stringResource(R.string.settings_hub_files), stringResource(R.string.settings_hub_files_detail), null) {
                    onOpen(SettingsRoute.Files)
                }
            }
            HubSection(stringResource(R.string.settings_hub_tools_section)) {
                HubRow(stringResource(R.string.settings_hub_agents), stringResource(R.string.settings_hub_agents_detail), null) {
                    onOpen(SettingsRoute.Agents)
                }
                HubRow(stringResource(R.string.settings_hub_jobs), stringResource(R.string.settings_hub_jobs_detail), null) {
                    onOpen(SettingsRoute.Jobs)
                }
                HubRow(stringResource(R.string.settings_hub_device_agents), stringResource(R.string.settings_hub_device_agents_detail), null) {
                    onOpen(SettingsRoute.Uias)
                }
                HubRow(stringResource(R.string.settings_hub_commands), stringResource(R.string.settings_hub_commands_detail), cloudBlock) {
                    onOpen(SettingsRoute.Commands)
                }
                HubRow(stringResource(R.string.settings_hub_knowledge), stringResource(R.string.settings_hub_knowledge_detail), null) {
                    onOpen(SettingsRoute.Knowledge)
                }
                HubRow("Termux", stringResource(R.string.settings_hub_termux_detail), null) { onOpen(SettingsRoute.Termux) }
            }
            HubSection(stringResource(R.string.settings_hub_app_section)) {
                if (BuildConfig.LOCAL_UIA) {
                    HubRow(stringResource(R.string.pin_biometrics_title), stringResource(R.string.pin_settings_summary), null) {
                        onOpen(SettingsRoute.PinBiometrics)
                    }
                }
                HubRow(stringResource(R.string.settings_hub_notifications), stringResource(R.string.settings_hub_notifications_detail), null) {
                    openNotificationSettings(context)
                }
                AppUpdateHubRow(context)
                LocalBackupSettings(localDisconnected, disconnectLocal)
                HubRow(stringResource(R.string.settings_hub_share_diagnostics), stringResource(R.string.settings_hub_share_diagnostics_detail), null) {
                    shareDiagnostic(context, diagnosticReport(state, appVersionOf(context)))
                }
                HubRow(stringResource(R.string.settings_hub_debug_log), stringResource(R.string.settings_hub_debug_log_detail), null) {
                    diagnosticsOpen = true
                }
            }
            HubSection(stringResource(R.string.settings_hub_advanced_section)) {
                AdvancedCloudSettings(state, connection, cloudBlock)
            }
        }
    }

    if (modelsFolderOpen) {
        ModelsFolderDialog(
            context = context,
            onDismiss = { modelsFolderOpen = false },
            onChanged = { modelsFolderRevision++ },
        )
    }
    if (diagnosticsOpen) {
        DiagnosticsPanel(diagnosticsConnection, diagnosticsLocal, onClose = { diagnosticsOpen = false })
    }
}

@Composable
private fun HubSection(title: String, first: Boolean = false, content: @Composable () -> Unit) {
    if (!first) {
        HorizontalDivider(Modifier.padding(vertical = 8.dp), color = MaterialTheme.colorScheme.outlineVariant)
    }
    SectionTitle(title)
    content()
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun DisplaySettings(context: Context) {
    var theme by remember { mutableStateOf(readThemePref(context)) }
    var showReasoning by remember { mutableStateOf(readShowReasoningPref(context)) }
    var screenshotProtection by remember { mutableStateOf(readScreenshotProtection(context)) }
    val homePrefs = remember(context) { SessionHomePreferences(context) }
    var showSystemSessions by remember(homePrefs) { mutableStateOf(homePrefs.showSystemSessions()) }
    FlowRow(
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
                label = { Text(stringResource(label)) },
            )
        }
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(androidx.compose.ui.res.stringResource(R.string.unified_home_show_internal), style = MaterialTheme.typography.bodyLarge)
            Text(androidx.compose.ui.res.stringResource(R.string.unified_home_internal_summary), style = MaterialTheme.typography.bodySmall)
        }
        Switch(
            checked = showSystemSessions,
            onCheckedChange = { value ->
                showSystemSessions = value
                homePrefs.setShowSystemSessions(value)
            },
        )
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(stringResource(R.string.settings_hub_reasoning), Modifier.weight(1f), style = MaterialTheme.typography.bodyLarge)
        Switch(
            checked = showReasoning,
            onCheckedChange = {
                showReasoning = it
                writeShowReasoningPref(context, it)
            },
        )
    }
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(stringResource(R.string.settings_hub_screenshots), style = MaterialTheme.typography.bodyLarge)
            Text(
                stringResource(R.string.settings_hub_screenshots_detail),
                style = MaterialTheme.typography.bodySmall,
            )
        }
        Switch(checked = screenshotProtection, onCheckedChange = {
            screenshotProtection = it
            writeScreenshotProtection(context, it)
        })
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun LocalImageSettings(context: Context) {
    val appContext = context.applicationContext
    val scope = rememberCoroutineScope()
    var imageConcurrency by remember { mutableStateOf<Int?>(null) }
    var saving by remember { mutableStateOf(false) }
    var notice by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(appContext) {
        imageConcurrency = withContext(Dispatchers.IO) {
            visionSettingsLock.withLock { readLocalImageConcurrency(appContext) }
        }
    }
    Column(
        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Text(stringResource(R.string.harw_local_vision_parallel_title), style = MaterialTheme.typography.bodyLarge)
        Text(
            stringResource(R.string.harw_local_vision_parallel_explainer),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            LocalVisionConcurrencyRange.forEach { count ->
                FilterChip(
                    selected = imageConcurrency == count,
                    enabled = imageConcurrency != null && !saving,
                    onClick = {
                        if (imageConcurrency != null && !saving && count != imageConcurrency) {
                            saving = true
                            notice = null
                            scope.launch {
                                // Once started, finish the runtime/persistence pair even if Settings closes.
                                val (previous, change) = withContext(Dispatchers.IO + NonCancellable) {
                                    visionSettingsLock.withLock {
                                        val current = readLocalImageConcurrency(appContext)
                                        current to changeVisionConcurrency(
                                            current,
                                            count,
                                            ::applyLocalImageConcurrency,
                                            { writeLocalImageConcurrency(appContext, it) },
                                        )
                                    }
                                }
                                imageConcurrency = if (change == VisionConcurrencyChange.Applied) count else previous
                                notice = when (change) {
                                    VisionConcurrencyChange.Applied, VisionConcurrencyChange.Unchanged -> null
                                    VisionConcurrencyChange.Invalid -> "Diese Bildparallelität wird nicht unterstützt."
                                    VisionConcurrencyChange.NativeUnavailable -> "Die lokale Bildparallelität ist in diesem Build nicht verfügbar."
                                    VisionConcurrencyChange.PersistenceFailed -> "Nicht gespeichert. Das bisherige Limit bleibt aktiv."
                                    VisionConcurrencyChange.RestoreFailed -> "Das bisherige Limit ließ sich nicht sicher wiederherstellen. Bitte starte Harw neu und prüfe die Einstellung."
                                }
                                saving = false
                            }
                        }
                    },
                    label = { Text(pluralStringResource(R.plurals.harw_local_vision_parallel_images, count, count)) },
                )
            }
        }
        imageConcurrency?.let { value ->
            Text(stringResource(R.string.harw_local_vision_parallel_requested, value), style = MaterialTheme.typography.bodySmall)
        }
        if (saving) Text("Wird gespeichert …", style = MaterialTheme.typography.bodySmall)
        notice?.let { StatusMessage(it, error = true) }
    }
}

@Composable
private fun AppUpdateHubRow(context: Context) {
    val availableUpdate by AppUpdates.available.collectAsState()
    val channel by AppUpdates.channel.collectAsState()
    var selectingChannel by remember { mutableStateOf(false) }
    val label = if (channel == AppReleaseChannel.Beta) "Beta" else "Stabil"
    HubRow("Update-Kanal", label, null) { selectingChannel = true }
    if (selectingChannel) {
        AlertDialog(
            onDismissRequest = { selectingChannel = false },
            title = { Text("Update-Kanal") },
            text = { Text("Beta enthält Vorabversionen. Beim Wechsel zu Stabil wird die nächste höhere stabile Version angeboten.") },
            confirmButton = {
                TextButton(onClick = {
                    AppUpdates.selectChannel(context, AppReleaseChannel.Beta)
                    selectingChannel = false
                }) { Text("Beta") }
            },
            dismissButton = {
                TextButton(onClick = {
                    AppUpdates.selectChannel(context, AppReleaseChannel.Stable)
                    selectingChannel = false
                }) { Text("Stabil") }
            },
        )
    }

    var updateStatus by remember(channel) { mutableStateOf<String?>(null) }
    val updateScope = rememberCoroutineScope()
    val subtitle = availableUpdate?.let { "Version ${it.versionName} ist verfügbar. Tippen zum Installieren." }
        ?: updateStatus
        ?: "Installiert: ${AppUpdates.installedName(context)}. Tippen, um nach Updates zu suchen."
    HubRow(stringResource(R.string.settings_hub_updates), subtitle, null) {
        val known = availableUpdate
        if (known != null) {
            AppUpdates.consider(context, known, force = true)
        } else {
            updateStatus = "Suche nach Updates …"
            updateScope.launch {
                val latest = withContext(Dispatchers.IO) { AppUpdates.fetchLatest(channel) }
                if (AppUpdates.channel.value != channel) return@launch
                if (latest == null) {
                    updateStatus = "Keine Verbindung zum Update-Dienst."
                } else if (latest.versionCode > AppUpdates.installedCode(context)) {
                    AppUpdates.consider(context, latest, force = true)
                    updateStatus = null
                } else {
                    AppUpdates.consider(context, latest)
                    updateStatus = "Du hast die neueste Version (${AppUpdates.installedName(context)})."
                }
            }
        }
    }
}

@Composable
private fun AdvancedCloudSettings(state: WorkspaceState, connection: HarwConnection, cloudBlock: String?) {
    var expanded by remember(connection, state.generation) { mutableStateOf(false) }
    var editing by remember(connection, state.generation, cloudBlock) { mutableStateOf<ManageConfigEntry?>(null) }
    HubRow(
        "Server- & TUI-Parameter",
        "Cloud-Profil · ${if (expanded) "Ausblenden" else "Anzeigen"}",
        null,
    ) {
        expanded = !expanded
    }
    if (!expanded) return

    LaunchedEffect(connection, state.generation, cloudBlock) {
        if (cloudBlock == null) connection.refreshConfig()
    }
    if (cloudBlock != null) {
        StatusMessage(cloudBlock, Modifier.padding(horizontal = 16.dp, vertical = 4.dp))
        return
    }

    val manage = state.manage
    val writable = configWritable(manage.support)
    if (!writable) {
        StatusMessage(CONFIG_READONLY_HINT, Modifier.padding(horizontal = 16.dp, vertical = 4.dp))
    }
    val configError = manage.configError
    if (configError != null) {
        StatusMessage(configError, Modifier.padding(horizontal = 16.dp, vertical = 4.dp), error = true)
    } else if (manage.config.isEmpty()) {
        StatusMessage("Noch keine Serverwerte verfügbar.", Modifier.padding(horizontal = 16.dp, vertical = 4.dp))
    }
    manage.config.forEach { entry ->
        HubRow(
            title = "${entry.key} = ${entry.value}",
            subtitle = "Cloud-Profil · Gilt ${configApplyLabel(entry.apply)}" + if (writable) "" else " · Nur lesen",
            block = null,
        ) { editing = entry }
    }

    editing?.let { entry ->
        ConfigEditDialog(
            entry = entry,
            enabled = manage.running == null && manage.stepUp == null && writable,
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

/** Untertitel der Zeile für den Modellordner: aktueller Pfad, Zusatz bei eigener Wahl. */
private fun modelsFolderLabel(context: Context, @Suppress("UNUSED_PARAMETER") revision: Int): String = try {
    val store = ModelStore(context)
    val custom = store.customDir()
    if (custom != null) "${custom.path} (eigener Ordner)" else "${store.defaultDir().path} (Standard)"
} catch (_: Exception) {
    "Ordner unbekannt"
}

/** Tree-URI des Dokumentenpickers -> Dateipfad im internen Speicher; `null` bei anderen Speichern. */
private fun treeUriToPath(uri: android.net.Uri): String? {
    val docId = try {
        android.provider.DocumentsContract.getTreeDocumentId(uri)
    } catch (_: Exception) {
        return null
    }
    val (volume, rel) = docId.split(":", limit = 2).let { it[0] to it.getOrElse(1) { "" } }
    if (volume != "primary") return null
    val root = android.os.Environment.getExternalStorageDirectory()
    return if (rel.isEmpty()) root.path else java.io.File(root, rel).path
}

@Composable
private fun ModelsFolderDialog(context: Context, onDismiss: () -> Unit, onChanged: () -> Unit) {
    val store = remember { ModelStore(context) }
    var path by remember { mutableStateOf((store.customDir() ?: store.defaultDir()).path) }
    var error by remember { mutableStateOf<String?>(null) }
    var ggufCount by remember { mutableStateOf<Int?>(null) }
    val picker = androidx.activity.compose.rememberLauncherForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.OpenDocumentTree(),
    ) { uri ->
        if (uri != null) {
            val picked = treeUriToPath(uri)
            if (picked == null) error = "Nur Ordner im internen Speicher des Geräts sind möglich." else {
                path = picked
                error = null
            }
        }
    }
    LaunchedEffect(path) {
        ggufCount = try {
            java.io.File(path).listFiles { f -> f.isFile && f.name.endsWith(".gguf", ignoreCase = true) }?.size
        } catch (_: Exception) {
            null
        }
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.settings_hub_model_folder)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    "Alle GGUF-Dateien in diesem Ordner erscheinen unter „Auf diesem Gerät“. " +
                        "Ein neues Modell muss danach erneut gewählt werden.",
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedTextField(
                    value = path,
                    onValueChange = { path = it.replace("\n", "").replace("\r", ""); error = null },
                    singleLine = true,
                    label = { Text("Pfad") },
                    supportingText = {
                        Text(ggufCount?.let { "$it GGUF-Datei(en) gefunden" } ?: "Ordner nicht lesbar oder neu")
                    },
                    modifier = Modifier.fillMaxWidth(),
                )
                TextButton(onClick = { picker.launch(null) }) { Text("Ordner wählen …") }
                TextButton(onClick = { path = store.dirFor(store.location).path; error = null }) {
                    Text("Standard verwenden")
                }
                error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Abbrechen") } },
        confirmButton = {
            TextButton(
                onClick = {
                    val dir = java.io.File(path.trim())
                    val isDefault = dir.absolutePath == store.dirFor(store.location).absolutePath
                    when {
                        path.isBlank() || !dir.isAbsolute -> error = "Bitte einen absoluten Pfad angeben."
                        !store.probeDir(dir) -> error = "Der Ordner ist nicht beschreibbar (Speicherzugriff prüfen)."
                        else -> {
                            store.setCustomDir(if (isDefault) null else dir)
                            onChanged()
                            onDismiss()
                        }
                    }
                },
            ) { Text("Übernehmen") }
        },
    )
}

@Composable
private fun SectionTitle(text: String) {
    Text(
        text,
        Modifier.padding(horizontal = 16.dp, vertical = 6.dp).semantics { heading() },
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

@Composable
private fun HubRow(
    title: String,
    subtitle: String,
    block: String?,
    onClick: () -> Unit,
) {
    val locked = block != null
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
                    "Cloud-Profil. Gilt ${configApplyLabel(entry.apply)}. Änderungen brauchen eine zusätzliche Gerätebestätigung.",
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
        Toast.makeText(context, R.string.settings_hub_notifications_error, Toast.LENGTH_SHORT).show()
    }
}

private fun shareDiagnostic(context: Context, report: String) {
    try {
        val send = Intent(Intent.ACTION_SEND)
            .setType("text/plain")
            .putExtra(Intent.EXTRA_SUBJECT, "Harw Diagnose")
            .putExtra(Intent.EXTRA_TEXT, report)
        context.startActivity(Intent.createChooser(send, context.getString(R.string.settings_hub_share_diagnostics)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    } catch (_: Exception) {
        Toast.makeText(context, "Teilen nicht möglich", Toast.LENGTH_SHORT).show()
    }
}
