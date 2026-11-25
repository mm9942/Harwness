package dev.harw.mobile

internal enum class SessionHomeBackend { Local, Cloud }

internal data class SessionHomeScope(val backend: SessionHomeBackend, val profileId: String)

internal data class SessionHomeId(val scope: SessionHomeScope, val sessionId: String)

/** Provenance supplied by the session source, never inferred from a title or message. */
internal enum class SessionHomeKind { User, Internal, UnknownLegacy }

internal data class SessionHomeRow(
    val id: SessionHomeId,
    val kind: SessionHomeKind,
    // Timestamp of observed activity, not creation, display changes, or this refresh.
    val activityAtMs: Long?,
    val title: String? = null,
)

internal data class SessionHomeProjection(
    val pinned: List<SessionHomeRow>,
    val recent: List<SessionHomeRow>,
    val ordinary: List<SessionHomeRow>,
    val internal: List<SessionHomeRow>,
    val unknownLegacy: List<SessionHomeRow>,
)

private const val UnifiedHomeRecentLimit = 3
internal const val DefaultRecentLimit = 10
internal const val MinimumRecentLimit = 5
internal const val MaximumRecentLimit = 20

internal fun recentSessionHomeLimit(value: Int?): Int =
    (value ?: DefaultRecentLimit).coerceIn(MinimumRecentLimit, MaximumRecentLimit)

private val activityOrder = compareBy<SessionHomeRow> { it.activityAtMs == null }
    .thenByDescending { it.activityAtMs }
    .thenBy { it.id.sessionId }

private val duplicatePreference = compareByDescending<SessionHomeRow> {
    when (it.kind) {
        // A conflicting internal classification must not surface as a user session.
        SessionHomeKind.Internal -> 2
        SessionHomeKind.User -> 1
        SessionHomeKind.UnknownLegacy -> 0
    }
}.then(activityOrder)
    .thenBy { it.title == null }
    .thenBy { it.title }

/** The unified directory has exactly three recent slots, independent of the legacy preference. */
internal fun projectUnifiedSessionHome(
    scope: SessionHomeScope,
    rows: List<SessionHomeRow>,
    pinnedIds: List<SessionHomeId>,
): SessionHomeProjection = projectSessionHomeRows(scope, rows, pinnedIds, UnifiedHomeRecentLimit, true)

/** Build disjoint sections from rows the source has already authorized for display. */
internal fun projectSessionHome(
    scope: SessionHomeScope,
    rows: List<SessionHomeRow>,
    pinnedIds: List<SessionHomeId>,
    recentLimit: Int = DefaultRecentLimit,
    // Legacy transport offers authorized rows but no typed session-purpose provenance.
    // The directory can keep these visible without relabeling them as verified user rows.
    includeUnknownLegacy: Boolean = false,
): SessionHomeProjection = projectSessionHomeRows(
    scope, rows, pinnedIds, recentSessionHomeLimit(recentLimit), includeUnknownLegacy,
)

private fun projectSessionHomeRows(
    scope: SessionHomeScope,
    rows: List<SessionHomeRow>,
    pinnedIds: List<SessionHomeId>,
    recentLimit: Int,
    includeUnknownLegacy: Boolean,
): SessionHomeProjection {
    val selected = rows.asSequence()
        .filter { it.id.scope == scope }
        .sortedWith(duplicatePreference)
        .distinctBy { it.id }
        .sortedWith(activityOrder)
        .toList()

    val users = selected.filter {
        it.kind == SessionHomeKind.User || (includeUnknownLegacy && it.kind == SessionHomeKind.UnknownLegacy)
    }
    val userById = users.associateBy { it.id }
    val pinned = pinnedIds.distinct().mapNotNull(userById::get)
    val pinnedSet = pinned.mapTo(hashSetOf()) { it.id }
    val remaining = users.filterNot { it.id in pinnedSet }
    val recent = remaining.asSequence()
        .filter { it.activityAtMs != null }
        .take(recentLimit)
        .toList()
    val recentSet = recent.mapTo(hashSetOf()) { it.id }

    return SessionHomeProjection(
        pinned = pinned,
        recent = recent,
        ordinary = remaining.filterNot { it.id in recentSet },
        internal = selected.filter { it.kind == SessionHomeKind.Internal },
        unknownLegacy = if (includeUnknownLegacy) emptyList() else selected.filter { it.kind == SessionHomeKind.UnknownLegacy },
    )
}
