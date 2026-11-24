package dev.harw.mobile

import android.app.Activity

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** The authenticated PIN session, never selectable by raw vault identity or handle. */
internal interface UiaRoomChatSession : VaultPinSession {
    fun chat(): UiaRoomChatConnection?
    fun memory(): UiaManualMemoryPort? = null
    fun cloudWork(profile: String): ManualCloudWorkPort? = null
}

internal enum class UiaPriorAttempt { None, Interrupted, Unknown }

internal sealed interface UiaNativeRead {
    class Snapshot(
        val id: UiaRoomId, val revision: Long, val pairs: List<UiaCommittedPair>,
        val priorAttempt: UiaPriorAttempt = UiaPriorAttempt.None,
    ) : UiaNativeRead
    class Failure(val code: Long) : UiaNativeRead
}

/** Native bindings retain all opaque tokens privately. No methods select a scope/identity. */
internal interface UiaNativeChat {
    fun snapshot(): UiaNativeRead
    fun reserve(attempt: Long): Boolean
    fun run(attempt: Long, revision: Long, text: String): Long
    // Older/test ports may not support vision. Never fall back to a text-only send.
    fun runPhoto(attempt: Long, revision: Long, text: String, image: PrivateLibraryAssistantImage): Long = -10L
    fun cancel(attempt: Long)
    fun supportsParking(photo: Boolean): Boolean = false
    fun park(attempt: Long): Boolean = false
}

