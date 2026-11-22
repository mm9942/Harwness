package dev.harw.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlin.coroutines.coroutineContext

/** Update-Dienst der App (upd.harw.dev): Manifest, SSE-Strom und APK-Download. */
internal const val UpdateBase = "https://upd.harw.dev"
private const val UpdatePrefs = "harw_update"
private const val UpdateChannel = "harw_updates"
private const val UpdateJobId = 7411
internal const val ExtraOpenUpdate = "dev.harw.mobile.OPEN_UPDATE"

internal data class UpdateInfo(
    val versionCode: Int,
    val versionName: String,
    val notes: String,
    val apkUrl: String,
    val sha256: String,
    val size: Long,
)

internal fun parseUpdate(json: String): UpdateInfo? = try {
    val o = JSONObject(json)
    val url = o.getString("apkUrl")
    // Nur der eigene Dienst darf Pakete liefern.
    if (!url.startsWith("$UpdateBase/apk/")) null
    else UpdateInfo(
        o.getInt("versionCode"), o.optString("versionName"), o.optString("notes"),
        url, o.getString("sha256").lowercase(), o.optLong("size"),
    )
} catch (_: Exception) {
    null
}

internal object AppUpdates {
    val pending = MutableStateFlow<UpdateInfo?>(null)

    fun installedCode(context: Context): Int =
        @Suppress("DEPRECATION") context.packageManager.getPackageInfo(context.packageName, 0).versionCode

    fun installedName(context: Context): String =
        context.packageManager.getPackageInfo(context.packageName, 0).versionName.orEmpty()

    private fun prefs(context: Context) = context.getSharedPreferences(UpdatePrefs, Context.MODE_PRIVATE)

    fun fetchLatest(): UpdateInfo? = try {
        val c = URL("$UpdateBase/v1/latest").openConnection() as HttpURLConnection
        c.connectTimeout = 10_000; c.readTimeout = 15_000
        c.inputStream.bufferedReader().use { parseUpdate(it.readText()) }
    } catch (_: Exception) {
        null
    }

    /** Setzt das Angebot, wenn es neuer ist und nicht innerhalb der letzten 24 h auf "Später" gestellt wurde. */
    fun consider(context: Context, info: UpdateInfo, force: Boolean = false) {
        if (info.versionCode <= installedCode(context)) { pending.value = null; return }
        val p = prefs(context)
        val snoozed = p.getInt("snooze_code", 0) == info.versionCode &&
            System.currentTimeMillis() - p.getLong("snooze_at", 0) < 24 * 3600_000L
        if (!snoozed || force) pending.value = info
    }

    fun snooze(context: Context, info: UpdateInfo) {
        prefs(context).edit().putInt("snooze_code", info.versionCode).putLong("snooze_at", System.currentTimeMillis()).apply()
        pending.value = null
    }

    /** SSE: bleibt offen, bis die Coroutine endet; Verbindungsabbrüche werden mit Pause wiederholt. */
    suspend fun listen(context: Context) {
        val job = coroutineContext.job
        withContext(Dispatchers.IO) {
            var wait = 3_000L
            while (isActive) {
                var conn: HttpURLConnection? = null
                try {
                    conn = URL("$UpdateBase/v1/events").openConnection() as HttpURLConnection
                    conn.connectTimeout = 10_000
                    conn.readTimeout = 70_000
                    conn.setRequestProperty("Accept", "text/event-stream")
                    val c = conn
                    job.invokeOnCompletion { c.disconnect() }
                    var event = ""
                    c.inputStream.bufferedReader().useLines { lines ->
                        wait = 3_000L
                        for (line in lines) {
                            when {
                                line.startsWith("event:") -> event = line.removePrefix("event:").trim()
                                line.startsWith("data:") && event == "update" ->
                                    parseUpdate(line.removePrefix("data:").trim())?.let { consider(context, it) }
                                line.isEmpty() -> event = ""
                            }
                        }
                    }
                } catch (_: Exception) {
                } finally {
                    conn?.disconnect()
                }
                if (!isActive) break
                delay(wait)
                wait = (wait * 2).coerceAtMost(60_000L)
            }
        }
    }

    fun schedule(context: Context) {
        val scheduler = context.getSystemService(JobScheduler::class.java)
        if (scheduler.getPendingJob(UpdateJobId) != null) return
        scheduler.schedule(
            JobInfo.Builder(UpdateJobId, ComponentName(context, UpdateCheckJob::class.java))
                .setPeriodic(30 * 60_000L)
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                .setPersisted(true)
                .build(),
        )
    }

    /** Benachrichtigung, wenn die App nicht offen ist; pro Version nur einmal. */
    fun notify(context: Context, info: UpdateInfo) {
        val p = prefs(context)
        if (p.getInt("notified_code", 0) >= info.versionCode) return
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(UpdateChannel, "App-Updates", NotificationManager.IMPORTANCE_DEFAULT))
        val open = PendingIntent.getActivity(
            context, 0,
            Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
                .putExtra(ExtraOpenUpdate, true),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = Notification.Builder(context, UpdateChannel)
            .setSmallIcon(R.drawable.ic_harw_notification)
            .setContentTitle("Harw ${info.versionName} ist verfügbar")
            .setContentText("Tippe, um Neuerungen zu sehen und zu installieren.")
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        try {
            nm.notify(UpdateJobId, n)
            p.edit().putInt("notified_code", info.versionCode).apply()
        } catch (_: SecurityException) {
        }
    }

