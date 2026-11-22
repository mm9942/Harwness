package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
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
import uniffi.harw_mobile_ffi.ChatgptProviderCredential
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.ConnectionStatus
import uniffi.harw_mobile_ffi.MobileCore
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.MobileHolder
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
import uniffi.harw_mobile_ffi.parseServerProfile

/** ONION needs an onion route; without one the preference falls back to AUTO. */
internal fun normalizedPreference(preference: RoutePreference, hasOnion: Boolean): RoutePreference =
    if (preference == RoutePreference.ONION && !hasOnion) RoutePreference.AUTO else preference

internal enum class NetworkSignal { None, Nudge, Restart }

internal fun networkSignal(phase: ConnectionPhase, stopping: Boolean, forgetting: Boolean, hasProfile: Boolean): NetworkSignal {
    if (stopping || forgetting || !hasProfile) return NetworkSignal.None
    return when (phase) {
        ConnectionPhase.CONNECTING, ConnectionPhase.RECONNECTING -> NetworkSignal.Nudge
        ConnectionPhase.FAILED -> NetworkSignal.Restart
        ConnectionPhase.CONNECTED, ConnectionPhase.DISCONNECTED, ConnectionPhase.REVOKED -> NetworkSignal.None
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
    val routePreference: RoutePreference = RoutePreference.AUTO,
    val activeRoute: ConnectionRoute? = null,
    val tor: TorBootstrap? = null,
    val attempt: UInt = 0u,
    val nextRetryAtMs: ULong? = null,
    val projectsSupported: Boolean = false,
    val projects: List<MobileProject> = emptyList(),
    val projectsLoaded: Boolean = false,
    val projectsError: String? = null,
    val projectsLegacy: Boolean = false,
    val projectsShare: MobileShareInfo? = null,
    /** Open project view; `null` while the project list is shown. */
    val projectView: ProjectSessionsState? = null,
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
        listError = null,
        projectsSupported = false,
        projects = emptyList(),
        projectsLoaded = false,
        projectsError = null,
        projectsLegacy = false,
        projectsShare = null,
        projectView = null,
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
            routePreference = RoutePreference.AUTO,
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
)

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

internal data class DraftAttempt(val text: String, val head: SessionCursor, val id: String)

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
)

internal sealed interface DraftResult {
    data object Accepted : DraftResult
    data class Rejected(val error: String, val ambiguous: Boolean = false) : DraftResult
}