/** Per-authenticated-lease projection of durable data. No saveable or global state. */
internal class UiaRoomChatConnection(
    private val port: UiaNativeChat,
    private val work: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
    private val vaultScope: VaultPinScope? = null,
) {
    private val guard = Any()
    private val state = MutableStateFlow<UiaRoomState>(UiaRoomState.Opening)
    val states: StateFlow<UiaRoomState> = state.asStateFlow()
    private var closed = false
    private var lease: UiaRoomUnlockLease? = null
    private var sequence = 0L
    private class Flight(val marker: UiaRoomAttempt, val serial: Long, val ticket: PrivateAssistantAttemptAdmission.Ticket?)
    private var active: Flight? = null
    private var pendingRead: Any? = null
    private var lastRevision = -1L

    init { reload() }

    fun reload() = synchronized(guard) {
        if (closed || active != null) return@synchronized
        val version = Any()
        pendingRead = version
        state.value = UiaRoomState.Opening
        work.launch {
            val read = safeRead()
            synchronized(guard) {
                if (!closed && active == null && pendingRead === version) {
                    pendingRead = null
                    publish(read)
                }
            }
        }
        Unit
    }

    fun submit(request: UiaRoomSend, activity: Activity? = null, profile: LocalUiaProfile? = null): Boolean = synchronized(guard) {
        val current = state.value
        if (closed || active != null || !request.isFor(current) || current !is UiaRoomState.Open || sequence == Long.MAX_VALUE) {
            return@synchronized false
        }
        val serial = ++sequence
        val keepRunning = activity != null && profile != null && vaultScope != null &&
            safely(false) { port.supportsParking(photo = request.image != null) }
        val ticket = if (keepRunning && activity != null && profile != null && vaultScope != null) {
            PrivateAssistantAttemptAdmission.request(activity, profile, vaultScope) {
                // Cancellation before native reservation merely withdraws the
                // foreground ticket; after detach only exact native Stop exists.
                synchronized(guard) { safely(Unit) { port.cancel(serial) } }
            } ?: run {
                state.value = UiaRoomState.Open(current.snapshot, UiaRoomWork.Failed(UiaRoomRetryable.ForegroundDenied))
                return@synchronized false
            }
        } else null
        if (ticket == null && !safely(false) { port.reserve(serial) }) return@synchronized false
        val flight = Flight(UiaRoomAttempt(), serial, ticket)
        active = flight
        state.value = UiaRoomState.Open(current.snapshot, UiaRoomWork.Processing(flight.marker))
        work.launch {
            val disposition = if (ticket == null) 1L else if (!ticket.awaitForeground()) -13L else
                synchronized(guard) {
                    when {
                        closed || active !== flight || !ticket.isForeground() -> -13L
                        !safely(false) { port.reserve(serial) } -> -3L
                        else -> 1L
                    }
                }
            val result = if (disposition != 1L) disposition else safely(-8L) {
                val photo = request.image
                if (photo == null) port.run(serial, request.expectedRevision, request.text)
                else port.runPhoto(serial, request.expectedRevision, request.text, photo.selection)
            }
            val read = if (result == 1L && synchronized(guard) { !closed && active === flight }) safeRead() else null
            synchronized(guard) finish@{
                if (closed || active !== flight) return@finish
                val visible = (state.value as? UiaRoomState.Open)?.snapshot ?: return@finish
                active = null
                if (read != null) {
                    publish(read, visible.revision + 1)
                } else {
                    state.value = when (result) {
                        -1L -> UiaRoomState.Locked
                        -2L -> UiaRoomState.Unavailable(UiaRoomIssue.Vault)
                        -4L -> UiaRoomState.Unavailable(UiaRoomIssue.LocalModel)
                        -5L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.Cancelled))
                        -6L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.Incomplete))
                        -3L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.Busy))
                        -7L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.Inference))
                        -10L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.PhotoModel))
                        -11L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.PhotoChanged))
                        -12L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.PhotoInvalid))
                        -13L -> UiaRoomState.Open(visible, UiaRoomWork.Failed(UiaRoomRetryable.ForegroundDenied))
                        else -> UiaRoomState.Unavailable(UiaRoomIssue.Reconciliation)
                    }
                }
            }
        }.invokeOnCompletion {
            // Also runs when hard relock cancels a queued coroutine before its
            // body begins. A running native call retains the single FGS slot.
            ticket?.finish()
        }
        true
    }

    fun cancel(request: UiaRoomCancel) = synchronized(guard) {
        val flight = active ?: return@synchronized
        if (!closed && request.isFor(state.value)) {
            if (flight.ticket == null) safely(Unit) { port.cancel(flight.serial) } else flight.ticket.stop()
        }
    }

    /**
     * Called only by Activity hiding, not explicit UI lock. Native proves the
     * exact attempt (including an eligible local photo) has crossed admission and drops reader/Library authority.
     * A queued, unsupported, or failed transition is never a retained reader.
     */
    fun parkForActivity(): Boolean = synchronized(guard) {
        val work = active ?: return@synchronized false
        val ticket = work.ticket ?: return@synchronized false
        if (closed || !ticket.isForeground() || !safely(false) { port.park(work.serial) }) return@synchronized false
        // Native now owns only the write permit. If Android locked during
        // the handoff, revoke it immediately, never reopen the read lease.
        if (!ticket.isForeground()) ticket.stop() else ticket.markParked()
        true
    }

    fun dismissIssue(expected: UiaRoomUnlockLease, issue: UiaRoomWork.Failed) = synchronized(guard) {
        val showing = state.value as? UiaRoomState.Open
        if (!closed && showing?.snapshot?.lease === expected && showing.work === issue) {
            state.value = UiaRoomState.Open(showing.snapshot, UiaRoomWork.Idle)
        }
    }

    /** Synchronous presentation fence. Native owner separately cancels/revokes. */
    fun onRevoked(parked: Boolean = false) {
        synchronized(guard) {
            closed = true
            if (!parked) active?.ticket?.stop()
            active = null
            pendingRead = null
            lease = null
            lastRevision = -1L
            state.value = UiaRoomState.Locked
        }
        if (!parked) work.cancel()
    }

    private fun safeRead(): UiaNativeRead = safely(UiaNativeRead.Failure(-2L)) { port.snapshot() }

    private fun publish(read: UiaNativeRead, requiredRevision: Long = lastRevision) {
        state.value = when (read) {
            is UiaNativeRead.Failure -> when (read.code) {
                -1L -> UiaRoomState.Locked
                -8L -> UiaRoomState.Unavailable(UiaRoomIssue.Reconciliation)
                else -> UiaRoomState.Unavailable(UiaRoomIssue.Vault)
            }
            is UiaNativeRead.Snapshot -> {
                val bound = lease
                if (read.revision < requiredRevision || (bound != null && bound.roomId != read.id)) {
                    UiaRoomState.Unavailable(UiaRoomIssue.Vault)
                } else {
                    val selected = bound ?: UiaRoomUnlockLease(read.id)
                    val snapshot = UiaRoomSnapshot.fromCore(selected, read.revision, read.pairs)
                    if (snapshot == null) UiaRoomState.Unavailable(UiaRoomIssue.Vault)
                    else {
                        lease = selected
                        lastRevision = read.revision
                        val previous = when (read.priorAttempt) {
                            UiaPriorAttempt.None -> UiaRoomWork.Idle
                            UiaPriorAttempt.Interrupted -> UiaRoomWork.Failed(UiaRoomRetryable.Interrupted)
                            UiaPriorAttempt.Unknown -> UiaRoomWork.Failed(UiaRoomRetryable.AttemptUnknown)
                        }
                        UiaRoomState.Open(snapshot, previous)
                    }
                }
            }
        }
    }

    private inline fun <T> safely(fallback: T, block: () -> T): T = try {
        block()
    } catch (_: LinkageError) {
        fallback
    } catch (_: Exception) {
        fallback
    }
}
