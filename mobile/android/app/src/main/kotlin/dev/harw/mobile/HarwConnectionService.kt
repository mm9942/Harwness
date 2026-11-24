package dev.harw.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.security.MessageDigest
import java.util.Base64
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileSessionState

// attempt == 0u -> null (Basistext verwenden); sonst die Versuchszahl als Int für %1$d.
internal fun reconnectAttemptArg(attempt: UInt): Int? =
    if (attempt == 0u) null else attempt.toInt()

internal enum class SessionAlertKind { Approval, TurnEnd }

internal data class SessionAlert(val sessionId: String, val kind: SessionAlertKind)

// Erkennt Zustandswechsel zwischen zwei Session-Listen. Die erste Liste nach einem Reset
// ist nur Basis und löst nichts aus; sonst würde jede Wiederverbindung alte Sessions melden.
internal class SessionAlertTracker {
    private var known: Map<String, MobileSessionState>? = null

    fun reset() {
        known = null
    }

    /** Liefert neue Hinweise und die Sessions, deren Freigabe-Hinweis zurückgenommen werden soll. */
    fun update(sessions: List<MobileSession>): Pair<List<SessionAlert>, List<String>> {
        val next = sessions.associate { it.sessionId to it.state }
        val before = known
        known = next
        if (before == null) return emptyList<SessionAlert>() to emptyList()
        val alerts = mutableListOf<SessionAlert>()
        val cleared = mutableListOf<String>()
        for ((id, state) in next) {
            val old = before[id] ?: continue
            if (old == state) continue
            if (old == MobileSessionState.WAITING_FOR_APPROVAL) cleared += id
            alertForTransition(old, state)?.let { alerts += SessionAlert(id, it) }
        }
        for (id in before.keys) if (id !in next) cleared += id
        return alerts to cleared
    }

    private fun alertForTransition(from: MobileSessionState, to: MobileSessionState): SessionAlertKind? = when {
        to == MobileSessionState.WAITING_FOR_APPROVAL -> SessionAlertKind.Approval
        to == MobileSessionState.IDLE && from in busyStates -> SessionAlertKind.TurnEnd
        else -> null
    }

    private companion object {
        val busyStates = setOf(
            MobileSessionState.RUNNING,
            MobileSessionState.WAITING_FOR_APPROVAL,
            MobileSessionState.WAITING_FOR_CHILD,
            MobileSessionState.QUEUED,
        )
    }
}

/** Snapshot from an authenticated backend's actual directory, never a cached list from a prior link. */
internal data class SessionAlertDirectory(
    val scope: SessionHomeScope,
    val generation: Long,
    val sessions: List<MobileSession>,
) {
    fun authorizes(target: SessionHomeId): Boolean =
        scope == target.scope && sessions.count { it.sessionId == target.sessionId } == 1
}

internal fun sessionAlertDirectory(state: WorkspaceState): SessionAlertDirectory? {
    val generation = state.generation ?: return null
    if (!state.online || !state.sessionsLoaded || state.sessionSummariesGeneration != generation ||
        state.setupPending || state.forgetRequired || state.forgetting
    ) return null
    val scope = sessionHomeScope(state) ?: return null
    return SessionAlertDirectory(scope, generation, state.sessions)
}

internal data class SessionAlertTarget(val id: SessionHomeId, val kind: SessionAlertKind) {
    // PendingIntent identity ignores extras. A scoped digest in its data URI separates
    // equal session IDs across backends, accounts, and alert kinds without exposing them.
    fun notificationTag(): String {
        val identity = "harw-session-alert-v1\n${id.scope.backend.name}\n${id.scope.profileId}\n${id.sessionId}\n${kind.name}"
        val digest = MessageDigest.getInstance("SHA-256").digest(identity.encodeToByteArray())
        return "session-alert:" + Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
    }

    fun intentData(): Uri =
        Uri.Builder().scheme("harw-alert").authority("session").appendPath(notificationTag()).build()
}

private const val ScopedAlertNotificationId = 1000

/** Keep existing app-owned work alive, without submitting or replaying any turn. */
internal fun holdWorkspaceConnection(cloud: WorkspaceState, local: WorkspaceState): Boolean =
    cloud.hasConnection || local.hasConnection || cloud.stopping || local.stopping || cloud.forgetting

internal class HarwConnectionService : Service() {
    private val connection get() = (application as HarwApplication).connection
    private val localConnection get() = (application as HarwApplication).localConnection
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var notifications: NotificationManager
    private var foreground = false
    private var latestStart = 0
    private var observation: Job? = null
    private var alertObservation: Job? = null
    private val alertTracker = SessionAlertTracker()
    private val localAlertTracker = SessionAlertTracker()

