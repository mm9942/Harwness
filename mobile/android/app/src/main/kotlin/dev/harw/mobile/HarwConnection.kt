package dev.harw.mobile

import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
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
import uniffi.harw_mobile_ffi.ChatgptProviderStatus
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.ConnectionStatus
import uniffi.harw_mobile_ffi.MobileCore
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.RoutePreference
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
        creating = false,
        generation = null,
        activeRoute = null,
        windows = emptyMap(),
        pendingActions = emptySet(),
        chatgpt = ChatgptAccountState(),
        sessionResetEpoch = sessionResetEpoch + 1,
    )

    fun invalidated() = if (phase == ConnectionPhase.REVOKED) this else clearSessions().copy(
        phase = ConnectionPhase.REVOKED,
        detail = "The server stopped authorizing this connection. Check the server policy and connect again.",
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
)

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
    val unavailable: String? = null,
)

internal fun ChatgptProviderStatus.toAccountState(): ChatgptAccountState =
    ChatgptAccountState(linked = linked, unavailable = chatgptUnavailableMessage(availability))

internal fun ChatgptAccountState.canStartSignIn(): Boolean = linked == false && unavailable == null

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
    private var setupJob: Job? = null
    private var accountJob: Job? = null
    private val gate = SessionActionGate()
    private val observers = mutableMapOf<String, Int>()
    private val opened = mutableSetOf<String>()
    private val detachJobs = mutableMapOf<String, Job>()
    private val detaching = mutableSetOf<String>()

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

    fun importProfilePhoto(file: File) {
        val accepted = importProfileDocument("Scan server profile") {
            try {
                decodeProfileQr(readPairingPhoto(file))
            } finally {
                discardPairingPhoto(file)
            }
        }
        if (!accepted) scope.launch(Dispatchers.IO) { discardPairingPhoto(file) }
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
        change { it.copy(setupError = "Android could not open the camera. You can import the profile file.") }
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
        return true
    }

    fun disconnect() {
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

    private fun startPolling(native: MobileCore, request: Long) {
        presenceJob = scope.launch {
            while (isActive && request == lifecycle) {
                val status = withContext(Dispatchers.IO) { native.status() }
                if (request != lifecycle) break
                applyStatus(status)
                if (status.phase in setOf(ConnectionPhase.DISCONNECTED, ConnectionPhase.FAILED, ConnectionPhase.REVOKED)) break
                if (status.phase == ConnectionPhase.CONNECTED) refreshWindows(native, request)
                delay(200)
            }
        }
        listLoop = scope.launch {
            while (isActive && request == lifecycle && state.value.hasConnection) {
                refreshSessions()
                delay(3_000)
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
                    Result.success(if (attached) native.snapshot(id) else native.openSession(id))
                } catch (error: Exception) {
                    Result.failure(error)
                }
            }
        }
        if (request != lifecycle || gate.generation != generation) return
        val updates = state.value.windows.toMutableMap()
        for ((id, result) in results) {
            val snapshot = result.getOrNull()
            if (snapshot != null && id !in detaching) opened.add(id)
            if (id !in observers) {
                if (id !in detachJobs) detachSession(id)
                continue
            }
            val previous = updates[id] ?: WindowState()
            updates[id] = if (snapshot != null) {
                previous.copy(snapshot = snapshot, generation = generation.takeIf { snapshot.online })
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
        }
    }

    private fun invalidate() {
        if (state.value.phase == ConnectionPhase.REVOKED) return
        clearSessionActions()
        change { it.invalidated() }
    }

    private fun beginForgetSetup() {
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
                withContext(Dispatchers.IO) { native?.destroy() }
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
    is MobileException.Transport -> detail
    is MobileException.Ambiguous -> detail
    is MobileException.Internal -> detail
    is MobileException.Disconnected -> "The connection was lost."
    is MobileException.Revoked -> "The server stopped authorizing this connection."
    else -> localizedMessage ?: "The operation failed."
}
