package dev.harw.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.os.Build
import android.os.IBinder
import android.os.PersistableBundle
import android.os.SystemClock
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.harw_mobile_ffi.CatalogModel

internal const val ModelDownloadJobId = 4201
internal const val ModelDownloadNotificationId = 2
internal const val ModelDownloadChannelId = "model_download"
internal const val ExtraModelId = "model_id"
internal const val ExtraWifiOnly = "wifi_only"
internal const val ActionStart = "dev.harw.mobile.MODEL_DOWNLOAD_START"
internal const val ActionPause = "dev.harw.mobile.MODEL_DOWNLOAD_PAUSE"
private const val NotifyMinIntervalMs = 1_000L
private const val FinalNotificationId = ModelDownloadNotificationId + 1
private const val TextVerified = "Pinned version, checksum verified"
private const val TextVerifying = "Verifying…"
private const val TextWaiting = "Waiting for Wi-Fi"

internal enum class DownloadPhase { Downloading, Verifying }

internal sealed interface ModelDownloadState {
    data object Idle : ModelDownloadState
    data class Running(val modelId: String, val phase: DownloadPhase, val done: Long, val total: Long) : ModelDownloadState
    data class WaitingForNetwork(val modelId: String) : ModelDownloadState
    data class Paused(val modelId: String) : ModelDownloadState
    data class Finished(val modelId: String, val result: DownloadResult) : ModelDownloadState
}

internal enum class StartResult { Started, AlreadyRunning, Refused, UnknownModel }

// ---- pure helpers (JVM-testable) ----

internal fun downloadPercent(done: Long, total: Long): Int =
    if (total <= 0L) 0 else (done * 100L / total).coerceIn(0L, 100L).toInt()

/** Notification body text for a state; empty for states without a running notification. */
internal fun notificationLine(state: ModelDownloadState): String = when (state) {
    is ModelDownloadState.Running ->
        if (state.phase == DownloadPhase.Verifying) TextVerifying else "Downloading ${downloadPercent(state.done, state.total)}%"
    is ModelDownloadState.WaitingForNetwork -> TextWaiting
    is ModelDownloadState.Paused -> "Paused"
    is ModelDownloadState.Finished -> when (val r = state.result) {
        is DownloadResult.Done -> TextVerified
        is DownloadResult.Failed -> r.message
        is DownloadResult.NoSpace -> "Not enough free space"
        DownloadResult.Cancelled -> "Paused"
        DownloadResult.WaitingForNetwork -> TextWaiting
    }
    ModelDownloadState.Idle -> ""
}

/** At most one notification update per interval; forced updates (phase change) always pass. */
internal fun shouldNotify(lastMs: Long, nowMs: Long, force: Boolean): Boolean =
    force || lastMs == 0L || nowMs - lastMs >= NotifyMinIntervalMs

/** Terminal mapping of a download result to the state shown in the UI. */
internal fun stateAfter(modelId: String, result: DownloadResult, userPaused: Boolean): ModelDownloadState = when (result) {
    DownloadResult.WaitingForNetwork -> ModelDownloadState.WaitingForNetwork(modelId)
    DownloadResult.Cancelled ->
        if (userPaused) ModelDownloadState.Paused(modelId) else ModelDownloadState.WaitingForNetwork(modelId)
    else -> ModelDownloadState.Finished(modelId, result)
}

internal fun isMeteredAllowed(wifiOnly: Boolean, metered: Boolean): Boolean = !wifiOnly || !metered

// ---- shared state / entry points ----

internal object ModelDownloads {
    private val mutableState = MutableStateFlow<ModelDownloadState>(ModelDownloadState.Idle)
    val state: StateFlow<ModelDownloadState> = mutableState

    internal val active = AtomicBoolean(false)

    @Volatile
    internal var userPaused = false

    fun start(context: Context, modelId: String): StartResult {
        val ctx = context.applicationContext
        val store = ModelStore(ctx)
        val model = ModelManager(store).catalog().firstOrNull { it.id == modelId } ?: return StartResult.UnknownModel
        if (!active.compareAndSet(false, true)) return StartResult.AlreadyRunning
        userPaused = false
        val wifiOnly = store.wifiOnly()
        update(ModelDownloadState.Running(modelId, DownloadPhase.Downloading, 0L, model.sizeBytes.toLong()))
        val started = try {
            if (Build.VERSION.SDK_INT >= 34) {
                val have = File(store.modelsDir(), model.fileName + ".part").let { if (it.isFile) it.length() else 0L }
                val extras = PersistableBundle().apply {
                    putString(ExtraModelId, modelId)
                    putInt(ExtraWifiOnly, if (wifiOnly) 1 else 0)
                }
                val info = JobInfo.Builder(ModelDownloadJobId, ComponentName(ctx, ModelDownloadJob::class.java))
                    .setUserInitiated(true)
                    .setRequiredNetworkType(if (wifiOnly) JobInfo.NETWORK_TYPE_UNMETERED else JobInfo.NETWORK_TYPE_ANY)
                    .setEstimatedNetworkBytes((model.sizeBytes.toLong() - have).coerceAtLeast(0L), 0L)
                    .setExtras(extras)
                    .build()
                val scheduler = checkNotNull(ctx.getSystemService(JobScheduler::class.java))
                scheduler.schedule(info) == JobScheduler.RESULT_SUCCESS
            } else {
                ContextCompat.startForegroundService(
                    ctx,
                    Intent(ctx, ModelDownloadService::class.java).setAction(ActionStart)
                        .putExtra(ExtraModelId, modelId).putExtra(ExtraWifiOnly, wifiOnly),
                )
                true
            }
        } catch (_: Exception) {
            false
        }
        if (!started) {
            active.set(false)
            update(ModelDownloadState.Idle)
            return StartResult.Refused
        }
        return StartResult.Started
    }

