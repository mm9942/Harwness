package dev.harw.mobile

import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.harw_mobile_ffi.CoreListener

/**
 * Bridges the core's change notifications to coroutines.
 *
 * The callbacks arrive on the core's dedicated notify thread ("harw-mobile-notify"),
 * so they only signal conflated channels or record an id: they never block, never
 * touch the main thread and never throw.
 */
internal class CoreEvents : CoreListener {
    private val status = Channel<Unit>(Channel.CONFLATED)
    private val sessions = Channel<Unit>(Channel.CONFLATED)
    private val lock = Any()
    private val pending = LinkedHashSet<String>()

    override fun statusChanged() {
        status.trySend(Unit)
    }

    override fun sessionsChanged() {
        sessions.trySend(Unit)
    }

    override fun sessionChanged(sessionId: String) {
        synchronized(lock) { pending.add(sessionId) }
        status.trySend(Unit)
    }

    /** True when a status change was signalled, false when [timeoutMs] elapsed first. */
    suspend fun awaitStatus(timeoutMs: Long): Boolean =
        withTimeoutOrNull(timeoutMs) { status.receive() } != null

    /** True when a session list change was signalled, false when [timeoutMs] elapsed first. */
    suspend fun awaitSessions(timeoutMs: Long): Boolean =
        withTimeoutOrNull(timeoutMs) { sessions.receive() } != null

    /** Returns the session ids changed since the last call and clears them. */
    fun takeChangedSessions(): Set<String> = synchronized(lock) {
        val copy = pending.toSet()
        pending.clear()
        copy
    }
}
