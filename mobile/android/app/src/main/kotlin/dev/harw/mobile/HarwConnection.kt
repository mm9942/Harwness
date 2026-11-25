package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.SystemClock
import android.util.Log
import java.io.File
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.ApprovalOutcome
import uniffi.harw_mobile_ffi.CloudCatalog
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedCommand
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedSnapshot
import uniffi.harw_mobile_ffi.MobileCloudAssistantSharedStatus
import uniffi.harw_mobile_ffi.MobileCloudAssistantSnapshot
import uniffi.harw_mobile_ffi.ChatgptProviderCredential
import uniffi.harw_mobile_ffi.ChatgptProviderStatus
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.ConnectionStatus
import uniffi.harw_mobile_ffi.ManageCommand
import uniffi.harw_mobile_ffi.ManageConfigEntry
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.ManageKnownProvider
import uniffi.harw_mobile_ffi.ManageRunResult
import uniffi.harw_mobile_ffi.ManageStepUp
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.MobileCore
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.MobileOwnerStatus
import uniffi.harw_mobile_ffi.MobileProject
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileSessionRow
import uniffi.harw_mobile_ffi.MobileShareInfo
import uniffi.harw_mobile_ffi.RoutePreference
import uniffi.harw_mobile_ffi.ScannedProfile
import uniffi.harw_mobile_ffi.ServerProfile
import uniffi.harw_mobile_ffi.SessionCursor
import uniffi.harw_mobile_ffi.SessionSnapshot
import uniffi.harw_mobile_ffi.SubmitOutcome
import uniffi.harw_mobile_ffi.TorBootstrap
import uniffi.harw_mobile_ffi.generateIdentity
import uniffi.harw_mobile_ffi.signAdminEnrollment
import uniffi.harw_mobile_ffi.parseServerProfile

internal enum class NetworkSignal { None, Nudge, Restart }

internal fun networkSignal(phase: ConnectionPhase, stopping: Boolean, forgetting: Boolean, hasProfile: Boolean): NetworkSignal {
    if (stopping || forgetting || !hasProfile) return NetworkSignal.None
    return when (phase) {
        ConnectionPhase.CONNECTING, ConnectionPhase.RECONNECTING -> NetworkSignal.Nudge
        ConnectionPhase.FAILED -> NetworkSignal.Restart
        ConnectionPhase.CONNECTED, ConnectionPhase.DISCONNECTED, ConnectionPhase.REVOKED -> NetworkSignal.None
    }
}

/** Ergebnis einer ausgefuehrten Zeile fuer die Ergebniskarte. */
internal data class ManageCard(
    val line: String,
    val result: ManageRunResult,
)

/** Verwalten (Welle 3). Nie ein Geheimnis in diesem Zustand. */
internal data class ManageState(
    val support: ManageSupport? = null,
    val commands: List<ManageCommand> = emptyList(),
    val commandsRev: String? = null,
    val commandsLoaded: Boolean = false,
    val commandsError: String? = null,
    /** Laufende Zeile oder Aktion; bei `secret.put` nie mit Wert. */
    val running: String? = null,
    val card: ManageCard? = null,
    val catalog: CloudCatalog? = null,
    val catalogSessionId: String? = null,
    val catalogError: String? = null,
    val known: List<ManageKnownProvider> = emptyList(),
    val knownError: String? = null,
    val config: List<ManageConfigEntry> = emptyList(),
    val configError: String? = null,
    /** Offene Herausforderung (genau eine). */
    val stepUp: ManageStepUp? = null,
    /** `SystemClock.elapsedRealtime()` bei Empfang. */
    val stepUpReceivedAt: Long = 0L,
    val stepUpError: String? = null,
)

internal const val STEPUP_OPEN_TEXT = "Erst den offenen Step-up abschließen."
internal const val STEPUP_EXPIRED_TEXT = "Bestätigung abgelaufen."
internal const val STEPUP_CONFIRM_RUNNING = "stepup.confirm"

/** Wendet das Ergebnis einer Zeile an: Herausforderung oeffnet den Dialog, alles andere wird Karte. */
internal fun applyManageResult(manage: ManageState, line: String, result: ManageRunResult, now: Long): ManageState {
    val challenge = result.stepup
    return if (result.effect == ManageEffect.STEPUP_REQUIRED && challenge != null) {
        manage.copy(
            running = null,
            card = null,
            commandsError = null,
            stepUp = challenge,
            stepUpReceivedAt = now,
            stepUpError = null,
        )
    } else {
        manage.copy(running = null, card = ManageCard(line, result), commandsError = null)
    }
}

internal data class WorkspaceState(
    val profile: ServerProfile? = null,
    val deviceNodeId: String? = null,
    val phase: ConnectionPhase = ConnectionPhase.DISCONNECTED,
    val detail: String? = null,
    val canCreate: Boolean = false,
    val stopping: Boolean = false,
    val setupPending: Boolean = true,
    val setupNotice: String? = null,
    val setupError: String? = null,
    val sessions: List<MobileSession> = emptyList(),
    val sessionsLoaded: Boolean = false,
    val sessionSummariesGeneration: Long? = null,
    val listError: String? = null,
    val creating: Boolean = false,
    val generation: Long? = null,
    val windows: Map<String, WindowState> = emptyMap(),
    val pendingActions: Set<SessionAction> = emptySet(),
    val sessionResetEpoch: Long = 0L,
    val hasSavedSetup: Boolean = false,
    val forgetRequired: Boolean = false,
    val forgetting: Boolean = false,
    val cleanupError: String? = null,
    val chatgpt: ChatgptAccountState = ChatgptAccountState(),
    val routePreference: RoutePreference = DefaultProfileRoutePreference,
    val activeRoute: ConnectionRoute? = null,
    val tor: TorBootstrap? = null,
    val attempt: UInt = 0u,
    val nextRetryAtMs: ULong? = null,
    val projectsSupported: Boolean = false,
    val projects: List<MobileProject> = emptyList(),
    val projectsLoaded: Boolean = false,
    val projectSummariesGeneration: Long? = null,
    val existingProjectAssignment: Boolean = false,
    val projectsError: String? = null,
    val projectsLegacy: Boolean = false,
    val projectsShare: MobileShareInfo? = null,
    /** Open project view; `null` while the project list is shown. */
    val projectView: ProjectSessionsState? = null,
    val manage: ManageState = ManageState(),
    /** True for the on-device connection (Cloud und Lokal laufen als zwei getrennte Verbindungen gleichzeitig). */
    val local: Boolean = false,
) {
    val online: Boolean get() = phase == ConnectionPhase.CONNECTED && !stopping
    val canSetUp: Boolean get() = !setupPending && !forgetRequired && cleanupError == null
    val canForget: Boolean get() = canSetUp && !hasConnection && !stopping && hasSavedSetup
    val hasConnection: Boolean
        get() = !stopping && phase in setOf(
            ConnectionPhase.CONNECTING, ConnectionPhase.CONNECTED, ConnectionPhase.RECONNECTING,
        )

    private fun clearSessions() = copy(
        canCreate = false,
        stopping = false,
        sessions = emptyList(),
        sessionsLoaded = false,
        sessionSummariesGeneration = null,
        listError = null,
        projectsSupported = false,
        projects = emptyList(),
        projectsLoaded = false,
        projectSummariesGeneration = null,
        existingProjectAssignment = false,
        projectsError = null,
        projectsLegacy = false,
        projectsShare = null,
        projectView = null,
        manage = ManageState(),
        creating = false,
        generation = null,
        activeRoute = null,
        windows = emptyMap(),
        pendingActions = emptySet(),
        chatgpt = ChatgptAccountState(),
        sessionResetEpoch = sessionResetEpoch + 1,
        attempt = 0u,
        nextRetryAtMs = null,
    )

    fun invalidated() = if (phase == ConnectionPhase.REVOKED) this else clearSessions().copy(
        phase = ConnectionPhase.REVOKED,
        detail = "The server stopped authorizing this connection. Check the server policy and connect again.",
        attempt = 0u,
        nextRetryAtMs = null,
    )

    fun forgetSetup(): WorkspaceState {
        val cleared = if (forgetRequired) this else clearSessions()
        return cleared.copy(
            profile = null,
            routePreference = DefaultProfileRoutePreference,
            deviceNodeId = null,
            phase = ConnectionPhase.DISCONNECTED,
            detail = "Removing saved phone setup.",
            setupNotice = null,
            setupError = null,
            forgetRequired = true,
            forgetting = true,
            cleanupError = null,
            attempt = 0u,
            nextRetryAtMs = null,
        )
    }

    fun connectionStatus(phase: ConnectionPhase, detail: String?, canCreate: Boolean, generation: Long?) =
        copy(phase = phase, detail = detail, canCreate = canCreate, generation = generation)

    fun routeStatus(route: ConnectionRoute?, tor: TorBootstrap?) = copy(activeRoute = route, tor = tor)

    fun cleanupCheckFailed() = copy(
        setupPending = false,
        cleanupError = "Android could not check for a pending phone setup reset.",
    )
}

internal data class WindowState(
    val snapshot: SessionSnapshot? = null,
    val generation: Long? = null,
    val error: String? = null,
    /** Last known lease holder (`holder` frames); `null` = free or unknown. */
    val holder: MobileHolder? = null,
    val answerReloadPending: Boolean = false,
    val preserveAnswerWhileReloading: Boolean = false,
)

/** Main-thread tickets for native attachment reads; reload invalidates only its own session. */
internal class AttachmentReadEpochs {
    private val reads = mutableMapOf<String, Any>()

    fun begin(sessionId: String): Any = Any().also { reads[sessionId] = it }

    fun invalidate(sessionId: String) {
        reads.remove(sessionId)
    }

    fun finish(sessionId: String, ticket: Any): Boolean {
        if (reads[sessionId] !== ticket) return false
        reads.remove(sessionId)
        return true
    }
}

internal const val ProjectPageSize = 50u

/** Appends `incoming` to `existing` (or replaces it) and drops duplicates by session id, keeping the newest copy. */
internal fun mergeSessionRows(
    existing: List<MobileSessionRow>,
    incoming: List<MobileSessionRow>,
    replace: Boolean,
): List<MobileSessionRow> {
    val base = if (replace) emptyList() else existing
    val seen = incoming.mapTo(HashSet()) { it.sessionId }
    return base.filterNot { it.sessionId in seen } + incoming
}

internal fun replaceSessionRow(rows: List<MobileSessionRow>, updated: MobileSessionRow): List<MobileSessionRow> =
    rows.map { if (it.sessionId == updated.sessionId) updated else it }

internal fun withoutSessionRow(rows: List<MobileSessionRow>, sessionId: String): List<MobileSessionRow> =
    rows.filterNot { it.sessionId == sessionId }

/** Result of a project mutation, applied to the open project view on the main thread. */
internal sealed interface ProjectChange {
    data object None : ProjectChange
    data class Replace(val row: MobileSessionRow) : ProjectChange
    data class Remove(val sessionId: String) : ProjectChange
}

internal fun ProjectSessionsState.applyChange(change: ProjectChange): ProjectSessionsState = when (change) {
    ProjectChange.None -> this
    is ProjectChange.Replace -> copy(rows = replaceSessionRow(rows, change.row), error = null)
    is ProjectChange.Remove -> copy(rows = withoutSessionRow(rows, change.sessionId), error = null)
}

/** Maps a submit receipt to the draft outcome; BUSY keeps the draft so the user can retry later. */
internal fun busyDraftMessage(detail: String?): String = detail?.takeUnless { it.isBlank() } ?: "Die Session ist gerade belegt."

/** The transient mobile echo has not been assigned a durable host item ID. */
internal fun isDurableRewriteItemId(id: String): Boolean =
    id.length in 1..128 && !id.startsWith("mobile-") && id.all { it in '!'..'~' }

internal fun SessionCursor.hasSameRewriteBoundary(other: SessionCursor): Boolean =
    generation == other.generation && durable == other.durable

internal fun SessionCursor.isAfterRewriteBoundary(other: SessionCursor): Boolean =
    generation > other.generation || generation == other.generation && durable > other.durable

internal data class DraftAttempt(
    val text: String,
    val head: SessionCursor,
    val id: String,
    val images: List<uniffi.harw_mobile_ffi.PromptImage> = emptyList(),
    /** Id der zu ersetzenden früheren Nachricht ("Neu generieren"): Verlauf wird davor abgeschnitten. */
    val rewindBefore: String? = null,
)

internal data class ChatgptTarget(
    val version: UInt,
    val label: String,
    val endpoint: String,
    val serverNodeId: String,
    val serverPublicKeyHex: String,
    val onion: String?,
    val generation: Long,
) {
    constructor(profile: ServerProfile, generation: Long) : this(
        profile.version, profile.label, profile.endpoint, profile.serverNodeId, profile.serverPublicKeyHex, profile.onion, generation,
    )

    val profile: ServerProfile get() = ServerProfile(version, label, endpoint, serverNodeId, serverPublicKeyHex, onion)
}
internal enum class ChatgptOperation { Checking, Browser, Validating, Linking, Unlinking }
internal data class ChatgptAccountState(
    val linked: Boolean? = null,
    val operation: ChatgptOperation? = null,
    val error: String? = null,
    val showPlanNotice: Boolean = false,
    val unavailable: String? = null,
)

internal fun ChatgptProviderStatus.toAccountState(): ChatgptAccountState =
    ChatgptAccountState(linked = linked, unavailable = chatgptUnavailableMessage(availability))

internal fun ChatgptAccountState.canStartSignIn(): Boolean = linked == false && unavailable == null

internal sealed interface DraftResult {
    data object Accepted : DraftResult
    /** Exact host-issued future canonical item identity; queue admission by itself is not a commit. */
    data class ExactAccepted(val replacementItemId: String) : DraftResult
    data class Rejected(val error: String, val ambiguous: Boolean = false) : DraftResult
}

/** Process owner. Public commands and state transitions run on Main; storage and UniFFI calls run on IO. */
/** Gemeinsame Sperre: Cloud- und Lokal-Verbindung lesen/erzeugen dieselbe Geraeteidentitaet. */
private val SharedStorageLock = Mutex()

private const val ArchiveControllerLimit = 16

