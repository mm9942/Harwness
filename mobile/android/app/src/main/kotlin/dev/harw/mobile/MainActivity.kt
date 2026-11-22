package dev.harw.mobile

import android.Manifest
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.Icon
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.core.content.ContextCompat
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.lifecycleScope
import java.util.UUID
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.SessionCursor

internal open class MainActivity : ComponentActivity() {
    private val window by viewModels<HarwWindowModel>()
    private val connection get() = (application as HarwApplication).connection
    private val approvalAuthorization = ApprovalAuthorization(this)
    private val stepUpAuthorization = StepUpAuthorization(this)
    private var themePref by mutableStateOf("system")
    private val themeListener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == PREF_THEME) themePref = readThemePref(this)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        approvalAuthorization.restoreCredentialState(savedInstanceState?.getBoolean(AwaitingCredentialKey) == true)
        lifecycleScope.launch {
            connection.state
                .map { state ->
                    val reset = state.sessionResetEpoch.takeIf {
                        state.phase == ConnectionPhase.REVOKED || state.forgetRequired ||
                            (state.profile == null && state.sessionResetEpoch > 0L)
                    }
                    Triple(reset, state.setupPending, state.profile)
                }
                .distinctUntilChanged()
                .collect { (epoch, setupPending, profile) ->
                    if (epoch != null && window.observeSessionReset(epoch)) {
                        approvalAuthorization.cancel()
                        intent.removeExtra(InitialSessionKey)
                        intent.removeExtra(WindowProfileKey)
                        intent.removeExtra(HarwConnectionService.ExtraOpenSession)
                        intent.data = null
                    }
                    if (!setupPending) window.useProfile(profile?.windowKey())
                }
        }
        if (savedInstanceState == null) handleLink(intent)
        enableEdgeToEdge()
        themePref = readThemePref(this)
        getSharedPreferences(UI_PREFS, MODE_PRIVATE).registerOnSharedPreferenceChangeListener(themeListener)
        setContent {
            HarwTheme(dark = themeDarkOverride(themePref)) {
                Surface(Modifier.fillMaxSize()) {
                    Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) { Workspace() }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleLink(intent)
    }

    // Deep-Link und Benachrichtigungs-Extra sind unvertrauene Eingabe: nur geprueft navigieren.
    private fun handleLink(intent: Intent?) {
        val link = linkFromIntent(intent) ?: return
        window.openLink(link)
        intent?.removeExtra(HarwConnectionService.ExtraOpenSession)
        intent?.data = null
    }

    override fun onDestroy() {
        approvalAuthorization.cancel()
        stepUpAuthorization.cancel()
        getSharedPreferences(UI_PREFS, MODE_PRIVATE).unregisterOnSharedPreferenceChangeListener(themeListener)
        super.onDestroy()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean(AwaitingCredentialKey, approvalAuthorization.awaitingCredential)
        super.onSaveInstanceState(outState)
    }

    @Composable
    private fun Workspace() {
        val state by connection.state.collectAsState()
        val sessionId = window.sessionId
        val projectKey = window.projectKey
        var settingsRouteName by rememberSaveable { mutableStateOf<String?>(null) }
        val settingsRoute = settingsRouteName?.let { runCatching { SettingsRoute.valueOf(it) }.getOrNull() }
        fun openSettings(route: SettingsRoute?) {
            settingsRouteName = route?.name
        }
        var connectingFromSetup by rememberSaveable { mutableStateOf(false) }
        var firstRunDone by remember { mutableStateOf(readFirstRunDone(this@MainActivity)) }
        fun finishFirstRun() {
            firstRunDone = true
            markFirstRunDone(this@MainActivity)
        }
        var notificationDenied by rememberSaveable { mutableStateOf(false) }
        val profilePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) {
            if (it != null) connection.importProfile(it)
        }
        val enrollmentPicker = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/json")) {
            if (it != null) connection.exportEnrollment(it)
        }
        val selectedProfileKey = remember(state.profile) { state.profile?.windowKey() }
        var scannerOpen by rememberSaveable { mutableStateOf(false) }
        var enrollmentQr by remember { mutableStateOf<android.graphics.Bitmap?>(null) }
        val cameraPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            if (granted) scannerOpen = true else connection.reportCameraUnavailable()
        }
        LaunchedEffect(state.deviceNodeId) {
            if (state.deviceNodeId == null) enrollmentQr = null
        }
        enrollmentQr?.let { EnrollmentQrDialog(it, onClose = { enrollmentQr = null }) }
        if (scannerOpen) {
            QrScanDialog(
                onDismiss = { scannerOpen = false },
                onImport = {
                    scannerOpen = false
                    connection.importScannedProfile(it)
                },
            )
        }
        val notificationPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
            notificationDenied = !it
            startConnection()
        }

        val showSetup = settingsRoute == SettingsRoute.Connection || (
            sessionId == null && state.sessions.isEmpty() && !state.sessionsLoaded && !state.online &&
                state.phase != ConnectionPhase.RECONNECTING
            )
        LaunchedEffect(state.online, connectingFromSetup) {
            if (state.online && connectingFromSetup) {
                openSettings(null)
                connectingFromSetup = false
            }
        }
        DisposableEffect(sessionId, selectedProfileKey) {
            if (sessionId != null) connection.retainSession(sessionId)
            onDispose { if (sessionId != null) connection.releaseSession(sessionId) }
        }
        BackHandler(backStep(settingsRoute != null, sessionId, projectKey) != BackStep.Exit) {
            when (backStep(settingsRoute != null, sessionId, projectKey)) {
                BackStep.CloseSettings -> openSettings(settingsRoute?.let(::settingsParent))
                BackStep.CloseSession -> window.select(null)
                BackStep.CloseProject -> {
                    window.selectProject(null)
                    connection.closeProject()
                }
                BackStep.Exit -> Unit
            }
        }
        // Step-up-Host: immer ueber allen Zweigen, auch im Hub. Nichts davon ueberlebt Prozesstod.
        state.manage.stepUp?.let { stepUp ->
            StepUpDialog(
                stepUp = stepUp,
                receivedAt = state.manage.stepUpReceivedAt,
                authorizing = stepUpAuthorization.busy || state.manage.running == STEPUP_CONFIRM_RUNNING,
                error = state.manage.stepUpError,
                onConfirm = {
                    stepUpAuthorization.confirm(stepUp) { error ->
                        when (error) {
                            null -> connection.confirmStepUp(stepUp)
                            STEPUP_CANCELED_TEXT -> Unit
                            else -> connection.noteStepUpError(stepUp, error)
                        }
                    }
                },
                onCancel = { reason ->
                    stepUpAuthorization.cancel()
                    connection.cancelStepUp(stepUp, reason)
                },
            )
        }
        val closedStepUpError = state.manage.stepUpError
        if (state.manage.stepUp == null && closedStepUpError != null) {
            AlertDialog(
                onDismissRequest = { connection.clearStepUpError() },
                title = { Text("Änderung bestätigen") },
                text = { Text(closedStepUpError) },
                confirmButton = { TextButton(onClick = { connection.clearStepUpError() }) { Text("OK") } },
            )
        }
        if (settingsRoute == SettingsRoute.Hub) {
            SettingsHub(
                state = state,
                connection = connection,
                onBack = { openSettings(null) },
                onOpen = ::openSettings,
            )
        } else if (settingsRoute == SettingsRoute.Models) {
            CloudModelsScreen(state, connection, sessionId, onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.Providers) {
            val providerStore = remember {
                uniffi.harw_mobile_ffi.ProviderStore(filesDir.path, AndroidPlatformKeystore(applicationContext))
            }
            ProvidersScreen(
                providerStore,
                onBack = { openSettings(SettingsRoute.Hub) },
                onOpenConnection = { openSettings(SettingsRoute.Connection) },
                onOpenLocalModels = { openSettings(SettingsRoute.LocalModels) },
            )
        } else if (settingsRoute == SettingsRoute.Agents) {
            AgentsScreen(state, sessionId ?: "", onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.Jobs) {
            JobsScreen(state, sessionId ?: "", onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.LocalModels) {
            val modelManager = remember { ModelManager(ModelStore(applicationContext)) }
            ModelsScreen(modelManager, onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.Files) {
            FilesScreen(onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.Termux) {
            TermuxScreen(onBack = { openSettings(SettingsRoute.Hub) })
        } else if (settingsRoute == SettingsRoute.Commands) {
            CommandsScreen(
                state = state,
                connection = connection,
                sessionId = null,
                onBack = { openSettings(SettingsRoute.Hub) },
                onOpenModels = { openSettings(SettingsRoute.Models) },
            )
        } else if (showSetup && settingsRoute == null && state.profile == null && !firstRunDone) {
            FirstRunScreen(
                onConnect = { finishFirstRun(); openSettings(SettingsRoute.Connection) },
                onProviders = { finishFirstRun(); openSettings(SettingsRoute.Providers) },
                onLocalModels = { finishFirstRun(); openSettings(SettingsRoute.LocalModels) },
                onSkip = { finishFirstRun() },
            )
        } else if (showSetup) {
            ConnectionScreen(
                state = state,
                notificationDenied = notificationDenied,
                canGoBack = settingsRoute == SettingsRoute.Connection &&
                    (state.online || state.sessions.isNotEmpty() || sessionId != null),
                onBack = { openSettings(SettingsRoute.Hub) },
                onImport = {
                    profilePicker.launch(arrayOf("application/json", "text/plain", "application/octet-stream"))
                },
                onExport = { enrollmentPicker.launch("harw-device-enrollment.json") },
                onShowEnrollmentQr = { connection.showEnrollmentQr { enrollmentQr = it } },
                scanning = scannerOpen,
                onScanProfile = {
                    if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
                        scannerOpen = true
                    } else {
                        cameraPermission.launch(Manifest.permission.CAMERA)
                    }
                },
                onConnect = {
                    connectingFromSetup = true
                    if (Build.VERSION.SDK_INT >= 33 && !notificationDenied &&
                        ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
                    ) {
                        notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
                    } else {
                        startConnection()
                    }
                },
                onConnectLocal = {
                    connectingFromSetup = true
                    connection.connectLocal()
                },
                onDisconnect = connection::disconnect,
                onSelectRoute = { connection.selectRoute(it) },
                onForgetSetup = connection::forgetSetup,
                onRetryCleanup = connection::retryCleanup,
                onCheckChatgpt = connection::refreshChatgptAccount,
                onChatgptSignIn = connection::startChatgptSignIn,
                onChatgptUnlink = connection::unlinkChatgptAccount,
                onChatgptCancel = connection::cancelChatgptBrowser,
                onDismissPlanNotice = connection::dismissChatgptPlanNotice,
                onManageChatgpt = ::openChatgptUsage,
            )
        } else if (projectKey == null && sessionId == null) {
            LaunchedEffect(state.online, state.generation) { if (state.online) connection.refreshProjects() }
            Column(Modifier.fillMaxSize()) {
                HarwHeader(title = "Projekte") {
                    ConnectionBadge(state)
                    IconButton(onClick = { openSettings(SettingsRoute.Hub) }) {
                        Icon(Icons.Outlined.Settings, "Einstellungen")
                    }
                }
                ProjectsScreen(
                    projects = state.projects,
                    loaded = state.projectsLoaded,
                    online = state.online,
                    canCreate = state.online && state.canCreate && !state.creating,
                    error = state.projectsError,
                    onSelect = window::selectProject,
                    onNewSession = { key -> connection.createInProject(key) { id -> window.openLink(DeepLink(key, id)) } },
                    onRefresh = connection::refreshProjects,
                )
            }
        } else {
            val key = projectKey
            if (key != null) {
                LaunchedEffect(key, state.online, state.generation) { if (state.online) connection.openProject(key) }
            }
            // E-10: eine globale Projektansicht; gehoert sie zu einem anderen Fenster, Rueckfall auf SessionDirectory.
            val view = if (key == null) null else state.projectView?.takeIf { it.projectKey == key }
            val actions = if (key == null) null else remember(key) {
                connection.projectActions { id -> window.openLink(DeepLink(key, id)) }
            }
            WorkspaceScreen(
                state = state,
                window = window,
                connection = connection,
                authorizingApproval = approvalAuthorization.busy,
                onDecide = ::authenticateDecision,
                onOpenSettings = { openSettings(SettingsRoute.Hub) },
                onNewWindow = ::openWindow,
                projectView = view,
                projectActions = if (view != null) actions else null,
            )
        }
    }

    private fun openChatgptUsage() {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://chatgpt.com/settings/usage")))
        } catch (_: Exception) {
            Toast.makeText(this, "Android could not open ChatGPT usage settings.", Toast.LENGTH_SHORT).show()
        }
    }

    private fun authenticateDecision(sessionId: String, generation: Long, approvalId: String, decision: ApprovalDecision) {
        approvalAuthorization.confirm(decision.displayName()) { error ->
            if (error != null) {
                connection.approvalFailure(sessionId, generation, error)
            } else if (window.sessionId == sessionId && !isFinishing &&
                !connection.decide(sessionId, generation, approvalId, decision)
            ) {
                connection.approvalFailure(sessionId, generation, "This approval is no longer available.")
            }
        }
    }

    private fun startConnection() {
        try {
            HarwConnectionService.startFrom(this)
        } catch (error: Exception) {
            connection.reportServiceError(error)
        }
    }

    private fun openWindow(sessionId: String) {
        startActivity(
            Intent(this, SessionWindowActivity::class.java)
                .putExtra(InitialSessionKey, sessionId)
                .putExtra(WindowProfileKey, connection.state.value.profile?.windowKey())
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK or Intent.FLAG_ACTIVITY_LAUNCH_ADJACENT),
        )
    }
}

