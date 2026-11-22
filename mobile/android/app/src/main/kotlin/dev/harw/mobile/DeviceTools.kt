package dev.harw.mobile

import android.content.Context
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.harw_mobile_ffi.MobileCore

/** One shell command waiting for the person's decision. */
internal class DeviceApproval(val argv: List<String>, val cwd: String?, val answer: CompletableDeferred<Boolean>)

/**
 * Shared platform implementations of the agent's device tools: the SAF-backed [SafStore] and the
 * [TermuxBridge]. [register] hands them to the Rust core before the on-device host starts.
 * Shell commands wait for an in-app approval dialog ([DeviceApprovalHost]); the bridge itself
 * adds a 60 s timeout that counts as declined.
 */
internal object DeviceTools {
    private val pendingState = MutableStateFlow<DeviceApproval?>(null)
    private val gate = Mutex()

    @Volatile private var saf: SafStore? = null
    @Volatile private var termux: TermuxBridge? = null

    val pending: StateFlow<DeviceApproval?> get() = pendingState

    fun files(context: Context): SafStore =
        saf ?: synchronized(this) { saf ?: SafStore(context.applicationContext).also { saf = it } }

    fun shell(context: Context): TermuxBridge =
        termux ?: synchronized(this) {
            termux ?: TermuxBridge(context.applicationContext, CommandApprover { argv, cwd -> askPerson(argv, cwd) }).also { termux = it }
        }

    private suspend fun askPerson(argv: List<String>, cwd: String?): Boolean =
        gate.withLock {
            val request = DeviceApproval(argv, cwd, CompletableDeferred())
            pendingState.value = request
            try {
                request.answer.await()
            } finally {
                if (pendingState.value === request) pendingState.value = null
            }
        }

    /** Registers shell and file access with the core; call before `connectLocal`. */
    fun register(context: Context, core: MobileCore) {
        core.setDeviceTools(shell(context), files(context))
    }
}

/** Approval dialog for agent shell commands; place it once above all screens. */
@Composable
internal fun DeviceApprovalHost() {
    val request by DeviceTools.pending.collectAsState()
    val current = request ?: return
    AlertDialog(
        onDismissRequest = { current.answer.complete(false) },
        title = { Text("Befehl in Termux ausführen?") },
        text = {
            val where = current.cwd?.let { "\nOrdner: $it" } ?: ""
            Text("Der Agent möchte diesen Befehl ausführen:\n\n" + current.argv.joinToString(" ") + where)
        },
        confirmButton = { TextButton(onClick = { current.answer.complete(true) }) { Text("Erlauben") } },
        dismissButton = { TextButton(onClick = { current.answer.complete(false) }) { Text("Ablehnen") } },
    )
}
