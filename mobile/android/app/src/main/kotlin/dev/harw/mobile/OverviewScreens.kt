package dev.harw.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.ManageRunResult
import uniffi.harw_mobile_ffi.OverviewAgent
import uniffi.harw_mobile_ffi.OverviewAgentDef
import uniffi.harw_mobile_ffi.OverviewAgents
import uniffi.harw_mobile_ffi.OverviewJob
import uniffi.harw_mobile_ffi.OverviewJobDetail
import uniffi.harw_mobile_ffi.OverviewJobs

/** Globale Jobs- und Agenten-Ansichten: Lokal = On-Device-Host, Cloud = Gateway. */
internal enum class OverviewTarget(val label: String) {
    Local("Lokal"),
    Cloud("Cloud"),
}

private const val REFRESH_MS = 5_000L

// ---------------------------------------------------------------------------
// Reine Funktionen (testbar)
// ---------------------------------------------------------------------------

internal data class JobFilter(val label: String, val states: List<String>)

internal val JOB_FILTERS = listOf(
    JobFilter("Alle", emptyList()),
    JobFilter("Läuft", listOf("running")),
    JobFilter("Wartet", listOf("queued", "blocked")),
    JobFilter("Fertig", listOf("succeeded")),
    JobFilter("Fehler", listOf("failed", "timed_out")),
    JobFilter("Abgebrochen", listOf("cancelled")),
)

internal fun jobStateLabel(state: String): String = when (state) {
    "queued" -> "wartet"
    "running" -> "läuft"
    "blocked" -> "wartet auf Freigabe"
    "succeeded" -> "fertig"
    "failed" -> "fehlgeschlagen"
    "timed_out" -> "Zeit überschritten"
    "cancelled" -> "abgebrochen"
    else -> state
}

internal fun agentStateLabel(state: String): String = when (state) {
    "running" -> "läuft"
    "waiting" -> "wartet"
    "done" -> "fertig"
    "failed" -> "fehlgeschlagen"
    else -> state
}

internal fun jobEventLabel(event: String): String = when (event) {
    "submitted" -> "Angelegt"
    "claimed" -> "Übernommen"
    "heartbeat" -> "Lebenszeichen"
    "cancelled" -> "Abgebrochen"
    "succeeded" -> "Erfolgreich beendet"
    "failed" -> "Fehlgeschlagen"
    "blocked" -> "Blockiert"
    else -> event
}

internal fun formatDuration(ms: Long): String = formatElapsed(0L, ms)

internal fun formatStamp(ms: Long): String =
    SimpleDateFormat("dd.MM. HH:mm:ss", Locale.GERMANY).format(Date(ms))

private fun resultFailure(result: ManageRunResult): String? =
    if (result.ok && result.effect != ManageEffect.REFUSED) null
    else result.text.ifBlank { "Der Host hat die Anfrage abgelehnt." }

// ---------------------------------------------------------------------------
// Gemeinsames Geruest: Kopf, Lokal|Cloud-Umschalter
// ---------------------------------------------------------------------------

@Composable
private fun TargetTabs(target: OverviewTarget, onSelect: (OverviewTarget) -> Unit) {
    TabRow(selectedTabIndex = target.ordinal) {
        OverviewTarget.entries.forEach { t ->
            Tab(selected = target == t, onClick = { onSelect(t) }, text = { Text(t.label) })
        }
    }
}

private fun targetConnection(
    target: OverviewTarget,
    cloud: HarwConnection,
    local: HarwConnection,
): HarwConnection = if (target == OverviewTarget.Cloud) cloud else local

private fun targetState(target: OverviewTarget, cloudState: WorkspaceState, localState: WorkspaceState): WorkspaceState =
    if (target == OverviewTarget.Cloud) cloudState else localState

/** Warum die Ansicht nicht laden kann, sonst `null`. */
private fun offlineReason(target: OverviewTarget, state: WorkspaceState): String? = when {
    state.online -> null
    target == OverviewTarget.Cloud && state.profile == null ->
        "Kein Cloud-Profil gekoppelt. Unter Einstellungen > Verbindung einrichten."
    target == OverviewTarget.Cloud -> "Nicht mit dem Gateway verbunden."
    else -> "Der Geräte-Host ist nicht gestartet."
}

