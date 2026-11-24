package dev.harw.mobile

import android.app.Activity
import android.app.KeyguardManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/** Queries both platform locks. Missing or denied state never grants continuation. */
private fun privateAssistantDeviceIsUnlocked(context: Context): Boolean = try {
    val power = context.getSystemService(PowerManager::class.java)
    val lock = context.getSystemService(KeyguardManager::class.java)
    power != null && lock != null && power.isInteractive && !lock.isKeyguardLocked && !lock.isDeviceLocked
} catch (_: Exception) {
    false
}

/** Snapshot existing local-host authority; this never connects or opens a vault. */
private fun activeAssistantHostGeneration(app: HarwApplication): Long? =
    app.localConnection.state.value.takeIf { it.local && it.online }?.generation

/**
 * In-process Android admission, not chat or vault authority. Holds one exact
 * user-started attempt. No prompt, image, room, or credentials enter an Intent.
 */
internal object PrivateAssistantAttemptAdmission {
    private val guard = Any()
    private var current: Ticket? = null

    internal class Ticket internal constructor(
        internal val id: String,
        internal val profile: LocalUiaProfile,
        internal val vaultScope: VaultPinScope,
        private val app: HarwApplication,
        private val hostGeneration: Long,
        private val cancel: () -> Unit,
    ) {
        internal val promoted = CompletableDeferred<Boolean>()
        internal val ended = CompletableDeferred<Unit>()
        // All ticket state is guarded by PrivateAssistantAttemptAdmission.guard.
        internal var parked = false
        internal var stopping = false
        internal var stopReturned = false
        internal var workFinished = false

        suspend fun awaitForeground(): Boolean {
            val ready = withTimeoutOrNull(6_000L) { promoted.await() } == true && isForeground()
            return ready
        }
        fun isForeground(): Boolean =
            activeAssistantHostGeneration(app) == hostGeneration &&
                privateAssistantDeviceIsUnlocked(app) && PrivateAssistantAttemptAdmission.isForeground(this)
        fun markParked() = synchronized(guard) {
            if (current === this && !stopping && !workFinished) parked = true
        }
        fun requiresParkedAuthorityCheck(): Boolean = synchronized(guard) {
            current === this && parked && !stopping && !workFinished
        }
        suspend fun parkedAuthorityIsCurrent(): Boolean =
            activeAssistantHostGeneration(app) == hostGeneration &&
                withContext(Dispatchers.IO) { LocalUiaDevCapability.current(app) == profile } &&
                activeAssistantHostGeneration(app) == hostGeneration
        fun finish() = finishWork(this)
        fun stop() = stopExact(this)
        internal fun cancelExact() = cancel()
    }

    fun request(
        activity: Activity, profile: LocalUiaProfile, vaultScope: VaultPinScope, cancel: () -> Unit,
    ): Ticket? {
        val life = (activity as? LifecycleOwner)?.lifecycle ?: return null
        val app = activity.application as? HarwApplication ?: return null
        val hostGeneration = activeAssistantHostGeneration(app) ?: return null
        if (activity.isFinishing || life.currentState != Lifecycle.State.RESUMED ||
            !privateAssistantDeviceIsUnlocked(activity)) return null
        val ticket = synchronized(guard) {
            if (current != null) return null
            Ticket(UUID.randomUUID().toString(), profile, vaultScope, app, hostGeneration, cancel).also { current = it }
        }
        return try {
            ContextCompat.startForegroundService(activity, PrivateAssistantAttemptService.intent(activity, ticket.id))
            ticket
        } catch (_: Exception) {
            ticket.finish()
            null
        }
    }

    fun cancelOnProfileChange(live: LocalUiaProfile?) {
        val retired = synchronized(guard) { current?.takeUnless { it.profile == live } }
        retired?.stop()
    }

    suspend fun stopForVaultReset(profile: LocalUiaProfile) {
        val ticket = synchronized(guard) { current?.takeIf { it.profile == profile } } ?: return
        ticket.stop()
        ticket.ended.await()
    }

    fun parkedStop(profile: LocalUiaProfile, vaultScope: VaultPinScope): (() -> Unit)? =
        synchronized(guard) {
            current?.takeIf {
                it.parked && !it.stopping && !it.workFinished && it.profile == profile && it.vaultScope == vaultScope
            }?.let { ticket ->
                ticket::stop
            }
        }

    internal fun exact(id: String?): Ticket? = synchronized(guard) {
        current?.takeIf { it.id == id && !it.stopping && !it.workFinished }
    }

    internal fun promote(ticket: Ticket): Boolean = synchronized(guard) {
        current === ticket && !ticket.stopping && !ticket.workFinished && ticket.promoted.complete(true)
    }

    private fun isForeground(ticket: Ticket): Boolean = synchronized(guard) {
        current === ticket && !ticket.stopping && !ticket.workFinished && ticket.promoted.isCompleted
    }

    private fun finishWork(ticket: Ticket) {
        val released = synchronized(guard) {
            if (current !== ticket || ticket.workFinished) return
            ticket.workFinished = true
            ticket.promoted.complete(false)
            releaseIfFinished(ticket)
        }
        if (released) ticket.ended.complete(Unit)
    }