internal class SessionWindowActivity : MainActivity()

internal data class SessionDraft(
    val text: String = "",
    val attempt: DraftAttempt? = null,
    val error: String? = null,
    val uncertain: Boolean = false,
    val sending: Boolean = false,
)

internal class HarwWindowModel(private val savedState: SavedStateHandle) : ViewModel() {
    var sessionId by mutableStateOf(
        if (savedState.contains(SelectedSessionKey)) savedState.get<String>(SelectedSessionKey)
        else savedState.get<String>(InitialSessionKey),
    )
        private set
    var projectKey by mutableStateOf(savedState.get<String>(SelectedProjectKey))
        private set
    private var drafts by mutableStateOf<Map<String, SessionDraft>>(emptyMap())
    private var sessionResetEpoch = 0L

    fun useProfile(key: String?) {
        val prior = savedState.get<String>(WindowProfileKey)
        if (prior != null && prior != key) {
            drafts = emptyMap()
            select(null)
            selectProject(null)
        }
        savedState[WindowProfileKey] = key
    }

    fun observeSessionReset(epoch: Long): Boolean {
        if (epoch == sessionResetEpoch) return false
        sessionResetEpoch = epoch
        drafts = emptyMap()
        savedState[WindowProfileKey] = null
        select(null)
        selectProject(null)
        return true
    }

