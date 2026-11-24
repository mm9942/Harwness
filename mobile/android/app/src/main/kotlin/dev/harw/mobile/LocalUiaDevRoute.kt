package dev.harw.mobile

import android.app.Activity
import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.graphics.Bitmap
import android.system.Os
import android.system.OsConstants
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.CancellationException

internal object LocalUiaDevCapability {
    private val lib by lazy { System.loadLibrary("harw_mobile_ffi") }

    fun current(app: HarwApplication): LocalUiaProfile? {
        if (!BuildConfig.LOCAL_UIA) return null
        return try {
            lib
            LocalUiaProfile.parse(activeProfileNative(app))
        } catch (_: LinkageError) {
            null
        } catch (_: Exception) {
            null
        }
    }

    private external fun activeProfileNative(app: Application): String?
}

internal class PinDigits {
    private val bytes = ByteArray(16)
    var count by mutableIntStateOf(0)
        private set
    fun add(digit: Int) { if (count < bytes.size && digit in 0..9) bytes[count++] = (0x30 + digit).toByte() }
    fun erase() { if (count > 0) bytes[--count] = 0 }
    fun clear() { bytes.fill(0); count = 0 }
    fun take(): ByteArray = bytes.copyOf(count).also { clear() }
}

internal sealed interface DevUiaView {
    data object Opening : DevUiaView
    data class Gate(
        val new: Boolean,
        val prompt: Boolean = false,
        val info: String? = null,
        val optIn: Boolean = false,
        val biometric: Boolean = false,
        val recovering: Boolean = false,
        val disableBiometric: Boolean = false,
        val biometricManagement: VaultBiometricAvailability = VaultBiometricAvailability.Unavailable,
    ) : DevUiaView
    data object OfferBiometric : DevUiaView
    class Parked(val stopExact: (() -> Unit)?) : DevUiaView
    data object ParkedCheckUnavailable : DevUiaView
    class Ready(val chat: UiaRoomChatConnection, val biometricNotice: String? = null) : DevUiaView
    class Stop(
        val text: String,
        val restartableScope: VaultPinScope? = null,
        val diagnostic: VaultPinEnrollmentStage? = null,
    ) : DevUiaView
}

private data class DevUiaBinding(
    val native: AndroidUiaRoomPinNative,
    val pin: VaultPinController,
    val biometric: AndroidVaultBiometricSlot,
) {
    fun close() {
        try { biometric.close() } finally {
            try { pin.relock() } finally { native.close() }
        }
    }
}

