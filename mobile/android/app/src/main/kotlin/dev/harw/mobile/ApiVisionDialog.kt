package dev.harw.mobile

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.net.Uri
import android.os.PersistableBundle
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ApiVisionChoice
import uniffi.harw_mobile_ffi.ApiVisionStatus as ApiVisionNativeStatus

internal const val ApiVisionParallelismPref = "api_vision_parallelism"

/** Standalone descriptions. Does not receive a chat ID, prompt, transcript or draft. */
@Composable
internal fun ApiVisionDialog(
    isCurrent: () -> Boolean,
    onClose: () -> Unit,
    onUseLocal: (() -> Unit)? = null,
    fallbackInvite: ConfiguredVisionFallbackInvite? = null,
) {
    // Keep the selection that opened this dialog, not a later rerender's
    // project, route generation, or profile.
    val ownerCurrent = remember { isCurrent }
    if (!ownerCurrent()) return
    val context = LocalContext.current
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    val scope = rememberCoroutineScope()
    val evidenceStore = remember(context) { ApiVisionEvidenceStore(context) }
    var routeSet by remember(context) { mutableStateOf<AppApiVisionRouteSet?>(null) }
    val choices = routeSet?.choices
    var choice by remember(context) { mutableStateOf<ApiVisionChoice?>(null) }
    var refs by remember { mutableStateOf<List<Uri>>(emptyList()) }
    var pickerNotice by remember { mutableStateOf<String?>(null) }
    var loadFailed by remember { mutableStateOf(false) }
    var checkingRoute by remember { mutableStateOf(false) }
    var routeNotice by remember { mutableStateOf<String?>(null) }
    var parallelism by remember { mutableIntStateOf(apiVisionConfiguredParallelism(prefs.all[ApiVisionParallelismPref])) }
    var started by remember { mutableStateOf(false) }
    var stopped by remember { mutableStateOf(false) }
    var finished by remember { mutableStateOf(false) }
    var usingFallback by remember(fallbackInvite) { mutableStateOf(fallbackInvite != null) }
    var fallbackOfferInvalid by remember { mutableStateOf(false) }
    var job by remember { mutableStateOf<Job?>(null) }
    var batch by remember { mutableStateOf<ApiVisionBatchProgress?>(null) }
    val results = remember { mutableStateMapOf<Int, ApiVisionStatus>() }
    val running = started && !finished && !stopped
    val fallback = if (started && finished && !stopped && !usingFallback && !fallbackOfferInvalid &&
        results.values.any {
            it == ApiVisionStatus.Unavailable || it == ApiVisionStatus.BillingBlocked ||
                it == ApiVisionStatus.ThrottleUnresolved
        }
    ) {
        routeSet?.let { readApiVisionFallback(prefs, it) }
            ?.takeUnless { sameVisionDestination(it, choice) }
    } else null

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(ApiVisionMaxRefs)) { picked ->
        if (ownerCurrent() && !started && !checkingRoute && picked.isNotEmpty()) {
            val combined = appendApiVisionRefs(refs, picked)
            if (combined == null) {
                pickerNotice = "Insgesamt sind höchstens $ApiVisionMaxRefs Bilder möglich. Auswahl unverändert."
            } else {
                refs = combined
                pickerNotice = null
            }
        }
    }

    LaunchedEffect(context, fallbackInvite) {
        try {
            val verified = appApiVisionRoutes(context)
            if (!ownerCurrent()) return@LaunchedEffect
            routeSet = verified
            if (fallbackInvite == null) {
                choice = readApiVisionDefault(prefs, verified)
            } else {
                val saved = readLocalVisionApiFallback(prefs, verified)
                if (verified.profileKey == fallbackInvite.profileKey && saved == fallbackInvite.choice) {
                    choice = saved
                    routeNotice = "Wähle die Bilder neu aus und bestätige Senden ausdrücklich. Es wurde noch nichts gesendet."
                } else {
                    choice = null
                    routeNotice = "Die gespeicherte Alternative oder ihr Profil hat sich geändert. Keine Bilder gesendet."
                }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            loadFailed = true
        }
    }

    fun stop() {
        stopped = true
        job?.cancel()
    }

    fun close() {
        if (running) stop() else job?.cancel()
        onClose()
    }

    fun openFallback(candidate: ApiVisionChoice) {
        val prior = routeSet ?: return
        if (!ownerCurrent() || checkingRoute || running || !finished || stopped || usingFallback || candidate != fallback) return
        checkingRoute = true
        routeNotice = null
        job = scope.launch {
            try {
                val fresh = appApiVisionRoutes(context)
                if (!ownerCurrent()) return@launch
                if (prior.profileKey != fresh.profileKey || candidate != readApiVisionFallback(prefs, fresh)) {
                    fallbackOfferInvalid = true
                    routeNotice = "Die gespeicherte Alternative oder ihr Profil ist nicht mehr aktuell. Keine Bilder gesendet."
                    return@launch
                }
                // Re-open only after the user clicks, without carrying any URI, bitmap, result,
                // network job, or proof of consent from the old batch into the new one.
                routeSet = fresh
                refs = emptyList()
                results.clear()
                batch = null
                pickerNotice = null
                started = false
                finished = false
                parallelism = apiVisionConfiguredParallelism(prefs.all[ApiVisionParallelismPref])
                choice = candidate
                usingFallback = true
                routeNotice = "Wähle Bilder für diese Alternative neu aus. Es wurde noch nichts gesendet."
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                routeNotice = "Die Alternative konnte nicht verifiziert werden. Keine Bilder gesendet."
            } finally {
                checkingRoute = false
            }
        }
    }

    AlertDialog(
        onDismissRequest = ::close,
        title = { Text("API-Bildanalyse") },
        text = {
            LazyColumn(
                Modifier.heightIn(max = 460.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                item {
                    Text("Je Bild geht eine separate Anfrage an den ausgewählten Anbieter. Der Chatverlauf und dein Entwurf werden nicht gesendet; die Beschreibungen werden nicht im Chat gespeichert.")
                }
                item {
                    Text("Der Anbieter verarbeitet die Bilder nach seinen Regeln. Anfragen können Kosten verursachen. Stoppen holt bereits übertragene Bilder nicht zurück. Nur vorübergehend gedrosselte Anfragen werden nach einer Wartezeit bis zu zweimal erneut versucht. Bei Abrechnungsfehlern oder unklarer Zustellung kein automatischer Neuversand. Kein automatischer Anbieterwechsel.")
                }
                if (loadFailed) {
                    item { Text("Die konfigurierten Vision-Routen konnten nicht gelesen werden.") }
                } else if (choices == null) {
                    item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                } else if (choices.isNullOrEmpty()) {
                    item { Text("Kein unterstützter API-Anbieter mit einem bildfähigen Modell ist konfiguriert.") }
                }
                if (onUseLocal != null && !running && !checkingRoute) {
                    item {
                        TextButton(onClick = { if (ownerCurrent()) onUseLocal() }) { Text("Stattdessen zur lokalen Analyse") }
                        Text(
                            "Getrennte Auswahl auf diesem Gerät. Keine Bilder oder Ergebnisse werden übernommen. Bereits gesendete Bilder kannst du damit nicht zurückholen.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                if (checkingRoute) item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                routeNotice?.let { notice -> item { Text(notice, color = MaterialTheme.colorScheme.error) } }
                if (fallback != null) {
                    item {
                        Text("Gespeicherte Alternative: " + fallback.routeId + " · " + fallback.modelId)
                        Text(fallback.endpoint, style = MaterialTheme.typography.bodySmall)
                        Text(
                            "Das öffnet eine leere Auswahl und ersetzt diese Ergebnisansicht. Kopiere erfolgreiche Beschreibungen bei Bedarf zuerst. Bereits übertragene Bilder lassen sich nicht zurückholen.",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        TextButton(enabled = !checkingRoute, onClick = { openFallback(fallback) }) {
                            Text("Neue Auswahl mit dieser Alternative")
                        }
                    }
                }
                if (!started) {
                    val available = if (usingFallback) choices.orEmpty().filter { it == choice } else choices.orEmpty()
                    if (available.isNotEmpty()) {
                        item {
                            Column(Modifier.selectableGroup()) {
                                Text(
                                    if (usingFallback) "Gewählte API-Alternative" else "Anbieter und Modell",
                                    style = MaterialTheme.typography.titleSmall,
                                )
                                for (option in available) {
                                    Row(
                                        Modifier.fillMaxWidth()
                                            .selectable(
                                                selected = choice == option,
                                                enabled = !checkingRoute && !usingFallback,
                                                onClick = { choice = option; routeNotice = null },
                                                role = Role.RadioButton,
                                            ).padding(vertical = 6.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                                    ) {
                                        RadioButton(selected = choice == option, onClick = null)
                                        Column {
                                            Text("${option.routeId} · ${option.modelId}")
                                            Text(option.endpoint, style = MaterialTheme.typography.bodySmall)
                                            Text(if (option.stream) "Streaming" else "Synchron", style = MaterialTheme.typography.bodySmall)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    item {
                        Text("Parallelität für die API: $parallelism")
                        val selected = choice
                        val profile = routeSet?.profileKey
                        val recommended = if (selected != null && profile != null) {
                            evidenceStore.recommendation(profile, selected)
                        } else null
                        if (recommended != null) {
                            Text(
                                "Beobachtung für diese Route und dieses Modell: $recommended parallele Aufträge in drei vollständigen Stapeln ohne gemeldete Drosselung. Keine Kapazitätsgarantie.",
                                style = MaterialTheme.typography.bodySmall,
                            )
                            if (recommended != parallelism) {
                                TextButton(enabled = !checkingRoute, onClick = { parallelism = recommended }) {
                                    Text("$recommended nur für diesen Stapel verwenden")
                                }
                            }
                        }
                        Slider(
                            value = parallelism.toFloat(),
                            enabled = !checkingRoute,
                            onValueChange = { parallelism = it.roundToInt() },
                            onValueChangeFinished = { prefs.edit().putInt(ApiVisionParallelismPref, parallelism).apply() },
                            valueRange = ApiVisionMinParallelism.toFloat()..ApiVisionMaxRefs.toFloat(),
                            steps = ApiVisionMaxRefs - ApiVisionMinParallelism - 1,
                        )
                        Text("Für diese Auswahl bis zu ${apiVisionWorkerCount(parallelism, refs.size)} Anfragen gleichzeitig. Eine bestätigte Drosselung senkt nur das Limit für noch nicht gestartete Anfragen.")
                        TextButton(
                            enabled = !checkingRoute && (!usingFallback || choice != null) && refs.size < ApiVisionMaxRefs,
                            onClick = {
                                picker.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                            },
                        ) { Text("Bilder auswählen (${refs.size}/$ApiVisionMaxRefs)") }
                        pickerNotice?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    }
                } else {
                    item {
                        choice?.let { Text("${it.routeId} · ${it.modelId} · ${it.endpoint}") }
                        val states = results.values.toList()
                        val completed = states.count { it is ApiVisionStatus.Caption }
                        val failed = states.count {
                            it == ApiVisionStatus.Unreadable || it == ApiVisionStatus.Unavailable ||
                                it == ApiVisionStatus.BillingBlocked || it == ApiVisionStatus.ThrottleUnresolved
                        }
                        val skipped = states.count { it == ApiVisionStatus.NotSent }
                        val settled = completed + failed + skipped
                        Text("$settled von ${refs.size} ausgewertet · $completed beschrieben · $failed fehlgeschlagen · $skipped nicht gesendet")
                        if (running) {
                            val preparing = states.count { it == ApiVisionStatus.Preparing }
                            val retryQueued = states.count { it is ApiVisionStatus.RetryQueued }
                            val queued = refs.size - states.size
                            Text("$queued in Warteschlange · $preparing in Vorbereitung · $retryQueued warten auf Wiederholung")
                        }
                        batch?.let { progress ->
                            val usage = if (running) " · ${progress.active} aktiv" else ""
                            Text("Parallelität für diesen Stapel: ${progress.configured} · wirksames Limit: ${progress.effective}$usage")
                            when (progress.pause) {
                                ApiVisionPause.Billing ->
                                    Text("Der Anbieter meldet ein Abrechnungs- oder Kontolimit. Noch nicht gestartete Anfragen wurden nicht gesendet.")
                                ApiVisionPause.RetryWaitExceeded ->
                                    Text("Die Wartezeit des Anbieters übersteigt das automatische Zeitbudget. Noch offene Anfragen werden nicht automatisch gesendet.")
                                null -> if (running && progress.coolingDown) {
                                    Text("Der Anbieter drosselt. Neue Anfragen warten; laufende dürfen fertig werden.")
                                } else Unit
                            }
                        }
                        if (running) {
                            LinearProgressIndicator(
                                progress = { settled.toFloat() / refs.size.coerceAtLeast(1) },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                        if (stopped) Text("Gestoppt. Ergebnisse bereits gesendeter, noch laufender Anfragen werden nicht übernommen.")
                    }
                }
                itemsIndexed(refs, key = { _, ref -> ref.toString() }) { index, ref ->
                    Column {
                        Text("Bild ${index + 1}", style = MaterialTheme.typography.titleSmall)
                        if (!started) {
                            TextButton(enabled = !checkingRoute, onClick = {
                                refs = refs.filterNot { it == ref }
                                pickerNotice = null
                            }) { Text("Bild ${index + 1} entfernen") }
                        } else {
                            when (val status = results[index]) {
                                is ApiVisionStatus.Caption -> {
                                    SelectionContainer { Text(status.text) }
                                    TextButton(onClick = { copyVisionCaption(context, index, status.text) }) {
                                        Text("Beschreibung für Bild ${index + 1} kopieren")
                                    }
                                }
                                ApiVisionStatus.Unreadable -> Text("Bild nicht lesbar; nicht gesendet.")
                                ApiVisionStatus.Unavailable -> Text("Keine Beschreibung erhalten; kein automatischer Neuversand.")
                                ApiVisionStatus.BillingBlocked -> Text("Abrechnung oder Kontolimit blockiert die Anfrage.")
                                ApiVisionStatus.NotSent -> Text("Nicht gesendet.")
                                ApiVisionStatus.ThrottleUnresolved -> Text("Gedrosselt; kein weiterer automatischer Versuch.")
                                is ApiVisionStatus.RetryQueued -> Text(
                                    if (stopped) "Gestoppt; kein weiterer Versuch." else "Gedrosselt; Versuch ${status.attempt} wartet.",
                                )
                                ApiVisionStatus.Preparing -> Text(if (stopped) "Gestoppt." else "Bild wird vorbereitet …")
                                ApiVisionStatus.Sending -> Text(if (stopped) "Nicht abgeschlossen; Übertragung möglich." else "Anfrage läuft …")
                                null -> Text(if (stopped) "Nicht gestartet." else "Wartet …")
                            }
                        }
                    }
                }
            }
        },
        confirmButton = {
            if (!started) {
                TextButton(enabled = !checkingRoute && choice != null && refs.isNotEmpty(), onClick = {
                    val approvedChoice = choice ?: return@TextButton
                    val previouslyShown = routeSet?.takeIf { approvedChoice in it.choices } ?: return@TextButton
                    val approvedRefs = refs.toList()
                    if (!ownerCurrent() || started || checkingRoute ||
                        approvedRefs.isEmpty() || approvedRefs.size > ApiVisionMaxRefs
                    ) return@TextButton
                    val approvedParallelism = parallelism
                    checkingRoute = true
                    routeNotice = null
                    job = scope.launch {
                        val approvedRouteSet = try {
                            appApiVisionRoutes(context)
                        } catch (cancelled: CancellationException) {
                            throw cancelled
                        } catch (_: Exception) {
                            routeNotice = "Die Route konnte nicht erneut verifiziert werden. Keine Bilder gesendet."
                            return@launch
                        } finally {
                            checkingRoute = false
                        }
                        if (!ownerCurrent()) return@launch
                        routeSet = approvedRouteSet
                        val configuredFallback = if (fallbackInvite != null) {
                            readLocalVisionApiFallback(prefs, approvedRouteSet)
                        } else {
                            readApiVisionFallback(prefs, approvedRouteSet)
                        }
                        if (previouslyShown.profileKey != approvedRouteSet.profileKey ||
                            approvedChoice !in approvedRouteSet.choices ||
                            (usingFallback && approvedChoice != configuredFallback)
                        ) {
                            choice = null
                            routeNotice = if (usingFallback) {
                                "Die Alternative ist nicht mehr aktuell. Schließe diesen Dialog und prüfe die API-Einstellungen. Keine Bilder gesendet."
                            } else {
                                "Profil oder Route hat sich geändert. Wähle Anbieter und Modell erneut. Keine Bilder gesendet."
                            }
                            return@launch
                        }
                        started = true
                        val admissionJob = checkNotNull(currentCoroutineContext()[Job])
                        suspend fun requireCurrentDispatch() {
                            currentCoroutineContext().ensureActive()
                            if (!ownerCurrent()) {
                                // Cancel the entire scheduler, not just this worker:
                                // a lone child cancellation would be treated as a request failure.
                                admissionJob.cancel()
                                throw CancellationException("API image analysis owner changed")
                            }
                        }
                        val evidence = ApiVisionBatchEvidence(approvedParallelism, approvedRefs.size)
                        try {
                            runAdaptiveApiVisionBatch(
                                refs = approvedRefs,
                                parallelism = approvedParallelism,
                                load = { index, uri ->
                                    requireCurrentDispatch()
                                    withContext(Dispatchers.IO) { prepareImage(context, uri, "Bild ${index + 1}")?.image }
                                },
                                describe = { image ->
                                    requireCurrentDispatch()
                                    val observation = approvedRouteSet.store.apiVisionObserve(approvedChoice, image)
                                    requireCurrentDispatch()
                                    when (observation.status) {
                                        ApiVisionNativeStatus.CAPTION ->
                                            observation.caption?.let(ApiVisionReply::Caption) ?: ApiVisionReply.Failed
                                        ApiVisionNativeStatus.THROTTLED -> ApiVisionReply.Throttled(observation.retryAfterMs)
                                        ApiVisionNativeStatus.BILLING_BLOCKED -> ApiVisionReply.BillingBlocked
                                        ApiVisionNativeStatus.FAILED -> ApiVisionReply.Failed
                                    }
                                },
                                update = { index, status -> if (!stopped && ownerCurrent()) results[index] = status },
                                progress = {
                                    if (!stopped && ownerCurrent()) {
                                        evidence.observe(it)
                                        batch = it
                                    }
                                },
                            )
                            requireCurrentDispatch()
                            if (!stopped) {
                                val captions = results.values.count { it is ApiVisionStatus.Caption }
                                evidenceStore.record(approvedRouteSet.profileKey, approvedChoice, evidence, captions)
                            }
                        } finally {
                            finished = true
                        }
                    }
                }) { Text("${refs.size} Bilder an API senden") }
            } else if (running) {
                TextButton(onClick = ::stop) { Text("Stoppen") }
            }
        },
        dismissButton = {
            TextButton(onClick = ::close) {
                Text(if (running) "Schließen und stoppen" else "Schließen")
            }
        },
    )
}

internal fun copyVisionCaption(context: Context, index: Int, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager ?: return
    val clip = ClipData.newPlainText("Beschreibung zu Bild ${index + 1}", text)
    clip.description.extras = PersistableBundle().apply { putBoolean("android.content.extra.IS_SENSITIVE", true) }
    clipboard.setPrimaryClip(clip)
}