    fun pause(context: Context) {
        val ctx = context.applicationContext
        val current = state.value
        val id = when (current) {
            is ModelDownloadState.Running -> current.modelId
            is ModelDownloadState.WaitingForNetwork -> current.modelId
            else -> return
        }
        userPaused = true
        update(ModelDownloadState.Paused(id))
        if (Build.VERSION.SDK_INT >= 34) {
            ctx.getSystemService(JobScheduler::class.java)?.cancel(ModelDownloadJobId)
        } else {
            ctx.stopService(Intent(ctx, ModelDownloadService::class.java))
        }
        active.set(false)
    }

    internal fun update(next: ModelDownloadState) {
        mutableState.value = next
    }
}

// ---- notification ----

private fun ensureChannel(context: Context): NotificationManager {
    val manager = checkNotNull(context.getSystemService(NotificationManager::class.java))
    manager.createNotificationChannel(
        NotificationChannel(
            ModelDownloadChannelId,
            context.getString(R.string.model_download_channel),
            NotificationManager.IMPORTANCE_LOW,
        ),
    )
    return manager
}

private fun openAppIntent(context: Context): PendingIntent = PendingIntent.getActivity(
    context, 0,
    Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
)

private fun buildNotification(context: Context, title: String, state: ModelDownloadState): Notification {
    val running = state is ModelDownloadState.Running || state is ModelDownloadState.WaitingForNetwork
    val builder = NotificationCompat.Builder(context, ModelDownloadChannelId)
        .setSmallIcon(R.drawable.harw_mark)
        .setContentTitle(title)
        .setContentText(notificationLine(state))
        .setOnlyAlertOnce(true)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        .setContentIntent(openAppIntent(context))
    if (running) {
        builder.setOngoing(true)
        if (state is ModelDownloadState.Running && state.phase == DownloadPhase.Downloading) {
            builder.setProgress(100, downloadPercent(state.done, state.total), false)
        } else {
            builder.setProgress(0, 0, true)
        }
        builder.addAction(
            0, "Pause",
            PendingIntent.getBroadcast(
                context, 0,
                Intent(context, ModelDownloadReceiver::class.java).setAction(ActionPause),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            ),
        )
    } else {
        builder.setOngoing(false).setAutoCancel(true)
    }
    return builder.build()
}

private fun postFinal(context: Context, title: String, state: ModelDownloadState) {
    if (state !is ModelDownloadState.Finished) return
    try {
        ensureChannel(context).notify(FinalNotificationId, buildNotification(context, title, state))
    } catch (_: SecurityException) {
        // no notification permission: the download result is still visible in the app
    }
}

/** One download run shared by the job and the service. */
private class DownloadRun(
    private val context: Context,
    private val model: CatalogModel,
    private val networkAllowed: () -> Boolean,
    private val post: (Notification) -> Unit,
) {
    private var lastNotifyMs = 0L

    private fun publish(state: ModelDownloadState, force: Boolean) {
        ModelDownloads.update(state)
        val now = SystemClock.elapsedRealtime()
        if (!shouldNotify(lastNotifyMs, now, force)) return
        lastNotifyMs = now
        try {
            post(buildNotification(context, model.label, state))
        } catch (_: SecurityException) {
            // no notification permission: keep downloading
        }
    }

    suspend fun run(): DownloadResult {
        publish(ModelDownloadState.Running(model.id, DownloadPhase.Downloading, 0L, model.sizeBytes.toLong()), true)
        val result = try {
            ModelManager(ModelStore(context)).download(
                model,
                onProgress = { done, total ->
                    publish(ModelDownloadState.Running(model.id, DownloadPhase.Downloading, done, total), false)
                },
                onVerify = { done, total ->
                    publish(ModelDownloadState.Running(model.id, DownloadPhase.Verifying, done, total), done == 0L)
                },
                networkAllowed = networkAllowed,
            )
        } catch (_: CancellationException) {
            DownloadResult.Cancelled
        } catch (e: Exception) {
            DownloadResult.Failed(e.message ?: "download failed")
        }
        ModelDownloads.update(stateAfter(model.id, result, ModelDownloads.userPaused))
        return result
    }
}

private fun findModel(context: Context, modelId: String?): CatalogModel? =
    ModelManager(ModelStore(context)).catalog().firstOrNull { it.id == modelId }