/** Created only from a verified on-device Harw runtime profile; never survives activity/back. */
internal class LocalUiaDevSession(
    private val app: HarwApplication,
    val profile: LocalUiaProfile,
) {
    private val worker = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val guard = Any()
    private val fence = LocalUiaRouteFence()
    private val profileMap = LocalUiaDevProfiles(app.noBackupFilesDir.toPath()) { path ->
        val fd = Os.open(path.toString(), OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW or OsConstants.O_CLOEXEC, 0)
        try {
            check(OsConstants.S_ISDIR(Os.fstat(fd).st_mode))
            Os.fsync(fd)
        } finally { Os.close(fd) }
    }
    private var binding: DevUiaBinding? = null
    private var registered: VaultPinScope? = null
    private var nativePromptVisible = false
    @Volatile private var biometricAvailability = VaultBiometricAvailability.Unavailable
    @Volatile private var biometricSupported = false

    private val biometricEnabled: Boolean
        get() = biometricSupported && biometricAvailability == VaultBiometricAvailability.Enrolled

    private fun biometricManagement(): VaultBiometricAvailability =
        if (!biometricSupported && biometricAvailability == VaultBiometricAvailability.Eligible) {
            VaultBiometricAvailability.Unavailable
        } else biometricAvailability
    private val projection = MutableStateFlow<DevUiaView>(DevUiaView.Opening)
    val view = projection.asStateFlow()
    val pin = PinDigits()
    val confirmation = PinDigits()

    init {
        worker.launch {
            var startupStage = VaultPinEnrollmentStage.Profile
            try {
                requireCurrent()
                val existing = profileMap.lookup(profile)
                startupStage = VaultPinEnrollmentStage.Claim
                val disk = existing?.let { AndroidVaultPinStateStore.reopenAssistantEnrollment(app, it) }
                val registration = disk?.read()?.registration
                val resumeGate = if (disk != null &&
                    (registration === VaultPinRegistration.Provisioning || registration is VaultPinRegistration.Recovering)
                ) {
                    val blocked = disk.enrollmentResumeBlocker()
                    if (blocked == null) DevUiaView.Gate(new = false, recovering = true)
                    else enrollmentStop(blocked, disk.scope.takeIf { mayRestartUncommitted(disk) })
                } else null
                val canUseBiometric = AndroidVaultBiometricSlot.supported(app)
                val slotState = if (registration is VaultPinRegistration.Ready && disk != null) {
                    runCatching { disk.uiaBiometricAvailability() }.getOrDefault(VaultBiometricAvailability.Unavailable)
                } else VaultBiometricAvailability.Unavailable
                synchronized(guard) {
                    if (!fence.active()) return@synchronized
                    registered = existing
                    biometricSupported = canUseBiometric
                    biometricAvailability = slotState
                    projection.value = when (registration) {
                        null, VaultPinRegistration.Fresh -> DevUiaView.Gate(new = true)
                        is VaultPinRegistration.Ready -> DevUiaView.Opening
                        VaultPinRegistration.Provisioning, is VaultPinRegistration.Recovering ->
                            resumeGate ?: enrollmentStop(VaultPinEnrollmentStage.Claim)
                    }
                }
                if (registration is VaultPinRegistration.Ready && disk != null) {
                    AndroidUiaRoomPinNative.forSelectedStore(app, disk).use { probe ->
                        if (!interceptParked(probe, disk.scope)) publishFreshPinGate()
                    }
                }
            } catch (e: CancellationException) { throw e }
            catch (_: Exception) { stop(enrollmentStop(startupStage)) }
        }
    }

    fun prompt() = synchronized(guard) {
        val gate = projection.value as? DevUiaView.Gate
        if (fence.active() && gate != null) projection.value = gate.copy(prompt = true)
    }

    fun cancelPrompt() = synchronized(guard) {
        clearDigits()
        val gate = projection.value as? DevUiaView.Gate
        if (fence.active() && gate != null) {
            projection.value = if (gate.new || gate.recovering) gate.copy(prompt = false, info = null)
                else pinGate()
        }
    }

    fun authenticate(activity: Activity?) {
        val next = synchronized(guard) {
            val gate = projection.value as? DevUiaView.Gate ?: return
            if (!fence.active() || !gate.prompt || pin.count !in 8..16 ||
                (gate.new && confirmation.count !in 8..16)
            ) return
            val first = pin.take()
            val second = confirmation.take()
            projection.value = DevUiaView.Opening
            Triple(gate, first, second)
        }
        val authentication = worker.launch {
            val (gate, entered, confirmed) = next
            val enroll = gate.new
            val resume = gate.recovering
            try {
                requireCurrent()
                // Do not create any journal or hardware key for mismatched PINs.
                if (enroll && !java.security.MessageDigest.isEqual(entered, confirmed)) {
                    showGate(true, "Die PINs stimmen nicht überein.")
                    return@launch
                }
                val current = getBinding(enroll) ?: return@launch
                val scope = synchronized(guard) { registered }
                if (!enroll && !resume) {
                    checkNotNull(scope)
                    if (interceptParked(current.native, scope)) return@launch
                }
                var bioSetupFailed = false
                var bioShutdown: VaultBiometricAvailability? = null
                val action = when {
                    enroll -> current.pin.create(entered, confirmed)
                    resume -> current.pin.resumeEnrollment(entered)
                    gate.disableBiometric -> current.pin.unlockAndDisableBiometric(entered, current.biometric) {
                        bioShutdown = it
                    }
                    gate.optIn -> {
                        // Prove the PIN before any permanent optional claim. Do not keep the
                        // room open under a system prompt and do not regenerate staged material.
                        val check = current.pin.unlock(entered.copyOf())
                        if (check != VaultPinAction.Unlocked) {
                            when (check) {
                                VaultPinAction.Rejected -> showGate(false, "PIN nicht akzeptiert.", optIn = true)
                                is VaultPinAction.Delayed -> showGate(false, delayInfo(check), optIn = true)
                                is VaultPinAction.RecoveryRequired -> stop(RECOVERY)
                                else -> showGate(false, "Biometrie nicht aktiviert. Nutze deine Harw-PIN.")
                            }
                            return@launch
                        }
                        current.pin.relock()
                        requireCurrent()
                        val grant = try {
                            activity?.let {
                                requestBioGrant(
                                    it, current.biometric::prepareOptIn, current.biometric::showOptIn, current.biometric::cancel,
                                    current.biometric::finishExplicitOptInCancellation,
                                )
                            }
                        } catch (e: CancellationException) {
                            throw e
                        } catch (_: VaultBiometricException) {
                            null
                        }
                        if (grant == null) {
                            biometricAvailability = current.biometric.availability()
                            showGate(false, biometricSetupResult())
                            return@launch
                        }
                        try {
                            requireCurrent()
                            if (scope != null && interceptParked(current.native, scope)) return@launch
                            current.pin.unlockAndEnableBiometric(entered, grant).also {
                                biometricAvailability = current.biometric.availability()
                                bioSetupFailed = it == VaultPinAction.Unlocked &&
                                    grant.state != VaultBiometricEnrollmentState.Enabled
                            }
                        } finally { grant.close() }
                    }
                    else -> current.pin.unlock(entered)
                }
                requireCurrent()
                if (bioSetupFailed) {
                    current.pin.relock()
                    showGate(false, biometricSetupResult())
                    return@launch
                }
                if (action == VaultPinAction.Unlocked) {
                    biometricAvailability = current.biometric.availability()
                    if ((enroll || resume) && biometricManagement() == VaultBiometricAvailability.Eligible) {
                        synchronized(guard) {
                            if (fence.active()) projection.value = DevUiaView.OfferBiometric
                        }
                        return@launch
                    }
                    val notice = when (bioShutdown) {
                        VaultBiometricAvailability.Revoked -> "Biometrie deaktiviert. Deine Harw-PIN bleibt unverändert."
                        VaultBiometricAvailability.RevocationPending ->
                            "Biometrie ist gesperrt. Das Löschen des Geräteschlüssels konnte nicht bestätigt werden."
                        null -> null
                        else -> "Die Deaktivierung konnte nicht bestätigt werden. Deine Harw-PIN bleibt unverändert."
                    }
                    publishAuthenticated(current, notice)
                } else when (action) {
                    VaultPinAction.Rejected -> showGate(false, "PIN nicht akzeptiert.", recovering = resume, disable = gate.disableBiometric)
                    is VaultPinAction.Delayed -> showGate(false, delayInfo(action), recovering = resume, disable = gate.disableBiometric)
                    VaultPinAction.ConfirmationMismatch -> showGate(true, "Die PINs stimmen nicht überein.")
                    VaultPinAction.InvalidPin -> showGate(enroll, "Die PIN muss 8 bis 16 Ziffern haben.", recovering = resume)
                    is VaultPinAction.EnrollmentFailed -> stopEnrollment(action.stage)
                    is VaultPinAction.RecoveryRequired -> if (enroll) stopEnrollment(VaultPinEnrollmentStage.Claim) else stop(RECOVERY)
                    else -> if (enroll || resume) stopEnrollment(VaultPinEnrollmentStage.NativeBridge)
                        else stop("Der private Assistent ist gerade nicht verfügbar. Öffne ihn erneut.")
                }
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                if (enroll || resume) stopEnrollment(VaultPinEnrollmentStage.Profile) else stop(RECOVERY)
            } finally {
                entered.fill(0)
                confirmed.fill(0)
            }
        }
        // A scheduled worker can be cancelled before entering its finally.
        authentication.invokeOnCompletion {
            next.second.fill(0)
            next.third.fill(0)
        }
    }

    /** Never preserve an unlocked route across pause; only its own locked system prompt may overlap. */
    fun retainOnlyLockedBiometricPrompt(): Boolean = synchronized(guard) {
        nativePromptVisible && fence.active() && projection.value == DevUiaView.Opening
    }

    fun chooseBiometric(enable: Boolean) {
        val owner = synchronized(guard) {
            if (!fence.active() || projection.value != DevUiaView.OfferBiometric) return
            if (enable) projection.value = DevUiaView.Opening
            binding
        } ?: return
        if (enable) {
            owner.pin.relock()
            synchronized(guard) {
                if (fence.active()) projection.value = pinGate(
                    prompt = true, info = "Bestätige deine Harw-PIN für die optionale Biometrie.", optIn = true,
                )
            }
        } else if (allowsLocalAction()) publishAuthenticated(owner)
    }

    fun changeBiometric(enable: Boolean) = synchronized(guard) {
        val gate = projection.value as? DevUiaView.Gate ?: return@synchronized
        if (!fence.active() || gate.new || gate.recovering || gate.prompt) return@synchronized
        val available = biometricManagement()
        if (enable && available != VaultBiometricAvailability.Eligible) return@synchronized
        if (!enable && available !in setOf(
                VaultBiometricAvailability.Enrolled, VaultBiometricAvailability.Interrupted, VaultBiometricAvailability.RevocationPending,
            )
        ) return@synchronized
        clearDigits()
        projection.value = pinGate(
            prompt = true, optIn = enable, disable = !enable,
            info = if (enable) "Bestätige deine Harw-PIN für die optionale Biometrie."
                else "Mit deiner Harw-PIN deaktivierst du Biometrie endgültig für dieses lokale Profil.",
        )
    }

    fun biometricUnlock(activity: Activity) {
        val allowed = synchronized(guard) {
            val gate = projection.value as? DevUiaView.Gate
            if (!fence.active() || gate?.biometric != true || gate.optIn || gate.disableBiometric) false
            else {
                clearDigits()
                projection.value = DevUiaView.Opening
                true
            }
        }
        if (!allowed) return
        worker.launch {
            try {
                requireCurrent()
                val owner = getBinding(enroll = false) ?: return@launch
                val scope = synchronized(guard) { registered } ?: return@launch
                if (interceptParked(owner.native, scope)) return@launch
                val grant = requestBioGrant(
                    activity, owner.biometric::prepareUnlock, owner.biometric::showUnlock, owner.biometric::cancel,
                )
                if (grant != null) {
                    try {
                        requireCurrent()
                        if (interceptParked(owner.native, scope)) return@launch
                        if (owner.pin.unlockWithBiometric(grant) == VaultPinAction.Unlocked) {
                            requireCurrent()
                            publishAuthenticated(owner)
                            return@launch
                        }
                    } finally { grant.close() }
                }
                showGate(false, "Biometrie nicht verfügbar. Nutze deine Harw-PIN.")
            } catch (_: Exception) {
                showGate(false, "Biometrie nicht verfügbar. Nutze deine Harw-PIN.")
            }
        }
    }

    private fun publishAuthenticated(owner: DevUiaBinding, biometricNotice: String? = null) {
        val room = (owner.pin.session() as? UiaRoomChatSession)?.chat()
        synchronized(guard) {
            if (fence.admit { runCatching { owner.close() } }) {
                projection.value = if (room != null) DevUiaView.Ready(room, biometricNotice)
                else enrollmentStop(VaultPinEnrollmentStage.AssistantHandoff)
            }
        }
        if (room == null) runCatching { owner.close() }
    }

    private suspend fun <T : AutoCloseable> requestBioGrant(
        activity: Activity,
        prepare: () -> VaultBiometricPromptTicket,
        show: (Activity, VaultBiometricPromptTicket, (Result<T>) -> Unit) -> Unit,
        cancel: (VaultBiometricPromptTicket) -> Unit,
        finishExplicitCancellation: ((VaultBiometricPromptTicket) -> Unit)? = null,
    ): T? {
        var ticket: VaultBiometricPromptTicket? = null
        var transferred = false
        try {
            val prepared = prepare()
            ticket = prepared
            val grant = withTimeoutOrNull(45_000) {
                withContext(Dispatchers.Main.immediate) {
                    if (activity.isFinishing || activity.isDestroyed ||
                        !synchronized(guard) {
                            (fence.active() && projection.value == DevUiaView.Opening).also { nativePromptVisible = it }
                        }
                    ) return@withContext null
                    val life = (activity as? LifecycleOwner)?.lifecycle ?: return@withContext null
                    val approved = suspendCancellableCoroutine<T?> { waiting ->
                        waiting.invokeOnCancellation { cancel(prepared) }
                        show(activity, prepared) { result ->
                            val item = result.getOrNull()
                            waiting.resume(item) { _, lost, _ -> lost?.close() }
                        }
                    } ?: return@withContext null
                    // Do not decrypt or restore a chat under an overlay or in background.
                    try {
                        life.currentStateFlow.first { it == Lifecycle.State.RESUMED || it == Lifecycle.State.DESTROYED }
                        if (life.currentState != Lifecycle.State.RESUMED || !fence.active()) {
                            approved.close()
                            null
                        } else approved
                    } catch (failure: Exception) {
                        approved.close()
                        throw failure
                    }
                }
            }
            transferred = grant != null
            return grant
        } finally {
            synchronized(guard) { nativePromptVisible = false }
            if (!transferred) ticket?.let {
                cancel(it)
                finishExplicitCancellation?.invoke(it)
            }
        }
    }

    fun requestReauth() = lockToPinGate(prompt = true)
    fun manageBiometric() = lockToPinGate(prompt = false)

    private fun lockToPinGate(prompt: Boolean) {
        val retired = synchronized(guard) {
            if (!fence.active() || projection.value !is DevUiaView.Ready) return
            clearDigits()
            projection.value = pinGate(prompt = prompt)
            binding.also { binding = null }
        }
        runCatching { retired?.close() }
    }

    /** Scope-bound Library is discovered only after this route has a real native PIN lease. */
    suspend fun privateLibrary(): Pair<AndroidPrivateLibraryPort, PrivateLibrarySetup>? {
        val port = withContext(Dispatchers.IO) {
            if (!allowsLocalAction()) return@withContext null
            val authenticated = synchronized(guard) {
                binding?.pin?.session() as? PrivateLibraryAuthenticatedSession
            }
            authenticated?.library()
        } ?: return null
        val status = port.setupStatus()
        return if (status == PrivateLibrarySetup.Unavailable || !port.active.value) null else port to status
    }

    /** Read only a user-opened occurrence from the current authenticated room. */
    suspend fun previewPrivateRoomPhoto(
        room: UiaRoomChatConnection, expected: UiaRoomPhotoPreviewRequest,
    ): Bitmap? = withContext(Dispatchers.IO) {
        fun current() = (projection.value as? DevUiaView.Ready)?.chat === room &&
            expected.isFor(room.states.value) && allowsLocalAction()
        if (!current()) return@withContext null
        val port = synchronized(guard) {
            if ((projection.value as? DevUiaView.Ready)?.chat !== room) null
            else (binding?.pin?.session() as? PrivateLibraryAuthenticatedSession)?.library()
        } ?: return@withContext null
        if (!current() || !port.active.value) return@withContext null
        val bitmap = port.previewRoomPhoto(expected) ?: return@withContext null
        if (!current() || !port.active.value) {
            bitmap.recycle()
            return@withContext null
        }
        bitmap
    }

    fun manualCloudWork(): ManualCloudWorkPort? = synchronized(guard) {
        (binding?.pin?.session() as? UiaRoomChatSession)?.cloudWork(profile.key)
    }

    fun manualMemory(): UiaManualMemoryPort? = synchronized(guard) {
        (binding?.pin?.session() as? UiaRoomChatSession)?.memory()
    }

    fun allowsLocalAction(): Boolean {
        val current = fence.active() && LocalUiaDevCapability.current(app) == profile
        if (!current) stop("Das lokale Profil hat sich geändert. Der Assistent wurde gesperrt.")
        return current
    }

    private fun requireCurrent() {
        check(fence.active() && LocalUiaDevCapability.current(app) == profile)
    }

    private fun getBinding(enroll: Boolean): DevUiaBinding? {
        synchronized(guard) { if (!fence.active()) return null; binding?.let { return it } }
        val existing = synchronized(guard) { registered }
        val disk = if (existing != null) AndroidVaultPinStateStore.open(app, existing) else {
            check(enroll)
            lateinit var allocated: VaultPinDiskStateStore
            val scope = profileMap.allocate(profile) {
                AndroidVaultPinStateStore.createForNewEnrollment(app).also { allocated = it }.scope
            }
            check(scope == allocated.scope)
            allocated
        }
        val native = AndroidUiaRoomPinNative.forSelectedStore(app, disk)
        val devices = object : UiaRoomPinDevices {
            override fun provisionNew(scope: VaultPinScope) = AndroidVaultPinMac.provisionNew(scope.vaultId)
            override fun openExisting(scope: VaultPinScope) = AndroidVaultPinMac.openExisting(scope.vaultId)
        }
        val backend = UiaRoomPinBackend(disk, VaultPinKdf(AndroidVaultPinKdf::deriveArgon2idV1), devices, native)
        val opened = DevUiaBinding(
            native, VaultPinController(disk.scope, disk, AndroidVaultPinClock(app), backend),
            AndroidVaultBiometricSlot.forSelectedStore(app, disk),
        )
        return synchronized(guard) {
            if (!fence.admit { runCatching { opened.close() } }) null else {
                registered = disk.scope
                binding = opened
                opened
            }
        }
    }

    private fun publishFreshPinGate() = synchronized(guard) {
        if (fence.active()) {
            clearDigits()
            projection.value = pinGate()
        }
    }

    /** Native owns the exact scope predicate, including after Stop while its actor unwinds. */
    private fun interceptParked(native: AndroidUiaRoomPinNative, scope: VaultPinScope): Boolean {
        when (native.pendingParkedResponse()) {
            ParkedRoomPending.None -> return false
            ParkedRoomPending.Unavailable -> synchronized(guard) {
                if (fence.active()) projection.value = DevUiaView.ParkedCheckUnavailable
            }
            ParkedRoomPending.Pending -> {
                synchronized(guard) {
                    if (fence.active()) {
                        clearDigits()
                        projection.value = DevUiaView.Parked(PrivateAssistantAttemptAdmission.parkedStop(profile, scope))
                    }
                }
                // Separate worker: PIN/biometric calling paths return immediately,
                // release their credentials and never resume authentication here.
                worker.launch { watchParked(scope) }
            }
        }
        return true
    }

    private suspend fun watchParked(scope: VaultPinScope) {
        try {
            requireCurrent()
            val disk = AndroidVaultPinStateStore.open(app, scope)
            AndroidUiaRoomPinNative.forSelectedStore(app, disk).use { probe ->
                while (fence.active() && synchronized(guard) { registered == scope }) {
                    requireCurrent()
                    when (probe.pendingParkedResponse()) {
                        ParkedRoomPending.None -> {
                            publishFreshPinGate()
                            return
                        }
                        ParkedRoomPending.Unavailable -> {
                            synchronized(guard) {
                                if (fence.active()) projection.value = DevUiaView.ParkedCheckUnavailable
                            }
                            return
                        }
                        ParkedRoomPending.Pending -> {
                            val stopExact = PrivateAssistantAttemptAdmission.parkedStop(profile, scope)
                            synchronized(guard) {
                                val shown = projection.value as? DevUiaView.Parked
                                if (fence.active() && (shown == null || shown.stopExact != stopExact)) {
                                    projection.value = DevUiaView.Parked(stopExact)
                                }
                            }
                        }
                    }
                    delay(250)
                }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (_: Exception) {
            synchronized(guard) {
                if (fence.active()) projection.value = DevUiaView.ParkedCheckUnavailable
            }
        }
    }

    fun retryParkedCheck() {
        val scope = synchronized(guard) {
            if (!fence.active() || projection.value != DevUiaView.ParkedCheckUnavailable) return
            val current = registered ?: return
            projection.value = DevUiaView.Opening
            current
        }
        worker.launch { watchParked(scope) }
    }

    fun stopParked(expected: DevUiaView.Parked) {
        val stopExact = synchronized(guard) {
            if (!fence.active() || projection.value !== expected) return
            val action = expected.stopExact ?: return
            projection.value = DevUiaView.Parked(null)
            action
        }
        stopExact()
    }

    private fun currentForEnrollmentRestart(): Boolean =
        fence.active() && LocalUiaDevCapability.current(app) == profile

    private fun mayRestartUncommitted(disk: VaultPinDiskStateStore): Boolean = try {
        disk.withUncommittedEnrollment {
            currentForEnrollmentRestart() && profileMap.lookup(profile) == disk.scope &&
                AndroidUiaRoomPinNative.forSelectedStore(app, disk).use { it.canRestartUncommitted() }
        } == true
    } catch (_: Exception) {
        false
    }

    private fun stopEnrollment(stage: VaultPinEnrollmentStage) {
        val (scope, retired) = synchronized(guard) {
            if (!fence.active()) return
            (registered to binding).also { binding = null }
        }
        val closed = runCatching { retired?.close() }.isSuccess
        val safe = if (closed && scope != null) runCatching {
            val disk = AndroidVaultPinStateStore.open(app, scope)
            scope.takeIf { mayRestartUncommitted(disk) }
        }.getOrNull() else null
        synchronized(guard) {
            if (fence.active() && registered == scope) {
                projection.value = enrollmentStop(stage, safe)
            }
        }
    }

    /** Only an explicit confirmation tied to the exact displayed proof can request a new scope. */
    fun restartUncommitted(expected: DevUiaView.Stop) {
        val scope = synchronized(guard) {
            val candidate = expected.restartableScope ?: return
            if (!fence.active() || projection.value !== expected || binding != null || registered != candidate) return
            clearDigits()
            projection.value = DevUiaView.Opening
            candidate
        }
        worker.launch {
            val newScope = try {
                requireCurrent()
                val disk = AndroidVaultPinStateStore.open(app, scope)
                AndroidUiaRoomPinNative.forSelectedStore(app, disk).use { proof ->
                    profileMap.replaceProvenUncommitted(
                        profile, disk, ::currentForEnrollmentRestart, proof::retireUncommitted,
                    ) { AndroidVaultPinStateStore.createForNewEnrollment(app).scope }
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                null
            }
            synchronized(guard) {
                if (!fence.active() || registered != scope || projection.value != DevUiaView.Opening) return@synchronized
                if (newScope == null) {
                    projection.value = DevUiaView.Stop(app.getString(R.string.local_pin_restart_not_confirmed))
                } else {
                    registered = newScope
                    biometricAvailability = VaultBiometricAvailability.Unavailable
                    projection.value = DevUiaView.Gate(new = true)
                }
            }
        }
    }

    private fun pinGate(
        prompt: Boolean = false, info: String? = null, optIn: Boolean = false, disable: Boolean = false,
    ): DevUiaView.Gate = DevUiaView.Gate(
        new = false, prompt = prompt, info = info, optIn = optIn,
        biometric = !optIn && !disable && biometricEnabled, disableBiometric = disable,
        biometricManagement = biometricManagement(),
    )

    private fun showGate(
        new: Boolean, info: String, recovering: Boolean = false, optIn: Boolean = false, disable: Boolean = false,
    ) = synchronized(guard) {
        if (fence.active()) {
            projection.value = if (new || recovering) DevUiaView.Gate(new, true, info, recovering = recovering)
                else pinGate(prompt = true, info = info, optIn = optIn, disable = disable)
        }
    }

    private fun biometricSetupResult(): String = when (biometricAvailability) {
        VaultBiometricAvailability.Eligible -> "Biometrie nicht aktiviert. Du kannst sie später erneut einrichten."
        VaultBiometricAvailability.Interrupted ->
            "Diese Biometrie-Einrichtung kann nicht sicher erneut gestartet werden. Nutze weiterhin deine Harw-PIN."
        else -> "Die Biometrie-Einrichtung konnte nicht bestätigt werden. Nutze weiterhin deine Harw-PIN."
    }

    private fun delayInfo(action: VaultPinAction.Delayed): String =
        "Versuche es in ${(action.remainingMillis + 999) / 1000} Sekunden erneut."

    private fun enrollmentStop(stage: VaultPinEnrollmentStage, restartableScope: VaultPinScope? = null): DevUiaView.Stop {
        val message = if (stage == VaultPinEnrollmentStage.AssistantHandoff) R.string.local_pin_assistant_handoff_failed
            else R.string.local_pin_setup_failed
        return DevUiaView.Stop(app.getString(message, stage.code), restartableScope, stage)
    }

    private fun stop(text: String) = stop(DevUiaView.Stop(text))

    private fun stop(state: DevUiaView.Stop) {
        val retired = synchronized(guard) {
            if (!fence.active()) return
            projection.value = state
            binding.also { binding = null }
        }
        runCatching { retired?.close() }
    }

    /** Transient Activity loss may retain only a proven native one-shot write permit. */
    fun hideForActivity() = retire(allowParking = true)

    fun close() = retire(allowParking = false)

    private fun retire(allowParking: Boolean) {
        val (retired, exact) = synchronized(guard) {
            fence.close()
            nativePromptVisible = false
            clearDigits()
            val chat = (projection.value as? DevUiaView.Ready)?.chat
            projection.value = DevUiaView.Stop("Assistent gesperrt.")
            binding.also { binding = null } to chat
        }
        try {
            if (allowParking) exact?.parkForActivity()
        } finally {
            // Normal revocation is mandatory unless native already atomically
            // fenced the read/Library lease for that exact foreground attempt.
            try { runCatching { retired?.close() } } finally { worker.cancel() }
        }
    }

    private fun clearDigits() { pin.clear(); confirmation.clear() }

    companion object {
        const val RECOVERY = "Die lokale Registrierung benötigt Wiederherstellung. Es wird kein neuer Tresor angelegt."
    }
}

@Composable
internal fun LocalUiaDevScreen(
    session: LocalUiaDevSession,
    onBack: () -> Unit,
    startInLibrary: Boolean = false,
    workHostState: WorkspaceState? = null,
    workConnection: HarwConnection? = null,
    cloudWorkHostState: WorkspaceState? = null,
    cloudWorkConnection: HarwConnection? = null,
    ownerStillCurrent: () -> Boolean = { false },
    authorizingWorkApproval: Boolean = false,
    onDecideWork: (LocalUiaWorkLink, String, Long, String, uniffi.harw_mobile_ffi.ApprovalDecision, () -> Boolean) -> Unit =
        { _, _, _, _, _, _ -> },
) {
    val view by session.view.collectAsState()
    val room = (view as? DevUiaView.Ready)?.chat
    val roomState = room?.let { it.states.collectAsState().value }
    val lease = (roomState as? UiaRoomState.Open)?.snapshot?.lease
    val work = remember(session, lease) { LocalUiaWorkNavigation() }
    val imageDraft = remember(work) { mutableStateOf<UiaRoomImageDraft?>(null) }
    val submittedDraft = remember(work) { mutableStateOf<SubmittedRoomDraft?>(null) }
    DisposableEffect(work) {
        onDispose {
            work.privateDraft.value = ""
            imageDraft.value = null
            submittedDraft.value = null
        }
    }
    val scope = rememberCoroutineScope()
    val cloudPort = remember(lease) { lease?.let { session.manualCloudWork() } }
    val cloudTarget by produceState<ManualCloudWorkTarget?>(null, lease, cloudWorkConnection,
        cloudWorkHostState?.generation, cloudWorkHostState?.profile, cloudWorkHostState?.online) {
        val generation = cloudWorkHostState?.generation
        value = if (cloudPort != null && generation != null && cloudWorkHostState?.online == true)
            cloudWorkConnection?.manualCloudWorkTarget(generation) else null
    }
    val cloudTask = remember(lease, cloudTarget) { ManualCloudWorkSelection() }
    fun permitsCloud(target: ManualCloudWorkTarget): Boolean =
        lease != null && (room?.states?.value as? UiaRoomState.Open)?.snapshot?.lease === lease &&
            cloudTarget === target && ownerStillCurrent() && target.native.isCurrent() && session.allowsLocalAction()

    val openCloud = cloudTask.openSession
    val selectedCloud = cloudTarget
    if (openCloud != null && selectedCloud != null && cloudWorkConnection != null && cloudWorkHostState != null &&
        cloudWorkHostState.generation == selectedCloud.generation
    ) {
        LaunchedEffect(openCloud, selectedCloud) {
            while (permitsCloud(selectedCloud)) delay(500)
            cloudTask.openSession = null
        }
        // Reuse normal cloud session UI, but never inherit Local Assistant approval authority.
        LocalUiaWorkConversation(
            link = openCloud.link, state = cloudWorkHostState, connection = cloudWorkConnection,
            allowed = { permitsCloud(selectedCloud) }, authorizingApproval = true,
            onDecide = { _, _, _, _ -> }, onBack = { cloudTask.openSession = null },
            windowOverride = openCloud.window, expectLocal = false,
        )
        return
    }

    fun mayUseWork(expectedGeneration: Long): Boolean {
        val current = room?.states?.value as? UiaRoomState.Open
        return lease != null && current?.snapshot?.lease === lease &&
            ownerStillCurrent() && workConnection?.canUseLocalUiaWork(expectedGeneration) == true &&
            session.allowsLocalAction()
    }

    val search = work.search
    val host = workHostState
    val connection = workConnection
    if (work.searchOpen && search != null && lease != null && host != null && connection != null &&
        host.local && host.online && host.generation == search.generation
    ) {
        fun allowed() = work.searchOpen && work.search === search && mayUseWork(search.generation)
        LaunchedEffect(search) {
            while (allowed() && connection.localAssistantSearchTargetCurrent(search, ::allowed)) {
                delay(1_000)
            }
            work.searchOpen = false
            work.search = null
        }
        val searchedWork = search.work
        if (search.showingWork && searchedWork != null) {
            LocalUiaWorkConversation(
                link = searchedWork.link,
                state = host,
                connection = connection,
                allowed = ::allowed,
                authorizingApproval = authorizingWorkApproval,
                onDecide = { id, generation, approval, decision ->
                    if (allowed()) {
                        onDecideWork(searchedWork.link, id, generation, approval, decision) {
                            search.showingWork && search.work === searchedWork && allowed()
                        }
                    }
                },
                onBack = { search.showingWork = false },
                windowOverride = searchedWork.window,
            )
        } else {
            AssistantSessionPicker(
                owner = search,
                summaries = host.sessions,
                fresh = localAssistantSummariesCurrent(host, search),
                manageable = search.canRename && localAssistantSummariesCurrent(host, search),
                allowArchive = false,
                projectChoices = assistantProjectChoices(host, search.generation),
                projectHostLabel = "Lokaler Host auf diesem Gerät",
                heading = "Lokale Sessions",
                scopeNotice = "Nur geladene Sessiontitel auf diesem Gerät. Keine Suche in Nachrichten, im privaten Verlauf oder in der Library.",
                onOpen = { id ->
                    if (!connection.localAssistantSearchTargetCurrent(search, ::allowed)) false
                    else localAssistantSummary(connection.state.value, search, id)?.let(search::select) == true
                },
                onChange = { change -> connection.changeLocalAssistantSession(search, change, ::allowed) },
                onBack = { work.searchOpen = false },
            )
        }
        return
    }

    val workLink = work.link
    if (work.detail && workLink != null && lease != null && workHostState != null && workConnection != null) {
        LocalUiaWorkConversation(
            link = workLink, state = workHostState, connection = workConnection,
            allowed = { mayUseWork(workLink.generation) }, authorizingApproval = authorizingWorkApproval,
            onDecide = { id, generation, approval, decision ->
                if (mayUseWork(workLink.generation)) {
                    onDecideWork(workLink, id, generation, approval, decision) {
                        work.detail && work.link === workLink && mayUseWork(workLink.generation)
                    }
                }
            },
            onBack = { work.detail = false },
        )
        return
    }
    BackHandler { onBack() }
    val context = LocalContext.current
    val activity = context as? Activity
    Column(Modifier.fillMaxSize()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            TextButton(onClick = onBack) { Text("Zurück") }
            Text("Nur lokal", Modifier.padding(vertical = 14.dp), style = MaterialTheme.typography.labelLarge)
        }
        when (val showing = view) {
            DevUiaView.Opening -> Text("Privaten Raum prüfen …", Modifier.padding(16.dp))
            is DevUiaView.Parked -> {
                Text(stringResource(R.string.private_assistant_parked_pending), Modifier.padding(16.dp))
                if (showing.stopExact != null) {
                    TextButton(
                        onClick = { session.stopParked(showing) },
                        modifier = Modifier.padding(horizontal = 16.dp),
                    ) { Text(stringResource(R.string.private_assistant_attempt_stop)) }
                }
            }
            DevUiaView.ParkedCheckUnavailable -> {
                Text(stringResource(R.string.private_assistant_parked_check_unavailable), Modifier.padding(16.dp))
                TextButton(onClick = session::retryParkedCheck, modifier = Modifier.padding(horizontal = 16.dp)) {
                    Text(stringResource(R.string.private_assistant_parked_check_again))
                }
            }
            is DevUiaView.Stop -> {
                Text(showing.text, Modifier.padding(16.dp))
                showing.diagnostic?.let { stage ->
                    var copyFailed by remember(session, showing) { mutableStateOf(false) }
                    TextButton(
                        onClick = {
                            // Only a fixed enum code goes to the clipboard; no IDs, state, exception, or PIN.
                            copyFailed = !runCatching {
                                val clipboard = context.getSystemService(ClipboardManager::class.java)
                                    ?: return@runCatching false
                                clipboard.setPrimaryClip(ClipData.newPlainText("Harw", stage.code))
                                true
                            }.getOrDefault(false)
                        },
                        modifier = Modifier.padding(horizontal = 16.dp),
                    ) { Text(stringResource(R.string.local_pin_copy_diagnostic)) }
                    if (copyFailed) Text(
                        stringResource(R.string.local_pin_copy_diagnostic_failed),
                        Modifier.padding(horizontal = 16.dp),
                    )
                }
                if (showing.restartableScope != null) {
                    var confirmRestart by remember(session, showing) { mutableStateOf(false) }
                    OutlinedButton(
                        onClick = { confirmRestart = true },
                        modifier = Modifier.padding(horizontal = 16.dp),
                    ) { Text(stringResource(R.string.local_pin_restart)) }
                    if (confirmRestart) AlertDialog(
                        onDismissRequest = { confirmRestart = false },
                        title = { Text(stringResource(R.string.local_pin_restart)) },
                        text = { Text(stringResource(R.string.local_pin_restart_confirm_detail)) },
                        confirmButton = {
                            TextButton(onClick = {
                                confirmRestart = false
                                session.restartUncommitted(showing)
                            }) { Text(stringResource(R.string.local_pin_restart_confirm)) }
                        },
                        dismissButton = {
                            TextButton(onClick = { confirmRestart = false }) { Text(stringResource(android.R.string.cancel)) }
                        },
                    )
                }
            }
            DevUiaView.OfferBiometric -> {
                Text("Harw-PIN eingerichtet. Möchtest du zusätzlich Fingerabdruck oder eine von Android als stark eingestufte Gesichtserkennung verwenden?", Modifier.padding(16.dp))
                Button(onClick = { session.chooseBiometric(true) }, modifier = Modifier.padding(horizontal = 16.dp)) {
                    Text("Biometrie einrichten")
                }
                TextButton(onClick = { session.chooseBiometric(false) }, modifier = Modifier.padding(horizontal = 16.dp)) {
                    Text("Nur Harw-PIN")
                }
            }
            is DevUiaView.Gate -> {
                if (!showing.prompt) {
                    Text(when {
                        showing.recovering -> "Vorhandene lokale Einrichtung abschließen"
                        showing.new -> "Lokalen Assistenten einrichten"
                        else -> "Lokaler Assistent gesperrt"
                    }, Modifier.padding(16.dp))
                    Button(onClick = session::prompt, modifier = Modifier.padding(horizontal = 16.dp)) {
                        Text(when {
                            showing.recovering -> "Mit bisheriger Harw-PIN fortsetzen"
                            showing.new -> "Harw-PIN festlegen"
                            else -> "Mit Harw-PIN entsperren"
                        })
                    }
                } else {
                    PinPad(session, showing, activity)
                }
                if (!showing.prompt && !showing.new && !showing.recovering) {
                    when (showing.biometricManagement) {
                        VaultBiometricAvailability.Eligible -> if (activity != null) {
                            TextButton(onClick = { session.changeBiometric(enable = true) }) { Text("Biometrie einrichten") }
                        }
                        VaultBiometricAvailability.Enrolled, VaultBiometricAvailability.Interrupted -> {
                            TextButton(onClick = { session.changeBiometric(enable = false) }) { Text("Biometrie deaktivieren") }
                        }
                        VaultBiometricAvailability.RevocationPending -> {
                            TextButton(onClick = { session.changeBiometric(enable = false) }) { Text("Biometrie-Deaktivierung abschließen") }
                        }
                        else -> Unit
                    }
                }
                if (showing.biometric && !showing.optIn && !showing.disableBiometric && activity != null) {
                    TextButton(onClick = { session.biometricUnlock(activity) }, modifier = Modifier.padding(horizontal = 16.dp)) {
                        Text("Mit Biometrie entsperren")
                    }
                }
            }
            is DevUiaView.Ready -> {
                var libraryOpen by remember(showing, startInLibrary) { mutableStateOf(startInLibrary) }
                showing.biometricNotice?.let { Text(it, Modifier.padding(horizontal = 16.dp)) }
                if (libraryOpen) {
                    // Only explicit Library navigation obtains its port. Opening
                    // the Assistant or searching ordinary sessions never does.
                    val library by produceState<Result<Pair<AndroidPrivateLibraryPort, PrivateLibrarySetup>?>?>(null, showing) {
                        value = try {
                            Result.success(session.privateLibrary())
                        } catch (error: CancellationException) {
                            throw error
                        } catch (error: Exception) {
                            Result.failure(error)
                        }
                    }
                    val available = library?.getOrNull()
                    val closeLibrary = { if (startInLibrary) onBack() else libraryOpen = false }
                    if (available != null) {
                        val expectedPort = available.first
                        val expectedRoom = roomState as? UiaRoomState.Open
                        val descriptionHeading = stringResource(R.string.private_library_assistant_draft_heading)
                        val receiver = expectedRoom?.takeIf { it.permitsSend }?.let { expected ->
                            PrivateLibraryCaptionReceiver stage@{ saved ->
                                val current = showing.chat.states.value as? UiaRoomState.Open ?: return@stage false
                                if (!libraryOpen || session.view.value !== showing || !ownerStillCurrent() ||
                                    !session.allowsLocalAction() || !expectedPort.active.value ||
                                    saved.lease !== expectedPort.lease || saved.vault != expectedPort.boundVault ||
                                    current.snapshot.lease !== expected.snapshot.lease ||
                                    current.snapshot.revision != expected.snapshot.revision || !current.permitsSend
                                ) return@stage false
                                val block = "$descriptionHeading\n" +
                                    saved.text.lineSequence().joinToString("\n") { "> $it" }
                                val draft = work.privateDraft.value
                                val next = if (draft.isBlank()) block else "$draft\n\n$block"
                                if (!uiaRoomAcceptsDraft(next)) return@stage false
                                // Only this room's existing unsent RAM draft. The
                                // user still has to press the private Send button.
                                work.privateDraft.value = next
                                libraryOpen = false
                                true
                            }
                        }
                        val imageReceiver = expectedRoom?.takeIf { it.permitsSend && imageDraft.value == null }
                            ?.let { expected ->
                                PrivateLibraryImageReceiver stage@{ selected ->
                                    val current = showing.chat.states.value as? UiaRoomState.Open ?: return@stage false
                                    if (!libraryOpen || session.view.value !== showing || !ownerStillCurrent() ||
                                        !session.allowsLocalAction() || !expectedPort.active.value ||
                                        selected.lease !== expectedPort.lease || selected.vault != expectedPort.boundVault ||
                                        current.snapshot.lease !== expected.snapshot.lease ||
                                        current.snapshot.revision != expected.snapshot.revision || !current.permitsSend ||
                                        imageDraft.value != null
                                    ) return@stage false
                                    imageDraft.value = UiaRoomImageDraft(current.snapshot.lease, current.snapshot.revision, selected)
                                    libraryOpen = false
                                    true
                                }
                            }
                        PrivateLibraryGate(
                            expectedPort, available.second, onBack = closeLibrary,
                            captionReceiver = receiver, imageReceiver = imageReceiver,
                        )
                    } else {
                        BackHandler(onBack = closeLibrary)
                        Text(
                            if (library == null) "Die private Library wird geprüft …"
                            else "Die private Library ist für dieses Profil nicht verfügbar.",
                            Modifier.padding(16.dp),
                        )
                        TextButton(onClick = closeLibrary) { Text("Zurück") }
                    }
                    return@Column
                }
                TextButton(onClick = session::manageBiometric) { Text("Biometrie verwalten") }
                TextButton(onClick = { libraryOpen = true }) { Text("Private Library") }
                UiaRoomPanel(
                    roomState ?: UiaRoomState.Locked,
                    onRequestUnlock = session::requestReauth,
                    onRetryOpen = { if (session.allowsLocalAction()) showing.chat.reload() },
                    onSubmit = { request ->
                        ownerStillCurrent() && session.allowsLocalAction() &&
                            (request.image == null || imageDraft.value === request.image) &&
                            showing.chat.submit(request, activity, session.profile)
                    },
                    onRequestCancel = showing.chat::cancel,
                    onDismissIssue = showing.chat::dismissIssue,
                    modifier = Modifier.weight(1f),
                    draftState = work.privateDraft,
                    imageDraftState = imageDraft,
                    submittedDraftState = submittedDraft,
                    loadRoomPhoto = { expected ->
                        if (!ownerStillCurrent() || session.view.value !== showing || !expected.isFor(showing.chat.states.value)) null
                        else session.previewPrivateRoomPhoto(showing.chat, expected)
                    },
                    roomPhotoAllowed = { expected ->
                        ownerStillCurrent() && session.view.value === showing &&
                            expected.isFor(showing.chat.states.value)
                    },
                    transcriptPosition = work.privateTranscriptPosition,
                    memoryPort = remember(lease) { lease?.let { session.manualMemory() } },
                    memoryAllowed = session::allowsLocalAction,
                    linkedWorkCard = {
                        if (cloudTarget != null && cloudPort != null) {
                            val target = cloudTarget
                            if (target != null) ManualCloudWorkCard(
                                target = target, port = cloudPort, selection = cloudTask,
                                allowed = { permitsCloud(target) },
                            )
                        }
                        val generation = workHostState?.generation
                        if (lease != null && generation != null && workConnection != null) {
                            OutlinedButton(
                                enabled = !work.searchOpening && workHostState.local && workHostState.online,
                                onClick = {
                                    if (!work.searchOpening && mayUseWork(generation)) {
                                        work.searchOpening = true
                                        work.searchIssue = false
                                        scope.launch {
                                            fun authorized() = mayUseWork(generation)
                                            try {
                                                val previous = work.search
                                                work.search = if (previous != null && previous.generation == generation &&
                                                    workConnection.localAssistantSearchTargetCurrent(previous, ::authorized)
                                                ) previous else workConnection.localAssistantSearchTarget(generation, ::authorized)
                                                work.searchOpen = work.search != null && authorized()
                                                work.searchIssue = !work.searchOpen && authorized()
                                            } catch (error: CancellationException) {
                                                throw error
                                            } catch (_: Exception) {
                                                work.searchIssue = authorized()
                                            } finally {
                                                work.searchOpening = false
                                            }
                                        }
                                    }
                                },
                            ) { Text("Lokale Sessions suchen") }
                            if (work.searchIssue) Text("Der lokale Host ist für die Suche gerade nicht verfügbar.")
                            LocalUiaWorkCard(
                                work,
                                enabled = workHostState.local && workHostState.online &&
                                    (work.link?.generation == generation || work.link == null && workHostState.canCreate),
                                onOpen = { link ->
                                    if (mayUseWork(link.generation)) work.detail = true
                                },
                                onCreate = {
                                    if (!work.starting && work.link == null && mayUseWork(generation)) {
                                        work.starting = true
                                        work.issue = null
                                        scope.launch {
                                            try {
                                                val result = workConnection.createLocalUiaWorkSession(generation) {
                                                    mayUseWork(generation)
                                                }
                                                if (mayUseWork(generation)) {
                                                    if (result is LocalUiaWorkCreate.Created) work.link = result.link
                                                    else work.issue = result
                                                }
                                            } finally { work.starting = false }
                                        }
                                    }
                                },
                            )
                        }
                    },
                )
            }
        }
    }
}

@Composable
private fun PinPad(session: LocalUiaDevSession, gate: DevUiaView.Gate, activity: Activity?) {
    var confirming by androidx.compose.runtime.remember(gate) { androidx.compose.runtime.mutableStateOf(false) }
    val bytes = if (confirming) session.confirmation else session.pin
    Column(Modifier.verticalScroll(rememberScrollState()).padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(if (confirming) "PIN wiederholen" else "Harw-PIN", style = MaterialTheme.typography.titleLarge)
        Text("•".repeat(bytes.count), style = MaterialTheme.typography.bodyLarge)
        gate.info?.let { Text(it) }
        for (row in listOf(listOf(1,2,3), listOf(4,5,6), listOf(7,8,9))) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (digit in row) OutlinedButton(onClick = { bytes.add(digit) }, Modifier.weight(1f)) { Text(digit.toString()) }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            TextButton(onClick = session::cancelPrompt, Modifier.weight(1f)) { Text("Abbrechen") }
            OutlinedButton(onClick = { bytes.add(0) }, Modifier.weight(1f)) { Text("0") }
            OutlinedButton(onClick = bytes::erase, Modifier.weight(1f)) { Text("Löschen") }
        }
        Button(onClick = {
            if (gate.new && !confirming) confirming = true else session.authenticate(activity)
        }, enabled = bytes.count >= 8) {
            Text(when {
                gate.new && !confirming -> "Weiter"
                gate.new -> "Einrichten"
                gate.disableBiometric -> "Biometrie endgültig deaktivieren"
                gate.optIn -> "Mit Harw-PIN bestätigen"
                else -> "Entsperren"
            })
        }
    }
}


@Composable
private fun PrivateLibraryGate(
    port: AndroidPrivateLibraryPort,
    initial: PrivateLibrarySetup,
    onBack: () -> Unit,
    captionReceiver: PrivateLibraryCaptionReceiver?,
    imageReceiver: PrivateLibraryImageReceiver?,
) {
    val active by port.active.collectAsState()
    var setup by remember(port.lease) { mutableStateOf<PrivateLibrarySetup?>(null) }
    LaunchedEffect(port.lease, initial) { setup = port.setupStatus() }
    var busy by remember(port.lease) { mutableStateOf(false) }
    var failure by remember(port.lease) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    if (!active) {
        TextButton(onClick = onBack) { Text("Zurück") }
        Text("Die private Bibliothek wurde gesperrt.", Modifier.padding(16.dp))
        return
    }
    if (setup == PrivateLibrarySetup.Ready) {
        PrivateLibraryScreen(
            port, onBack = onBack, draftReceiver = null, captionReceiver = captionReceiver,
            assistantImageReceiver = imageReceiver,
        )
        return
    }
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        TextButton(onClick = onBack) { Text("Zurück") }
        Text("Private Library für dieses lokale Vault-Profil einrichten")
        Text("Die Library speichert Bilder und Beschreibungen nur verschlüsselt auf diesem Gerät.")
        if (failure || setup == PrivateLibrarySetup.Unavailable) {
            Text("Die Einrichtung konnte nicht bestätigt werden. Öffne den privaten Raum erneut.")
        }
        Button(
            enabled = !busy && !failure && setup == PrivateLibrarySetup.New,
            onClick = {
                busy = true
                scope.launch {
                    try {
                        port.provision()
                        setup = port.setupStatus()
                        failure = setup != PrivateLibrarySetup.Ready
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (_: Exception) {
                        failure = true
                    } finally { busy = false }
                }
            },
        ) { Text("Verschlüsselte Library einrichten") }
    }
}
