package dev.harw.mobile

import android.os.SystemClock
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.lifecycle.SavedStateHandle
import kotlinx.coroutines.launch
import java.nio.ByteBuffer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ManualCloudWorkAccess

internal class ManualCloudWorkTarget(
    val native: ManualCloudWorkAccess,
    val generation: Long,
    val profileLabel: String,
)

internal sealed interface ManualCloudWorkReceipt {
    data object Empty : ManualCloudWorkReceipt
    data object Pending : ManualCloudWorkReceipt
    data object Unconfirmed : ManualCloudWorkReceipt
    data object Unavailable : ManualCloudWorkReceipt
    class Confirmed(val sessionId: String) : ManualCloudWorkReceipt
}

internal class ManualCloudWorkPort(private val call: (String, Int, ByteBuffer) -> Long) {
    suspend fun attempt(target: ManualCloudWorkTarget, action: Int): ManualCloudWorkReceipt =
        withContext(Dispatchers.IO) {
            if (!target.native.isCurrent()) return@withContext ManualCloudWorkReceipt.Unavailable
            val buffer = ByteBuffer.allocateDirect(128)
            try {
                val status = call(target.native.routeTicket(), action, buffer)
                if (!target.native.isCurrent()) return@withContext ManualCloudWorkReceipt.Unconfirmed
                when {
                    status == 0L -> ManualCloudWorkReceipt.Empty
                    action == 0 && status == 1L -> ManualCloudWorkReceipt.Pending
                    status == -8L -> ManualCloudWorkReceipt.Unconfirmed
                    action != 0 && status in 1L..128L -> {
                        val id = ByteArray(status.toInt()).also { buffer.get(it) }.decodeToString(throwOnInvalidSequence = true)
                        if (id.all { it in '!'..'~' }) ManualCloudWorkReceipt.Confirmed(id)
                        else ManualCloudWorkReceipt.Unconfirmed
                    }
                    action != 0 -> ManualCloudWorkReceipt.Unconfirmed
                    else -> ManualCloudWorkReceipt.Unavailable
                }
            } catch (_: LinkageError) {
                ManualCloudWorkReceipt.Unavailable
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Exception) {
                ManualCloudWorkReceipt.Unconfirmed
            } finally {
                buffer.clear()
                while (buffer.hasRemaining()) buffer.put(0)
            }
        }
}

internal class ManualCloudWorkSelection {
    var openSession by mutableStateOf<LocalAssistantSearchedWork?>(null)
    var work: LocalAssistantSearchedWork? = null
    var receipt by mutableStateOf<ManualCloudWorkReceipt>(ManualCloudWorkReceipt.Unavailable)
}

@Composable
internal fun ManualCloudWorkCard(
    target: ManualCloudWorkTarget, port: ManualCloudWorkPort, selection: ManualCloudWorkSelection,
    allowed: () -> Boolean,
) {
    val latestAllowed by rememberUpdatedState(allowed)
    val scope = rememberCoroutineScope()
    var loading by remember(target) { mutableStateOf(false) }
    var confirm by remember(target) { mutableStateOf(false) }
    LaunchedEffect(target, selection) {
        if (latestAllowed() && selection.receipt == ManualCloudWorkReceipt.Unavailable) {
            val result = port.attempt(target, 0) // Local vault read: no RPC.
            if (latestAllowed()) selection.receipt = result
        }
    }
    fun request(action: Int) {
        if (!latestAllowed() || loading) return
        loading = true
        scope.launch {
            try {
                val result = port.attempt(target, action)
                if (!latestAllowed()) return@launch
                if (result is ManualCloudWorkReceipt.Confirmed && selection.work?.link?.sessionId != result.sessionId) {
                    val link = LocalUiaWorkLink(result.sessionId, target.generation, "Cloud-Arbeitssession",
                        System.currentTimeMillis(), SystemClock.elapsedRealtime())
                    selection.work = LocalAssistantSearchedWork(link, HarwWindowModel(SavedStateHandle()).also { it.select(result.sessionId) })
                }
                // An attempted publication with a missing record is still uncertain; don't show a new Create.
                selection.receipt = if (result == ManualCloudWorkReceipt.Empty) ManualCloudWorkReceipt.Unconfirmed else result
            } finally { loading = false }
        }
    }
    when (selection.receipt) {
        ManualCloudWorkReceipt.Empty -> OutlinedButton(onClick = { confirm = true }, enabled = !loading) {
            Text("Leere Cloud-Arbeitssession anlegen")
        }
        ManualCloudWorkReceipt.Pending, ManualCloudWorkReceipt.Unconfirmed -> {
            OutlinedButton(onClick = { request(2) }, enabled = !loading) { Text("Cloud-Anlage prüfen") }
            Text("Nur die frühere Anfrage wird geprüft. Es wird keine zweite Session angelegt.")
        }
        is ManualCloudWorkReceipt.Confirmed -> OutlinedButton(
            enabled = !loading && selection.work != null,
            onClick = { if (latestAllowed()) selection.openSession = selection.work },
        ) { Text("Bestätigte Cloud-Arbeitssession öffnen") }
        ManualCloudWorkReceipt.Unavailable -> Unit
    }
    if (loading) Text("Cloud-Arbeitssession wird geprüft …")
    if (confirm) AlertDialog(
        onDismissRequest = { confirm = false },
        title = { Text("Leere Cloud-Arbeitssession anlegen?") },
        text = { Text("Ziel: "+target.profileLabel+". Dein privater Verlauf, Entwurf und deine Merkliste bleiben auf dem Gerät. Ohne eine separat gesendete Nachricht startet kein Modell.") },
        dismissButton = { TextButton(onClick = { confirm = false }) { Text("Abbrechen") } },
        confirmButton = { TextButton(onClick = { confirm = false; request(1) }) { Text("Leer anlegen") } },
    )
}
