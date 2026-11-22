package dev.harw.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
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
            if (state == MobileSessionState.WAITING_FOR_APPROVAL) {
                alerts += SessionAlert(id, SessionAlertKind.Approval)
            } else {
                if (old == MobileSessionState.WAITING_FOR_APPROVAL) cleared += id
                if (state == MobileSessionState.IDLE && old in busyStates) {
                    alerts += SessionAlert(id, SessionAlertKind.TurnEnd)
                }
            }
        }
        for (id in before.keys) if (id !in next) cleared += id
        return alerts to cleared
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

// Stabile Benachrichtigungs-Id je Session und Art, ohne die Id selbst preiszugeben.
internal fun alertNotificationId(sessionId: String, kind: SessionAlertKind): Int =
    AlertIdBase + (sessionId.hashCode() and 0x3fffffff) % 1_000_000 * 2 + kind.ordinal

private const val AlertIdBase = 1000

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
        when (intent?.action) {
            DisconnectAction -> {
                writeManualDisconnect(this, true)
                connection.disconnect()
                localConnection.disconnect()
                if (!foreground) stopSelfResult(startId)
            }
            ConnectAction -> if (foreground) {
                connection.connect()
                localConnection.connectLocal()
            } else if (!connection.state.value.stopping) {
                try {
                    val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
                    ServiceCompat.startForeground(
                        this, NotificationId, notification(R.string.notification_connecting), type,
                    )
                    foreground = true
                    // Cloud und Lokal laufen gleichzeitig; ein fehlendes Cloud-Profil stoppt Lokal nicht.
                    val cloudStarted = connection.connect()
                    val localStarted = localConnection.connectLocal()
                    if (cloudStarted || localStarted) {
                        if (observation?.isActive != true) observeConnection()
                        if (alertObservation?.isActive != true) observeAlerts()
                    } else {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                        foreground = false
                        stopSelfResult(startId)
                    }
                } catch (error: Exception) {
                    connection.reportServiceError(error)
                    stopSelfResult(startId)
                }
            }
            else -> stopSelfResult(startId)
        }
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        scope.cancel()
        connection.disconnect()
        localConnection.disconnect()
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
            combine(connection.state, localConnection.state) { c, l ->
                val active = setOf(ConnectionPhase.CONNECTING, ConnectionPhase.CONNECTED, ConnectionPhase.RECONNECTING)
                val primary = if (c.phase !in active && l.phase in active) l else c
                Observed(primary.phase, c.stopping || l.stopping, c.forgetting, primary.attempt)
            }
                .distinctUntilChanged()
                .collect { (phase, stopping, forgetting, attempt) ->
                    val reconnecting = !forgetting && !stopping && phase == ConnectionPhase.RECONNECTING
                    val text = when {
                        forgetting -> getString(R.string.notification_forgetting)
                        stopping -> getString(R.string.notification_disconnecting)
                        else -> when (phase) {
                            ConnectionPhase.CONNECTING -> getString(R.string.notification_connecting)
                            ConnectionPhase.CONNECTED -> getString(R.string.notification_connected)
                            ConnectionPhase.RECONNECTING -> reconnectAttemptArg(attempt)
                                ?.let { getString(R.string.notification_reconnecting_attempt, it) }
                                ?: getString(R.string.notification_reconnecting)
                            else -> null
                        }
                    }
                    if (text == null) {
                        stopForeground(STOP_FOREGROUND_REMOVE)
                        foreground = false
                        stopSelfResult(latestStart)
                    } else {
                        val power = getSystemService(PowerManager::class.java)
                        val battery = reconnecting && power != null &&
                            !power.isIgnoringBatteryOptimizations(packageName)
                        notifications.notify(
                            NotificationId,
                            notification(text, stopAction = !forgetting && !stopping, batteryAction = battery),
                        )
                    }
                }
        }
    }

    private fun observeAlerts() {
        alertObservation = scope.launch {
            launch { observeAlerts(connection, alertTracker) }
            launch { observeAlerts(localConnection, localAlertTracker) }
        }
    }

    private suspend fun observeAlerts(source: HarwConnection, tracker: SessionAlertTracker) {
        source.state
            .map { if (it.online && it.sessionsLoaded) it.sessions else null }
            .distinctUntilChanged()
            .collect { sessions ->
                if (sessions == null) {
                    tracker.reset()
                    return@collect
                }
                val (alerts, cleared) = tracker.update(sessions)
                for (id in cleared) notifications.cancel(alertNotificationId(id, SessionAlertKind.Approval))
                if (notifications.areNotificationsEnabled()) alerts.forEach { postAlert(it) }
            }
    }

    // Kein Titel und kein Projektname: die Meldung erscheint auch auf dem gesperrten Bildschirm.
    private fun postAlert(alert: SessionAlert) {
        val open = PendingIntent.getActivity(
            this,
            alertNotificationId(alert.sessionId, alert.kind),
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(ExtraOpenSession, alert.sessionId),
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
        notifications.notify(alertNotificationId(alert.sessionId, alert.kind), notification)
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

        /** Intent-Extra mit der Session-Id; MainActivity (A6) löst Projekt -> Session daraus auf. */
        const val ExtraOpenSession = "dev.harw.mobile.OPEN_SESSION"
        private const val ConnectAction = "dev.harw.mobile.CONNECT"
        private const val DisconnectAction = "dev.harw.mobile.DISCONNECT"

        fun startFrom(activity: Context) {
            ContextCompat.startForegroundService(
                activity, Intent(activity, HarwConnectionService::class.java).setAction(ConnectAction),
            )
        }
    }
}
