package dev.harw.mobile

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.Key
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.RoutePreference
import uniffi.harw_mobile_ffi.ServerProfile
import uniffi.harw_mobile_ffi.TorBootstrap

@Composable
internal fun ConnectionScreen(
    state: WorkspaceState,
    notificationDenied: Boolean,
    canGoBack: Boolean,
    onBack: () -> Unit,
    onImport: () -> Unit,
    onExport: () -> Unit,
    onShowEnrollmentQr: () -> Unit,
    scanning: Boolean,
    adminUrlSuggestion: String,
    onAdminSetup: (String) -> Unit,
    onScanProfile: () -> Unit,
    onConnect: () -> Unit,
    onConnectLocal: () -> Unit,
    onDisconnect: () -> Unit,
    onSelectRoute: (RoutePreference) -> Unit,
    onForgetSetup: () -> Unit,
    onRetryCleanup: () -> Unit,
    onCheckChatgpt: () -> Unit,
    onChatgptSignIn: (ChatgptTarget) -> Unit,
    onChatgptUnlink: (ChatgptTarget) -> Unit,
    onChatgptCancel: () -> Unit,
    onDismissPlanNotice: () -> Unit,
    onManageChatgpt: () -> Unit,
) {
    var confirmForget by rememberSaveable { mutableStateOf(false) }
    var adminDialog by rememberSaveable { mutableStateOf(false) }
    var adminUrl by rememberSaveable(adminUrlSuggestion) { mutableStateOf(adminUrlSuggestion) }
    if (adminDialog) {
        val valid = normalizeAdminUrl(adminUrl) != null
        AlertDialog(
            onDismissRequest = { adminDialog = false },
            title = { Text("Mit Admin-Konto einrichten") },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        "Gib die Adresse des Admin-Dienstes deines Harw-Servers ein. Das Telefon sendet seinen öffentlichen Schlüssel, danach meldest du dich im Browser an und gibst es frei.",
                        style = MaterialTheme.typography.bodyMedium,
                    )
                    OutlinedTextField(
                        value = adminUrl,
                        onValueChange = { adminUrl = it },
                        label = { Text("Admin-Adresse (https://…)") },
                        placeholder = { Text("https://admin.beispiel.de") },
                        singleLine = true,
                        isError = adminUrl.isNotBlank() && !valid,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    if (adminUrlSuggestion.isBlank()) {
                        Text(
                            "Der Server hat noch keine öffentliche Adresse mitgeteilt (in der config.toml des Servers: [admin] public_url).",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            },
            dismissButton = { TextButton(onClick = { adminDialog = false }) { Text("Abbrechen") } },
            confirmButton = {
                Button(
                    enabled = valid,
                    onClick = {
                        adminDialog = false
                        onAdminSetup(adminUrl)
                    },
                ) { Text("Weiter zum Browser") }
            },
        )
    }
    if (confirmForget) {
        AlertDialog(
            onDismissRequest = { confirmForget = false },
            title = { Text("Telefon-Einrichtung vergessen?") },
            text = { Text("Harw entfernt die gespeicherte Identität und das Serverprofil dieses Telefons. Richte das Telefon neu ein, bevor du dich wieder verbindest.") },
            dismissButton = { TextButton(onClick = { confirmForget = false }) { Text("Abbrechen") } },
            confirmButton = {
                Button(
                    enabled = state.canForget,
                    onClick = {
                        confirmForget = false
                        onForgetSetup()
                    },
                ) { Text("Einrichtung vergessen") }
            },
        )
    }
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Verbindung", onBack = onBack.takeIf { canGoBack }) {
            ConnectionBadge(state)
        }
        BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
            val wide = maxWidth >= 690.dp
            Column(
                Modifier.widthIn(max = 870.dp).fillMaxWidth().verticalScroll(rememberScrollState())
                    .padding(horizontal = if (wide) 28.dp else 20.dp, vertical = 24.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                if (state.profile == null && state.canSetUp) {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Dieses Telefon koppeln", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold)
                        Text(
                            "Registriere dieses Telefon mit einem öffentlichen Anmeldecode oder einer Anmeldedatei und scanne oder importiere dann das Profil von deinem Server-Betreiber.",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
                val device: @Composable () -> Unit = {
                    val node = state.deviceNodeId
                    SetupCard(Icons.Outlined.Key, "Dieses Telefon") {
                        Text(
                            if (node == null) "Erstelle und speichere die öffentliche Anmeldedatei des Telefons."
                            else "Anmeldeschlüssel ${node.take(16)}",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(enabled = state.canSetUp, onClick = onShowEnrollmentQr) {
                                Text("Anmelde-QR anzeigen")
                            }
                            OutlinedButton(enabled = state.canSetUp, onClick = onExport) {
                                Text("Anmeldedatei exportieren")
                            }
                        }
                    }
                }
                val profile: @Composable () -> Unit = {
                    val saved = state.profile
                    SetupCard(Icons.Outlined.Dns, saved?.label ?: "Harw-Server") {
                        if (saved == null) {
                            Text(
                                "Importiere das Serverprofil, um die Adresse festzulegen und die Identität zu prüfen.",
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        } else {
                            Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                                Text(
                                    "Direkt  ${saved.endpoint}",
                                    style = MaterialTheme.typography.bodyMedium,
                                    fontFamily = FontFamily.Monospace,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                saved.onion?.let {
                                    Text(
                                        "Tor  ${shortOnion(it)}",
                                        style = MaterialTheme.typography.bodyMedium,
                                        fontFamily = FontFamily.Monospace,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                            routeHint(saved, state.routePreference)?.let {
                                Text(
                                    it,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                for ((choice, name) in ROUTE_CHOICES) {
                                    val enabled = routeChoiceEnabled(choice, saved) &&
                                        state.canSetUp && !state.hasConnection && !state.stopping
                                    if (state.routePreference == choice) {
                                        Button(enabled = enabled, onClick = { onSelectRoute(choice) }) { Text(name) }
                                    } else {
                                        OutlinedButton(enabled = enabled, onClick = { onSelectRoute(choice) }) { Text(name) }
                                    }
                                }
                            }
                        }
                        run {
                            val ctx = androidx.compose.ui.platform.LocalContext.current
                            var auto by remember { mutableStateOf(readAutoConnect(ctx)) }
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text("Automatisch verbinden", Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
                                androidx.compose.material3.Switch(
                                    checked = auto,
                                    onCheckedChange = { auto = it; writeAutoConnect(ctx, it) },
                                )
                            }
                        }
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(
                                enabled = state.canSetUp && !state.hasConnection && !state.stopping && !scanning,
                                onClick = onScanProfile,
                            ) { Text("Server-QR scannen") }
                            OutlinedButton(
                                enabled = state.canSetUp && !state.hasConnection && !state.stopping,
                                onClick = onImport,
                            ) {
                                Icon(Icons.Outlined.Description, contentDescription = null, Modifier.size(17.dp))
                                Text(if (saved == null) "Profil importieren" else "Profil ersetzen", Modifier.padding(start = 7.dp))
                            }
                            if (state.hasConnection || state.stopping) {
                                Button(onClick = onDisconnect, enabled = !state.stopping) { Text("Trennen") }
                            } else {
                                Button(onClick = onConnect, enabled = saved != null && state.canSetUp) {
                                    Text(when (state.phase) {
                                        ConnectionPhase.FAILED -> "Erneut versuchen"
                                        ConnectionPhase.REVOKED -> "Erneut verbinden"
                                        else -> "Verbinden"
                                    })
                                }
                            }
                        }
                    }
                }
                Button(
                    enabled = state.canSetUp && !state.hasConnection && !state.stopping,
                    onClick = { adminDialog = true },
                ) { Text("Mit Admin-Konto einrichten") }
                OutlinedButton(
                    enabled = state.canSetUp && !state.hasConnection && !state.stopping,
                    onClick = onConnectLocal,
                ) { Text("Auf diesem Gerät starten") }
                if (wide) {
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                        Box(Modifier.weight(1f)) { device() }
                        Box(Modifier.weight(1.25f)) { profile() }
                    }
                } else {
                    device()
                    profile()
                }
                if (state.profile != null) {
                    ChatgptAccountCard(
                        state, onCheckChatgpt, onChatgptSignIn, onChatgptUnlink,
                        onChatgptCancel, onDismissPlanNotice, onManageChatgpt,
                    )
                }
                if (state.setupPending) {
                    Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                        Text("Einrichtung wird vorbereitet", style = MaterialTheme.typography.bodyMedium)
                    }
                }
                state.setupNotice?.let { StatusMessage(it) }
                state.setupError?.let { StatusMessage(it, error = true) }
                torProgressText(state.tor, state.phase, state.activeRoute)?.let { StatusMessage(it) }
                state.detail?.let {
                    StatusMessage(it, error = state.phase in setOf(ConnectionPhase.FAILED, ConnectionPhase.REVOKED))
                }
                state.cleanupError?.let {
                    StatusMessage(it, error = true)
                    OutlinedButton(enabled = !state.setupPending, onClick = onRetryCleanup) {
                        Text(if (state.forgetRequired) "Bereinigung wiederholen" else "Prüfung wiederholen")
                    }
                }
                if (state.canForget) {
                    OutlinedButton(onClick = { confirmForget = true }) { Text("Telefon-Einrichtung vergessen") }
                }
                if (notificationDenied) {
                    StatusMessage("Benachrichtigungen sind aus. Du kannst die Verbindung auf diesem Bildschirm trennen.")
                }
            }
        }
    }
}

@Composable
internal fun EnrollmentQrDialog(bitmap: android.graphics.Bitmap, onClose: () -> Unit) {
    Dialog(onDismissRequest = onClose) {
        Surface(shape = RoundedCornerShape(23.dp)) {
            BoxWithConstraints(Modifier.fillMaxWidth().padding(14.dp)) {
                val imageSize = minOf(maxWidth, maxHeight - 96.dp).coerceAtLeast(72.dp)
                Column(
                    Modifier.fillMaxWidth(),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text("Anmelde-QR des Telefons", style = MaterialTheme.typography.titleMedium)
                    Image(
                        bitmap.asImageBitmap(), contentDescription = "Öffentlicher Harw-Anmeldecode des Telefons",
                        modifier = Modifier.size(imageSize), filterQuality = FilterQuality.None,
                    )
                    TextButton(onClick = onClose) { Text("Fertig") }
                }
            }
        }
    }
}

@Composable
private fun ChatgptAccountCard(
    state: WorkspaceState,
    onCheck: () -> Unit,
    onSignIn: (ChatgptTarget) -> Unit,
    onUnlink: (ChatgptTarget) -> Unit,
    onCancel: () -> Unit,
    onDismissNotice: () -> Unit,
    onManage: () -> Unit,
) {
    val profile = state.profile ?: return
    val target = remember(profile, state.generation, state.online) {
        state.generation?.takeIf { state.online }?.let { ChatgptTarget(profile, it) }
    }
    val account = state.chatgpt
    var consent by remember { mutableStateOf<ChatgptTarget?>(null) }
    var unlink by remember { mutableStateOf<ChatgptTarget?>(null) }
    LaunchedEffect(target) { if (target != null) onCheck() }

    consent?.let { selected ->
        AlertDialog(
            onDismissRequest = { consent = null },
            title = { Text("ChatGPT mit ${selected.label} verknüpfen?") },
            text = {
                Text(
                    "Fahre nur fort, wenn du ${selected.label} (${selected.endpoint}) kontrollierst. " +
                        "Harw sendet die freigegebenen ChatGPT-Zugangsdaten an diesen geprüften Server, damit er deine berechtigten Anfragen ausführen kann.",
                )
            },
            dismissButton = { TextButton(onClick = { consent = null }) { Text("Abbrechen") } },
            confirmButton = {
                ContinueWithChatgpt(target == selected && account.operation == null) {
                    consent = null
                    onSignIn(selected)
                }
            },
        )
    }
    unlink?.let { selected ->
        AlertDialog(
            onDismissRequest = { unlink = null },
            title = { Text("ChatGPT von ${selected.label} trennen?") },
            text = { Text("Das entfernt das verknüpfte Konto vom ausgewählten Harw-Server.") },
            dismissButton = { TextButton(onClick = { unlink = null }) { Text("Abbrechen") } },
            confirmButton = {
                Button(
                    enabled = target == selected && account.operation == null,
                    onClick = {
                        unlink = null
                        onUnlink(selected)
                    },
                ) { Text("Konto trennen") }
            },
        )
    }
    if (account.showPlanNotice) {
        AlertDialog(
            onDismissRequest = onDismissNotice,
            title = { Text("ChatGPT-Abo verfügbar") },
            text = {
                Text(
                    "Berechtigte Harw-Anfragen auf ${profile.label} können dein ChatGPT-Abo nutzen. " +
                        "Zugriff und Limits verwaltest du in den ChatGPT-Einstellungen.",
                )
            },
            confirmButton = { Button(onClick = onDismissNotice) { Text("Verstanden") } },
        )
    }

    SetupCard(Icons.Outlined.AccountCircle, "ChatGPT-Konto") {
        val unavailable = account.unavailable
        val status = when (account.operation) {
            ChatgptOperation.Checking -> "Konto auf ${profile.label} wird geprüft …"
            ChatgptOperation.Browser -> "Schließe die Anmeldung ab und prüfe den Zugriff in deinem Browser."
            ChatgptOperation.Validating -> "ChatGPT-Autorisierung wird geprüft …"
            ChatgptOperation.Linking -> "Konto wird mit ${profile.label} verknüpft …"
            ChatgptOperation.Unlinking -> "Konto wird von ${profile.label} getrennt …"
            null -> when {
                target == null -> "Verbinde dich mit ${profile.label}, um den ChatGPT-Zugriff zu verwalten."
                unavailable != null -> unavailable
                account.linked == true -> "Verknüpft mit ${profile.label}"
                account.linked == false -> "Nutze dein ChatGPT-Abo für berechtigte Harw-Anfragen auf ${profile.label}."
                else -> "Prüfe, ob ${profile.label} ein ChatGPT-Konto verknüpfen kann."
            }
        }
        Text(status, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        account.error?.let { StatusMessage(it, error = true) }
        if (account.operation in setOf(ChatgptOperation.Browser, ChatgptOperation.Validating)) {
            OutlinedButton(onClick = onCancel) { Text("Anmeldung abbrechen") }
        } else if (target != null && account.operation == null) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (account.canStartSignIn()) {
                    ContinueWithChatgpt { consent = target }
                } else if (account.linked == true) {
                    OutlinedButton(onClick = { unlink = target }) { Text("Konto trennen") }
                    TextButton(onClick = onManage) { Text("Nutzung verwalten") }
                }
                if (account.linked != false || account.error != null || unavailable != null) {
                    TextButton(onClick = onCheck) { Text("Konto prüfen") }
                }
            }
        }
    }
}

@Composable
private fun ContinueWithChatgpt(enabled: Boolean = true, onClick: () -> Unit) {
    val colors = MaterialTheme.colorScheme
    Button(
        onClick = onClick,
        enabled = enabled,
        colors = ButtonDefaults.buttonColors(containerColor = colors.onSurface, contentColor = colors.surface),
    ) {
        Text("Weiter mit ChatGPT")
    }
}

@Composable
internal fun HarwHeader(
    title: String,
    onBack: (() -> Unit)? = null,
    actions: @Composable RowScope.() -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(start = if (onBack == null) 17.dp else 4.dp, end = 12.dp, top = 3.dp, bottom = 3.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        if (onBack != null) {
            IconButton(onClick = onBack) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, contentDescription = "Zurück") }
        } else {
            Surface(shape = RoundedCornerShape(11.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh) {
                Image(painterResource(R.drawable.harw_mark), contentDescription = null, Modifier.padding(6.dp).size(27.dp))
            }
        }
        Column(Modifier.weight(1f).padding(start = 4.dp, top = 6.dp, bottom = 6.dp)) {
            Text("Harw", style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary, fontWeight = FontWeight.Bold)
            Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        actions()
    }
    HorizontalDivider(color = MaterialTheme.colorScheme.outlineVariant)
}

@Composable
internal fun ConnectionBadge(state: WorkspaceState) {
    val label = connectionBadgeText(state.phase, state.stopping, state.activeRoute)
    val colors = MaterialTheme.colorScheme
    val (background, foreground) = when {
        state.online -> colors.tertiaryContainer to colors.onTertiaryContainer
        state.phase in setOf(ConnectionPhase.FAILED, ConnectionPhase.REVOKED) -> colors.errorContainer to colors.onErrorContainer
        else -> colors.secondaryContainer to colors.onSecondaryContainer
    }
    Surface(shape = CircleShape, color = background, contentColor = foreground) {
        Row(
            Modifier.padding(horizontal = 10.dp, vertical = 5.dp),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(Modifier.size(6.dp).background(foreground, CircleShape))
            Text(label, style = MaterialTheme.typography.labelMedium, maxLines = 1)
        }
    }
}

@Composable
internal fun StatusMessage(text: String, modifier: Modifier = Modifier, error: Boolean = false) {
    val colors = MaterialTheme.colorScheme
    Surface(
        modifier.fillMaxWidth(),
        shape = RoundedCornerShape(12.dp),
        color = if (error) colors.errorContainer else colors.surfaceContainer,
        contentColor = if (error) colors.onErrorContainer else colors.onSurfaceVariant,
    ) {
        Text(text, Modifier.padding(horizontal = 12.dp, vertical = 9.dp), style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun SetupCard(icon: ImageVector, title: String, content: @Composable () -> Unit) {
    Surface(
        Modifier.fillMaxWidth(), shape = RoundedCornerShape(22.dp),
        color = MaterialTheme.colorScheme.surfaceContainerLowest,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(9.dp)) {
                Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.primary, modifier = Modifier.size(21.dp))
                Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            }
            content()
        }
    }
}

private val ROUTE_CHOICES = listOf(
    RoutePreference.AUTO to "Auto",
    RoutePreference.ONION to "Tor (.onion)",
    RoutePreference.DIRECT to "Direkt",
)

/** German hint under the route chips; null when nothing needs explaining. */
internal fun routeHint(profile: ServerProfile?, preference: RoutePreference): String? = when {
    profile == null -> null
    profile.onion == null ->
        "Tor ist nicht verfügbar: Das Gateway hat noch keine .onion-Adresse gemeldet. " +
            "Verbinde dich einmal direkt – die Adresse wird dann automatisch übernommen " +
            "(das Gateway braucht dafür onion = true und einen Build mit Tor-Unterstützung)."
    preference == RoutePreference.AUTO -> "Auto verbindet zuerst direkt und weicht bei Fehlschlag auf Tor aus."
    preference == RoutePreference.ONION -> "Nur über Tor. Der erste Start kann bis zu zwei Minuten dauern."
    else -> null
}

/** Routes a saved profile offers, in display order; empty without a profile. */
internal fun profileRoutes(profile: ServerProfile?): List<ConnectionRoute> = when {
    profile == null -> emptyList()
    profile.onion != null -> listOf(ConnectionRoute.ONION, ConnectionRoute.DIRECT)
    else -> listOf(ConnectionRoute.DIRECT)
}

/** Whether a route preference can be selected for this profile. */
internal fun routeChoiceEnabled(choice: RoutePreference, profile: ServerProfile?): Boolean = when {
    profile == null -> false
    choice == RoutePreference.ONION -> profile.onion != null
    else -> true
}

/** Badge label for the connection phase, with the active route while a link is being made or live. */
internal fun connectionBadgeText(phase: ConnectionPhase, stopping: Boolean, route: ConnectionRoute?): String {
    if (stopping) return "Wird beendet"
    val base = when (phase) {
        ConnectionPhase.DISCONNECTED -> "Offline"
        ConnectionPhase.CONNECTING -> "Verbinde"
        ConnectionPhase.CONNECTED -> "Live"
        ConnectionPhase.RECONNECTING -> "Verbinde erneut"
        ConnectionPhase.FAILED -> "Fehlgeschlagen"
        ConnectionPhase.REVOKED -> "Zugriff beendet"
    }
    val routed = phase == ConnectionPhase.CONNECTING || phase == ConnectionPhase.CONNECTED ||
        phase == ConnectionPhase.RECONNECTING
    if (!routed || route == null) return base
    return base + when (route) {
        ConnectionRoute.ONION -> " · Onion"
        ConnectionRoute.DIRECT -> " · direkt"
    }
}

/** Tor bootstrap status line, or null when there is nothing to show. */
internal fun torProgressText(tor: TorBootstrap?, phase: ConnectionPhase, route: ConnectionRoute?): String? {
    if (tor == null) return null
    tor.blocked?.let { return "Tor blockiert: $it" }
    if (!tor.ready) return "Tor-Start ${tor.percent}% · ${tor.summary}"
    val dialing = phase == ConnectionPhase.CONNECTING || phase == ConnectionPhase.RECONNECTING
    return if (route == ConnectionRoute.ONION && dialing) "Tor bereit · erreiche den .onion-Dienst" else null
}

/** Shortened display form of an onion locator: first 6 characters, then the port. */
internal fun shortOnion(locator: String): String =
    locator.take(6) + "….onion:" + locator.substringAfterLast(':')
