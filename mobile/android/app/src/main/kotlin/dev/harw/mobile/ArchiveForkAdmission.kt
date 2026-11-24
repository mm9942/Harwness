package dev.harw.mobile

import uniffi.harw_mobile_ffi.SessionCursor

/** The durable cursor is the next record not yet seen, not the live-update counter. */
internal fun observedArchiveRevision(
    sessionId: String,
    observedSessionId: String?,
    active: Boolean,
    replaying: Boolean,
    head: SessionCursor?,
): ArchiveRevision? =
    head?.takeIf { active && !replaying && observedSessionId == sessionId && it.durable != 0uL }
        ?.let { ArchiveRevision(it.generation, it.durable) }

/** Delivery of a fork result must not redirect a closed sheet or a different conversation. */
internal data class ArchiveForkRoute(
    val sessionId: String,
    val projectKey: String?,
    val attachment: ArchiveAttachment,
    val sheetToken: Any,
) {
    fun isStillOpen(
        currentSessionId: String?,
        currentProjectKey: String?,
        currentAttachment: ArchiveAttachment?,
        currentSheetToken: Any?,
    ): Boolean =
        currentSheetToken === sheetToken && currentSessionId == sessionId && currentProjectKey == projectKey &&
            currentAttachment == attachment
}
