package dev.harw.mobile

import java.time.OffsetDateTime
import java.time.format.DateTimeParseException
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.sync.Mutex
import uniffi.harw_mobile_ffi.TranscriptEntry

internal enum class ArchiveBackend { LOCAL, CLOUD }

/** serverId is the authenticated backend identity, not a display name or URL. */
internal data class ArchiveScope(
    val backend: ArchiveBackend,
    val profileId: String,
    val serverId: String,
    val sessionId: String,
)

/** Connection lifecycle epoch, distinct from the host transcript revision. */
internal data class ArchiveAttachment(
    val scope: ArchiveScope,
    val epoch: Long,
    val archiveCheckpointV1: Boolean,
    val archiveRetainedRewriteV1: Boolean = false,
)

internal data class ArchiveRevision(val generation: UInt, val durableNext: ULong)
internal data class ArchiveCheckpoint(val sessionId: String, val checkpointId: String, val revision: ArchiveRevision)
internal data class ArchivePredecessor(
    val checkpoint: ArchiveCheckpoint,
    val beforeSequence: ULong,
    val recordedAt: String,
    val userEdit: Boolean,
)
internal data class ArchiveCheckpointRef(val scope: ArchiveScope, val checkpointId: String)
internal data class ArchiveRowLocator(val sequence: ULong, val itemId: String)
internal enum class ArchiveRowRole { USER, ASSISTANT }
internal data class ArchiveRow(
    val locator: ArchiveRowLocator,
    val role: ArchiveRowRole,
    val textParts: List<String>,
    val recordedAt: String,
    val imageCount: UInt,
)
/** Solely an authenticated archive view; it never becomes the live transcript or a cached edit alternate. */
internal class RetainedTimeline(
    val attachment: ArchiveAttachment,
    val checkpoint: ArchiveCheckpointRef,
    val predecessor: ArchivePredecessor,
    val rows: List<ArchiveRow>,
    val complete: Boolean,
)

/** Link-scoped host proof of one particular row's ordered image occurrences. */
internal data class VerifiedHistoricalMedia(
    val attachment: ArchiveAttachment,
    val checkpoint: ArchiveCheckpointRef,
    val revision: ArchiveRevision,
    val row: ArchiveRowLocator,
    val count: Int,
)

internal data class ArchivePageRequest(
    val sessionId: String,
    val checkpointId: String,
    val beforeSequence: ULong?,
    val limit: UShort,
)
internal data class ArchivePage(
    val sessionId: String,
    val checkpointId: String,
    val rows: List<ArchiveRow>,
    val nextBeforeSequence: ULong?,
)
internal data class ArchiveForkRequest(
    val sessionId: String,
    val checkpointId: String,
    val row: ArchiveRowLocator,
    val clientOperationId: String,
    val displayedPrefix: List<TranscriptEntry>? = null,
)

internal sealed interface ArchiveForkReply {
    data class Created(val sessionId: String) : ArchiveForkReply
    /** The adapter proved the RPC definitively rejected this operation. */
    data object Rejected : ArchiveForkReply
    /** Busy, transport loss, timeout, ambiguous or otherwise unproven outcome. */
    data object OutcomeUnknown : ArchiveForkReply
}

/**
 * Bind each RPC to the supplied verified attachment before dispatch. Return only
 * typed server results. This interface has no legacy, cache, or fake app adapter.
 */
internal interface ArchiveForkPort {
    suspend fun attachment(): ArchiveAttachment?
    suspend fun issue(at: ArchiveAttachment, expected: ArchiveRevision): ArchiveCheckpoint
    suspend fun describe(at: ArchiveAttachment, checkpointId: String): ArchiveCheckpoint
    suspend fun previous(at: ArchiveAttachment, beforeSequence: ULong?): ArchivePredecessor? = null
    suspend fun page(at: ArchiveAttachment, request: ArchivePageRequest): ArchivePage
    suspend fun fork(at: ArchiveAttachment, request: ArchiveForkRequest): ArchiveForkReply
}