    private fun stopExact(ticket: Ticket) {
        synchronized(guard) {
            if (current !== ticket || ticket.stopping || ticket.workFinished) return
            ticket.stopping = true
            ticket.promoted.complete(false)
        }
        try {
            ticket.cancelExact()
        } finally {
            val released = synchronized(guard) {
                ticket.stopReturned = true
                releaseIfFinished(ticket)
            }
            if (released) ticket.ended.complete(Unit)
        }
    }

    /** Stop fences admission immediately, but the slot remains occupied until both sides finish. */
    private fun releaseIfFinished(ticket: Ticket): Boolean {
        if (current !== ticket || !ticket.workFinished || (ticket.stopping && !ticket.stopReturned)) return false
        current = null
        return true
    }
}

/** Non-sticky: a lost Android process never recreates or replays a request. */
internal class PrivateAssistantAttemptService : Service() {
    private val worker = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var watching: Job? = null
    private var owned: PrivateAssistantAttemptAdmission.Ticket? = null
    private var latestStart = 0
    private var screenReceiverRegistered = false
    private val screenReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == Intent.ACTION_SCREEN_OFF) owned?.stop()
        }
    }

    private fun watchDeviceLock() {
        if (screenReceiverRegistered) return
        ContextCompat.registerReceiver(
            this, screenReceiver, IntentFilter(Intent.ACTION_SCREEN_OFF), ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        screenReceiverRegistered = true
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStart = startId
        val ticket = PrivateAssistantAttemptAdmission.exact(intent?.data?.lastPathSegment)
        when (intent?.action) {
            StopAction -> {
                // A stale PendingIntent can never stop a replacement attempt.
                ticket?.takeIf { it === owned }?.stop()
                if (owned?.ended?.isCompleted != false) stopSelfResult(startId)
            }
            StartAction -> {
                if (ticket == null) {
                    if (owned?.ended?.isCompleted != false) stopSelfResult(startId)
                } else if (ticket !== owned) {
                    owned = ticket
                    try {
                        // Register before issuing the grant. Lifecycle callbacks can
                        // lag SCREEN_OFF, and an already-parked request has no Activity.
                        watchDeviceLock()
                        check(privateAssistantDeviceIsUnlocked(this))
                        val manager = checkNotNull(getSystemService(NotificationManager::class.java))
                        manager.createNotificationChannel(NotificationChannel(
                            Channel, getString(R.string.private_assistant_attempt_channel), NotificationManager.IMPORTANCE_LOW,
                        ))
                        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
                        ServiceCompat.startForeground(this, NotificationId, notification(ticket), type)
                        check(privateAssistantDeviceIsUnlocked(this) &&
                            PrivateAssistantAttemptAdmission.promote(ticket))
                        watching?.cancel()
                        watching = worker.launch {
                            // The non-privileged API has no reliable keyguard-
                            // locked callback when the screen stays interactive.
                            val lockCheck = launch {
                                while (!ticket.ended.isCompleted) {
                                    if (!privateAssistantDeviceIsUnlocked(this@PrivateAssistantAttemptService)) {
                                        ticket.stop()
                                        break
                                    }
                                    delay(250L)
                                }
                            }
                            // Once parked there might be no Activity/Compose
                            // observer. Watch only the admitted host/profile
                            // while leaving the separate lock check responsive.
                            val authorityCheck = launch {
                                while (!ticket.ended.isCompleted) {
                                    if (ticket.requiresParkedAuthorityCheck() && !ticket.parkedAuthorityIsCurrent()) {
                                        ticket.stop()
                                        break
                                    }
                                    delay(750L)
                                }
                            }
                            try { ticket.ended.await() } finally {
                                authorityCheck.cancel()
                                lockCheck.cancel()
                            }
                            if (owned === ticket) stopSelfResult(latestStart)
                        }
                    } catch (_: Exception) {
                        ticket.stop()
                        stopSelfResult(startId)
                    }
                }
            }
            else -> if (owned?.ended?.isCompleted != false) stopSelfResult(startId)
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        owned?.stop()
        owned = null
        worker.cancel()
        if (screenReceiverRegistered) {
            unregisterReceiver(screenReceiver)
            screenReceiverRegistered = false
        }
        super.onDestroy()
    }

    private fun notification(ticket: PrivateAssistantAttemptAdmission.Ticket): Notification {
        val stop = PendingIntent.getService(
            this, 0,
            Intent(this, PrivateAssistantAttemptService::class.java)
                .setAction(StopAction).setData(ticketUri(ticket.id)),
            PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, Channel)
            .setSmallIcon(R.drawable.ic_harw_notification)
            .setContentTitle(getString(R.string.private_assistant_attempt_running))
            .setVisibility(NotificationCompat.VISIBILITY_SECRET)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
            .addAction(0, getString(R.string.private_assistant_attempt_stop), stop)
            .build()
    }

    companion object {
        private const val Channel = "harw_private_assistant_attempt"
        private const val NotificationId = 1041
        private const val StartAction = "dev.harw.mobile.PRIVATE_ASSISTANT_HOLD"
        private const val StopAction = "dev.harw.mobile.PRIVATE_ASSISTANT_STOP"
        private fun ticketUri(ticketId: String): Uri =
            Uri.Builder().scheme("harw-private-attempt").authority("local").appendPath(ticketId).build()
        internal fun intent(activity: Activity, ticketId: String): Intent =
            Intent(activity, PrivateAssistantAttemptService::class.java)
                .setAction(StartAction).setData(ticketUri(ticketId))
    }
}
