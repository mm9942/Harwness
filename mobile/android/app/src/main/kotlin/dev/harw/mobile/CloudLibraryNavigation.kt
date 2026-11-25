package dev.harw.mobile

/** Exact authenticated target resolved by the host connection, not a route argument. */
internal data class CloudLibraryTarget<P, S>(val profile: P, val generation: Long, val scope: S)

/** Reference identity fences one open occurrence, even when reopening the same target. */
internal class CloudLibraryVisit<P, S>(val target: CloudLibraryTarget<P, S>)

internal class CloudLibraryPick<P, S>(val visit: CloudLibraryVisit<P, S>, val revision: ULong)

internal class CloudLibraryPicked<I>(val revision: ULong, val selected: I)

/**
 * Activity-owned, non-saveable navigation. A system picker may suspend the
 * same visit, but no snapshot, image bytes, caption or authorization is kept
 * in navigation. Only the one-shot picker selection can survive that pause.
 */
internal data class CloudLibraryNavigation<P, S, I>(
    val resumed: Boolean = false,
    val visit: CloudLibraryVisit<P, S>? = null,
    val request: CloudLibraryPick<P, S>? = null,
    val photo: CloudLibraryPicked<I>? = null,
) {
    val visible: CloudLibraryVisit<P, S>? get() = visit.takeIf { resumed }

    fun resume() = copy(resumed = true)

    fun pause(): CloudLibraryNavigation<P, S, I> =
        copy(resumed = false, visit = visit.takeIf { request?.visit === it }, photo = null)

    fun open(target: CloudLibraryTarget<P, S>): CloudLibraryNavigation<P, S, I> =
        if (!resumed || visit != null) this else copy(visit = CloudLibraryVisit(target), photo = null)

    // Keep an invalidated request in the slot until Android delivers its
    // result. Its callback carries no nonce and may arrive after a reopen.
    fun close() = copy(visit = null, photo = null)

    fun retain(target: CloudLibraryTarget<P, S>?): CloudLibraryNavigation<P, S, I> =
        if (visit != null && visit.target != target) close() else this

    fun beginPick(revision: ULong): CloudLibraryNavigation<P, S, I> {
        val owner = visible ?: return this
        if (request != null || photo != null) return this
        return copy(request = CloudLibraryPick(owner, revision))
    }

    fun pickResult(selected: I?, target: CloudLibraryTarget<P, S>?): CloudLibraryNavigation<P, S, I> {
        val awaiting = request ?: return this
        val current = retain(target)
        val picked = if (selected != null && current.visit === awaiting.visit) {
            CloudLibraryPicked<I>(awaiting.revision, selected)
        } else null
        return current.copy(request = null, photo = picked)
    }

    fun consume(
        owner: CloudLibraryVisit<P, S>, expected: CloudLibraryPicked<I>,
    ): CloudLibraryNavigation<P, S, I> =
        if (visible !== owner || photo !== expected) this else copy(photo = null)
}