internal data class ArchiveForkState(
    val scope: ArchiveScope,
    val checkpoint: ArchiveCheckpointRef? = null,
    val revision: ArchiveRevision? = null,
    val rowsEpoch: Long? = null,
    val rows: List<ArchiveRow> = emptyList(),
    val currentBefore: ULong? = null,
    val nextBefore: ULong? = null,
    val busy: Boolean = false,
    val outcomeUnknown: Boolean = false,
    val notice: String? = null,
    val predecessor: ArchivePredecessor? = null,
    val noOlderPredecessor: Boolean = false,
) {
    fun matches(at: ArchiveAttachment?): Boolean =
        at?.archiveCheckpointV1 == true && at.scope == scope

    fun rowsAreVerified(at: ArchiveAttachment?): Boolean =
        matches(at) && rowsEpoch != null && rowsEpoch == at?.epoch
}

/** Dedicated server-archive UI state. A legacy ChatVersion never supplies these rows. */
internal class ArchiveForkController(
    private val scope: ArchiveScope,
    private val port: ArchiveForkPort,
    private val operationId: () -> String = { UUID.randomUUID().toString() },
) {
    private val work = Mutex()
    private val mutable = MutableStateFlow(ArchiveForkState(scope))
    val state = mutable.asStateFlow()
    private var pending: ArchiveForkRequest? = null

    suspend fun issue(expected: ArchiveRevision) = withWork {
        if (pending != null) return@withWork
        val at = checkedAttachment() ?: return@withWork
        startLoading()
        try {
            val checkpoint = port.issue(at, expected)
            if (checkpoint.revision != expected) {
                notice("Der Server hat einen anderen Stand bestätigt.")
                return@withWork
            }
            showCheckpoint(at, checkpoint)
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            notice("Der Server konnte diesen Stand nicht sichern.")
        }
    }

    suspend fun open(ref: ArchiveCheckpointRef) = withWork {
        if (pending != null || ref.scope != scope || !validId(ref.checkpointId)) return@withWork
        val at = checkedAttachment() ?: return@withWork
        startLoading()
        try {
            val described = port.describe(at, ref.checkpointId)
            if (described.checkpointId != ref.checkpointId) {
                notice("Der Server hat eine andere Archivfassung bestätigt.")
                return@withWork
            }
            showCheckpoint(at, described)
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            notice("Diese Server-Archivfassung ist nicht verfügbar.")
        }
    }

    suspend fun openPreviousRevision() = withWork {
        if (pending != null) return@withWork
        val previous = mutable.value
        if (previous.noOlderPredecessor) return@withWork
        val at = checkedAttachment()?.takeIf { it.archiveRetainedRewriteV1 } ?: return@withWork
        val before = previous.predecessor?.beforeSequence
        mutable.update { it.copy(notice = null) }
        try {
            val found = port.previous(at, before)
            if (port.attachment() != at) return@withWork
            if (found == null) {
                mutable.update { it.copy(noOlderPredecessor = true) }
                notice("Keine ältere bestätigte Server-Fassung gefunden.")
                return@withWork
            }
            if ((before != null && found.beforeSequence >= before) || !validRecordedAt(found.recordedAt)) {
                notice("Die frühere Fassung konnte nicht bestätigt werden.")
                return@withWork
            }
            showCheckpoint(at, found.checkpoint)
            if (mutable.value.checkpoint?.checkpointId == found.checkpoint.checkpointId && mutable.value.notice == null) {
                mutable.update { it.copy(predecessor = found, noOlderPredecessor = false) }
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            notice("Ältere bestätigte Fassungen konnten nicht geladen werden.")
        }
    }

    suspend fun loadOlder() = withWork {
        if (pending != null) return@withWork
        val current = mutable.value
        val ref = current.checkpoint ?: return@withWork
        val revision = current.revision ?: return@withWork
        val before = current.nextBefore ?: return@withWork
        val at = checkedAttachment() ?: return@withWork
        if (current.rowsAreVerified(at)) tryPage(at, ref, revision, before)
    }

    suspend fun loadNewest() = withWork {
        if (pending != null) return@withWork
        val current = mutable.value
        val ref = current.checkpoint ?: return@withWork
        val revision = current.revision ?: return@withWork
        val at = checkedAttachment() ?: return@withWork
        tryPage(at, ref, revision, null)
    }

    /** Returns only the exact verified child ID; caller must still own this UI route. */
    suspend fun fork(locator: ArchiveRowLocator): String? = withWork {
        val available = mutable.value
        val ref = available.checkpoint ?: return@withWork null
        if (pending != null || available.rows.none { it.locator == locator }) return@withWork null
        val at = checkedAttachment() ?: return@withWork null
        if (!available.rowsAreVerified(at)) return@withWork null
        val id = operationId()
        if (!validId(id)) return@withWork null
        val request = ArchiveForkRequest(scope.sessionId, ref.checkpointId, locator, id)
        tryFork(at, request)
    }

    /**
     * Match a displayed row against authenticated archive rows, never against message text.
     * Retained predecessors are considered without reading or rewriting the live transcript.
     */
    suspend fun forkDisplayed(
        selected: HistoricalForkSelection,
        currentRevision: ArchiveRevision?,
        displayedPrefix: List<TranscriptEntry>? = null,
        stillOpen: () -> Boolean,
    ): String? = withWork {
        if (!stillOpen() || selected.visibleSuffix.isEmpty()) return@withWork null
        if (displayedPrefix != null && (!selected.archived || retainedDisplayedSelection(displayedPrefix) != selected)) {
            notice("Die angezeigte ältere Fassung kann nicht exakt geprüft werden.")
            return@withWork null
        }
        val at = checkedAttachment() ?: return@withWork null
        val unknown = pending
        if (unknown != null) {
            // A second click on the exact same visible prefix can only resolve
            // its existing operation. A different selection never creates a duplicate.
            if (displayedPrefix != null && unknown.displayedPrefix == displayedPrefix) {
                return@withWork tryFork(at, unknown, stillOpen)
            }
            notice("Ein Fork ist noch unbestätigt. Prüfe zuerst denselben Vorgang.")
            return@withWork null
        }
        val result = try {
            if (selected.archived) {
                if (!at.archiveRetainedRewriteV1) {
                    notice("Dieser Host bietet keine bestätigten früheren Fassungen an.")
                    return@withWork null
                }
                findRetained(at, selected, stillOpen)
            } else {
                if (currentRevision == null) {
                    notice("Der aktuelle Stand konnte nicht bestätigt werden.")
                    return@withWork null
                }
                val checkpoint = port.issue(at, currentRevision)
                if (checkpoint.revision != currentRevision) {
                    notice("Der aktuelle Stand hat sich geändert.")
                    return@withWork null
                }
                checkedCandidate(at, checkpoint, selected, stillOpen)
            }
        } catch (e: CancellationException) {
            throw e
        } catch (_: ArchiveSearchLimit) {
            notice("Die historische Zeile konnte innerhalb des Suchlimits nicht eindeutig bestätigt werden. Es wurde nichts kopiert.")
            return@withWork null
        } catch (_: AmbiguousArchiveRow) {
            notice("Mehrere frühere Stände passen zur Anzeige. Es wurde nichts kopiert.")
            return@withWork null
        } catch (_: Exception) {
            notice("Der Host konnte die Fassung gerade nicht bestätigen.")
            return@withWork null
        }
        if (!stillOpen() || port.attachment() != at) return@withWork null
        if (result == null) {
            notice(if (selected.archived) "Diese ältere Nachricht ist im Server-Archiv nicht erhalten." else "Diese Nachricht konnte im bestätigten Verlauf nicht gefunden werden.")
            return@withWork null
        }
        val id = operationId()
        if (!validId(id) || !stillOpen()) return@withWork null
        // Expose precisely the verified checkpoint/page if the completion is unknown.
        mutable.update { it.copy(
            checkpoint = result.ref, revision = result.revision, rowsEpoch = at.epoch,
            rows = result.page.rows, currentBefore = result.before, nextBefore = result.page.nextBeforeSequence,
            predecessor = null, noOlderPredecessor = false, notice = null,
        ) }
        tryFork(at, ArchiveForkRequest(scope.sessionId, result.ref.checkpointId, result.locator, id, displayedPrefix), stillOpen)
    }

    /** Resolve an inline preview only from the host-verified boundary that was shown before rewrite. */
    suspend fun resolveDisplayedArchiveMedia(
        selection: HistoricalForkSelection,
        beforeDurable: ULong,
        expectedImageCount: Int,
        stillOpen: () -> Boolean,
    ): VerifiedHistoricalMedia? = withWork {
        if (pending != null || !selection.archived || beforeDurable == 0uL ||
            expectedImageCount !in 1..50 || !stillOpen()
        ) return@withWork null
        val attachment = checkedAttachment()?.takeIf { it.archiveRetainedRewriteV1 }
            ?: return@withWork null
        val candidate = findRetained(attachment, selection, stillOpen, beforeDurable)
            ?: return@withWork null
        val row = candidate.page.rows.singleOrNull {
            it.locator == candidate.locator && it.role == ArchiveRowRole.USER
        } ?: return@withWork null
        if (!stillOpen() || port.attachment() != attachment ||
            candidate.revision.durableNext != beforeDurable || row.imageCount != expectedImageCount.toUInt()
        ) return@withWork null
        VerifiedHistoricalMedia(attachment, candidate.ref, candidate.revision, row.locator, expectedImageCount)
    }

    /** Open the exact retained page for pictures, without dispatching a fork or turn. */
    suspend fun openDisplayedArchiveMedia(
        selected: HistoricalForkSelection,
        stillOpen: () -> Boolean,
    ) = withWork {
        if (pending != null || !selected.archived || !stillOpen()) return@withWork
        val at = checkedAttachment()
        if (at == null || !at.archiveRetainedRewriteV1) {
            notice("Dieser Host bietet keine bestätigten früheren Fassungen an.")
            return@withWork
        }
        val result = try {
            findRetained(at, selected, stillOpen)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: ArchiveSearchLimit) {
            if (stillOpen()) notice("Die Fassung liegt außerhalb des sicheren Suchbereichs. Wähle sie direkt im Server-Archiv.")
            return@withWork
        } catch (_: AmbiguousArchiveRow) {
            if (stillOpen()) notice("Mehrere Serverfassungen passen. Wähle die richtige Fassung im Archiv.")
            return@withWork
        } catch (_: Exception) {
            if (stillOpen()) notice("Die Archivfassung konnte nicht geladen werden.")
            return@withWork
        }
        if (!stillOpen() || port.attachment() != at) return@withWork
        if (result == null) {
            notice("Diese Nachricht ist nicht als bestätigte Serverfassung vorhanden.")
            return@withWork
        }
        mutable.update { it.copy(
            checkpoint = result.ref, revision = result.revision, rowsEpoch = at.epoch,
            rows = result.page.rows, currentBefore = result.before, nextBefore = result.page.nextBeforeSequence,
            predecessor = null, noOlderPredecessor = false, notice = null,
        ) }
    }

    /** Read the selected verified revision (default latest). Long histories
     * get an explicitly incomplete read-only tail, never a forkable prefix. */
    suspend fun readRetainedTimeline(
        expected: ArchiveAttachment,
        stillOpen: () -> Boolean,
        selected: ArchivePredecessor? = null,
    ): RetainedTimeline? = withWork {
        suspend fun current(): Boolean =
            stillOpen() && expected.archiveRetainedRewriteV1 && checkedAttachment() == expected
        if (!current()) return@withWork null
        val prior = if (selected == null) {
            port.previous(expected, null) ?: return@withWork null
        } else {
            val verified = mutable.value
            if (!verified.rowsAreVerified(expected) || verified.predecessor != selected ||
                verified.checkpoint?.checkpointId != selected.checkpoint.checkpointId ||
                verified.revision != selected.checkpoint.revision
            ) return@withWork null
            selected
        }
        if (!current()) return@withWork null
        val checkpoint = prior.checkpoint
        check(checkpoint.sessionId == scope.sessionId && validId(checkpoint.checkpointId) &&
            prior.beforeSequence != 0uL && checkpoint.revision.durableNext == prior.beforeSequence &&
            validRecordedAt(prior.recordedAt))
        val ref = ArchiveCheckpointRef(scope, checkpoint.checkpointId)
        var before: ULong? = null
        val newestFirst = ArrayList<ArchiveRow>()
        val seen = HashSet<String>()
        repeat(MaxSearchPages) {
            if (!current()) return@withWork null
            val page = port.page(expected, ArchivePageRequest(scope.sessionId, ref.checkpointId, before, PageRows))
            if (!current()) return@withWork null
            check(validPage(page, ref, checkpoint.revision, before))
            if (newestFirst.size + page.rows.size > MaxRetainedTimelineRows) throw ArchiveSearchLimit()
            for (row in page.rows.asReversed()) {
                // Refuse a partial visual projection that could otherwise Fork
                // images the viewer would omit.
                check(seen.add(row.locator.itemId) && row.imageCount <= 50u &&
                    (row.role != ArchiveRowRole.ASSISTANT || row.imageCount == 0u))
                newestFirst.add(row)
            }
            val next = page.nextBeforeSequence
            if (next == null) {
                return@withWork newestFirst.asReversed().toList().takeIf { it.isNotEmpty() }?.let {
                    RetainedTimeline(expected, ref, prior, it, complete = true)
                }
            }
            before = next
        }
        if (!current()) return@withWork null
        newestFirst.asReversed().toList().takeIf { it.isNotEmpty() }?.let {
            RetainedTimeline(expected, ref, prior, it, complete = false)
        }
    }

    /** Hand a bounded chat preview back to this exact paged archive revision. */
    suspend fun openPagedTimeline(
        timeline: RetainedTimeline,
        stillOpen: () -> Boolean,
    ): Boolean = withWork {
        if (pending != null || !stillOpen() || timeline.checkpoint.scope != scope ||
            checkedAttachment() != timeline.attachment
        ) return@withWork false
        val prior = timeline.predecessor
        if (prior.checkpoint.checkpointId != timeline.checkpoint.checkpointId ||
            prior.checkpoint.revision.durableNext != prior.beforeSequence
        ) return@withWork false
        showCheckpoint(timeline.attachment, prior.checkpoint)
        val visible = mutable.value
        if (!stillOpen() || checkedAttachment() != timeline.attachment ||
            !visible.rowsAreVerified(timeline.attachment) ||
            visible.checkpoint != timeline.checkpoint ||
            visible.revision != prior.checkpoint.revision || visible.notice != null
        ) return@withWork false
        mutable.update { it.copy(predecessor = prior, noOlderPredecessor = false) }
        true
    } ?: false

    /** The complete shown timeline starts at the first verified visible row. */
    suspend fun forkRetainedTimeline(
        timeline: RetainedTimeline,
        row: ArchiveRowLocator,
        stillOpen: () -> Boolean,
    ): String? = withWork {
        if (!timeline.complete || !stillOpen() || timeline.checkpoint.scope != scope ||
            timeline.rows.none { it.locator == row } || checkedAttachment() != timeline.attachment
        ) return@withWork null
        val unknown = pending
        val request = if (unknown == null) {
            val id = operationId()
            if (!validId(id)) return@withWork null
            ArchiveForkRequest(scope.sessionId, timeline.checkpoint.checkpointId, row, id)
        } else {
            if (unknown.checkpointId != timeline.checkpoint.checkpointId || unknown.row != row ||
                unknown.displayedPrefix != null
            ) {
                notice("Ein anderer Fork ist noch unbestätigt.")
                return@withWork null
            }
            unknown
        }
        tryFork(timeline.attachment, request, stillOpen)
    }

    private suspend fun findRetained(
        at: ArchiveAttachment, selection: HistoricalForkSelection, stillOpen: () -> Boolean,
        requiredBoundary: ULong? = null,
    ): DisplayedArchiveCandidate? {
        var olderThan: ULong? = null
        var first: DisplayedArchiveCandidate? = null
        // One extra discovery call proves traversal ended rather than treating a budget miss as not retained.
        repeat(MaxSearchPredecessors + 1) { index ->
            if (!stillOpen() || port.attachment() != at) return null
            val previous = port.previous(at, olderThan) ?: return first
            if (index == MaxSearchPredecessors) throw ArchiveSearchLimit()
            if (previous.beforeSequence == 0uL || previous.checkpoint.revision.durableNext != previous.beforeSequence ||
                (olderThan != null && previous.beforeSequence >= olderThan!!) || !validRecordedAt(previous.recordedAt)
            ) error("invalid predecessor")
            if (requiredBoundary != null) {
                if (previous.beforeSequence < requiredBoundary) return null
                if (previous.beforeSequence == requiredBoundary) {
                    return checkedCandidate(at, previous.checkpoint, selection, stillOpen)
                }
                olderThan = previous.beforeSequence
                return@repeat
            }
            val candidate = checkedCandidate(at, previous.checkpoint, selection, stillOpen)
            if (candidate != null) {
                // Retained checkpoints may cover the same unchanged prefix. A different
                // sequence or prefix is a different result, not a tie-break by recency.
                if (first != null && first?.prefix != candidate.prefix) throw AmbiguousArchiveRow()
                if (first == null) first = candidate
            }
            olderThan = previous.beforeSequence
        }
        return first
    }

    private suspend fun checkedCandidate(
        at: ArchiveAttachment, checkpoint: ArchiveCheckpoint, selection: HistoricalForkSelection,
        stillOpen: () -> Boolean,
    ): DisplayedArchiveCandidate? {
        check(checkpoint.sessionId == scope.sessionId && validId(checkpoint.checkpointId))
        val ref = ArchiveCheckpointRef(scope, checkpoint.checkpointId)
        var before: ULong? = null
        var selectedPage: ArchivePage? = null
        var selectedBefore: ULong? = null
        var locator: ArchiveRowLocator? = null
        val throughBySequence = ArrayList<Pair<ArchiveRowLocator, ForkRowIdentity>>()
        val seenIds = HashSet<String>()
        repeat(MaxSearchPages) {
            if (!stillOpen() || port.attachment() != at) return null
            val page = port.page(at, ArchivePageRequest(scope.sessionId, ref.checkpointId, before, PageRows))
            if (!stillOpen() || port.attachment() != at) return null
            check(validPage(page, ref, checkpoint.revision, before))
            for (row in page.rows.asReversed()) {
                val identity = ForkRowIdentity(row.locator.itemId, row.role)
                // Canonical host archives guarantee unique item IDs within a revision.
                // Reject a malformed/mocked response instead of guessing which duplicate.
                check(seenIds.add(identity.itemId))
                if (identity == selection.selected) {
                    locator = row.locator
                    selectedPage = page
                    selectedBefore = before
                }
                if (locator != null) throughBySequence += row.locator to identity
            }
            val next = page.nextBeforeSequence
            if (next == null) {
                val found = locator ?: return null
                val prefix = throughBySequence.asReversed()
                if (prefix.takeLast(selection.visibleSuffix.size).map { it.second } != selection.visibleSuffix) return null
                return DisplayedArchiveCandidate(ref, checkpoint.revision, checkNotNull(selectedPage), selectedBefore, found, prefix.map { it.first })
            }
            before = next
        }
        throw ArchiveSearchLimit()
    }

    /** Only a user pressing Resolve retries an uncertain fork, with exactly the same operation. */
    suspend fun retryUnknown(): String? = withWork {
        val request = pending ?: return@withWork null
        val at = checkedAttachment() ?: return@withWork null
        tryFork(at, request)
    }

    private suspend fun checkedAttachment(): ArchiveAttachment? =
        port.attachment()?.takeIf { mutable.value.matches(it) }

    private fun startLoading() {
        mutable.value = ArchiveForkState(scope, busy = true)
    }

    private suspend fun showCheckpoint(at: ArchiveAttachment, value: ArchiveCheckpoint) {
        if (port.attachment() != at || value.sessionId != scope.sessionId || !validId(value.checkpointId)) {
            notice("Die Sitzung oder Archivfassung hat sich geändert.")
            return
        }
        tryPage(at, ArchiveCheckpointRef(scope, value.checkpointId), value.revision, null)
    }

    private suspend fun tryPage(
        at: ArchiveAttachment, ref: ArchiveCheckpointRef, revision: ArchiveRevision, before: ULong?,
    ) {
        if (port.attachment() != at) return
        try {
            val page = port.page(at, ArchivePageRequest(scope.sessionId, ref.checkpointId, before, PageRows))
            if (port.attachment() != at) return
            if (!validPage(page, ref, revision, before)) {
                notice("Die Server-Seite passt nicht zur gewählten Archivfassung.")
                return
            }
            mutable.update {
                it.copy(
                    checkpoint = ref,
                    revision = revision,
                    rowsEpoch = at.epoch,
                    rows = page.rows.map { row -> row.copy(textParts = row.textParts.toList()) },
                    currentBefore = before,
                    nextBefore = page.nextBeforeSequence,
                    notice = null,
                )
            }
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            notice("Diese Server-Seite konnte nicht geladen werden.")
        }
    }

    private suspend fun tryFork(
        at: ArchiveAttachment, request: ArchiveForkRequest, stillOpen: () -> Boolean = { true },
    ): String? {
        if (port.attachment() != at || !stillOpen()) return null
        // Mark pending before dispatch; cancellation or a lost response cannot authorize a fresh ID.
        pending = request
        mutable.update { it.copy(outcomeUnknown = true, notice = null) }
        val outcome = try {
            port.fork(at, request)
        } catch (error: CancellationException) {
            throw error
        } catch (_: Exception) {
            ArchiveForkReply.OutcomeUnknown
        }
        if (port.attachment() != at) return null
        return when (outcome) {
            is ArchiveForkReply.Created -> {
                val id = outcome.sessionId
                if (!validId(id) || id == scope.sessionId) return null
                pending = null
                mutable.update { it.copy(outcomeUnknown = false, notice = null) }
                id
            }
            ArchiveForkReply.Rejected -> {
                pending = null
                mutable.update { it.copy(outcomeUnknown = false, notice = "Der Server hat das Abzweigen abgelehnt.") }
                null
            }
            ArchiveForkReply.OutcomeUnknown -> {
                notice("Ergebnis unklar. Prüfe genau diesen Vorgang, statt einen neuen zu starten.")
                null
            }
        }
    }

    private fun notice(value: String) {
        mutable.update { it.copy(notice = value) }
    }

    private suspend fun <T> withWork(block: suspend () -> T): T? {
        if (!work.tryLock()) return null
        try {
            mutable.update { it.copy(busy = true) }
            return block()
        } finally {
            mutable.update { it.copy(busy = false) }
            work.unlock()
        }
    }
}

private class ArchiveSearchLimit : Exception()
private class AmbiguousArchiveRow : Exception()
private const val MaxSearchPredecessors = 8
private const val MaxSearchPages = 8
private data class DisplayedArchiveCandidate(
    val ref: ArchiveCheckpointRef,
    val revision: ArchiveRevision,
    val page: ArchivePage,
    val before: ULong?,
    val locator: ArchiveRowLocator,
    val prefix: List<ArchiveRowLocator>,
)

private val PageRows = 64u.toUShort()
private const val MaxRetainedTimelineRows = 512

private fun validId(value: String): Boolean =
    value.length in 1..128 && value.all { it in '!'..'~' }

private fun validPage(
    page: ArchivePage, ref: ArchiveCheckpointRef, revision: ArchiveRevision, before: ULong?,
): Boolean {
    if (page.sessionId != ref.scope.sessionId || page.checkpointId != ref.checkpointId ||
        page.rows.size > PageRows.toInt()) return false
    if (page.nextBeforeSequence != null && page.nextBeforeSequence != page.rows.firstOrNull()?.locator?.sequence) return false
    var last: ULong? = null
    for (row in page.rows) {
        val sequence = row.locator.sequence
        if (sequence >= revision.durableNext || (last != null && sequence <= last) ||
            (before != null && sequence >= before) ||
            !validId(row.locator.itemId) || !validRecordedAt(row.recordedAt)) return false
        last = sequence
    }
    return true
}

private fun validRecordedAt(value: String): Boolean {
    if (value.length !in 20..45) return false
    return try {
        OffsetDateTime.parse(value)
        true
    } catch (_: DateTimeParseException) {
        false
    }
}
