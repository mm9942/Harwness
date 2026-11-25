package dev.harw.mobile

import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

/** A display-side selection hint only. No text, media, roles or rows are imported. */
internal data class ForkRowIdentity(val itemId: String, val role: ArchiveRowRole)

internal data class HistoricalForkSelection(
    val visibleSuffix: List<ForkRowIdentity>,
    val archived: Boolean,
) {
    val selected: ForkRowIdentity get() = visibleSuffix.last()
}

internal fun selectedForkRow(
    displayed: List<TranscriptEntry>, index: Int, firstArchived: Int,
): HistoricalForkSelection? {
    val picked = displayed.getOrNull(index)?.takeIf { it.kind == TranscriptKind.USER } ?: return null
    if (picked.id.isBlank()) return null
    val preceding = displayed.subList(0, index + 1).asReversed().asSequence().mapNotNull { entry ->
        val role = when (entry.kind) {
            TranscriptKind.USER -> ArchiveRowRole.USER
            TranscriptKind.ASSISTANT -> ArchiveRowRole.ASSISTANT
            else -> return@mapNotNull null
        }
        ForkRowIdentity(entry.id, role)
    }.take(128).toList().asReversed()
    if (preceding.any { it.itemId.isBlank() } || preceding.map { it.itemId }.toSet().size != preceding.size) return null
    return HistoricalForkSelection(preceding, index >= firstArchived)
}

/** A complete displayed prefix for retained Fork; media lookup keeps its smaller suffix hint. */
internal fun retainedDisplayedSelection(rows: List<TranscriptEntry>): HistoricalForkSelection? {
    if (rows.size !in 1..512) return null
    val identities = rows.map { entry ->
        if (!isDurableRewriteItemId(entry.id)) return null
        val role = when (entry.kind) {
            TranscriptKind.USER -> ArchiveRowRole.USER
            TranscriptKind.ASSISTANT -> ArchiveRowRole.ASSISTANT
            else -> return null
        }
        ForkRowIdentity(entry.id, role)
    }
    if (identities.map { it.itemId }.toSet().size != identities.size) return null
    return HistoricalForkSelection(identities, archived = true)
}