// ---- API 34+: user-initiated job ----

internal class ModelDownloadJob : JobService() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var work: Job? = null

    @Volatile
    private var stopped = false

    override fun onStartJob(params: JobParameters): Boolean {
        val model = findModel(this, params.extras.getString(ExtraModelId))
        if (model == null) {
            ModelDownloads.update(
                ModelDownloadState.Finished(params.extras.getString(ExtraModelId).orEmpty(), DownloadResult.Failed("unknown model")),
            )
            ModelDownloads.active.set(false)
            return false
        }
        stopped = false
        ModelDownloads.active.set(true)
        val manager = ensureChannel(this)
        val first = buildNotification(this, model.label, ModelDownloadState.Running(model.id, DownloadPhase.Downloading, 0L, model.sizeBytes.toLong()))
        if (Build.VERSION.SDK_INT >= 34) {
            setNotification(params, ModelDownloadNotificationId, first, JOB_END_NOTIFICATION_POLICY_REMOVE)
        }
        val run = DownloadRun(this, model, networkAllowed = { true }) { notification ->
            if (Build.VERSION.SDK_INT >= 34 && !stopped) {
                setNotification(params, ModelDownloadNotificationId, notification, JOB_END_NOTIFICATION_POLICY_REMOVE)
            } else {
                manager.notify(ModelDownloadNotificationId, notification)
            }
        }
        work = scope.launch {
            val result = run.run()
            ModelDownloads.active.set(false)
            postFinal(this@ModelDownloadJob, model.label, stateAfter(model.id, result, ModelDownloads.userPaused))
            if (!stopped) jobFinished(params, result is DownloadResult.WaitingForNetwork)
        }
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        stopped = true
        work?.cancel()
        val paused = ModelDownloads.userPaused
        val id = params.extras.getString(ExtraModelId).orEmpty()
        ModelDownloads.update(if (paused) ModelDownloadState.Paused(id) else ModelDownloadState.WaitingForNetwork(id))
        return !paused
    }

    override fun onDestroy() {
        work?.cancel()
        super.onDestroy()
    }
}

// ---- API 28-33: foreground service dataSync ----

internal class ModelDownloadService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var work: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val modelId = intent?.getStringExtra(ExtraModelId)
        val model = if (intent?.action == ActionStart) findModel(this, modelId) else null
        val wifiOnly = intent?.getBooleanExtra(ExtraWifiOnly, true) ?: true
        if (model == null) {
            // startForegroundService() obliges a startForeground() call; give a short-lived one before leaving.
            runCatching { startForegroundSafely(ensureChannelNotification("")) }
            ModelDownloads.update(ModelDownloadState.Finished(modelId.orEmpty(), DownloadResult.Failed("unknown model")))
            ModelDownloads.active.set(false)
            leave()
            return START_NOT_STICKY
        }
        if (work?.isActive == true) return START_NOT_STICKY
        val connectivity = getSystemService(ConnectivityManager::class.java)
        val allowed = { isMeteredAllowed(wifiOnly, connectivity?.isActiveNetworkMetered ?: true) }
        val first = buildNotification(
            this, model.label,
            if (allowed()) ModelDownloadState.Running(model.id, DownloadPhase.Downloading, 0L, model.sizeBytes.toLong())
            else ModelDownloadState.WaitingForNetwork(model.id),
        )
        try {
            startForegroundSafely(first)
        } catch (e: Exception) {
            ModelDownloads.update(ModelDownloadState.Finished(model.id, DownloadResult.Failed(e.message ?: "cannot start download service")))
            ModelDownloads.active.set(false)
            stopSelf()
            return START_NOT_STICKY
        }
        ModelDownloads.active.set(true)
        if (!allowed()) {
            ModelDownloads.update(ModelDownloadState.WaitingForNetwork(model.id))
            ModelDownloads.active.set(false)
            leave()
            return START_NOT_STICKY
        }
        val manager = ensureChannel(this)
        val run = DownloadRun(this, model, networkAllowed = allowed) { manager.notify(ModelDownloadNotificationId, it) }
        work = scope.launch {
            val result = run.run()
            ModelDownloads.active.set(false)
            leave()
            postFinal(this@ModelDownloadService, model.label, stateAfter(model.id, result, ModelDownloads.userPaused))
        }
        return START_NOT_STICKY
    }

    private fun ensureChannelNotification(text: String): Notification {
        ensureChannel(this)
        return NotificationCompat.Builder(this, ModelDownloadChannelId)
            .setSmallIcon(R.drawable.harw_mark)
            .setContentText(text)
            .build()
    }

    private fun startForegroundSafely(notification: Notification) {
        ensureChannel(this)
        val type = if (Build.VERSION.SDK_INT >= 29) ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC else 0
        ServiceCompat.startForeground(this, ModelDownloadNotificationId, notification, type)
    }

    private fun leave() {
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    override fun onDestroy() {
        work?.cancel()
        super.onDestroy()
    }
}

// ---- notification action ----

internal class ModelDownloadReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == ActionPause) ModelDownloads.pause(context)
    }
}