/** Process owner. Public commands and state transitions run on Main; storage and UniFFI calls run on IO. */
internal class HarwConnection(private val context: Context) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val commands = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val mutableState = MutableStateFlow(WorkspaceState())
    val state = mutableState.asStateFlow()

    private val store by lazy { DeviceStore(context) }
    private val storageLock = Mutex()
    private val coreLock = Mutex()
    private var core: MobileCore? = null
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
    private val events = CoreEvents()

    init {
        loadSavedSetup()
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
                val (hasSavedSetup, profile) = withContext(Dispatchers.IO) {
                    val (saved, document) = storageLock.withLock { store.hasSavedSetup() to store.readProfile() }
                    saved to document?.let(::parseServerProfile)
                }
                val preference = normalizedPreference(loadRoutePreference(), profile?.onion != null)
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

    fun importProfile(uri: Uri) = importProfileDocument("Import profile") {
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

    private fun importProfileDocument(operation: String, read: () -> String): Boolean {
        val current = state.value
        if (current.hasConnection || current.stopping || !current.canSetUp) return false
        setup(operation) {
            val (document, profile) = withContext(Dispatchers.IO) { read().let { it to parseServerProfile(it) } }
            withContext(Dispatchers.IO) { storageLock.withLock { store.saveProfile(document) } }
            val replaced = profile.windowKey() != state.value.profile?.windowKey()
            if (replaced) opened.clear()
            change {
                it.copy(
                    profile = profile,
                    routePreference = normalizedPreference(it.routePreference, profile.onion != null),
                    hasSavedSetup = true,
                    setupNotice = "Profile imported.",
                    phase = ConnectionPhase.DISCONNECTED,
                    detail = null,
                    sessions = if (replaced) emptyList() else it.sessions,
                    sessionsLoaded = if (replaced) false else it.sessionsLoaded,
                    windows = if (replaced) emptyMap() else it.windows,
                    listError = null,
                )
            }
        }
        return true
    }

    fun selectRoute(preference: RoutePreference) {
        val current = state.value
        if (current.hasConnection || current.stopping) return
        if (preference == RoutePreference.ONION && current.profile?.onion == null) return
        saveRoutePreference(preference)
        change { it.copy(routePreference = preference) }
    }

    private val routePrefs by lazy { context.getSharedPreferences(RoutePrefsName, Context.MODE_PRIVATE) }

    private fun loadRoutePreference(): RoutePreference = try {
        routePrefs.getString(RoutePrefKey, null)?.let { RoutePreference.valueOf(it) } ?: RoutePreference.AUTO
    } catch (error: Exception) {
        if (error is CancellationException) throw error
        RoutePreference.AUTO
    }

    private fun saveRoutePreference(preference: RoutePreference) {
        try {
            routePrefs.edit().putString(RoutePrefKey, preference.name).apply()
        } catch (error: Exception) {
            Log.w(LogTag, "Could not persist the route preference", error)
        }
    }

    fun reportCameraUnavailable() {
        change { it.copy(setupError = "Camera access is needed to scan. You can import the profile file instead.") }
    }

    fun reportServiceError(error: Exception) {
        change { it.copy(setupError = "Start connection: ${error.displayMessage()}") }
    }

    fun connect(): Boolean {
        val start = state.value
        val profile = start.profile ?: return false
        if (start.hasConnection || start.stopping || !start.canSetUp) return start.hasConnection
        val request = ++lifecycle
        change {
            it.copy(
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
                        if (request == lifecycle) change { it.routeStatus(live.route, live.tor) }
                    }
                }
                val status = try {
                    withContext(Dispatchers.IO) {
                        native.setRoutePreference(start.routePreference)
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
            ChatgptAccountState(linked = status.linked)
        }
    }

    fun startChatgptSignIn(target: ChatgptTarget) {
        if (state.value.chatgpt.linked != false) return
        runChatgpt(target, ChatgptOperation.Browser) { native, captured ->
            val key = captured.profile.windowKey()
            val (hostId, registration) = withContext(Dispatchers.IO) {
                storageLock.withLock { store.chatgptHostId() to store.chatgptRegistration(key) }
            }
            val tokens = signInWithChatgpt(
                hostId = hostId,
                returningClient = registration?.clientId,
                expectedSubject = registration?.subject,
                openBrowser = { url ->
                    ensureChatgptTarget(captured)
                    context.startActivity(
                        Intent(Intent.ACTION_VIEW, Uri.parse(url))
                            .addCategory(Intent.CATEGORY_BROWSABLE)
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                },
                onVerifying = { changeChatgpt { it.copy(operation = ChatgptOperation.Validating) } },
            )
            ensureChatgptTarget(captured)
            withContext(Dispatchers.IO) {
                storageLock.withLock {
                    store.saveChatgptRegistration(key, SiwcRegistration(tokens.clientId, tokens.identity.subject))
                }
            }
            ensureChatgptTarget(captured)
            changeChatgpt { it.copy(operation = ChatgptOperation.Linking) }
            val status = withContext(Dispatchers.IO) {
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
            ChatgptAccountState(linked = true, showPlanNotice = registration == null)
        }
    }

    fun unlinkChatgptAccount(target: ChatgptTarget) {
        if (state.value.chatgpt.linked != true) return
        runChatgpt(target, ChatgptOperation.Unlinking) { native, captured ->
            var warning: String? = null
            val status = withContext(Dispatchers.IO) {
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
                storageLock.withLock { store.clearChatgptRegistration(captured.profile.windowKey()) }
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
                    // Native and HTTP errors may include response details. Only
                    // the app's fixed SIWC messages are safe in this view.
                    val detail = if (error is SiwcFailure) error.message else when (operation) {
                        ChatgptOperation.Checking -> "This server does not currently offer ChatGPT account access to this phone."
                        ChatgptOperation.Unlinking -> "Harw could not confirm that the account was unlinked. Check the server’s account status."
                        else -> "Harw could not finish linking ChatGPT. Check your browser and the server’s account status."
                    }
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
                    change { it.copy(sessions = sessions, sessionsLoaded = true, listError = null) }
                }
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                if (generation == gate.generation) {
                    change { it.copy(listError = "Refresh sessions: ${error.displayMessage()}") }
                }
            }
        }
    }

    // ---- Projekte und Seiten (Paket A5) ----

    fun refreshProjects() {
        val generation = gate.generation ?: return
        val native = core ?: return
        if (projectsJob?.isActive == true) return
        projectsJob = scope.launch {
            try {
                val (supported, page) = withContext(Dispatchers.IO) {
                    native.projectsSupported() to native.listProjects(null, null, null)
                }
                if (generation == gate.generation) {
                    change {
                        it.copy(
                            projectsSupported = supported,
                            projects = page.projects,
                            projectsLoaded = true,
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
                    native.listSessionsPage(key, query, false, cursor, ProjectPageSize)
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

    fun submit(sessionId: String, generation: Long, attempt: DraftAttempt, result: (DraftResult) -> Unit): Boolean =
        runAction(sessionId, generation, SessionActionKind.Send, failure = { error ->
            result(DraftResult.Rejected(error.displayMessage(), error is MobileException.Ambiguous))
        }) {
            val receipt = withContext(Dispatchers.IO) { it.submit(sessionId, attempt.text, attempt.head, attempt.id) }
            val outcome = when (receipt.outcome) {
                SubmitOutcome.ACCEPTED -> DraftResult.Accepted
                SubmitOutcome.STALE -> DraftResult.Rejected("The session changed. Review the latest messages before sending again.")
                SubmitOutcome.QUEUE_FULL -> DraftResult.Rejected(receipt.detail ?: "The server's queue is full.")
                SubmitOutcome.DENIED -> DraftResult.Rejected(receipt.detail ?: "The server refused this prompt.")
                SubmitOutcome.BUSY -> DraftResult.Rejected(busyDraftMessage(receipt.detail))
                SubmitOutcome.IN_FLIGHT -> DraftResult.Rejected(receipt.detail ?: "Check the transcript before retrying this prompt.", ambiguous = true)
            }
            result(outcome)
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
        val windows = observers.keys.filterNot { it in detaching }.map { id -> id to (id in opened) }
        if (windows.isEmpty()) return
        val results = withContext(Dispatchers.IO) {
            windows.map { (id, attached) ->
                id to try {
                    Result.success(
                        (if (attached) native.snapshot(id) else native.openSession(id)).let { snap ->
                            snap to runCatching { native.sessionHolder(id) }.getOrNull()
                        },
                    )
                } catch (error: Exception) {
                    Result.failure(error)
                }
            }
        }
        if (request != lifecycle || gate.generation != generation) return
        val updates = state.value.windows.toMutableMap()
        for ((id, result) in results) {
            val snapshot = result.getOrNull()?.first
            val holder = result.getOrNull()?.second
            if (snapshot != null && id !in detaching) opened.add(id)
            if (id !in observers) {
                if (id !in detachJobs) detachSession(id)
                continue
            }
            val previous = updates[id] ?: WindowState()
            updates[id] = if (snapshot != null) {
                previous.copy(snapshot = snapshot, generation = generation.takeIf { snapshot.online }, holder = holder)
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
        change { it.invalidated() }
    }

    private fun beginForgetSetup() {
        unwatchNetwork()
        if (state.value.forgetting) return
        clearSessionActions()
        setupJob?.cancel()
        change { it.forgetSetup() }
        purgeSetup()
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
            val storageFailed = try {
                withContext(Dispatchers.IO) {
                    storageLock.withLock {
                        store.purgeSetup()
                        if (!routePrefs.edit().clear().commit()) error("route preference not cleared")
                    }
                }
                false
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                true
            }
            setupJob?.cancelAndJoin()
            connectJob?.join()
            presenceJob?.cancelAndJoin()
            listLoop?.cancelAndJoin()
            refreshJob?.cancelAndJoin()
            accountJob?.cancelAndJoin()
            commands.coroutineContext[Job]?.children?.toList()?.joinAll()
            val native = coreLock.withLock { core.also { core = null } }
            val disposalFailed = try {
                withContext(Dispatchers.IO) { native?.let { it.clearListener(); it.destroy() } }
                false
            } catch (error: Exception) {
                if (error is CancellationException) throw error
                true
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
                try {
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
        const val LogTag = "HarwConnection"
        const val RoutePrefsName = "harw_connection"
        const val RoutePrefKey = "route_preference"
        const val ChatgptUnlinkWarning = "The server removed its local ChatGPT link. Revocation at OpenAI was not confirmed. Review the agent in ChatGPT Settings."
    }
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
