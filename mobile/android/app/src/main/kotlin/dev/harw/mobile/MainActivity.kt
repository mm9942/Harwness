package dev.harw.mobile

import android.Manifest
import android.content.Intent
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
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
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
                    }
                    if (!setupPending) window.useProfile(profile?.windowKey())
                }
        }
        enableEdgeToEdge()
        setContent {
            HarwTheme {
                Surface(Modifier.fillMaxSize()) {
                    Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) { Workspace() }
                }
            }
        }
    }

    override fun onDestroy() {
        approvalAuthorization.cancel()
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
        var showSettings by rememberSaveable { mutableStateOf(false) }
        var connectingFromSetup by rememberSaveable { mutableStateOf(false) }
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

        val showSetup = showSettings || (
            sessionId == null && state.sessions.isEmpty() && !state.sessionsLoaded && !state.online &&
                state.phase != ConnectionPhase.RECONNECTING
            )
        LaunchedEffect(state.online, connectingFromSetup) {
            if (state.online && connectingFromSetup) {
                showSettings = false
                connectingFromSetup = false
            }
        }
        DisposableEffect(sessionId, selectedProfileKey) {
            if (sessionId != null) connection.retainSession(sessionId)
            onDispose { if (sessionId != null) connection.releaseSession(sessionId) }
        }
        BackHandler(showSettings || sessionId != null) {
            if (showSettings) showSettings = false
            else window.select(null)
        }
        if (showSetup) {
            ConnectionScreen(
                state = state,
                notificationDenied = notificationDenied,
                canGoBack = showSettings && (state.online || state.sessions.isNotEmpty() || sessionId != null),
                onBack = { showSettings = false },
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
        } else {
            WorkspaceScreen(
                state = state,
                window = window,
                connection = connection,
                authorizingApproval = approvalAuthorization.busy,
                onDecide = ::authenticateDecision,
                onOpenSettings = { showSettings = true },
                onNewWindow = ::openWindow,
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
    private var drafts by mutableStateOf<Map<String, SessionDraft>>(emptyMap())
    private var sessionResetEpoch = 0L

    fun useProfile(key: String?) {
        val prior = savedState.get<String>(WindowProfileKey)
        if (prior != null && prior != key) {
            drafts = emptyMap()
            select(null)
        }
        savedState[WindowProfileKey] = key
    }

    fun observeSessionReset(epoch: Long): Boolean {
        if (epoch == sessionResetEpoch) return false
        sessionResetEpoch = epoch
        drafts = emptyMap()
        savedState[WindowProfileKey] = null
        select(null)
        return true
    }

    fun select(id: String?) {
        sessionId = id
        savedState[SelectedSessionKey] = id
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
private const val WindowProfileKey = "window_profile"
private const val AwaitingCredentialKey = "awaiting_approval_credential"
