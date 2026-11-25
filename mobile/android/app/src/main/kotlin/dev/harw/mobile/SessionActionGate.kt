package dev.harw.mobile

internal enum class SessionActionKind { Send, Interrupt, Decide }

internal data class SessionAction(val sessionId: String, val kind: SessionActionKind)

// Accessed by the single process controller on the main dispatcher.
internal class SessionActionGate {
    class Ticket(val generation: Long, val action: SessionAction)

    private var nextGeneration = 0L
    var generation: Long? = null
        private set

    private val pending = mutableSetOf<SessionAction>()

    fun open(): Long {
        val next = ++nextGeneration
        generation = next
        return next
    }

    fun close() {
        generation = null
        pending.clear()
    }

    fun begin(generation: Long, action: SessionAction): Ticket? {
        if (generation != this.generation || !pending.add(action)) return null
        return Ticket(generation, action)
    }

    fun finish(ticket: Ticket): Boolean =
        ticket.generation == generation && pending.remove(ticket.action)

}
