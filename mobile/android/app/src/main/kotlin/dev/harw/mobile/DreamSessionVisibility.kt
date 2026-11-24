package dev.harw.mobile

import uniffi.harw_mobile_ffi.MobileSessionPage
import uniffi.harw_mobile_ffi.MobileSessionPurpose

private val DreamRunId = Regex("^dream-[0-9]{8}T[0-9]{6}(?:-[A-Za-z0-9-]{1,64})?$")
private val DreamPromptBeginning = Regex(
    """^Du [Tt]räumst \((?:Zeitplan|von Hand \(/dream run\)|Leerlauf \([0-9]{1,20} min\))\)\. Konsolidiere still, was zuletzt wichtig war, welche offenen Fäden bleiben""",
)

/** Exact legacy producer signature from harw-ops; display-only, never authorization. */
internal fun suppressLegacyDreamRow(sessionId: String, firstMessage: String?): Boolean =
    DreamRunId.matches(sessionId) && firstMessage?.take(220)?.let(DreamPromptBeginning::containsMatchIn) == true

internal fun MobileSessionPage.filterDreamVisibility(includeDreams: Boolean = false): MobileSessionPage =
    copy(sessions = sessions.filter { row ->
        if (row.purpose.isSystemRun()) includeDreams
        else !suppressLegacyDreamRow(row.sessionId, row.firstMessage)
    })


/** Metadata-only Internal projection of rows already visible on the exact authorized legacy route. */
internal fun MobileSessionPage.projectLegacyDreamsForInternal(): MobileSessionPage =
    copy(sessions = sessions.mapNotNull { row ->
        if (row.purpose == MobileSessionPurpose.UNKNOWN && suppressLegacyDreamRow(row.sessionId, row.firstMessage)) {
            row.copy(purpose = MobileSessionPurpose.DREAM, continuable = false)
        } else null
    })