@Composable
private fun OfflineNotice(reason: String, target: OverviewTarget, state: WorkspaceState, local: HarwConnection) {
    StatusMessage(reason, Modifier.padding(12.dp))
    if (target == OverviewTarget.Local && state.phase != ConnectionPhase.CONNECTING) {
        TextButton(onClick = { local.connectLocal() }, modifier = Modifier.padding(horizontal = 8.dp)) {
            Text("Geräte-Host starten")
        }
    }
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun JobsOverviewScreen(
    cloudState: WorkspaceState,
    cloud: HarwConnection,
    localState: WorkspaceState,
    local: HarwConnection,
    initialTarget: OverviewTarget,
    onBack: () -> Unit,
    onOpenSession: (sessionId: String, local: Boolean) -> Unit,
) {
    var target by rememberSaveable { mutableStateOf(initialTarget) }
    var filterIndex by rememberSaveable { mutableIntStateOf(0) }
    var selected by rememberSaveable { mutableStateOf<String?>(null) }
    var refreshKey by remember { mutableIntStateOf(0) }
    val connection = targetConnection(target, cloud, local)
    val state = targetState(target, cloudState, localState)
    val filter = JOB_FILTERS[filterIndex.coerceIn(0, JOB_FILTERS.lastIndex)]
    var jobs by remember(target) { mutableStateOf<OverviewJobs?>(null) }
    var loadError by remember(target) { mutableStateOf<String?>(null) }
    var actionError by remember(target) { mutableStateOf<String?>(null) }
    val offline = offlineReason(target, state)

    LaunchedEffect(target, filterIndex, state.online, state.generation, refreshKey) {
        if (!state.online) return@LaunchedEffect
        while (true) {
            val result = connection.overviewCall { it.overviewJobs(filter.states, 200u) }
            result.onSuccess {
                jobs = it
                loadError = null
            }.onFailure { loadError = it.message ?: "Jobs konnten nicht geladen werden." }
            delay(REFRESH_MS)
        }
    }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Jobs", onBack = if (selected != null) ({ selected = null }) else onBack) {
            TextButton(onClick = { refreshKey++ }, enabled = state.online) { Text("Aktualisieren") }
        }
        TargetTabs(target) {
            target = it
            selected = null
        }
        val picked = selected
        when {
            offline != null -> OfflineNotice(offline, target, state, local)
            picked != null -> JobDetailView(
                connection = connection,
                workId = picked,
                isLocal = target == OverviewTarget.Local,
                onChanged = { refreshKey++ },
                onOpenSession = onOpenSession,
            )
            else -> {
                FlowRow(
                    Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 6.dp),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    JOB_FILTERS.forEachIndexed { index, item ->
                        FilterChip(
                            selected = index == filterIndex,
                            onClick = { filterIndex = index },
                            label = { Text(item.label) },
                        )
                    }
                }
                loadError?.let { StatusMessage(it, Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
                actionError?.let { StatusMessage(it, Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
                val data = jobs
                when {
                    data == null && loadError == null -> StatusMessage("Jobs werden geladen …", Modifier.padding(12.dp))
                    data == null -> Unit
                    data.jobs.isEmpty() -> StatusMessage(
                        if (filter.states.isEmpty()) "Keine Jobs im Profil." else "Keine Jobs in diesem Zustand.",
                        Modifier.padding(12.dp),
                    )
                    else -> {
                        val nowMs = System.currentTimeMillis()
                        LazyColumn(
                            contentPadding = PaddingValues(12.dp),
                            verticalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            items(data.jobs, key = { it.workId }) { job ->
                                JobCard(job, nowMs, onClick = { selected = job.workId })
                            }
                            if (data.truncated) {
                                item { StatusMessage("Weitere Jobs vorhanden; Filter eingrenzen.") }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun StateText(state: String, label: String) {
    val colors = MaterialTheme.colorScheme
    val color = when (state) {
        "failed", "timed_out" -> colors.error
        "running" -> colors.primary
        else -> colors.onSurfaceVariant
    }
    Text(label, style = MaterialTheme.typography.bodyMedium, color = color)
}

@Composable
private fun JobCard(job: OverviewJob, nowMs: Long, onClick: () -> Unit) {
    Surface(
        Modifier.fillMaxWidth().clickable(onClick = onClick),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.surfaceContainer,
    ) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(job.title, style = MaterialTheme.typography.titleSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Text(job.kind, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            StateText(job.state, jobStateLabel(job.state))
            job.summary?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
            val duration = job.durationMs?.let { " · Dauer ${formatDuration(it.toLong())}" }.orEmpty()
            Text(
                "Angelegt ${formatStamp(job.submittedMs.toLong())} (vor ${formatElapsed(job.submittedMs.toLong(), nowMs)})$duration",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun JobDetailView(
    connection: HarwConnection,
    workId: String,
    isLocal: Boolean,
    onChanged: () -> Unit,
    onOpenSession: (String, Boolean) -> Unit,
) {
    var detail by remember(workId) { mutableStateOf<OverviewJobDetail?>(null) }
    var error by remember(workId) { mutableStateOf<String?>(null) }
    var notice by remember(workId) { mutableStateOf<String?>(null) }
    var busy by remember(workId) { mutableStateOf(false) }
    var reload by remember(workId) { mutableIntStateOf(0) }
    var confirm by remember(workId) { mutableStateOf<String?>(null) }
    val scope = androidx.compose.runtime.rememberCoroutineScope()

    LaunchedEffect(workId, reload) {
        while (true) {
            connection.overviewCall { it.overviewJob(workId) }
                .onSuccess {
                    detail = it
                    error = null
                }
                .onFailure { error = it.message ?: "Job konnte nicht geladen werden." }
            delay(REFRESH_MS)
        }
    }

    fun act(retry: Boolean) {
        busy = true
        notice = null
        scope.launch {
            val msg = connection.overviewMsgId()
            val result = connection.overviewCall {
                if (retry) it.overviewJobRetry(workId, msg) else it.overviewJobCancel(workId, msg)
            }
            busy = false
            result.onSuccess { run ->
                val failure = resultFailure(run)
                if (failure == null) notice = run.text else error = failure
            }.onFailure { error = it.message }
            reload++
            onChanged()
        }
    }

    confirm?.let { which ->
        val retry = which == "retry"
        AlertDialog(
            onDismissRequest = { confirm = null },
            title = { Text(if (retry) "Job erneut starten?" else "Job abbrechen?") },
            text = {
                Text(
                    if (retry) "Der Job wird erneut zur Ausführung eingereiht (Retry-Regeln des Hosts gelten)."
                    else "Der Job wird beendet. Bereits getane Arbeit bleibt erhalten.",
                )
            },
            dismissButton = { TextButton(onClick = { confirm = null }) { Text("Zurück") } },
            confirmButton = {
                TextButton(onClick = {
                    confirm = null
                    act(retry)
                }) { Text(if (retry) "Erneut starten" else "Abbrechen") }
            },
        )
    }

    val d = detail
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        error?.let { StatusMessage(it, error = true) }
        notice?.let { StatusMessage(it) }
        if (d == null) {
            if (error == null) StatusMessage("Job wird geladen …")
            return@Column
        }
        val job = d.job
        Text(job.title, style = MaterialTheme.typography.titleMedium)
        Text(job.workId, style = MaterialTheme.typography.labelSmall, fontFamily = FontFamily.Monospace)
        StateText(job.state, "${jobStateLabel(job.state)} · ${job.kind}")
        val nowMs = System.currentTimeMillis()
        Text("Angelegt: ${formatStamp(job.submittedMs.toLong())}", style = MaterialTheme.typography.bodySmall)
        job.startedMs?.let { Text("Gestartet: ${formatStamp(it.toLong())}", style = MaterialTheme.typography.bodySmall) }
        job.finishedMs?.let { Text("Beendet: ${formatStamp(it.toLong())}", style = MaterialTheme.typography.bodySmall) }
        job.durationMs?.let { Text("Dauer: ${formatDuration(it.toLong())}", style = MaterialTheme.typography.bodySmall) }
        Text("Versuche: ${job.attempts}", style = MaterialTheme.typography.bodySmall)
        d.holder?.let { Text("Ausführend: $it", style = MaterialTheme.typography.bodySmall) }
        job.sessionId?.let { sid ->
            OutlinedButton(onClick = { onOpenSession(sid, isLocal) }) { Text("Session öffnen") }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (job.canCancel) {
                OutlinedButton(onClick = { confirm = "cancel" }, enabled = !busy) {
                    Text(if (busy) "Bitte warten …" else "Job abbrechen", color = MaterialTheme.colorScheme.error)
                }
            }
            if (job.canRetry) {
                OutlinedButton(onClick = { confirm = "retry" }, enabled = !busy) { Text("Erneut starten") }
            }
        }
        if (d.specSummary.isNotBlank()) {
            Text("Spezifikation", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
            SelectionContainer {
                Text(d.specSummary, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
            }
        }
        Text("Statusverlauf", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
        d.history.forEach { entry ->
            Column {
                Text(
                    "${formatStamp(entry.atMs.toLong())} · ${jobEventLabel(entry.event)}",
                    style = MaterialTheme.typography.bodySmall,
                )
                entry.detail?.let {
                    Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        d.outputTail?.let { tail ->
            Text("Ausgabe", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
            SelectionContainer {
                Text(tail, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
            }
        }
        if (job.state == "running") {
            Text(
                "Läuft seit ${formatElapsed(job.startedMs?.toLong() ?: job.submittedMs.toLong(), nowMs)}",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Agenten
// ---------------------------------------------------------------------------

@Composable
internal fun AgentsOverviewScreen(
    cloudState: WorkspaceState,
    cloud: HarwConnection,
    localState: WorkspaceState,
    local: HarwConnection,
    initialTarget: OverviewTarget,
    onBack: () -> Unit,
    onOpenSession: (sessionId: String, local: Boolean) -> Unit,
    onOpenUias: () -> Unit,
) {
    var target by rememberSaveable { mutableStateOf(initialTarget) }
    var refreshKey by remember { mutableIntStateOf(0) }
    val connection = targetConnection(target, cloud, local)
    val state = targetState(target, cloudState, localState)
    var data by remember(target) { mutableStateOf<OverviewAgents?>(null) }
    var loadError by remember(target) { mutableStateOf<String?>(null) }
    var failure by remember(target) { mutableStateOf<String?>(null) }
    var confirming by remember(target) { mutableStateOf<OverviewAgent?>(null) }
    var pending by remember(target) { mutableStateOf<Set<String>>(emptySet()) }
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val offline = offlineReason(target, state)

    LaunchedEffect(target, state.online, state.generation, refreshKey) {
        if (!state.online) return@LaunchedEffect
        while (true) {
            connection.overviewCall { it.overviewAgents() }
                .onSuccess {
                    data = it
                    loadError = null
                }
                .onFailure { loadError = it.message ?: "Agenten konnten nicht geladen werden." }
            nowMs = System.currentTimeMillis()
            delay(REFRESH_MS)
        }
    }

    confirming?.let { agent ->
        AlertDialog(
            onDismissRequest = { confirming = null },
            title = { Text("Agent stoppen?") },
            text = { Text("Der Agent \"${agent.role}\" und seine Unteragenten werden beendet. Bereits getane Arbeit bleibt erhalten.") },
            dismissButton = { TextButton(onClick = { confirming = null }) { Text("Abbrechen") } },
            confirmButton = {
                TextButton(onClick = {
                    confirming = null
                    pending = pending + agent.agentId
                    failure = null
                    scope.launch {
                        val msg = connection.overviewMsgId()
                        val result = connection.overviewCall {
                            it.overviewAgentStop(agent.sessionId, agent.agentId, msg)
                        }
                        pending = pending - agent.agentId
                        result.onSuccess { run -> failure = resultFailure(run) }
                            .onFailure { failure = it.message }
                        refreshKey++
                    }
                }) { Text("Stoppen") }
            },
        )
    }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Agenten", onBack = onBack) {
            TextButton(onClick = { refreshKey++ }, enabled = state.online) { Text("Aktualisieren") }
        }
        TargetTabs(target) { target = it }
        if (offline != null) {
            OfflineNotice(offline, target, state, local)
            return@Column
        }
        loadError?.let { StatusMessage(it, Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
        failure?.let { StatusMessage("Agent stoppen: $it", Modifier.padding(horizontal = 12.dp, vertical = 4.dp), error = true) }
        val agents = data
        if (agents == null) {
            if (loadError == null) StatusMessage("Agenten werden geladen …", Modifier.padding(12.dp))
            return@Column
        }
        LazyColumn(
            contentPadding = PaddingValues(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item { OverviewSection("Laufende Agenten · ${agents.running.count { it.state == "running" || it.state == "waiting" }}") }
            if (agents.running.isEmpty()) {
                item { StatusMessage("Zurzeit läuft kein Unteragent in einer Session.") }
            }
            items(agents.running, key = { it.sessionId + "/" + it.agentId }) { agent ->
                RunningAgentCard(
                    agent = agent,
                    nowMs = nowMs,
                    stopping = agent.agentId in pending,
                    onStop = if (agent.canStop && (agent.state == "running" || agent.state == "waiting")) {
                        { confirming = agent }
                    } else {
                        null
                    },
                    onOpenSession = { onOpenSession(agent.sessionId, target == OverviewTarget.Local) },
                )
            }
            item { OverviewSection("Agentenbibliothek · ${agents.library.size}") }
            item {
                if (target == OverviewTarget.Local) {
                    OutlinedButton(onClick = onOpenUias) { Text("UI-Agenten (UIA) verwalten und aktivieren") }
                } else {
                    StatusMessage("Aktivieren: am Gateway mit /agent use <Name>, gilt ab der nächsten Session.")
                }
            }
            if (agents.library.isEmpty()) {
                item { StatusMessage("Keine Agentendefinitionen gefunden.") }
            }
            items(agents.library, key = { it.source + "/" + it.name }) { def -> AgentDefCard(def) }
        }
    }
}

@Composable
private fun OverviewSection(text: String) {
    Text(text, style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
}

@Composable
private fun RunningAgentCard(
    agent: OverviewAgent,
    nowMs: Long,
    stopping: Boolean,
    onStop: (() -> Unit)?,
    onOpenSession: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(Modifier.fillMaxWidth(), shape = MaterialTheme.shapes.medium, color = colors.surfaceVariant) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(agent.role, style = MaterialTheme.typography.titleSmall, color = colors.onSurface)
            Text(agent.model ?: "Standardmodell", style = MaterialTheme.typography.bodySmall, color = colors.onSurfaceVariant)
            val status = agentStateLabel(agent.state) + (agent.currentTool?.let { " · $it" } ?: "")
            Text(
                status,
                style = MaterialTheme.typography.bodyMedium,
                color = when (agent.state) {
                    "failed" -> colors.error
                    "running" -> colors.primary
                    else -> colors.onSurfaceVariant
                },
            )
            Text(
                "${formatElapsed(agent.startedMs.toLong(), nowMs)} · ${agent.toolCalls} Werkzeugaufrufe",
                style = MaterialTheme.typography.bodySmall,
                color = colors.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(onClick = onOpenSession) { Text("Session öffnen") }
                if (onStop != null) {
                    TextButton(onClick = onStop, enabled = !stopping) {
                        Text(if (stopping) "Wird gestoppt …" else "Agent stoppen", color = colors.error)
                    }
                }
            }
        }
    }
}

@Composable
private fun AgentDefCard(def: OverviewAgentDef) {
    val colors = MaterialTheme.colorScheme
    Surface(Modifier.fillMaxWidth(), shape = RoundedCornerShape(12.dp), color = colors.surfaceContainer) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Text(
                def.name + if (def.active) " · aktiv" else "",
                style = MaterialTheme.typography.titleSmall,
                color = if (def.active) colors.primary else colors.onSurface,
            )
            val source = if (def.source == "custom") "eigene Definition" else "eingebaut"
            Text(
                "${def.role} · $source · ${if (def.readOnly) "lesend" else "schreibend"}",
                style = MaterialTheme.typography.labelSmall,
                color = colors.onSurfaceVariant,
            )
            def.model?.let { Text("Modell: $it", style = MaterialTheme.typography.bodySmall) }
            def.description?.let {
                Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 3, overflow = TextOverflow.Ellipsis)
            }
        }
    }
}