    /** Lädt die APK, prüft SHA-256 und gibt die Datei zurück (null bei Fehler). */
    suspend fun download(context: Context, info: UpdateInfo, onProgress: (Float) -> Unit): File? = withContext(Dispatchers.IO) {
        try {
            val dir = File(context.cacheDir, "updates").apply { mkdirs(); listFiles()?.forEach { it.delete() } }
            val out = File(dir, "harw-${info.versionCode}.apk")
            val c = URL(info.apkUrl).openConnection() as HttpURLConnection
            c.connectTimeout = 15_000; c.readTimeout = 30_000
            val total = c.contentLengthLong.takeIf { it > 0 } ?: info.size
            val md = MessageDigest.getInstance("SHA-256")
            var done = 0L
            c.inputStream.use { input ->
                out.outputStream().use { os ->
                    val buf = ByteArray(64 * 1024)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        os.write(buf, 0, n); md.update(buf, 0, n); done += n
                        if (total > 0) onProgress((done.toFloat() / total).coerceIn(0f, 1f))
                    }
                }
            }
            val hex = md.digest().joinToString("") { "%02x".format(it) }
            if (hex != info.sha256) { out.delete(); null } else out
        } catch (_: Exception) {
            null
        }
    }

    /** true, wenn der Installer geöffnet wurde; false, wenn zuerst die Berechtigung "Unbekannte Apps" nötig ist. */
    fun install(context: Context, apk: File): Boolean {
        if (!context.packageManager.canRequestPackageInstalls()) {
            context.startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
            return false
        }
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.updates", apk)
        context.startActivity(
            Intent(Intent.ACTION_VIEW).setDataAndType(uri, "application/vnd.android.package-archive")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK),
        )
        return true
    }
}

/** Periodische Prüfung im Hintergrund; meldet neue Versionen per Benachrichtigung. */
internal class UpdateCheckJob : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
        Thread {
            val info = AppUpdates.fetchLatest()
            if (info != null && info.versionCode > AppUpdates.installedCode(this)) AppUpdates.notify(this, info)
            jobFinished(params, false)
        }.start()
        return true
    }

    override fun onStopJob(params: JobParameters) = true
}

/** Hält die Verbindung zum Update-Strom, solange die App sichtbar ist, und zeigt das Update-Fenster. */
@Composable
internal fun UpdateHost(openRequested: Boolean) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            launch(Dispatchers.IO) {
                AppUpdates.fetchLatest()?.let { AppUpdates.consider(context, it, force = openRequested) }
            }
            AppUpdates.listen(context)
        }
    }
    val info by AppUpdates.pending.collectAsState()
    val offer = info ?: return
    val scope = rememberCoroutineScope()
    var showNotes by remember(offer.versionCode) { mutableStateOf(false) }
    var progress by remember(offer.versionCode) { mutableFloatStateOf(-1f) }
    var message by remember(offer.versionCode) { mutableStateOf<String?>(null) }
    val busy = progress >= 0f
    AlertDialog(
        onDismissRequest = { if (!busy) AppUpdates.snooze(context, offer) },
        title = { Text("Update verfügbar") },
        text = {
            Column(Modifier.fillMaxWidth(), verticalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp)) {
                Text("Version ${offer.versionName} ist erschienen (installiert: ${AppUpdates.installedName(context)}).")
                if (offer.size > 0) Text("Größe: ${offer.size / 1_000_000} MB", style = MaterialTheme.typography.labelMedium)
                if (offer.notes.isNotBlank()) {
                    TextButton(onClick = { showNotes = !showNotes }) { Text(if (showNotes) "Release Notes ausblenden" else "Release Notes anzeigen") }
                    if (showNotes) {
                        Column(Modifier.heightIn(max = 260.dp).verticalScroll(rememberScrollState())) {
                            Text(offer.notes, style = MaterialTheme.typography.bodyMedium)
                        }
                    }
                }
                if (busy) {
                    LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth().padding(top = 4.dp))
                    Text(
                        "Lädt … ${(progress * 100).toInt()} %" + if (offer.size > 0) " von ${offer.size / 1_000_000} MB" else "",
                        style = MaterialTheme.typography.labelMedium,
                    )
                }
                message?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            }
        },
        confirmButton = {
            TextButton(
                enabled = !busy,
                onClick = {
                    message = null; progress = 0f
                    scope.launch {
                        val file = AppUpdates.download(context, offer) { progress = it }
                        progress = -1f
                        if (file == null) message = "Download fehlgeschlagen oder Prüfsumme falsch."
                        else if (!AppUpdates.install(context, file)) message = "Erlaube der App die Installation und tippe dann erneut auf Jetzt installieren."
                    }
                },
            ) { Text("Jetzt installieren") }
        },
        dismissButton = { TextButton(enabled = !busy, onClick = { AppUpdates.snooze(context, offer) }) { Text("Später") } },
    )
}