    fun select(id: String?) {
        sessionId = id
        savedState[SelectedSessionKey] = id
    }

    fun selectProject(key: String?) {
        projectKey = key
        savedState[SelectedProjectKey] = key
    }

    /** Oeffnet ein geprueftes Ziel: Projekt zuerst, dann die Session darin. */
    fun openLink(link: DeepLink) {
        if (link.projectKey != null) selectProject(link.projectKey)
        select(link.sessionId)
    }

    fun draft(id: String): SessionDraft = drafts[id] ?: SessionDraft()

    fun edit(id: String, text: String) {
        val previous = draft(id)
        if (previous.uncertain || previous.sending) return
        drafts = drafts + (id to SessionDraft(text))
    }

    fun discardUncertainDraft(id: String) {
        val draft = draft(id)
        if (draft.uncertain && !draft.sending) drafts = drafts - id
    }

    fun submit(connection: HarwConnection, id: String, generation: Long, head: SessionCursor) {
        val current = draft(id)
        if (current.text.isBlank() || current.sending || current.uncertain) return
        val attempt = current.attempt ?: DraftAttempt(current.text, head, UUID.randomUUID().toString())
        drafts = drafts + (id to current.copy(attempt = attempt, sending = true, error = null))
        val admitted = connection.submit(id, generation, attempt) { result ->
            val latest = draft(id)
            if (latest.attempt?.id != attempt.id) return@submit
            drafts = when (result) {
                DraftResult.Accepted -> drafts - id
                is DraftResult.Rejected -> {
                    val uncertain = latest.uncertain || result.ambiguous
                    drafts + (id to latest.copy(
                        error = result.error,
                        uncertain = uncertain,
                        attempt = attempt.takeIf { uncertain },
                        sending = false,
                    ))
                }
            }
        }
        if (!admitted) drafts = drafts + (id to current)
    }
}