/** Eine Verbindung zu genau einem Backend: Gateway (Cloud) oder On-Device-Host ([localBackend]). */
internal class HarwConnection(private val context: Context, private val localBackend: Boolean = false) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val commands = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val mutableState = MutableStateFlow(WorkspaceState())
    val state = mutableState.asStateFlow()

    private val store by lazy { DeviceStore(context) }
    private val storageLock = SharedStorageLock
    private val coreLock = Mutex()
    private val ownerStatusRead = Mutex()
    private var core: MobileCore? = null
    private var archiveCore: MobileCore? = null
    private val archiveControllers = LinkedHashMap<ArchiveScope, ArchiveForkController>()
    private var lifecycle = 0L
    private var connectJob: Job? = null
    private var presenceJob: Job? = null
    private var listLoop: Job? = null
    private var refreshJob: Job? = null
    private var projectsJob: Job? = null
    private var projectSessionsJob: Job? = null
    private var projectCursor: String? = null
    private var setupJob: Job? = null
    private var accountJob: Job? = null
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var networkJob: Job? = null
    private val gate = SessionActionGate()
    private val observers = mutableMapOf<String, Int>()
    private val opened = mutableSetOf<String>()
    private val detachJobs = mutableMapOf<String, Job>()
    private val detaching = mutableSetOf<String>()
    private val attachmentReadEpochs = AttachmentReadEpochs()
    private val events = CoreEvents()

    init {
        if (localBackend) {
            change { it.copy(local = true, setupPending = false) }
        } else {
            loadSavedSetup()
        }
    }

    private fun loadSavedSetup() {
        change { it.copy(setupPending = true) }
        scope.launch {
            val pending = try {
                withContext(Dispatchers.IO) { storageLock.withLock { store.hasPendingReset() } }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                change { it.cleanupCheckFailed() }
                return@launch
            }
            if (pending) {
                beginForgetSetup()
                change { it.copy(setupPending = false) }
                return@launch
            }
            try {
                val (hasSavedSetup, profile, preference) = withContext(Dispatchers.IO) {
                    storageLock.withLock {
                        val profile = store.readProfile()?.let(::parseServerProfile)
                        Triple(store.hasSavedSetup(), profile, loadRoutePreference(profile))
                    }
                }
                change {
                    it.copy(profile = profile, routePreference = preference, hasSavedSetup = hasSavedSetup, setupPending = false, cleanupError = null)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                change {
                    it.copy(
                        setupError = "Saved profile: ${error.displayMessage()}",
                        hasSavedSetup = true,
                        setupPending = false,
                        cleanupError = null,
                    )
                }
            }
        }
    }

    fun exportEnrollment(uri: Uri) = setup("Export enrollment") {
        val native = loadCore()
        withContext(Dispatchers.IO) {
            val document = native.exportEnrollmentJson()
            checkNotNull(context.contentResolver.openOutputStream(uri, "wt")) {
                "The selected document is unavailable."
            }.use { it.write(document.toByteArray(Charsets.UTF_8)) }
        }
        change { it.copy(setupNotice = "Device enrollment saved.") }
    }

    fun importProfile(uri: Uri) = importProfileDocument("Import profile", allowOwnerStatusHub = true) {
        checkNotNull(context.contentResolver.openInputStream(uri)) {
            "The selected document is unavailable."
        }.use(::readProfileDocument)
    }

    fun importScannedProfile(scanned: ScannedProfile) {
        importProfileDocument("Scan server profile") { scanned.documentJson }
    }

    fun showEnrollmentQr(onPreview: (Bitmap) -> Unit) = setup("Show enrollment QR") {
        val native = loadCore()
        val image = withContext(Dispatchers.IO) {
            val enrollment = native.enrollment()
            check(enrollment.version == 1U) { "The enrollment version is not supported by QR setup." }
            pairingQrImage(enrollmentQrText(enrollment.label, enrollment.nodeId, enrollment.publicKeyHex))
        }
        onPreview(image)
    }

    private fun importProfileDocument(
        operation: String,
        allowOwnerStatusHub: Boolean = false,
        read: () -> String,
    ): Boolean {
        val current = state.value
        if (current.hasConnection || current.stopping || !current.canSetUp) return false
        setup(operation) {
            applyProfileDocument(withContext(Dispatchers.IO) { read() }, "Profile imported.", allowOwnerStatusHub)
        }
        return true
    }

    private suspend fun applyProfileDocument(
        document: String,
        notice: String,
        allowOwnerStatusHub: Boolean = false,
    ) {
        val profile = withContext(Dispatchers.IO) { parseServerProfile(document) }
        // Neither the enrollment server's reply nor a pairing QR can
        // appoint a second trust anchor for this profile.
        require(allowOwnerStatusHub || profile.ownerStatusHub == null) {
            context.getString(R.string.chat_owner_status_import_required)
        }
        if (profile.routePreferenceKey() != state.value.profile?.routePreferenceKey()) stopRetainedCloudDial()
        val preference = withContext(Dispatchers.IO) {
            storageLock.withLock {
                store.saveProfile(document)
                loadRoutePreference(profile)
            }
        }
        val replaced = profile.windowKey() != state.value.profile?.windowKey()
        if (replaced) opened.clear()
        change {
            it.copy(
                profile = profile,
                routePreference = preference,
                activeRoute = null,
                tor = null,
                hasSavedSetup = true,
                setupNotice = notice,
                phase = ConnectionPhase.DISCONNECTED,
                detail = null,
                sessions = if (replaced) emptyList() else it.sessions,
                sessionsLoaded = !replaced && it.sessionsLoaded,
                windows = if (replaced) emptyMap() else it.windows,
                listError = null,
            )
        }
    }

    private val adminPrefs by lazy { context.getSharedPreferences(AdminPrefsName, Context.MODE_PRIVATE) }

    /** Zuletzt benutzte Admin-Adresse (Vorschlag im Dialog), falls vorhanden. */
    fun lastAdminUrl(): String? = adminPrefs.getString(AdminPrefBase, null)

    /**
     * Selbstbedienungs-Einrichtung: holt ein Nonce vom Admin-Dienst, sendet die öffentlichen
     * Anmeldefelder samt Besitznachweis (Signatur über das Nonce) und öffnet danach die
     * Freigabeseite im Browser. Der private Schlüssel verlässt die App nie.
     */
    fun startAdminEnrollment(input: String, openBrowser: (android.net.Uri) -> Unit) = setup("Admin-Einrichtung") {
        val base = normalizeAdminUrl(input)
            ?: error("Die Admin-Adresse muss mit https:// beginnen und darf keine Zugangsdaten enthalten.")
        val requestStartedAt = System.currentTimeMillis()
        val request = withContext(Dispatchers.IO) {
            val seed = storageLock.withLock {
                store.loadOrGenerateIdentity { generateIdentity(DeviceLabel).privateKey }
            }
            try {
                val nonce = adminJsonCall("$base/api/enroll/nonce", null).getString("nonce")
                val proof = signAdminEnrollment(seed, DeviceLabel, nonce)
                val body = org.json.JSONObject()
                    .put("nonce", nonce)
                    .put("node_id", proof.nodeId)
                    .put("public_key_hex", proof.publicKeyHex)
                    .put("label", proof.label)
                    .put("signature_hex", proof.signatureHex)
                    .toString()
                val reply = adminJsonCall("$base/api/enroll", body)
                val remaining = if (reply.has("expires_in")) reply.optLong("expires_in", -1L)
                    else AdminServerTtlSeconds // Legacy gateways may omit the receipt lifetime.
                check(remaining in 1..AdminServerTtlSeconds) {
                    "Die Admin-Anfrage ist abgelaufen oder der Admin-Dienst hat eine ungültige Frist geliefert."
                }
                AdminRequest(reply.getString("req"), reply.optString("fingerprint"), base, remaining)
            } finally {
                seed.fill(0)
            }
        }
        check(AdminRequestPattern.matches(request.id)) { "Der Admin-Dienst hat eine ungültige Anfrage-ID geliefert." }
        val sameRequest = adminPrefs.getString(AdminPrefReq, null) == request.id &&
            adminPrefs.getString(AdminPrefBase, null) == request.base
        val originalStartedAt = if (sameRequest) adminPrefs.getLong(AdminPrefAt, requestStartedAt)
            else requestStartedAt
        val deadline = adminCallbackDeadline(requestStartedAt, request.remainingSeconds)
        val originalDeadline = if (sameRequest) adminPrefs.getLong(
            AdminPrefDeadline, originalStartedAt + AdminRequestMaxAgeMs,
        ) else deadline
        adminPrefs.edit()
            .putString(AdminPrefReq, request.id)
            .putLong(AdminPrefAt, originalStartedAt)
            .putLong(AdminPrefDeadline, minOf(deadline, originalDeadline))
            .putString(AdminPrefBase, request.base)
            .apply()
        change {
            it.copy(setupNotice = "Anfrage gesendet. Prüfcode ${request.fingerprint}: melde dich im Browser an und gib dieses Telefon frei.")
        }
        withContext(Dispatchers.Main) { openBrowser(android.net.Uri.parse("${request.base}/enroll?req=${request.id}")) }
    }

    /**
     * Nimmt den Rückweg `harw://profile?req=<id>&data=<base64url>` an. Der Link ist unvertraute
     * Eingabe: er muss zur offenen Anfrage dieses Telefons und seiner ursprünglichen Frist passen;
     * das Profil läuft danach durch denselben Parser wie jeder Import.
     */
    fun installAdminProfile(req: String?, data: String?, onInstalled: () -> Unit) {
        val pending = adminPrefs.getString(AdminPrefReq, null)
        val started = adminPrefs.getLong(AdminPrefAt, 0L)
        val now = System.currentTimeMillis()
        val deadline = adminPrefs.getLong(AdminPrefDeadline, started + AdminRequestMaxAgeMs)
        val fresh = deadline >= now && now - started in 0..AdminRequestMaxAgeMs
        if (req == null || data == null || pending == null || req != pending || !fresh ||
            !AdminRequestPattern.matches(req) || data.length > 20_000
        ) {
            change { it.copy(setupError = "Der Einrichtungslink passt zu keiner offenen Anfrage dieses Telefons und wurde verworfen.") }
            return
        }
        val current = state.value
        if (current.hasConnection || current.stopping || !current.canSetUp) {
            change { it.copy(setupError = "Trenne zuerst die bestehende Verbindung, bevor du ein neues Profil einrichtest.") }
            return
        }
        setup("Admin-Profil") {
            val document = withContext(Dispatchers.IO) {
                String(android.util.Base64.decode(data, android.util.Base64.URL_SAFE or android.util.Base64.NO_PADDING or android.util.Base64.NO_WRAP), Charsets.UTF_8)
            }
            applyProfileDocument(document, "Profil vom Admin-Dienst eingerichtet.")
            adminPrefs.edit().remove(AdminPrefReq).remove(AdminPrefAt).remove(AdminPrefDeadline).apply()
            onInstalled()
        }
    }

    fun selectRoute(expectedProfile: ServerProfile, preference: RoutePreference) {
        val current = state.value
        if (localBackend || !current.canSetUp || current.hasConnection || current.stopping ||
            current.profile != expectedProfile || current.routePreference == preference ||
            !routeChoiceEnabled(preference, expectedProfile)
        ) return

        setup(context.getString(R.string.harw_route_settings_title)) {
            // Remove any retryable native intent carrying the previously consented
            // mode before recording the new one. A network callback may not revive it.
            stopRetainedCloudDial()
            withContext(Dispatchers.IO) {
                storageLock.withLock {
                    val saved = store.readProfile()?.let(::parseServerProfile)
                    check(saved?.routePreferenceKey() == expectedProfile.routePreferenceKey()) {
                        context.getString(R.string.harw_route_profile_changed)
                    }
                    val key = expectedProfile.routePreferenceKey()
                    val previous = routePrefs.all[key] as? String
                    if (!routePrefs.edit().putString(key, preference.name).commit()) {
                        // SharedPreferences may update its process cache before disk commit fails.
                        routePrefs.edit().putString(key, previous).commit()
                        error(context.getString(R.string.harw_route_save_failed))
                    }
                }
            }
            change {
                if (it.profile != expectedProfile) it
                else it.copy(
                    phase = ConnectionPhase.DISCONNECTED,
                    detail = null,
                    activeRoute = null,
                    tor = null,
                    routePreference = preference,
                )
            }
        }
    }

    private val routePrefs by lazy { context.getSharedPreferences(RoutePrefsName, Context.MODE_PRIVATE) }

    private fun loadRoutePreference(profile: ServerProfile?): RoutePreference = try {
        storedProfileRoute(profile?.let { routePrefs.getString(it.routePreferenceKey(), null) })
    } catch (_: RuntimeException) {
        DefaultProfileRoutePreference
    }

    private suspend fun stopRetainedCloudDial() {
        unwatchNetwork()
        ++lifecycle
        withContext(Dispatchers.IO) { core?.disconnect() }
        connectJob?.cancelAndJoin()
        networkJob?.cancelAndJoin()
        presenceJob?.cancelAndJoin()
        withContext(Dispatchers.IO) { core?.disconnect() }
    }

    fun reportCameraUnavailable() {
        change { it.copy(setupError = "Camera access is needed to scan. You can import the profile file instead.") }
    }

    fun reportServiceError(error: Exception) {
        change { it.copy(setupError = "Start connection: ${error.displayMessage()}") }
    }

    fun connect(force: Boolean = false): Boolean {
        val start = state.value
        val profile = start.profile ?: return false
        if (start.stopping || !start.canSetUp) return start.hasConnection
        if (start.hasConnection && !force) return true
        if (!routeChoiceEnabled(start.routePreference, profile)) {
            change { it.copy(setupError = context.getString(R.string.harw_route_unavailable)) }
            return false
        }
        val request = ++lifecycle
        change {
            it.copy(
                local = false,
                phase = ConnectionPhase.CONNECTING, detail = null, listError = null,
                setupError = null, setupNotice = null,
            )
        }
        connectJob = scope.launch {
            try {
                val native = loadCore()
                if (request != lifecycle) return@launch
                val routePoller = scope.launch {
                    while (isActive && request == lifecycle) {
                        delay(500)
                        val live = withContext(Dispatchers.IO) { native.status() }
                        if (request == lifecycle) change {
                            it.routeStatus(live.route, live.tor).copy(
                                attempt = live.attempt,
                                nextRetryAtMs = live.nextRetryAtMs,
                            )
                        }
                    }
                }
                val status = try {
                    withContext(Dispatchers.IO) {
                        native.setProfileRoutePreference(profile, start.routePreference)
                        native.connect(profile)
                        native.status()
                    }
                } finally {
                    routePoller.cancel()
                }
                if (request != lifecycle) return@launch
                applyStatus(status)
                if (request == lifecycle) startPolling(native, request)
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (request == lifecycle) {
                    closeGate()
                    if (error is MobileException.Revoked) {
                        invalidate()
                    } else {
                        change { it.connectionStatus(ConnectionPhase.FAILED, error.displayMessage(), false, null) }
                    }
                }
            }
        }
        watchNetwork()
        return true
    }

    fun connectLocal(force: Boolean = false): Boolean {
        val start = state.value
        if (start.stopping || !start.canSetUp) return start.hasConnection
        if (start.hasConnection && !force) return true
        val request = ++lifecycle
        change {
            it.copy(
                local = true,
                phase = ConnectionPhase.CONNECTING, detail = null, listError = null,
                setupError = null, setupNotice = null,
            )
        }
        connectJob = scope.launch {
            try {
                val native = loadCore()
                if (request != lifecycle) return@launch
                val status = withContext(Dispatchers.IO) {
                    DeviceTools.register(context, native)
                    native.connectLocal(context.filesDir.path, AndroidPlatformKeystore(context))
                    native.status()
                }
                if (request != lifecycle) return@launch
                applyStatus(status)
                if (request == lifecycle) startPolling(native, request)
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (request == lifecycle) {
                    closeGate()
                    change { it.connectionStatus(ConnectionPhase.FAILED, error.displayMessage(), false, null) }
                }
            }
        }
        return true
    }

    /** Creates an untitled chat. Only an explicit project selection is applied here. */
    fun startChat(
        projectKey: String?,
        model: String? = null,
        opened: (String, String?) -> Unit,
        notice: (String) -> Unit,
    ) {
        val generation = gate.generation ?: return
        val native = core ?: return
        val current = state.value
        if (!current.canCreate || current.creating) return
        change { it.copy(creating = true, listError = null) }
        commands.launch {
            try {
                val existing = projectKey?.takeIf { it.isNotEmpty() }
                var assigned: String? = null
                var note: String? = null
                val session = withContext(Dispatchers.IO) {
                    val supported = native.projectsSupported()
                    val created = if (existing != null && supported) {
                        assigned = existing
                        native.createInProject(existing, null)
                    } else {
                        native.createSession(null)
                    }
                    if (existing != null && !supported) note = NoProjectsSupportText
                    if (model != null) {
                        try {
                            native.setSessionModel(created.sessionId, model)
                        } catch (error: Exception) {
                            if (error is CancellationException) throw error
                            note = "Modell „$model“ konnte nicht gesetzt werden: ${error.displayMessage()}"
                        }
                    }
                    created
                }
                if (generation == gate.generation) {
                    change { it.copy(sessions = listOf(session) + it.sessions.filterNot { s -> s.sessionId == session.sessionId }) }
                    opened(session.sessionId, assigned)
                    note?.let(notice)
                    refreshSessions()
                    refreshProjectsQuietly()
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) notice("Neuer Chat: ${error.displayMessage()}")
            } finally {
                change { it.copy(creating = false) }
            }
        }
    }

    /**
     * Legt ein Projekt mit [name] an (Wire: `session.create_in` mit dem Namen als Projektschluessel).
     * [result] bekommt `null` bei Erfolg, sonst eine Erklaerung.
     */
    fun createProject(name: String, result: (String?) -> Unit, opened: (String, String) -> Unit) {
        val slug = slugify(name)
        if (slug.isBlank()) {
            result("Bitte einen Namen eingeben.")
            return
        }
        val generation = gate.generation
        val native = core
        val current = state.value
        if (generation == null || native == null || !current.canCreate) {
            result("Keine Verbindung oder keine Berechtigung zum Anlegen.")
            return
        }
        if (!current.projectsSupported) {
            result(NoProjectsSupportText)
            return
        }
        commands.launch {
            try {
                val session = withContext(Dispatchers.IO) { native.createInProject(slug, null) }
                if (generation == gate.generation) {
                    opened(session.sessionId, slug)
                    refreshProjectsQuietly()
                    result(null)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                result("Projekt „$slug“ konnte nicht angelegt werden: ${error.displayMessage()}. $ServerCreatesNoProjectsText")
            }
        }
    }

    fun disconnect() {
        unwatchNetwork()
        if (!state.value.hasConnection) return
        ++lifecycle
        closeGate()
        change { it.copy(stopping = true, canCreate = false) }
        scope.launch {
            val error = try {
                val native = core
                val revoked = withContext(Dispatchers.IO) {
                    native?.disconnect()
                    native?.status()?.phase == ConnectionPhase.REVOKED
                }
                if (revoked) {
                    invalidate()
                    return@launch
                }
                null
            } catch (error: Exception) {
                error.displayMessage()
            }
            connectJob?.join()
            presenceJob?.cancelAndJoin()
            listLoop?.cancelAndJoin()
            refreshJob?.cancelAndJoin()
            accountJob?.cancelAndJoin()
            commands.coroutineContext[Job]?.children?.toList()?.joinAll()
            // Connect may have reached native code immediately after the first disconnect.
            val native = core
            val (revoked, finalError) = withContext(Dispatchers.IO) {
                try {
                    native?.let { if (it.status().phase != ConnectionPhase.DISCONNECTED) it.disconnect() }
                    (native?.status()?.phase == ConnectionPhase.REVOKED) to null
                } catch (error: Exception) {
                    false to error.displayMessage()
                }
            }
            if (revoked) {
                invalidate()
                return@launch
            }
            change {
                it.copy(
                    phase = ConnectionPhase.DISCONNECTED,
                    detail = finalError ?: error,
                    stopping = false,
                    creating = false,
                )
            }
        }
    }

    fun forgetSetup() {
        if (state.value.canForget) beginForgetSetup()
    }

    fun retryCleanup() {
        val current = state.value
        if (current.cleanupError == null || current.forgetting || current.setupPending) return
        if (!current.forgetRequired) {
            loadSavedSetup()
            return
        }
        beginForgetSetup()
    }

    fun refreshChatgptAccount() {
        val current = state.value
        val profile = current.profile ?: return
        val generation = current.generation ?: return
        runChatgpt(ChatgptTarget(profile, generation), ChatgptOperation.Checking) { native, target ->
            val status = withContext(Dispatchers.IO) { native.chatgptProviderStatus(target.profile) }
            status.toAccountState()
        }
    }

    fun startChatgptSignIn(target: ChatgptTarget) {
        if (!state.value.chatgpt.canStartSignIn()) return
        runChatgpt(target, ChatgptOperation.Browser) { native, captured ->
            val key = captured.profile.chatgptGatewayScope()
            val registration = withContext(Dispatchers.IO) {
                storageLock.withLock {
                    ensureChatgptTarget(captured)
                    store.chatgptGatewayRegistration(key, captured.profile.windowKey())
                }
            }
            var persisted = registration
            val tokens = signInWithChatgpt(
                hostId = registration.hostId,
                returningClient = registration.clientId,
                expectedSubject = registration.subject,
                openBrowser = { url ->
                    ensureChatgptTarget(captured)
                    try {
                        context.startActivity(
                            Intent(Intent.ACTION_VIEW, Uri.parse(url))
                                .addCategory(Intent.CATEGORY_BROWSABLE)
                                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                        )
                    } catch (_: android.content.ActivityNotFoundException) {
                        throw SiwcFailure("No web browser is available to sign in to ChatGPT.")
                    }
                },
                onVerifying = {
                    ensureChatgptTarget(captured)
                    changeChatgpt { it.copy(operation = ChatgptOperation.Validating) }
                },
                beforeTokenExchange = { issuedClient ->
                    storageLock.withLock {
                        ensureChatgptTarget(captured)
                        val pending = persisted.beforeExchange(issuedClient)
                        store.saveChatgptGatewayRegistration(key, persisted, pending)
                        persisted = pending
                    }
                    ensureChatgptTarget(captured)
                },
            )
            ensureChatgptTarget(captured)
            withContext(Dispatchers.IO) {
                storageLock.withLock {
                    ensureChatgptTarget(captured)
                    val confirmed = persisted.afterVerification(tokens.clientId, tokens.identity.subject)
                    store.saveChatgptGatewayRegistration(key, persisted, confirmed)
                }
            }
            ensureChatgptTarget(captured)
            changeChatgpt { it.copy(operation = ChatgptOperation.Linking) }
            val status = withContext(Dispatchers.IO) {
                ensureChatgptTarget(captured)
                native.linkChatgptProvider(
                    captured.profile,
                    ChatgptProviderCredential(
                        tokens.clientId, tokens.accessToken, tokens.refreshToken, tokens.idToken,
                        tokens.scope, tokens.expiresAt, tokens.earliestRefreshAt,
                        tokens.identity.subject, tokens.identity.email,
                    ),
                )
            }
            if (!status.linked) throw SiwcFailure("The server has not accepted this ChatGPT account.")
            ChatgptAccountState(linked = true, showPlanNotice = registration.subject == null)
        }
    }

    fun unlinkChatgptAccount(target: ChatgptTarget) {
        if (state.value.chatgpt.linked != true) return
        runChatgpt(target, ChatgptOperation.Unlinking) { native, captured ->
            val key = captured.profile.chatgptGatewayScope()
            val registration = withContext(Dispatchers.IO) {
                storageLock.withLock {
                    ensureChatgptTarget(captured)
                    store.chatgptGatewayRegistration(key, captured.profile.windowKey())
                }
            }
            var warning: String? = null
            val status = withContext(Dispatchers.IO) {
                ensureChatgptTarget(captured)
                try {
                    native.unlinkChatgptProvider(captured.profile)
                } catch (error: MobileException.Session) {
                    if (error.detail != ChatgptUnlinkWarning) throw error
                    warning = ChatgptUnlinkWarning
                    native.chatgptProviderStatus(captured.profile)
                }
            }
            if (status.linked) throw SiwcFailure("The server still reports a linked ChatGPT account.")
            withContext(Dispatchers.IO) {
                storageLock.withLock {
                    ensureChatgptTarget(captured)
                    store.saveChatgptGatewayRegistration(key, registration, registration.afterUnlink())
                }
            }
            ChatgptAccountState(linked = false, error = warning)
        }
    }

    fun cancelChatgptBrowser() {
        if (state.value.chatgpt.operation !in setOf(ChatgptOperation.Browser, ChatgptOperation.Validating)) return
        accountJob?.cancel()
        changeChatgpt { it.copy(operation = null) }
    }

    fun dismissChatgptPlanNotice() = changeChatgpt { it.copy(showPlanNotice = false) }

    private fun runChatgpt(
        target: ChatgptTarget,
        operation: ChatgptOperation,
        perform: suspend (MobileCore, ChatgptTarget) -> ChatgptAccountState,
    ) {
        val native = core ?: return
        if (!isChatgptTarget(target) || accountJob?.isActive == true) return
        changeChatgpt { it.copy(operation = operation, error = null) }
        accountJob = scope.launch {
            try {
                val result = perform(native, target)
                if (isChatgptTarget(target)) changeChatgpt { result }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (isChatgptTarget(target)) {
                    val detail = chatgptFailureMessage(operation, error)
                    Log.w(LogTag, "ChatGPT ${operation.name} failed: ${error.javaClass.simpleName}")
                    changeChatgpt { it.copy(operation = null, error = detail) }
                }
            }
        }
    }

    private fun isChatgptTarget(target: ChatgptTarget): Boolean =
        state.value.online && gate.generation == target.generation &&
            state.value.profile?.windowKey() == target.profile.windowKey()

    private fun ensureChatgptTarget(target: ChatgptTarget) {
        if (!isChatgptTarget(target)) throw SiwcFailure("The authorization server selection changed. Start sign-in again.")
    }

    private fun changeChatgpt(transform: (ChatgptAccountState) -> ChatgptAccountState) {
        change { it.copy(chatgpt = transform(it.chatgpt)) }
    }

    fun retainSession(sessionId: String) {
        detachJobs.remove(sessionId)?.cancel()
        observers[sessionId] = (observers[sessionId] ?: 0) + 1
    }

    fun releaseSession(sessionId: String) {
        val count = observers[sessionId] ?: return
        if (count > 1) {
            observers[sessionId] = count - 1
            return
        }
        observers.remove(sessionId)
        detachJobs[sessionId] = scope.launch {
            // Configuration changes briefly dispose and rebuild the same Activity.
            delay(5_000)
            detachJobs.remove(sessionId)
            detachSession(sessionId)
            change { it.copy(windows = it.windows - sessionId) }
        }
    }

    /** Reattach the reader, not the turn. This never sends or interrupts a prompt. */
    fun reloadAnswer(sessionId: String) {
        val native = core ?: return
        gate.generation ?: return
        val request = lifecycle
        val previous = state.value.windows[sessionId] ?: return
        if (previous.snapshot == null || sessionId !in observers ||
            sessionId !in opened || !detaching.add(sessionId)
        ) return

        // The old read can return a snapshot but must not re-register this attachment.
        // A reload never races a pending first open for this session.
        attachmentReadEpochs.invalidate(sessionId)
        opened.remove(sessionId)
        change {
            it.copy(windows = it.windows + (sessionId to previous.copy(
                generation = null, error = null, answerReloadPending = true,
                preserveAnswerWhileReloading = true,
            )))
        }
        commands.launch {
            var failure: String? = null
            try {
                withContext(Dispatchers.IO) { native.detachSession(sessionId) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                failure = error.displayMessage()
            } finally {
                detaching.remove(sessionId)
                if (request == lifecycle && sessionId in observers) {
                    change { current ->
                        val window = current.windows[sessionId] ?: return@change current
                        current.copy(windows = current.windows + (sessionId to window.copy(
                            answerReloadPending = false, error = failure,
                        )))
                    }
                    // The regular reader now opens this same session and replays
                    // its authoritative transcript and current running turn.
                    events.statusChanged()
                }
            }
        }
    }

    private fun detachSession(sessionId: String) {
        if (!opened.remove(sessionId) || !detaching.add(sessionId)) return
        val native = core
        val generation = gate.generation
        commands.launch {
            try {
                withContext(Dispatchers.IO) { native?.detachSession(sessionId) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation != null && gate.generation == generation && sessionId in observers) {
                    windowError(sessionId, error.displayMessage())
                }
            } finally {
                detaching.remove(sessionId)
            }
        }
    }

    fun refreshSessions() {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (refreshJob?.isActive == true) return
        refreshJob = scope.launch {
            try {
                val sessions = withContext(Dispatchers.IO) { native.listSessions() }
                if (generation == gate.generation) {
                    change { it.copy(sessions = sessions, sessionsLoaded = true, sessionSummariesGeneration = generation, listError = null) }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    change { it.copy(listError = "Refresh sessions: ${error.displayMessage()}") }
                }
            }
        }
    }

    // ---- Verwalten (Welle 3, C7e) ----

    private fun newMsgId(): String = java.util.UUID.randomUUID().toString()

    private inline fun changeManage(crossinline transform: (ManageState) -> ManageState) {
        change { it.copy(manage = transform(it.manage)) }
    }

    fun refreshManageSupport() {
        val generation = gate.generation ?: return
        val native = core ?: return
        scope.launch {
            try {
                val support = withContext(Dispatchers.IO) { native.manageSupport() }
                if (generation == gate.generation) changeManage { it.copy(support = support) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
            }
        }
    }

    fun refreshCommands(sessionId: String?) {
        val generation = gate.generation ?: return
        val native = core ?: return
        scope.launch {
            try {
                val list = withContext(Dispatchers.IO) { native.commands(sessionId) }
                if (generation == gate.generation) {
                    changeManage {
                        it.copy(commands = list.commands, commandsRev = list.rev, commandsLoaded = true, commandsError = null)
                    }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(commandsLoaded = true, commandsError = "Befehle laden: ${error.displayMessage()}") }
                }
            }
        }
    }

    /** Fetch this session's catalog without reading or replacing another screen's shared cache. */
    suspend fun composerCommandCatalog(sessionId: String, expectedGeneration: Long): List<ManageCommand> {
        val native = core ?: return emptyList()
        fun current() = gate.generation == expectedGeneration && core === native &&
            state.value.online && state.value.windows[sessionId]?.generation == expectedGeneration
        if (!current()) return emptyList()
        val list = withContext(Dispatchers.IO) {
            if (!native.manageSupport().manage) emptyList() else native.commands(sessionId).commands
        }
        return if (current()) list else emptyList()
    }

    /** Fuehrt eine Zeile aus; `onDone(null)` bei Fehler oder wenn ein Step-up offen ist. */
    fun runCommand(
        sessionId: String?,
        line: String,
        publishCard: Boolean = true,
        onDone: (ManageRunResult?) -> Unit = {},
    ) {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (state.value.manage.stepUp != null) {
            changeManage { it.copy(commandsError = STEPUP_OPEN_TEXT) }
            onDone(null)
            return
        }
        changeManage { it.copy(running = line, commandsError = null) }
        scope.launch {
            try {
                val result = withContext(Dispatchers.IO) { native.command(sessionId, line, newMsgId()) }
                if (generation == gate.generation) {
                    changeManage {
                        if (publishCard || result.stepup != null) applyManageResult(it, line, result, SystemClock.elapsedRealtime())
                        else it.copy(running = null, commandsError = null)
                    }
                    onDone(result)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage {
                        it.copy(running = null, commandsError = if (publishCard) "$line: ${error.displayMessage()}" else null)
                    }
                    onDone(null)
                }
            }
        }
    }

    fun clearCommandCard() {
        changeManage { it.copy(card = null) }
    }

    /** Setzt das Modell einer Cloud-Session (`session.set_model`, `<provider>/<modell>`); gilt ab der naechsten Nachricht. */
    fun setSessionModel(sessionId: String, model: String, onDone: (String?) -> Unit = {}) {
        val generation = gate.generation ?: return onDone("Keine Verbindung")
        val native = core ?: return onDone("Keine Verbindung")
        scope.launch {
            try {
                withContext(Dispatchers.IO) { native.setSessionModel(sessionId, model) }
                if (generation == gate.generation) {
                    refreshCatalog(sessionId)
                    onDone(null)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) onDone("Modellwechsel: ${error.displayMessage()}")
            }
        }
    }

    fun refreshCatalog(sessionId: String?) {
        val generation = gate.generation ?: return
        val native = core ?: return
        scope.launch {
            try {
                val catalog = withContext(Dispatchers.IO) { native.catalog(sessionId) }
                if (generation == gate.generation) changeManage {
                    it.copy(catalog = catalog, catalogSessionId = sessionId, catalogError = null)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(catalogError = "Katalog laden: ${error.displayMessage()}") }
                }
            }
        }
    }

    fun refreshKnownProviders() {
        val generation = gate.generation ?: return
        val native = core ?: return
        scope.launch {
            try {
                val known = withContext(Dispatchers.IO) { native.knownProviders(newMsgId()) }
                if (generation == gate.generation) changeManage { it.copy(known = known, knownError = null) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(knownError = "Provider laden: ${error.displayMessage()}") }
                }
            }
        }
    }

    fun refreshConfig() {
        val generation = gate.generation ?: return
        val native = core ?: return
        scope.launch {
            try {
                val entries = withContext(Dispatchers.IO) { native.configGet(null) }
                if (generation == gate.generation) changeManage { it.copy(config = entries, configError = null) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(configError = "Einstellungen laden: ${error.displayMessage()}") }
                }
            }
        }
    }

    private fun manageWrite(label: String, call: (MobileCore) -> ManageRunResult) {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (state.value.manage.stepUp != null) {
            changeManage { it.copy(configError = STEPUP_OPEN_TEXT) }
            return
        }
        changeManage { it.copy(running = label, configError = null) }
        scope.launch {
            try {
                val result = withContext(Dispatchers.IO) { call(native) }
                if (generation == gate.generation) {
                    changeManage { applyManageResult(it, label, result, SystemClock.elapsedRealtime()) }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(running = null, configError = "$label: ${error.displayMessage()}") }
                }
            }
        }
    }

    fun setConfig(key: String, value: String) {
        val id = newMsgId()
        manageWrite("config.set") { it.configSet(key, value, id) }
    }

    fun resetConfig(key: String) {
        val id = newMsgId()
        manageWrite("config.reset") { it.configReset(key, id) }
    }

    /** Legt einen Provider an: optional `secretPut`, dann die Zeile. Der Wert wird nur durchgereicht. */
    fun addProvider(request: ProviderAddRequest, secret: String?, onDone: (String?) -> Unit) {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (state.value.manage.stepUp != null) {
            onDone(STEPUP_OPEN_TEXT)
            return
        }
        changeManage { it.copy(running = "provider.add", commandsError = null) }
        scope.launch {
            try {
                val id = newMsgId()
                val (line, result) = withContext(Dispatchers.IO) {
                    val handle = if (secret != null) {
                        native.secretPut(request.keyName, request.name, secret).handle
                    } else {
                        null
                    }
                    val built = providerAddLine(request, handle)
                        ?: throw IllegalArgumentException("Ungültige Provider-Angaben.")
                    built to native.command(null, built, id)
                }
                if (generation == gate.generation) {
                    changeManage { applyManageResult(it, line, result, SystemClock.elapsedRealtime()) }
                    onDone(null)
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(running = null) }
                    onDone(error.displayMessage())
                }
            }
        }
    }

    fun confirmStepUp(stepUp: ManageStepUp) {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (state.value.manage.running == STEPUP_CONFIRM_RUNNING) return
        changeManage { it.copy(running = STEPUP_CONFIRM_RUNNING, stepUpError = null) }
        scope.launch {
            try {
                val result = withContext(Dispatchers.IO) { native.stepupConfirm(stepUp) }
                if (generation == gate.generation) {
                    changeManage {
                        it.copy(stepUp = null, running = null, stepUpError = null, card = ManageCard(stepUp.action, result))
                    }
                    val action = stepUp.action
                    if (action.startsWith("models.") || action.startsWith("provider.")) refreshCatalog(null)
                    if (action.startsWith("config.") || action.startsWith("mode.")) refreshConfig()
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    changeManage { it.copy(stepUp = null, running = null, stepUpError = error.displayMessage()) }
                }
            }
        }
    }

    fun cancelStepUp(stepUp: ManageStepUp, reason: String? = null) {
        val generation = gate.generation
        val native = core
        val shown = when (reason) {
            null -> null
            "expired" -> STEPUP_EXPIRED_TEXT
            else -> reason
        }
        changeManage { it.copy(stepUp = null, stepUpError = shown) }
        if (generation == null || native == null) return
        scope.launch {
            try {
                withContext(Dispatchers.IO) { native.stepupCancel(stepUp.stepupId) }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (reason == null && generation == gate.generation) {
                    changeManage { it.copy(stepUpError = error.displayMessage()) }
                }
            }
        }
    }

    fun noteStepUpError(stepUp: ManageStepUp, text: String) {
        changeManage {
            if (it.stepUp?.stepupId == stepUp.stepupId) it.copy(stepUpError = text) else it
        }
    }

    fun clearStepUpError() {
        changeManage { it.copy(stepUpError = null) }
    }

    // ---- Projekte und Seiten (Paket A5) ----

    fun refreshProjects() {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (projectsJob?.isActive == true) return
        projectsJob = scope.launch {
            try {
                val (supported, page, canAssignExisting) = withContext(Dispatchers.IO) {
                    Triple(
                        native.projectsSupported(),
                        native.listProjects(null, null, null),
                        native.existingProjectAssignmentSupported(),
                    )
                }
                if (generation == gate.generation) {
                    change {
                        it.copy(
                            projectsSupported = supported,
                            projects = page.projects,
                            projectsLoaded = true,
                            projectSummariesGeneration = generation,
                            existingProjectAssignment = canAssignExisting,
                            projectsError = null,
                            projectsLegacy = page.legacy,
                            projectsShare = page.share,
                            projectView = it.projectView?.let { v ->
                                v.copy(
                                    project = page.projects.firstOrNull { p -> p.projectKey == v.projectKey } ?: v.project,
                                    projects = page.projects,
                                    share = page.share,
                                )
                            },
                        )
                    }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    change { it.copy(projectsError = "Projekte laden: ${error.displayMessage()}") }
                }
            }
        }
    }

    /** Opens the session list of one project (`""` = "Ohne Projekt"). */
    fun openProject(projectKey: String) {
        if (state.value.projectView?.projectKey == projectKey) {
            refreshProjectSessions()
            return
        }
        projectSessionsJob?.cancel()
        projectCursor = null
        val project = state.value.projects.firstOrNull { it.projectKey == projectKey }
        change {
            it.copy(
                projectView = ProjectSessionsState(
                    project = project,
                    legacy = it.projectsLegacy,
                    share = it.projectsShare,
                    projects = it.projects,
                    loading = true,
                    projectKey = projectKey,
                ),
            )
        }
        loadProjectSessions(replace = true)
    }

    fun closeProject() {
        projectSessionsJob?.cancel()
        projectCursor = null
        change { it.copy(projectView = null) }
        refreshProjects()
    }

    fun setProjectQuery(query: String) {
        if (state.value.projectView == null) return
        projectCursor = null
        change { it.copy(projectView = it.projectView?.copy(query = query, loading = true)) }
        loadProjectSessions(replace = true)
    }

    fun refreshProjectSessions() {
        if (state.value.projectView == null) return
        projectCursor = null
        loadProjectSessions(replace = true)
    }

    fun loadMoreProjectSessions() {
        val view = state.value.projectView ?: return
        if (view.loading || !view.hasMore || projectCursor == null) return
        change { it.copy(projectView = it.projectView?.copy(loading = true)) }
        loadProjectSessions(replace = false)
    }

    private fun loadProjectSessions(replace: Boolean) {
        val generation = gate.generation ?: return
        val native = core ?: return
        val view = state.value.projectView ?: return
        val key = view.projectKey
        val query = view.query.takeUnless { it.isBlank() }
        val cursor = if (replace) null else projectCursor
        projectSessionsJob?.cancel()
        projectSessionsJob = scope.launch {
            try {
                val page = withContext(Dispatchers.IO) {
                    native.listSessionsPage(key, query, false, cursor, ProjectPageSize).filterDreamVisibility()
                }
                if (generation == gate.generation && state.value.projectView != null) {
                    projectCursor = page.nextCursor
                    change {
                        it.copy(
                            projectView = it.projectView?.copy(
                                rows = mergeSessionRows(it.projectView.rows, page.sessions, replace),
                                hasMore = page.nextCursor != null,
                                loading = false,
                                error = null,
                                legacy = page.legacy,
                            ),
                        )
                    }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    change {
                        it.copy(projectView = it.projectView?.copy(loading = false, error = error.displayMessage()))
                    }
                }
            }
        }
    }

    /** Creates a session in the open project and selects it. */
    fun createInProject(select: (String) -> Unit) {
        val key = state.value.projectView?.projectKey ?: return
        createInProject(key, select)
    }

    /**
     * Creates a session in [projectKey] (`""` = "Ohne Projekt") and selects it.
     * Falls back to a plain session when the gateway has no project support.
     */
    fun createInProject(projectKey: String, select: (String) -> Unit) {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (!state.value.canCreate || state.value.creating) return
        if (!state.value.projectsSupported) {
            createSession(null, select)
            return
        }
        change { it.copy(creating = true) }
        commands.launch {
            try {
                val session = withContext(Dispatchers.IO) { native.createInProject(projectKey, null) }
                if (generation == gate.generation) {
                    select(session.sessionId)
                    if (state.value.projectView?.projectKey == projectKey) refreshProjectSessions()
                    refreshProjectsQuietly()
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    val message = "Neue Session: ${error.displayMessage()}"
                    if (state.value.projectView?.projectKey == projectKey) {
                        projectViewError(message)
                    } else {
                        change { it.copy(projectsError = message) }
                    }
                }
            } finally {
                change { it.copy(creating = false) }
            }
        }
    }

    /** Takes over a profile session so it can be continued from the phone. */
    fun adoptSession(sessionId: String, done: (Boolean) -> Unit = {}) {
        projectMutation("Session übernehmen", done) { native ->
            native.adoptSession(sessionId)
            ProjectChange.None
        }
    }

    fun renameSession(sessionId: String, title: String) {
        projectMutation("Umbenennen") { native ->
            ProjectChange.Replace(native.renameSession(sessionId, title))
        }
    }

    fun archiveSession(sessionId: String) {
        projectMutation("Archivieren") { native ->
            native.archiveSession(sessionId, true)
            ProjectChange.Remove(sessionId)
        }
    }

    fun assignProject(sessionId: String, projectKey: String) {
        projectMutation("Projekt zuweisen") { native ->
            native.setProject(sessionId, projectKey)
            ProjectChange.Remove(sessionId)
        }
    }

    private fun cloudAssistantSelected(profile: ServerProfile, generation: Long): Boolean {
        val current = state.value
        return !localBackend && current.online && !current.stopping &&
            current.profile == profile && current.generation == generation && gate.generation == generation
    }

    /** Snapshot one explicit status read without changing the ordinary route or profile identity. */
    fun ownerStatusTarget(sessionId: String): OwnerStatusTarget? {
        val before = state.value
        val profile = before.profile ?: return null
        val hub = profile.ownerStatusHub ?: return null
        val generation = before.generation ?: return null
        val device = before.deviceNodeId ?: return null
        val target = OwnerStatusTarget(
            profile.copy(ownerStatusHub = hub.copy()), generation, before.sessionResetEpoch, device, sessionId,
        )
        return target.takeIf(::ownerStatusTargetCurrent)
    }

    fun ownerStatusTargetCurrent(target: OwnerStatusTarget): Boolean {
        val now = state.value
        val chat = now.windows[target.sessionId]
        return !localBackend && !now.local && now.online &&
            now.activeRoute == ConnectionRoute.ONION && target.profile.onion != null &&
            target.profile.ownerStatusHub != null && now.profile == target.profile &&
            now.deviceNodeId == target.deviceNodeId && now.generation == target.generation &&
            gate.generation == target.generation && now.sessionResetEpoch == target.resetEpoch &&
            chat?.generation == target.generation && chat?.snapshot?.online == true
    }

    /** An explicit specialist read, with no local, direct IP or V1 fallback. */
    suspend fun fencedOwnerStatus(target: OwnerStatusTarget): MobileOwnerStatus? {
        val native = core ?: return null
        if (!ownerStatusTargetCurrent(target) || !ownerStatusRead.tryLock()) return null
        return try {
            val observed = withContext(Dispatchers.IO) {
                native.fencedOwnerStatus(target.profile, target.sessionId)
            }
            observed.takeIf { core === native && ownerStatusTargetCurrent(target) }
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            // Do not surface transport details, profile pins or session IDs.
            null
        } finally {
            ownerStatusRead.unlock()
        }
    }

    /** Pure local native hello check; opening Home does not call a remote Assistant method. */
    suspend fun cloudAssistantTarget(profile: ServerProfile, generation: Long): CloudAssistantTarget? {
        val native = core ?: return null
        if (!cloudAssistantSelected(profile, generation)) return null
        val offered = withContext(Dispatchers.IO) {
            val access = native.cloudAssistantAccess(profile) ?: return@withContext null
            val shared = native.cloudAssistantSharedSupported(access)
            CloudAssistantTarget(
                profile, generation, access,
                recallSupported = native.cloudAssistantRecallSupported(access),
                sharedSupported = shared,
                sharedDeviceFingerprint = if (shared) native.cloudAssistantSharedDeviceFingerprint(access) else null,
            )
        } ?: return null
        return if (core === native && cloudAssistantSelected(profile, generation)) {
            offered
        } else {
            offered.access.destroy()
            null
        }
    }

    suspend fun cloudAssistantTargetCurrent(target: CloudAssistantTarget): Boolean {
        val native = core ?: return false
        if (!cloudAssistantSelected(target.profile, target.generation)) return false
        val current = withContext(Dispatchers.IO) { native.cloudAssistantAccessCurrent(target.access) }
        return current && core === native && cloudAssistantSelected(target.profile, target.generation)
    }

    /** Explicit specialist RPC only. Never falls through to the normal session composer. */
    suspend fun cloudAssistantCall(
        target: CloudAssistantTarget,
        operation: CloudAssistantCommand,
    ): MobileCloudAssistantSnapshot {
        val native = core ?: throw MobileException.Disconnected()
        if (!cloudAssistantSelected(target.profile, target.generation)) throw MobileException.Disconnected()
        val result = withContext(Dispatchers.IO) {
            val access = target.access
            val snapshot = when (operation) {
                CloudAssistantCommand.Status -> native.cloudAssistantStatus(access)
                CloudAssistantCommand.Provision -> native.cloudAssistantProvision(access)
                CloudAssistantCommand.Revoke -> native.cloudAssistantRevoke(access)
                is CloudAssistantCommand.Submit -> {
                    if (operation.recallMessageIds.isEmpty()) {
                        native.cloudAssistantSubmit(access, operation.id, operation.text)
                    } else {
                        if (!target.recallSupported) throw MobileException.Denied("Cloud Assistant recall was not negotiated")
                        native.cloudAssistantSubmitWithRecall(access, operation.id, operation.text, operation.recallMessageIds)
                    }
                }
            }
            if (!native.cloudAssistantAccessCurrent(access)) throw MobileException.Disconnected()
            snapshot
        }
        if (core !== native || !cloudAssistantSelected(target.profile, target.generation)) throw MobileException.Disconnected()
        return result
    }

    /** The opted-in shared room has no per-device, local, or ordinary session fallback. */
    suspend fun cloudSharedAssistantCall(
        target: CloudAssistantTarget,
        action: CloudSharedAssistantAction,
    ): MobileCloudAssistantSharedSnapshot {
        val native = core ?: throw MobileException.Disconnected()
        if (!target.sharedSupported) throw MobileException.Denied("Shared Assistant was not negotiated")
        return withContext(Dispatchers.IO) {
            val access = target.access
            fun requireCurrent() {
                if (core !== native || !cloudAssistantSelected(target.profile, target.generation) ||
                    !native.cloudAssistantAccessCurrent(access)
                ) throw MobileException.Disconnected()
                if (!native.cloudAssistantSharedSupported(access)) {
                    throw MobileException.Denied("Shared Assistant is no longer available")
                }
            }
            fun status(): MobileCloudAssistantSharedSnapshot {
                requireCurrent()
                return native.cloudAssistantShared(access, MobileCloudAssistantSharedCommand.Status).also { requireCurrent() }
            }
            fun requireStatus(condition: Boolean) {
                if (!condition) throw MobileException.Denied("Shared Assistant status changed")
            }

            // Revalidate each consented authority change against the current link and
            // room. Approval requires an independently entered complete fingerprint.
            val command = when (action) {
                CloudSharedAssistantAction.Status -> return@withContext status()
                CloudSharedAssistantAction.Provision -> {
                    requireStatus(status().status == MobileCloudAssistantSharedStatus.UNPROVISIONED)
                    MobileCloudAssistantSharedCommand.Provision
                }
                CloudSharedAssistantAction.RequestJoin -> {
                    requireStatus(status().status == MobileCloudAssistantSharedStatus.NOT_GRANTED)
                    MobileCloudAssistantSharedCommand.RequestJoin
                }
                CloudSharedAssistantAction.Revoke -> {
                    requireStatus(status().status == MobileCloudAssistantSharedStatus.OWNER)
                    MobileCloudAssistantSharedCommand.RevokeRoom
                }
                is CloudSharedAssistantAction.Approve -> {
                    val device = action.device
                    val current = status()
                    requireStatus(
                        current.status == MobileCloudAssistantSharedStatus.OWNER &&
                            validCloudSharedRequestId(device.requestId) && device.pending &&
                            canonicalCloudSharedFingerprint(action.fingerprint) == device.keyFingerprintHex &&
                            current.devices.any { it == device },
                    )
                    MobileCloudAssistantSharedCommand.Approve(device.requestId)
                }
                is CloudSharedAssistantAction.Remove -> {
                    val current = status()
                    requireStatus(
                        current.status == MobileCloudAssistantSharedStatus.OWNER &&
                            validCloudSharedRequestId(action.device.requestId) &&
                            current.devices.any { it == action.device },
                    )
                    MobileCloudAssistantSharedCommand.WithdrawOrRemove(action.device.requestId)
                }
                is CloudSharedAssistantAction.Leave -> {
                    val current = status()
                    requireStatus(
                        current.status == action.expectedStatus &&
                            (current.status == MobileCloudAssistantSharedStatus.PENDING ||
                                current.status == MobileCloudAssistantSharedStatus.PARTICIPANT) &&
                            validCloudSharedRequestId(action.requestId) && current.ownRequestId == action.requestId,
                    )
                    MobileCloudAssistantSharedCommand.WithdrawOrRemove(action.requestId)
                }
                is CloudSharedAssistantAction.Send ->
                    MobileCloudAssistantSharedCommand.Submit(action.id, action.text)
            }
            requireCurrent()
            native.cloudAssistantShared(access, command).also { requireCurrent() }
        }.also {
            if (core !== native || !cloudAssistantSelected(target.profile, target.generation)) throw MobileException.Disconnected()
        }
    }

    suspend fun changeCloudAssistantSession(
        target: CloudAssistantTarget,
        change: AssistantSessionChange,
    ) {
        val native = core ?: throw MobileException.Disconnected()
        val before = state.value
        if (!cloudAssistantSelected(target.profile, target.generation) ||
            !canManageCloudAssistantSessions(before, target) ||
            before.sessions.asSequence().take(CloudAssistantSummaryLimit).none { it.sessionId == change.id }
        ) throw MobileException.Disconnected()
        if (change is AssistantSessionChange.AssignProject &&
            change.destination !in assistantProjectChoices(before, target.generation)
        ) throw MobileException.Denied("Project selection changed")
        withContext(Dispatchers.IO) {
            when (change) {
                is AssistantSessionChange.Rename -> native.cloudAssistantRenameSession(target.access, change.id, change.title)
                is AssistantSessionChange.Archive -> native.cloudAssistantArchiveSession(target.access, change.id)
                is AssistantSessionChange.AssignProject ->
                    native.cloudAssistantAssignExistingProject(target.access, change.id, change.destination.key)
            }
            if (!native.cloudAssistantAccessCurrent(target.access)) throw MobileException.Disconnected()
        }
        if (core !== native || !cloudAssistantSelected(target.profile, target.generation)) throw MobileException.Disconnected()
        refreshSessions()
        if (change is AssistantSessionChange.Archive || change is AssistantSessionChange.AssignProject) refreshProjectsQuietly()
    }

    /** Read from the profile that is still selected after the asynchronous native call. */
    suspend fun sessionHomePage(
        expectedScope: SessionHomeScope,
        expectedGeneration: Long,
        cursor: String?,
        includeDreams: Boolean = false,
    ): uniffi.harw_mobile_ffi.MobileSessionPage? {
        val native = core ?: return null
        if (!canUseSessionHome(expectedScope, expectedGeneration)) return null
        val page = withContext(Dispatchers.IO) {
            native.listSessionsPage(null, null, false, cursor, SessionHomePageSize.toUInt(), includeDreams)
                .filterDreamVisibility(includeDreams)
        }
        return page.takeIf {
            core === native && canUseSessionHome(expectedScope, expectedGeneration)
        }
    }

    suspend fun systemSessionsPage(
        expectedScope: SessionHomeScope,
        expectedGeneration: Long,
        cursor: String?,
    ): SystemSessionPage? {
        val native = core ?: return null
        if (!canUseSessionHome(expectedScope, expectedGeneration)) return null
        val result = withContext(Dispatchers.IO) {
            val complete = native.systemSessionRowsSupported()
            val legacyDream = !complete && native.dreamSessionRowsSupported()
            val page = native.listSessionsPage(
                null, null, false, cursor, SessionHomePageSize.toUInt(), legacyDream, complete,
            ).let { data ->
                if (complete || legacyDream) {
                    data.copy(sessions = data.sessions.filter { it.purpose.isSystemRun() })
                } else {
                    // A pre-catalog host can only supply its already authorized regular page.
                    // Accept the exact legacy producer fingerprint, never the title alone.
                    data.projectLegacyDreamsForInternal()
                }
            }
            SystemSessionPage(page, complete, legacyDiscovery = !complete && !legacyDream)
        }
        return result.takeIf { core === native && canUseSessionHome(expectedScope, expectedGeneration) }
    }

    fun canUseSessionHome(scope: SessionHomeScope, generation: Long): Boolean {
        val current = state.value
        return current.online && current.generation == generation &&
            gate.generation == generation && sessionHomeScope(current) == scope
    }

    /** Sessions ohne Projekt (für Vorschläge zusammengehöriger Chats). */
    suspend fun unassignedRows(): List<uniffi.harw_mobile_ffi.MobileSessionRow> {
        val native = core ?: return emptyList()
        if (!state.value.projectsSupported) return emptyList()
        return try {
            withContext(Dispatchers.IO) { native.listSessionsPage("", null, false, null, ProjectPageSize).filterDreamVisibility().sessions }
        } catch (error: Exception) {
            if (error is CancellationException) throw error
            emptyList()
        }
    }

    /** Ordnet mehrere Sessions einem Projekt zu (legt es dabei an, falls es das noch nicht gibt). */
    fun assignMany(sessionIds: List<String>, projectKey: String) {
        val generation = gate.generation ?: return
        val native = core ?: return
        commands.launch {
            try {
                withContext(Dispatchers.IO) { sessionIds.forEach { native.setProject(it, projectKey) } }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
            }
            if (generation == gate.generation) {
                refreshSessions()
                refreshProjectsQuietly()
            }
        }
    }

    private fun refreshProjectsQuietly() {
        projectsJob?.cancel()
        refreshProjects()
    }

    private fun projectViewError(message: String) {
        change { it.copy(projectView = it.projectView?.copy(error = message)) }
    }

    private fun projectMutation(
        label: String,
        done: (Boolean) -> Unit = {},
        block: suspend (MobileCore) -> ProjectChange,
    ) {
        val generation = gate.generation ?: return
        val native = core ?: return
        commands.launch {
            try {
                val outcome = withContext(Dispatchers.IO) { block(native) }
                if (generation == gate.generation) {
                    change { it.copy(projectView = it.projectView?.applyChange(outcome)) }
                    if (outcome is ProjectChange.Remove) refreshProjectsQuietly()
                }
                done(true)
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                done(false)
                if (generation == gate.generation) projectViewError("$label: ${error.displayMessage()}")
            }
        }
    }

    /** Actions for `WorkspaceScreen` (A4); `onNew` selects the created session through `select`. */
    fun projectActions(select: (String) -> Unit) = ProjectSessionsActions(
        onNew = { createInProject(select) },
        onRefresh = ::refreshProjectSessions,
        onLoadMore = ::loadMoreProjectSessions,
        onRename = ::renameSession,
        onArchive = ::archiveSession,
        onAssign = ::assignProject,
    )

    suspend fun manualCloudWorkTarget(expectedGeneration: Long): ManualCloudWorkTarget? {
        val native = core ?: return null
        val stateBefore = state.value
        if (localBackend || !stateBefore.online || stateBefore.generation != expectedGeneration) return null
        val profile = stateBefore.profile ?: return null
        val access = withContext(Dispatchers.IO) { native.manualCloudWorkAccess() } ?: return null
        val after = state.value
        if (core !== native || !after.online || after.generation != expectedGeneration || after.profile != profile ||
            !access.isCurrent()) return null
        return ManualCloudWorkTarget(access, expectedGeneration, profile.label)
    }

    /** A single explicit empty work session, never dispatched to a Cloud core. */
    suspend fun createLocalUiaWorkSession(
        expectedGeneration: Long,
        stillAuthorized: () -> Boolean,
    ): LocalUiaWorkCreate {
        val native = core ?: return LocalUiaWorkCreate.Unavailable
        fun current() = core === native && canUseLocalUiaWork(expectedGeneration)
        if (!current() || !state.value.canCreate || state.value.creating || !stillAuthorized()) {
            return LocalUiaWorkCreate.Unavailable
        }
        change { it.copy(creating = true) }
        try {
            val created = withContext(Dispatchers.IO) { native.createSession(LocalUiaWorkTitle) }
            if (!current() || !stillAuthorized()) return LocalUiaWorkCreate.Unconfirmed
            change { state ->
                state.copy(sessions = listOf(created) + state.sessions.filterNot { it.sessionId == created.sessionId })
            }
            refreshSessions()
            return LocalUiaWorkCreate.Created(
                LocalUiaWorkLink(
                    created.sessionId, expectedGeneration, LocalUiaWorkTitle,
                    System.currentTimeMillis(), android.os.SystemClock.elapsedRealtime(),
                ),
            )
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            return LocalUiaWorkCreate.Unconfirmed
        } finally {
            if (core === native && gate.generation == expectedGeneration) change { it.copy(creating = false) }
        }
    }

    suspend fun localAssistantSearchTarget(
        generation: Long, stillAuthorized: () -> Boolean,
    ): LocalAssistantSearchTarget? {
        val native = core ?: return null
        if (!canUseLocalUiaWork(generation) || !stillAuthorized()) return null
        val scope = withContext(Dispatchers.IO) {
            native.localAssistantSessionAccess()?.let { it to native.localAssistantCanRenameSession(it) }
        } ?: return null
        if (core !== native || !canUseLocalUiaWork(generation) || !stillAuthorized()) return null
        return LocalAssistantSearchTarget(generation, scope.first, scope.second)
    }

    suspend fun localAssistantSearchTargetCurrent(
        target: LocalAssistantSearchTarget, stillAuthorized: () -> Boolean,
    ): Boolean {
        val native = core ?: return false
        if (!canUseLocalUiaWork(target.generation) || !stillAuthorized()) return false
        val current = withContext(Dispatchers.IO) { native.localAssistantSessionAccessCurrent(target.access) }
        return current && core === native && canUseLocalUiaWork(target.generation) && stillAuthorized()
    }

    suspend fun changeLocalAssistantSession(
        target: LocalAssistantSearchTarget, change: AssistantSessionChange, stillAuthorized: () -> Boolean,
    ) {
        val native = core ?: throw MobileException.Disconnected()
        val before = state.value
        if (!target.canRename || !canUseLocalUiaWork(target.generation) || !stillAuthorized() ||
            localAssistantSummary(before, target, change.id) == null
        ) throw MobileException.Disconnected()
        if (change is AssistantSessionChange.AssignProject &&
            change.destination !in assistantProjectChoices(before, target.generation)
        ) throw MobileException.Denied("Project selection changed")
        withContext(Dispatchers.IO) {
            when (change) {
                is AssistantSessionChange.Rename -> native.localAssistantRenameSession(target.access, change.id, change.title)
                is AssistantSessionChange.AssignProject ->
                    native.localAssistantAssignExistingProject(target.access, change.id, change.destination.key)
                is AssistantSessionChange.Archive -> throw MobileException.Denied("Archive unavailable in Local Assistant")
            }
            if (!native.localAssistantSessionAccessCurrent(target.access)) throw MobileException.Disconnected()
        }
        if (core !== native || !canUseLocalUiaWork(target.generation) || !stillAuthorized()) {
            throw MobileException.Disconnected()
        }
        refreshSessions()
        if (change is AssistantSessionChange.AssignProject) refreshProjectsQuietly()
    }

    fun canUseLocalUiaWork(expectedGeneration: Long): Boolean {
        val current = state.value
        return localBackend && current.local && current.online &&
            gate.generation == expectedGeneration && current.generation == expectedGeneration
    }

    fun createSession(title: String?, select: (String) -> Unit) {
        val generation = gate.generation ?: return
        val native = core ?: return
        val current = state.value
        if (!current.canCreate || current.creating) return
        change { it.copy(creating = true, listError = null) }
        commands.launch {
            try {
                val session = withContext(Dispatchers.IO) { native.createSession(title) }
                if (generation == gate.generation) {
                    change { it.copy(sessions = listOf(session) + it.sessions.filterNot { existing -> existing.sessionId == session.sessionId }) }
                    select(session.sessionId)
                    refreshSessions()
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    change { it.copy(listError = "Create session: ${error.displayMessage()}") }
                }
            } finally {
                change { it.copy(creating = false) }
            }
        }
    }

    /** Archive belongs to the live authenticated backend, not to a replayed window state. */
    private suspend fun archiveBinding(sessionId: String): NativeArchiveBinding? {
        val native = core ?: return null
        val generation = gate.generation ?: return null
        val initial = state.value
        if (!archiveSourceKnown(initial, sessionId, generation)) return null
        val (scope, retainedRewrite) = withContext(Dispatchers.IO) {
            native.archiveScope()?.let { it to native.archivePreviousAvailable(it) }
        } ?: return null
        val current = state.value
        if (core !== native || gate.generation != generation ||
            initial.profile != current.profile || !archiveSourceKnown(current, sessionId, generation)
        ) return null
        val attachment = ArchiveAttachment(
            archiveScopeFor(scope, sessionId), generation,
            archiveCheckpointV1 = true, archiveRetainedRewriteV1 = retainedRewrite,
        )
        return NativeArchiveBinding(native, scope, attachment)
    }

    suspend fun cloudLibraryScope(): uniffi.harw_mobile_ffi.MobileCloudLibraryScope? {
        val native = core ?: return null
        val before = state.value
        val generation = before.generation ?: return null
        if (localBackend || !before.online || before.profile == null) return null
        val scope = withContext(Dispatchers.IO) { native.cloudLibraryScope() } ?: return null
        val current = state.value
        return scope.takeIf {
            current.online && current.generation == generation && current.profile == before.profile && core === native
        }
    }

    suspend fun cloudLibrary(
        scope: uniffi.harw_mobile_ffi.MobileCloudLibraryScope,
        command: uniffi.harw_mobile_ffi.MobileCloudLibraryCommand,
    ): uniffi.harw_mobile_ffi.MobileCloudLibraryReply {
        val before = state.value
        val native = core ?: throw MobileException.Disconnected()
        if (localBackend || !before.online || before.generation == null) throw MobileException.Disconnected()
        val result = native.cloudLibrary(scope, command)
        val current = state.value
        if (!current.online || current.generation != before.generation || current.profile != before.profile || core !== native) {
            (result as? uniffi.harw_mobile_ffi.MobileCloudLibraryReply.Preview)?.jpeg?.fill(0)
            if (command !is uniffi.harw_mobile_ffi.MobileCloudLibraryCommand.List &&
                command !is uniffi.harw_mobile_ffi.MobileCloudLibraryCommand.Preview
            ) throw MobileException.Ambiguous("Cloud Library connection changed during save")
            throw MobileException.Disconnected()
        }
        return result
    }

    fun cloudLibraryCaptionAccess(
        scope: uniffi.harw_mobile_ffi.MobileCloudLibraryScope,
        revision: ULong,
        assetId: String,
    ): uniffi.harw_mobile_ffi.CloudLibraryCaptionAccess {
        val native = core ?: throw MobileException.Disconnected()
        if (localBackend || !state.value.online) throw MobileException.Disconnected()
        return native.cloudLibraryCaptionAccess(scope, revision, assetId)
    }

    suspend fun cloudLibraryCaptionLocal(
        access: uniffi.harw_mobile_ffi.CloudLibraryCaptionAccess,
        device: HarwConnection,
        choice: uniffi.harw_mobile_ffi.LocalVisionChoice,
    ): uniffi.harw_mobile_ffi.CloudLibraryGeneratedCaption {
        val native = core ?: throw MobileException.Disconnected()
        val localNative = device.core ?: throw MobileException.Disconnected()
        if (localBackend || !state.value.online || !device.localBackend || !device.state.value.online) {
            throw MobileException.Disconnected()
        }
        return native.cloudLibraryCaptionLocal(access, localNative, choice)
    }

    suspend fun cloudLibraryCaptionApi(
        access: uniffi.harw_mobile_ffi.CloudLibraryCaptionAccess,
        provider: uniffi.harw_mobile_ffi.ProviderStore,
        choice: uniffi.harw_mobile_ffi.ApiVisionChoice,
    ): uniffi.harw_mobile_ffi.CloudLibraryGeneratedCaption {
        val native = core ?: throw MobileException.Disconnected()
        if (localBackend || !state.value.online) throw MobileException.Disconnected()
        return native.cloudLibraryCaptionApi(access, provider, choice)
    }

    suspend fun saveCloudLibraryCaption(
        caption: uniffi.harw_mobile_ffi.CloudLibraryGeneratedCaption,
    ) {
        val native = core ?: throw MobileException.Disconnected()
        if (localBackend || !state.value.online) throw MobileException.Disconnected()
        native.cloudLibrarySaveCaption(caption)
    }

    private fun archiveSourceKnown(current: WorkspaceState, sessionId: String, generation: Long): Boolean =
        current.online && current.generation == generation &&
            (current.windows[sessionId]?.generation == generation || current.sessions.any { it.sessionId == sessionId })

    suspend fun archiveAttachment(sessionId: String): ArchiveAttachment? = archiveBinding(sessionId)?.attachment

    /** Retain unresolved same-operation retries after the sheet, route, or Activity is closed. */
    suspend fun archiveAccess(sessionId: String): ArchiveForkController? {
        val active = archiveBinding(sessionId) ?: return null
        if (archiveCore !== active.native) {
            archiveControllers.clear()
            archiveCore = active.native
        }
        val scope = active.attachment.scope
        archiveControllers.keys.removeAll {
            it.profileId != scope.profileId || it.serverId != scope.serverId || it.backend != scope.backend
        }
        val existing = archiveControllers[scope]
        if (existing != null) return existing
        if (archiveControllers.size >= ArchiveControllerLimit) {
            val replaceable = archiveControllers.entries.firstOrNull { (_, controller) ->
                val status = controller.state.value
                !status.busy && !status.outcomeUnknown
            }?.key ?: return null
            archiveControllers.remove(replaceable)
        }
        val port = NativeArchiveForkPort(
            binding = { archiveBinding(sessionId)?.takeIf { it.native === active.native } },
            onCreated = { child ->
                change { state ->
                    state.copy(sessions = listOf(child) + state.sessions.filterNot { it.sessionId == child.sessionId })
                }
                // The host inherited and authorized the source project. No client-side reassign.
                refreshSessions()
                refreshProjectSessions()
            },
        )
        return ArchiveForkController(scope, port).also { archiveControllers[scope] = it }
    }

    /** Normal Fork is based only on this exact attached displayed prefix, not the archive. */
    suspend fun forkDisplayedSession(
        sourceId: String,
        generation: Long,
        head: SessionCursor,
        rows: List<uniffi.harw_mobile_ffi.TranscriptEntry>,
    ): String {
        val native = core ?: throw MobileException.Disconnected()
        val current = state.value
        if (generation != gate.generation || current.generation != generation || !current.online ||
            current.windows[sourceId]?.generation != generation
        ) throw MobileException.Disconnected()
        if (!current.canCreate) throw IllegalStateException("Diese Verbindung darf keine neuen Chats anlegen.")
        if (current.creating) throw IllegalStateException("Ein neuer Chat wird bereits angelegt.")
        change { it.copy(creating = true, listError = null) }
        try {
            val child = withContext(Dispatchers.IO) {
                native.forkDisplayedSession(sourceId, head, rows)
            }
            val after = state.value
            if (child.sessionId == sourceId || child.sessionId.isBlank() || core !== native ||
                gate.generation != generation || after.profile != current.profile
            ) throw IllegalStateException("Der neue Chat konnte auf dieser Verbindung nicht bestätigt werden.")
            change { state -> state.copy(sessions = listOf(child) +
                state.sessions.filterNot { it.sessionId == child.sessionId }) }
            refreshSessions()
            refreshProjectSessions()
            return child.sessionId
        } finally {
            if (core === native && gate.generation == generation) change { it.copy(creating = false) }
        }
    }

    /** Fork: neue Session mit dem Verlauf der Quelle bis vor der Nachricht [beforeItem]; bleibt im selben Projekt. */
    fun forkSession(sourceId: String, beforeItem: String, projectKey: String?, select: (String) -> Unit, failed: (String) -> Unit) {
        val generation = gate.generation ?: return
        val native = core ?: return
        val current = state.value
        if (!current.canCreate || current.creating) return
        change { it.copy(creating = true, listError = null) }
        commands.launch {
            try {
                val session = withContext(Dispatchers.IO) {
                    val created = native.forkSession(sourceId, beforeItem, null)
                    val key = projectKey?.takeIf { it.isNotEmpty() && it != NoProjectKey }
                    if (key != null) {
                        try { native.setProject(created.sessionId, key) } catch (error: Exception) {
                            if (error is CancellationException) throw error
                        }
                    }
                    created
                }
                if (generation == gate.generation) {
                    change { it.copy(sessions = listOf(session) + it.sessions.filterNot { existing -> existing.sessionId == session.sessionId }) }
                    select(session.sessionId)
                    refreshSessions()
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) failed("Fork: ${error.displayMessage()}")
            } finally {
                change { it.copy(creating = false) }
            }
        }
    }

    fun exactRewindAvailable(sessionId: String, generation: Long): Boolean {
        val native = core ?: return false
        val current = state.value
        val window = current.windows[sessionId] ?: return false
        val snapshot = window.snapshot ?: return false
        return current.online && current.generation == generation && gate.generation == generation &&
            window.generation == generation && snapshot.online && snapshot.canSubmit &&
            snapshot.runningTurnId == null && native.supportsExactRewriteReceipts()
    }

    fun submit(
        sessionId: String, generation: Long, attempt: DraftAttempt,
        cloudLibraryScope: uniffi.harw_mobile_ffi.MobileCloudLibraryScope? = null,
        result: (DraftResult) -> Unit,
    ): Boolean =
        runAction(sessionId, generation, SessionActionKind.Send, failure = { error ->
            result(DraftResult.Rejected(error.displayMessage(), error is MobileException.Ambiguous))
        }) {
            val receipt = withContext(Dispatchers.IO) {
                if (cloudLibraryScope != null) {
                    it.submitCloudLibraryImages(cloudLibraryScope, sessionId, attempt.text, attempt.head, attempt.id, attempt.images)
                } else if (attempt.images.isEmpty() && attempt.rewindBefore == null) {
                    it.submit(sessionId, attempt.text, attempt.head, attempt.id)
                } else {
                    it.submitEdit(sessionId, attempt.text, attempt.head, attempt.id, attempt.images, attempt.rewindBefore)
                }
            }
            val outcome = when (receipt.outcome) {
                SubmitOutcome.ACCEPTED -> receipt.replacementItemId
                    ?.takeIf { attempt.rewindBefore != null && isDurableRewriteItemId(it) }
                    ?.let { DraftResult.ExactAccepted(it) } ?: DraftResult.Accepted
                SubmitOutcome.STALE -> DraftResult.Rejected("The session changed. Review the latest messages before sending again.")
                SubmitOutcome.QUEUE_FULL -> DraftResult.Rejected(receipt.detail ?: "The server's queue is full.")
                SubmitOutcome.DENIED -> DraftResult.Rejected(receipt.detail ?: "The server refused this prompt.")
                SubmitOutcome.BUSY -> DraftResult.Rejected(busyDraftMessage(receipt.detail))
                SubmitOutcome.IN_FLIGHT -> DraftResult.Rejected(receipt.detail ?: "Check the transcript before retrying this prompt.", ambiguous = true)
            }
            result(outcome)
        }

    suspend fun sessionMediaPreview(
        expectedProfile: ServerProfile?,
        expectedDevice: String?,
        expectedGeneration: Long?,
        expectedRewrite: UInt?,
        sessionId: String,
        itemId: String,
        index: UInt,
        expectedMedia: String,
        mode: uniffi.harw_mobile_ffi.MobileMediaPreviewMode,
    ): uniffi.harw_mobile_ffi.MobileMediaPreview {
        val native = core ?: return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        val generation = expectedGeneration ?: return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        val rewrite = expectedRewrite ?: return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        if (index >= 50u || expectedMedia.isEmpty() || !isDurableRewriteItemId(itemId)) {
            return uniffi.harw_mobile_ffi.MobileMediaPreview.Unreadable
        }
        fun stillAttached(): Boolean {
            val current = state.value
            val window = current.windows[sessionId] ?: return false
            val snapshot = window.snapshot ?: return false
            if (native !== core || !current.online || current.generation != generation ||
                current.profile != expectedProfile || current.deviceNodeId != expectedDevice ||
                window.generation != generation || window.answerReloadPending || !snapshot.online ||
                snapshot.sessionId != sessionId || snapshot.head?.generation != rewrite
            ) return false
            // A per-row ordinal may only resolve the exact retained opaque
            // reference projected into that authorized message, never a new slot.
            val row = snapshot.transcript.singleOrNull { it.id == itemId } ?: return false
            return row.kind == uniffi.harw_mobile_ffi.TranscriptKind.USER &&
                row.images.getOrNull(index.toInt()) == expectedMedia
        }
        if (!stillAttached()) return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        // This only captures a witness under the native state lock. It must
        // happen before waiting on another dispatcher or issuing any RPC.
        val pinned = try {
            native.mediaPreviewScope(expectedProfile, sessionId)
        } catch (_: MobileException.Denied) {
            return uniffi.harw_mobile_ffi.MobileMediaPreview.Denied
        } catch (_: MobileException.Revoked) {
            return uniffi.harw_mobile_ffi.MobileMediaPreview.Denied
        } catch (_: MobileException) {
            return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        }
        try {
            if (!stillAttached()) return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
            val result = withContext(Dispatchers.IO) { native.mediaPreview(pinned, itemId, index, mode) }
            return if (stillAttached()) result else uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        } finally {
            pinned.destroy()
        }
    }

    suspend fun archiveMediaPreview(
        expected: ArchiveAttachment,
        checkpointId: String,
        row: ArchiveRowLocator,
        index: UInt,
        mode: uniffi.harw_mobile_ffi.MobileMediaPreviewMode,
    ): uniffi.harw_mobile_ffi.MobileMediaPreview {
        val sessionId = expected.scope.sessionId
        val active = archiveBinding(sessionId)?.takeIf { it.attachment == expected }
            ?: return uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
        val result = withContext(Dispatchers.IO) {
            active.native.archiveMediaPreview(
                active.scope, sessionId, checkpointId,
                uniffi.harw_mobile_ffi.MobileArchiveRowLocator(row.sequence, row.itemId), index, mode,
            )
        }
        return if (archiveBinding(sessionId) == active) result
        else uniffi.harw_mobile_ffi.MobileMediaPreview.Offline
    }

    /** Setzt eine durch Neustart unterbrochene Session fort; `true` bei Erfolg. */
    suspend fun resumeSession(sessionId: String): Boolean {
        val native = core ?: return false
        return withContext(Dispatchers.IO) {
            try {
                native.resumeSession(sessionId)
                true
            } catch (_: Exception) {
                false
            }
        }
    }

    suspend fun diagnostics(command: DiagnosticCommand): DiagnosticView? {
        val native = core ?: return null
        val view = withContext(Dispatchers.IO) {
            when (command) {
                DiagnosticCommand.Refresh -> Unit
                DiagnosticCommand.Clear -> native.diagnosticsClear()
                is DiagnosticCommand.Set -> native.diagnosticsSetLevel(command.domain, command.level)
                is DiagnosticCommand.Trace -> native.diagnosticsEnableTrace(command.domain)
            }
            DiagnosticView(native.diagnosticsSnapshot(), uniffi.harw_mobile_ffi.Domain.entries.associateWith {
                native.diagnosticsLevel(it)
            })
        }
        return view.takeIf { core === native }
    }

    suspend fun apiImageEditChoices(): List<uniffi.harw_mobile_ffi.ApiImageEditChoice> {
        if (!localBackend) return emptyList()
        val native = core ?: return emptyList()
        return withContext(Dispatchers.IO) { native.apiImageEditChoices() }
    }

    suspend fun prepareApiImageEdit(
        choice: uniffi.harw_mobile_ffi.ApiImageEditChoice,
        png: ByteArray,
        instruction: String,
    ): uniffi.harw_mobile_ffi.ApiImageEditDraft {
        check(localBackend) { "Bildbearbeitung ist nicht im Cloud-Chat verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        // Caller owns the native preview handle and dispatches preparation off the UI thread.
        return native.prepareApiImageEdit(choice, png, instruction)
    }

    suspend fun confirmApiImageEdit(
        draft: uniffi.harw_mobile_ffi.ApiImageEditDraft,
        explicitlySynchronous: Boolean,
    ): uniffi.harw_mobile_ffi.ApiImageEditAsset {
        check(localBackend) { "Bildbearbeitung ist nicht im Cloud-Chat verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        return withContext(Dispatchers.IO) { native.confirmApiImageEdit(draft, explicitlySynchronous) }
    }

    suspend fun apiVisionChoices(): List<uniffi.harw_mobile_ffi.ApiVisionChoice> {
        if (!localBackend) return emptyList()
        val native = core ?: return emptyList()
        return withContext(Dispatchers.IO) { native.apiVisionChoices() }
    }

    suspend fun apiVisionDescribe(
        choice: uniffi.harw_mobile_ffi.ApiVisionChoice,
        image: uniffi.harw_mobile_ffi.PromptImage,
    ): String {
        check(localBackend) { "Keine lokale API-Bildroute verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        return native.apiVisionDescribe(choice, image)
    }

    suspend fun apiVisionObserve(
        choice: uniffi.harw_mobile_ffi.ApiVisionChoice,
        image: uniffi.harw_mobile_ffi.PromptImage,
    ): uniffi.harw_mobile_ffi.ApiVisionObservation {
        check(localBackend) { "Keine lokale API-Bildroute verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        return native.apiVisionObserve(choice, image)
    }

    suspend fun withAutoTitle(
        expectedProfile: ServerProfile?,
        expectedGeneration: Long,
        sessionId: String,
        generate: suspend (List<uniffi.harw_mobile_ffi.AutoTitleSeedPair>) -> String?,
    ) {
        val native = core ?: return
        fun current(): Boolean = state.value.let {
            core === native && it.online && it.profile == expectedProfile && it.generation == expectedGeneration &&
                it.windows[sessionId]?.generation == expectedGeneration
        }
        if (!current()) return
        // This native method only captures the current link and must stay before dispatch.
        val access = native.autoTitleAccess(sessionId, expectedProfile) ?: return
        try {
            if (!current()) return
            val pairs = withContext(Dispatchers.IO) { native.autoTitleSeed(access) } ?: return
            if (!current()) return
            val title = generate(pairs) ?: return
            if (!current()) return
            val outcome = withContext(Dispatchers.IO) { native.publishAutoTitle(access, title) }
            if (current() && outcome == uniffi.harw_mobile_ffi.AutoTitlePublication.APPLIED) {
                refreshSessions()
                refreshProjectSessions()
            }
        } finally {
            access.destroy()
        }
    }

    suspend fun sessionTitleLocalModels(): List<uniffi.harw_mobile_ffi.SessionTitleLocalChoice> {
        if (!localBackend) return emptyList()
        val native = core ?: return emptyList()
        return withContext(Dispatchers.IO) { native.sessionTitleLocalChoices() }
    }

    suspend fun generateLocalSessionTitle(
        choice: uniffi.harw_mobile_ffi.SessionTitleLocalChoice,
        pairs: List<uniffi.harw_mobile_ffi.SessionTitleTurn>,
    ): String {
        check(localBackend) { "Das gewählte Titelmodell ist nicht lokal geladen." }
        val native = core ?: error("Das gewählte Titelmodell ist nicht lokal geladen.")
        return withContext(Dispatchers.IO) { native.sessionTitleGenerateLocal(choice, pairs) }
    }

    suspend fun localVisionChoices(): List<uniffi.harw_mobile_ffi.LocalVisionChoice> {
        check(localBackend) { "Kein lokales Modell verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        return withContext(Dispatchers.IO) { native.localVisionChoices() }
    }

    suspend fun localVisionDescribe(
        choice: uniffi.harw_mobile_ffi.LocalVisionChoice,
        images: List<uniffi.harw_mobile_ffi.LocalVisionImage>,
        progress: uniffi.harw_mobile_ffi.LocalVisionProgress,
    ) {
        check(localBackend) { "Kein lokales Modell verfügbar" }
        val native = core ?: error("Lokaler Host ist nicht verbunden")
        // UniFFI serializes the bounded image payload before yielding to the Rust future.
        withContext(Dispatchers.IO) { native.localVisionDescribe(choice, images, progress) }
    }

    suspend fun hybridVisionChoices(): List<uniffi.harw_mobile_ffi.HybridVisionChoice> {
        check(localBackend) { "Cloud-Bildanalyse ist nur im lokalen Chat verfügbar" }
        val native = core ?: error("Lokaler Chat ist nicht verbunden")
        return withContext(Dispatchers.IO) { native.hybridVisionChoices() }
    }

    suspend fun hybridVisionReply(
        choice: uniffi.harw_mobile_ffi.HybridVisionChoice,
        images: List<uniffi.harw_mobile_ffi.PromptImage>,
        question: String,
        progress: uniffi.harw_mobile_ffi.HybridVisionProgress,
    ): String {
        check(localBackend) { "Cloud-Bildanalyse ist nur im lokalen Chat verfügbar" }
        val native = core ?: error("Lokaler Chat ist nicht verbunden")
        return native.hybridVisionReply(choice, images, question, progress)
    }

    /** Nimmt der aktive Host Bilder am Prompt an (Feature `images`)? */
    suspend fun supportsImages(): Boolean {
        val native = core ?: return false
        return withContext(Dispatchers.IO) { native.supportsImages() }
    }

    /** Stoppt einen Kind-Agenten (`/agent stop <id>`); `onDone(null)` bei Erfolg, sonst ein deutscher Fehlertext. */
    fun stopAgent(sessionId: String, agentId: String, onDone: (String?) -> Unit) =
        stopTarget(onDone) { it.stopAgent(sessionId, agentId, newMsgId()) }

    /** Bricht einen Hintergrund-Job ab (`/jobs stop <id>`); `onDone(null)` bei Erfolg, sonst ein deutscher Fehlertext. */
    fun cancelJob(sessionId: String, workId: String, onDone: (String?) -> Unit) =
        stopTarget(onDone) { it.cancelJob(sessionId, workId, newMsgId()) }

    /**
     * Globale Jobs-/Agenten-Uebersicht (Wire-Feature `overview`): fuehrt `call` auf einem IO-Thread gegen den
     * nativen Kern dieser Verbindung aus. Fehler kommen als `Result.failure` mit deutschem Text zurueck.
     */
    suspend fun <T> overviewCall(call: (MobileCore) -> T): Result<T> {
        val native = core ?: return Result.failure(IllegalStateException("Nicht verbunden."))
        return withContext(Dispatchers.IO) {
            try {
                Result.success(call(native))
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                Result.failure(IllegalStateException(error.displayMessage()))
            }
        }
    }

    /** Neue Idempotenz-Kennung fuer eine schreibende Uebersichts-Aktion. */
    fun overviewMsgId(): String = newMsgId()

    private fun stopTarget(onDone: (String?) -> Unit, call: (MobileCore) -> ManageRunResult) {
        val generation = gate.generation
        val native = core
        if (generation == null || native == null) {
            onDone("Nicht verbunden.")
            return
        }
        scope.launch {
            val failure = try {
                val result = withContext(Dispatchers.IO) { call(native) }
                when {
                    result.ok && result.effect != ManageEffect.REFUSED -> null
                    else -> result.text.ifBlank { "Der Host hat die Anfrage abgelehnt." }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                error.displayMessage()
            }
            if (generation == gate.generation) onDone(failure)
        }
    }

    fun interrupt(sessionId: String, generation: Long, turnId: String) {
        if (state.value.windows[sessionId]?.snapshot?.runningTurnId != turnId) return
        runAction(sessionId, generation, SessionActionKind.Interrupt) {
            withContext(Dispatchers.IO) { it.interrupt(sessionId, turnId) }
        }
    }

    fun decide(sessionId: String, generation: Long, approvalId: String, decision: ApprovalDecision): Boolean {
        val approval = state.value.windows[sessionId]?.snapshot?.approvals?.firstOrNull { it.id == approvalId }
        if (approval == null || decision !in approval.decisions) return false
        return runAction(sessionId, generation, SessionActionKind.Decide) {
            val receipt = withContext(Dispatchers.IO) { it.decide(sessionId, approvalId, decision) }
            if (gate.generation != generation || receipt.outcome == ApprovalOutcome.RESOLVED) return@runAction
            val detail = receipt.detail ?: when (receipt.outcome) {
                ApprovalOutcome.ALREADY_RESOLVED -> "This approval was already decided."
                ApprovalOutcome.EXPIRED -> "This approval expired."
                ApprovalOutcome.DENIED -> "The server refused this decision."
                ApprovalOutcome.RESOLVED -> error("Successful decisions are handled above")
            }
            windowError(sessionId, detail)
        }
    }

    fun approvalFailure(sessionId: String, generation: Long, detail: String) {
        if (generation == gate.generation && state.value.windows[sessionId]?.generation == generation) {
            windowError(sessionId, detail)
        }
    }

    private fun runAction(
        sessionId: String,
        generation: Long,
        kind: SessionActionKind,
        failure: ((Exception) -> Unit)? = null,
        action: suspend (MobileCore) -> Unit,
    ): Boolean {
        val native = core ?: return false
        val window = state.value.windows[sessionId] ?: return false
        val snapshot = window.snapshot ?: return false
        if (window.generation != generation || !snapshot.online) return false
        val allowed = when (kind) {
            SessionActionKind.Send -> snapshot.canSubmit
            SessionActionKind.Interrupt -> snapshot.canInterrupt
            SessionActionKind.Decide -> snapshot.canApprove
        }
        if (!allowed) return false
        val key = SessionAction(sessionId, kind)
        val ticket = gate.begin(generation, key) ?: return false
        windowError(sessionId, null)
        change { it.copy(pendingActions = it.pendingActions + key) }
        commands.launch {
            try {
                action(native)
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (failure != null) {
                    failure(error)
                } else if (gate.generation == generation) {
                    windowError(sessionId, error.displayMessage())
                }
            } finally {
                if (gate.finish(ticket)) change { it.copy(pendingActions = it.pendingActions - key) }
            }
        }
        return true
    }

    private fun watchNetwork() {
        if (networkCallback != null) return
        val manager = connectivity ?: return
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                scope.launch { networkAvailable() }
            }
        }
        try {
            manager.registerDefaultNetworkCallback(callback)
            networkCallback = callback
        } catch (error: Exception) {
            Log.w("HarwConnection", "network callback unavailable", error)
        }
    }

    private fun unwatchNetwork() {
        val callback = networkCallback ?: return
        networkCallback = null
        try {
            connectivity?.unregisterNetworkCallback(callback)
        } catch (_: Exception) {
        }
    }

    internal fun networkAvailable() {
        val s = state.value
        if (!s.canSetUp || (!localBackend && !routeChoiceEnabled(s.routePreference, s.profile))) return
        when (networkSignal(s.phase, s.stopping, s.forgetting, s.profile != null)) {
            NetworkSignal.None -> return
            NetworkSignal.Nudge -> {
                if (networkJob?.isActive == true) return
                networkJob = scope.launch {
                    val native = core ?: return@launch
                    runCatching { withContext(Dispatchers.IO) { native.networkAvailable() } }
                }
            }
            NetworkSignal.Restart -> {
                if (networkJob?.isActive == true) return
                networkJob = scope.launch {
                    val native = core ?: return@launch
                    val status = try {
                        withContext(Dispatchers.IO) {
                            native.networkAvailable()
                            native.status()
                        }
                    } catch (error: Exception) {
                        if (error is CancellationException) throw error
                        if (error is MobileException.Revoked) invalidate()
                        return@launch
                    }
                    if (status.phase in setOf(
                            ConnectionPhase.CONNECTING, ConnectionPhase.CONNECTED, ConnectionPhase.RECONNECTING,
                        )
                    ) {
                        val request = ++lifecycle
                        applyStatus(status)
                        startPolling(native, request)
                    } else {
                        applyStatus(status)
                    }
                }
            }
        }
    }

    private fun startPolling(native: MobileCore, request: Long) {
        presenceJob = scope.launch {
            while (isActive && request == lifecycle) {
                val status = withContext(Dispatchers.IO) { native.status() }
                if (request != lifecycle) break
                applyStatus(status)
                if (status.phase in setOf(ConnectionPhase.DISCONNECTED, ConnectionPhase.FAILED, ConnectionPhase.REVOKED)) break
                if (status.phase == ConnectionPhase.CONNECTED) refreshWindows(native, request)
                events.awaitStatus(1_000)
            }
        }
        listLoop = scope.launch {
            while (isActive && request == lifecycle && state.value.hasConnection) {
                refreshSessions()
                refreshProjects()
                events.awaitSessions(3_000)
            }
        }
    }

    private suspend fun refreshWindows(native: MobileCore, request: Long) {
        val generation = gate.generation ?: return
        val windows = observers.keys.filterNot { it in detaching }.map { id ->
            Triple(id, id in opened, attachmentReadEpochs.begin(id))
        }
        if (windows.isEmpty()) return
        val results = try {
            withContext(Dispatchers.IO) {
                windows.map { (id, attached, ticket) ->
                    Triple(id, ticket, try {
                        Result.success(
                            (if (attached) native.snapshot(id) else native.openSession(id)).let { snap ->
                                snap to runCatching { native.sessionHolder(id) }.getOrNull()
                            },
                        )
                    } catch (error: Exception) {
                        if (error is CancellationException) throw error
                        Result.failure(error)
                    })
                }
            }
        } catch (error: CancellationException) {
            for ((id, _, ticket) in windows) attachmentReadEpochs.finish(id, ticket)
            throw error
        }
        val current = results.filter { (id, ticket, _) -> attachmentReadEpochs.finish(id, ticket) }
        if (request != lifecycle || gate.generation != generation || current.isEmpty()) return
        val updates = state.value.windows.toMutableMap()
        for ((id, _, result) in current) {
            if (id in detaching) continue
            val snapshot = result.getOrNull()?.first
            val holder = result.getOrNull()?.second
            if (snapshot != null && id !in detaching) opened.add(id)
            if (id !in observers) {
                if (id !in detachJobs) detachSession(id)
                continue
            }
            val previous = updates[id] ?: WindowState()
            updates[id] = if (snapshot != null) {
                // Native replay rebuilds from an authoritative snapshot. Hold the prior
                // visible text (read-only) while that same observed session catches up;
                // never concatenate its stale deltas with the replacement stream.
                val prior = previous.snapshot
                val keepStale = !snapshot.online && prior?.sessionId == snapshot.sessionId &&
                    (previous.preserveAnswerWhileReloading ||
                        (!prior?.liveText.isNullOrBlank() && snapshot.detail in setOf(
                            "Attaching to session", "Reconnecting to the session stream",
                            "Catching up with the server", "Synchronizing the session transcript",
                        )))
                val shown = if (keepStale) {
                    previous.snapshot?.copy(
                        online = false, canSubmit = false, canInterrupt = false, canApprove = false,
                        detail = snapshot.detail,
                    ) ?: snapshot
                } else snapshot
                previous.copy(
                    snapshot = shown, generation = generation.takeIf { snapshot.online }, holder = holder,
                    preserveAnswerWhileReloading = keepStale,
                )
            } else {
                previous.copy(generation = null, error = result.exceptionOrNull()?.displayMessage())
            }
        }
        change { it.copy(windows = updates) }
    }

    private fun applyStatus(status: ConnectionStatus) {
        if (status.phase == ConnectionPhase.REVOKED) {
            invalidate()
            return
        }
        if (status.phase == ConnectionPhase.CONNECTED) {
            if (gate.generation == null) gate.open()
        } else {
            closeGate()
        }
        change {
            it.connectionStatus(status.phase, status.detail, status.canCreate, gate.generation)
                .routeStatus(status.route, status.tor)
                .copy(attempt = status.attempt, nextRetryAtMs = status.nextRetryAtMs)
        }
    }

    private fun invalidate() {
        unwatchNetwork()
        if (state.value.phase == ConnectionPhase.REVOKED) return
        clearSessionActions()
        clearArchiveAccess()
        change { it.invalidated() }
    }

    private fun beginForgetSetup() {
        unwatchNetwork()
        if (state.value.forgetting) return
        clearSessionActions()
        clearArchiveAccess()
        setupJob?.cancel()
        change { it.forgetSetup() }
        purgeSetup()
    }

    private fun clearArchiveAccess() {
        archiveControllers.clear()
        archiveCore = null
    }

    private fun clearSessionActions() {
        ++lifecycle
        gate.close()
        accountJob?.cancel()
        opened.clear()
        detachJobs.values.forEach(Job::cancel)
        detachJobs.clear()
    }

    private fun purgeSetup() {
        scope.launch {
            // Setup erasure can only wait on local storage writes before erasing the
            // identity. Native RPCs and Android document providers can finish later.
            val storageFailed = setupRemovalFailed {
                withContext(Dispatchers.IO) {
                    storageLock.withLock {
                        store.purgeSetup()
                        if (!routePrefs.edit().clear().commit()) error("route preference not cleared")
                    }
                }
            }
            setupJob?.cancelAndJoin()
            connectJob?.join()
            presenceJob?.cancelAndJoin()
            listLoop?.cancelAndJoin()
            refreshJob?.cancelAndJoin()
            accountJob?.cancelAndJoin()
            commands.coroutineContext[Job]?.children?.toList()?.joinAll()
            val native = coreLock.withLock { core.also { core = null } }
            val disposalFailed = setupRemovalFailed {
                withContext(Dispatchers.IO) {
                    native?.let {
                        it.clearListener()
                        it.destroy()
                    }
                }
            }
            if (storageFailed || disposalFailed) {
                change {
                    it.copy(
                        forgetting = false,
                        detail = null,
                        cleanupError = "Android could not finish removing the saved phone setup.",
                    )
                }
            } else {
                change {
                    it.copy(
                        hasSavedSetup = false,
                        forgetting = false,
                        forgetRequired = false,
                        detail = "Saved phone setup was removed.",
                    )
                }
            }
        }
    }

    private suspend fun setupRemovalFailed(remove: suspend () -> Unit): Boolean = try {
        remove()
        false
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        true
    }

    private fun closeGate() {
        gate.close()
        accountJob?.cancel()
        change { it.copy(generation = null, pendingActions = emptySet(), chatgpt = ChatgptAccountState()) }
    }

    private suspend fun loadCore(): MobileCore = coreLock.withLock {
        core ?: withContext(Dispatchers.IO) {
            val privateKey = storageLock.withLock {
                store.loadOrGenerateIdentity { generateIdentity(DeviceLabel).privateKey }
            }
            try {
                val native = MobileCore(privateKey, DeviceLabel)
                native.setListener(events)
                if (!localBackend) try {
                    native.setTorDirectories(File(context.noBackupFilesDir, "tor/state").path, File(context.cacheDir, "tor").path)
                } catch (error: MobileException) {
                    // Only the onion route needs Tor storage; direct connections must keep working.
                    Log.w(LogTag, "Tor storage is unavailable; the onion route will fail", error)
                }
                native to native.enrollment().nodeId
            } finally {
                privateKey.fill(0)
            }
        }.let { (native, nodeId) ->
            core = native
            change { it.copy(deviceNodeId = nodeId, hasSavedSetup = true) }
            native
        }
    }

    private fun setup(label: String, action: suspend () -> Unit) {
        if (!state.value.canSetUp) return
        change { it.copy(setupError = null, setupNotice = null, setupPending = true) }
        setupJob = scope.launch {
            try {
                action()
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                change { it.copy(setupError = "$label: ${error.displayMessage()}") }
            } finally {
                change { it.copy(setupPending = false) }
            }
        }
    }

    private fun windowError(id: String, error: String?) {
        change { it.copy(windows = it.windows + (id to (it.windows[id] ?: WindowState()).copy(error = error))) }
    }

    private inline fun change(transform: (WorkspaceState) -> WorkspaceState) {
        mutableState.value = transform(mutableState.value)
    }

    private companion object {
        // This fixed label is part of the persisted enrollment format.
        const val DeviceLabel = "Harw Android"
        const val AdminPrefsName = "harw_admin_enroll"
        const val AdminPrefReq = "req"
        const val AdminPrefAt = "started_at_ms"
        const val AdminPrefDeadline = "callback_deadline_ms"
        const val AdminPrefBase = "base_url"
        const val LogTag = "HarwConnection"
        const val RoutePrefsName = "harw_connection"
        const val ChatgptUnlinkWarning = "The server removed its local ChatGPT link. Revocation at OpenAI was not confirmed. Review the agent in ChatGPT Settings."
    }
}

internal fun displayedForkFailure(error: Throwable): String = when (error) {
    is MobileException.Disconnected, is MobileException.Ambiguous, is MobileException.Transport ->
        "Verbindung unterbrochen. Öffne nach dem Wiederverbinden denselben Fork, um das Ergebnis zu prüfen."
    else -> error.displayMessage()
}

private fun Throwable.displayMessage(): String = when (this) {
    is MobileException.InvalidIdentity -> detail
    is MobileException.InvalidProfile -> detail
    is MobileException.Connect -> detail
    is MobileException.Session -> detail
    is MobileException.Denied -> detail
    is MobileException.Busy -> detail
    is MobileException.Transport -> detail
    is MobileException.Ambiguous -> detail
    is MobileException.Internal -> detail
    is MobileException.Disconnected -> "The connection was lost."
    is MobileException.Revoked -> "The server stopped authorizing this connection."
    else -> localizedMessage ?: "The operation failed."
}
