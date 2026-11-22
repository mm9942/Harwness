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
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.ConnectionPhase

// attempt == 0u -> null (Basistext verwenden); sonst die Versuchszahl als Int für %1$d.
internal fun reconnectAttemptArg(attempt: UInt): Int? =
    if (attempt == 0u) null else attempt.toInt()

internal class HarwConnectionService : Service() {
    private val connection get() = (application as HarwApplication).connection
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private lateinit var notifications: NotificationManager
    private var foreground = false
    private var latestStart = 0
    private var observation: Job? = null

    override fun onCreate() {
        super.onCreate()
        notifications = checkNotNull(getSystemService(NotificationManager::class.java))
        notifications.createNotificationChannel(
            NotificationChannel(ChannelId, getString(R.string.connection_channel), NotificationManager.IMPORTANCE_LOW),
        )
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStart = startId
        when (intent?.action) {
            DisconnectAction -> {
                connection.disconnect()
                if (!foreground) stopSelfResult(startId)
            }
            ConnectAction -> if (!foreground && !connection.state.value.stopping) {
                try {
                    val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
                    ServiceCompat.startForeground(
                        this, NotificationId, notification(R.string.notification_connecting), type,
                    )
                    foreground = true
                    if (connection.connect()) {
                        if (observation?.isActive != true) observeConnection()
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
            connection.state
                .map { Observed(it.phase, it.stopping, it.forgetting, it.attempt) }
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
        private const val ConnectAction = "dev.harw.mobile.CONNECT"
        private const val DisconnectAction = "dev.harw.mobile.DISCONNECT"

        fun startFrom(activity: Context) {
            ContextCompat.startForegroundService(
                activity, Intent(activity, HarwConnectionService::class.java).setAction(ConnectAction),
            )
        }
    }
}