private const val InitialSessionKey = "initial_session"
private const val SelectedSessionKey = "selected_session"
private const val SelectedProjectKey = "selected_project"
private const val WindowProfileKey = "window_profile"
private const val AwaitingCredentialKey = "awaiting_approval_credential"

// ---- Routen Projekte -> Sessions -> Unterhaltung (Paket A6) ----

/** Was die Zurueck-Taste als Naechstes schliesst; Reihenfolge: Einstellungen, Unterhaltung, Projekt. */
internal enum class BackStep { CloseSettings, CloseSession, CloseProject, Exit }

internal fun backStep(showSettings: Boolean, sessionId: String?, projectKey: String?): BackStep = when {
    showSettings -> BackStep.CloseSettings
    sessionId != null -> BackStep.CloseSession
    projectKey != null -> BackStep.CloseProject
    else -> BackStep.Exit
}

/** Geprueftes Navigationsziel; mindestens eines der Felder ist gesetzt. */
internal data class DeepLink(val projectKey: String?, val sessionId: String?)

private val SessionIdPattern = Regex("^[A-Za-z0-9_-]{1,64}$")
private val ProjectKeyPattern = Regex("^[A-Za-z0-9._:-]{1,128}$")

/**
 * Prueft ein Ziel `harw://open?project=<key>&session=<id>` aus rohen Teilen.
 * Leeres `project` bedeutet "Ohne Projekt". Jede ungueltige Angabe verwirft das ganze Ziel.
 */