    override fun onCreate() {
        super.onCreate()
        notifications = checkNotNull(getSystemService(NotificationManager::class.java))
        notifications.createNotificationChannel(
            NotificationChannel(ChannelId, getString(R.string.connection_channel), NotificationManager.IMPORTANCE_LOW),
        )
        notifications.createNotificationChannel(
            NotificationChannel(AlertChannelId, getString(R.string.notification_channel_sessions), NotificationManager.IMPORTANCE_HIGH),
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStart = startId
        val action = intent?.action
        when (action) {
            DisconnectAction -> {
                // This notification action is an explicit stop, unlike removing
                // an Activity/task or Android reclaiming the service.
                writeManualDisconnect(this, true)
                PrivateAssistantAttemptAdmission.cancelOnProfileChange(null)
                (application as HarwApplication).localAutostart.suppress()
                connection.disconnect()
                localConnection.disconnect()
                if (!foreground) stopSelfResult(startId)
            }
            ConnectAction, ConnectLocalAction, HoldExistingAction -> {
                try {
                    // Promote even if a session ended between enqueueing this
                    // startForegroundService call and receiving it here.
                    ensureForeground()
                    when (action) {
                        ConnectAction -> {
                            connection.connect()
                            localConnection.connectLocal()
                        }
                        ConnectLocalAction -> {
                            if (localConnection.connectLocal()) {
                                (application as HarwApplication).localAutostart.resume()
                            }
                        }
                        // The Local autostart/other window already owns this
                        // connection. Adoption must not reconnect or resend.
                        HoldExistingAction -> Unit
                    }
                    if (holdWorkspaceConnection(connection.state.value, localConnection.state.value)) {
                        if (observation?.isActive != true) observeConnection()
                        if (alertObservation?.isActive != true) observeAlerts()
                    } else {
                        finishForeground(startId)
                    }
                } catch (error: Exception) {
                    if (action == ConnectLocalAction) localConnection.reportServiceError(error)
                    else connection.reportServiceError(error)
                    finishForeground(startId)
                }
            }
            else -> if (!foreground) stopSelfResult(startId)
        }
        // A lost process cannot restore a live native/provider request. Never
        // auto-replay a prompt to recover from system kill or user force-stop.
        return START_NOT_STICKY
    }

    private fun ensureForeground() {
        if (foreground) return
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        ServiceCompat.startForeground(this, NotificationId, notification(R.string.notification_connecting), type)
        foreground = true
    }

    private fun finishForeground(startId: Int) {
        if (stopSelfResult(startId) && foreground) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            foreground = false
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        // Runtime and commands are owned by HarwApplication, not this listener.
        // Only the explicit DisconnectAction should cancel their work.
        scope.cancel()
        super.onDestroy()
    }

    private data class Observed(
        val phase: ConnectionPhase,
        val stopping: Boolean,
        val forgetting: Boolean,
        val attempt: UInt,
    )

    private fun observeConnection() {
        observation = scope.launch {
            combine(connection.state, localConnection.state) { cloud, local -> observedConnection(cloud, local) }
                .distinctUntilChanged()
                .collect { applyObservedConnection(it) }
        }
    }

    private fun observedConnection(cloud: WorkspaceState, local: WorkspaceState): Observed {
        val active = setOf(ConnectionPhase.CONNECTING, ConnectionPhase.CONNECTED, ConnectionPhase.RECONNECTING)
        val primary = if (cloud.phase !in active && local.phase in active) local else cloud
        return Observed(primary.phase, cloud.stopping || local.stopping, cloud.forgetting, primary.attempt)
    }

    private fun observedConnectionText(observed: Observed): String? {
        if (observed.forgetting) return getString(R.string.notification_forgetting)
        if (observed.stopping) return getString(R.string.notification_disconnecting)
        return when (observed.phase) {
            ConnectionPhase.CONNECTING -> getString(R.string.notification_connecting)
            ConnectionPhase.CONNECTED -> getString(R.string.notification_connected)
            ConnectionPhase.RECONNECTING -> reconnectAttemptArg(observed.attempt)
                ?.let { getString(R.string.notification_reconnecting_attempt, it) }
                ?: getString(R.string.notification_reconnecting)
            else -> null
        }
    }

    private fun applyObservedConnection(observed: Observed) {
        val (phase, stopping, forgetting) = observed
        val text = observedConnectionText(observed)
        if (text == null) {
            // Drop an obsolete inactive emission if a new start became active
            // before the combined collector reached it.
            if (!holdWorkspaceConnection(connection.state.value, localConnection.state.value)) {
                finishForeground(latestStart)
            }
            return
        }
        if (!foreground) return
        val reconnecting = !forgetting && !stopping && phase == ConnectionPhase.RECONNECTING
        val power = getSystemService(PowerManager::class.java)
        val battery = reconnecting && power != null && !power.isIgnoringBatteryOptimizations(packageName)
        notifications.notify(
            NotificationId,
            notification(text, stopAction = !forgetting && !stopping, batteryAction = battery),
        )
    }

    private fun observeAlerts() {
        alertObservation = scope.launch {
            launch { observeAlerts(connection, alertTracker) }
            launch { observeAlerts(localConnection, localAlertTracker) }
        }
    }

    private suspend fun observeAlerts(source: HarwConnection, tracker: SessionAlertTracker) {
        var previous: SessionAlertDirectory? = null
        val posted = mutableSetOf<SessionAlertTarget>()
        fun cancelAlert(target: SessionAlertTarget) {
            notifications.cancel(target.notificationTag(), ScopedAlertNotificationId)
            posted.remove(target)
        }
        try {
            source.state.map(::sessionAlertDirectory).distinctUntilChanged().collect { directory ->
                if (directory?.scope != previous?.scope || directory?.generation != previous?.generation) {
                    posted.toList().forEach(::cancelAlert)
                    tracker.reset()
                }
                previous = directory
                directory ?: return@collect
                val (alerts, cleared) = tracker.update(directory.sessions)
                val present = directory.sessions.mapTo(hashSetOf()) { it.sessionId }
                posted.filter { it.id.sessionId !in present }.forEach(::cancelAlert)
                for (id in cleared) {
                    cancelAlert(SessionAlertTarget(SessionHomeId(directory.scope, id), SessionAlertKind.Approval))
                }
                if (notifications.areNotificationsEnabled()) {
                    alerts.forEach { alert ->
                        val target = SessionAlertTarget(SessionHomeId(directory.scope, alert.sessionId), alert.kind)
                        if (directory.authorizes(target.id)) {
                            postAlert(target)
                            posted += target
                        }
                    }
                }
            }
        } finally {
            posted.toList().forEach(::cancelAlert)
            tracker.reset()
        }
    }

    // Kein Titel und kein Projektname: die Meldung erscheint auch auf dem gesperrten Bildschirm.
    private fun postAlert(alert: SessionAlertTarget) {
        val open = PendingIntent.getActivity(
            this,
            ScopedAlertNotificationId,
            Intent(this, MainActivity::class.java)
                .setAction(OpenSessionAlertAction)
                .setData(alert.intentData())
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(ExtraOpenSession, alert.id.sessionId)
                .putExtra(ExtraAlertBackend, alert.id.scope.backend.name)
                .putExtra(ExtraAlertProfile, alert.id.scope.profileId)
                .putExtra(ExtraAlertKind, alert.kind.name),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val approval = alert.kind == SessionAlertKind.Approval
        val notification = NotificationCompat.Builder(this, AlertChannelId)
            .setSmallIcon(R.drawable.ic_harw_notification)
            .setContentTitle(getString(if (approval) R.string.notification_approval_title else R.string.notification_turn_done_title))
            .setContentText(getString(if (approval) R.string.notification_approval_text else R.string.notification_turn_done_text))
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(true)
            .setCategory(if (approval) NotificationCompat.CATEGORY_MESSAGE else NotificationCompat.CATEGORY_STATUS)
            .setPriority(if (approval) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_DEFAULT)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .build()
        notifications.notify(alert.notificationTag(), ScopedAlertNotificationId, notification)
    }

    private fun notification(text: Int): Notification =
        notification(getString(text), stopAction = true, batteryAction = false)

    private fun notification(text: String, stopAction: Boolean, batteryAction: Boolean): Notification {
        val open = PendingIntent.getActivity(
            this, 1, Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 2, Intent(this, HarwConnectionService::class.java).setAction(DisconnectAction),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = NotificationCompat.Builder(this, ChannelId)
            .setSmallIcon(R.drawable.ic_harw_notification)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(text)
            .setContentIntent(open)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        if (stopAction) {
            builder.addAction(0, getString(R.string.notification_stop), stop)
        }
        if (batteryAction) {
            val battery = PendingIntent.getActivity(
                this, 3, Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS),
                PendingIntent.FLAG_IMMUTABLE,
            )
            builder.addAction(0, getString(R.string.notification_battery), battery)
        }
        return builder.build()
    }

    companion object {
        private const val ChannelId = "connection"
        private const val NotificationId = 1
        private const val AlertChannelId = "alerts"

        const val OpenSessionAlertAction = "dev.harw.mobile.OPEN_SESSION_ALERT"
        const val ExtraOpenSession = "dev.harw.mobile.OPEN_SESSION"
        const val ExtraAlertBackend = "dev.harw.mobile.ALERT_BACKEND"
        const val ExtraAlertProfile = "dev.harw.mobile.ALERT_PROFILE"
        const val ExtraAlertKind = "dev.harw.mobile.ALERT_KIND"
        private const val ConnectAction = "dev.harw.mobile.CONNECT"
        private const val ConnectLocalAction = "dev.harw.mobile.CONNECT_LOCAL"
        private const val HoldExistingAction = "dev.harw.mobile.HOLD_WORKSPACE"
        private const val DisconnectAction = "dev.harw.mobile.DISCONNECT"

        // Only an Activity may request a new foreground service here. The
        // Local autostart job never tries to bypass Android's background limit.
        fun startFrom(activity: android.app.Activity) = startFrom(activity, ConnectAction)
        fun startLocalFrom(activity: android.app.Activity) = startFrom(activity, ConnectLocalAction)
        fun holdFrom(activity: android.app.Activity) = startFrom(activity, HoldExistingAction)

        private fun startFrom(activity: android.app.Activity, action: String) {
            ContextCompat.startForegroundService(
                activity, Intent(activity, HarwConnectionService::class.java).setAction(action),
            )
        }
    }
}