internal fun parseDeepLink(scheme: String?, host: String?, project: String?, session: String?): DeepLink? {
    if (scheme != "harw" || host != "open") return null
    val projectKey = when {
        project == null -> null
        project.isEmpty() -> NoProjectKey
        ProjectKeyPattern.matches(project) -> project
        else -> return null
    }
    val sessionId = when {
        session == null -> null
        SessionIdPattern.matches(session) -> session
        else -> return null
    }
    if (projectKey == null && sessionId == null) return null
    return DeepLink(projectKey, sessionId)
}

/** Nur die Session-Id aus dem Benachrichtigungs-Extra (A9), geprueft. */
internal fun parseOpenSessionExtra(sessionId: String?): DeepLink? =
    sessionId?.takeIf { SessionIdPattern.matches(it) }?.let { DeepLink(null, it) }

internal fun linkFromIntent(intent: Intent?): DeepLink? {
    if (intent == null) return null
    val uri = intent.data
    if (uri != null) {
        return try {
            parseDeepLink(uri.scheme, uri.host, uri.getQueryParameter("project"), uri.getQueryParameter("session"))
        } catch (_: Exception) {
            null
        }
    }
    return parseOpenSessionExtra(intent.getStringExtra(HarwConnectionService.ExtraOpenSession))
}
