package dev.harw.mobile

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.MobileArchiveBackend
import uniffi.harw_mobile_ffi.MobileArchiveCheckpoint
import uniffi.harw_mobile_ffi.MobileArchiveRowLocator
import uniffi.harw_mobile_ffi.MobileArchiveScope
import uniffi.harw_mobile_ffi.MobileArchiveRewriteCause
import uniffi.harw_mobile_ffi.MobileCore
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.MobileSession

internal data class NativeArchiveBinding(
    val native: MobileCore,
    val scope: MobileArchiveScope,
    val attachment: ArchiveAttachment,
)

internal fun archiveScopeFor(native: MobileArchiveScope, sessionId: String): ArchiveScope = ArchiveScope(
    backend = when (native.backend) {
        MobileArchiveBackend.LOCAL -> ArchiveBackend.LOCAL
        MobileArchiveBackend.CLOUD -> ArchiveBackend.CLOUD
    },
    // This is only an in-process identity; never persist it as a profile ID.
    profileId = native.profileEpoch.toString(),
    serverId = native.authorityId,
    sessionId = sessionId,
)

internal class NativeArchiveForkPort(
    private val binding: suspend () -> NativeArchiveBinding?,
    private val onCreated: (MobileSession) -> Unit,
) : ArchiveForkPort {
    override suspend fun attachment(): ArchiveAttachment? = binding()?.attachment

    private suspend fun requireBinding(at: ArchiveAttachment): NativeArchiveBinding =
        binding()?.takeIf { it.attachment == at } ?: throw MobileException.Disconnected()

    override suspend fun issue(at: ArchiveAttachment, expected: ArchiveRevision): ArchiveCheckpoint {
        val (core, scope) = requireBinding(at)
        return withContext(Dispatchers.IO) {
            core.archiveCheckpointIssue(scope, at.scope.sessionId, expected.generation, expected.durableNext)
        }.checkpoint()
    }

    override suspend fun describe(at: ArchiveAttachment, checkpointId: String): ArchiveCheckpoint {
        val (core, scope) = requireBinding(at)
        return withContext(Dispatchers.IO) {
            core.archiveCheckpointDescribe(scope, at.scope.sessionId, checkpointId)
        }.checkpoint()
    }

    override suspend fun previous(at: ArchiveAttachment, beforeSequence: ULong?): ArchivePredecessor? {
        check(at.archiveRetainedRewriteV1)
        val (core, scope) = requireBinding(at)
        val result = withContext(Dispatchers.IO) {
            core.archiveCheckpointPrevious(scope, at.scope.sessionId, beforeSequence)
        } ?: return null
        return ArchivePredecessor(
            result.checkpoint.checkpoint(), result.beforeSequence, result.recordedAt,
            result.cause == MobileArchiveRewriteCause.USER_EDIT,
        )
    }

    override suspend fun page(at: ArchiveAttachment, request: ArchivePageRequest): ArchivePage {
        check(request.sessionId == at.scope.sessionId)
        val (core, scope) = requireBinding(at)
        return withContext(Dispatchers.IO) {
            core.archiveCheckpointPage(scope, request.sessionId, request.checkpointId, request.beforeSequence, request.limit)
        }.let { page ->
            ArchivePage(
                page.sessionId,
                page.checkpointId,
                page.rows.map { row ->
                    ArchiveRow(
                        ArchiveRowLocator(row.locator.sequence, row.locator.itemId),
                        when (row.role) {
                            uniffi.harw_mobile_ffi.MobileArchiveRole.USER -> ArchiveRowRole.USER
                            uniffi.harw_mobile_ffi.MobileArchiveRole.ASSISTANT -> ArchiveRowRole.ASSISTANT
                        },
                        row.textParts,
                        row.recordedAt,
                        row.imageCount,
                    )
                },
                page.nextBeforeSequence,
            )
        }
    }

    override suspend fun fork(at: ArchiveAttachment, request: ArchiveForkRequest): ArchiveForkReply {
        check(request.sessionId == at.scope.sessionId)
        val (core, scope) = requireBinding(at)
        val created = try {
            withContext(Dispatchers.IO) {
                val locator = MobileArchiveRowLocator(request.row.sequence, request.row.itemId)
                val displayed = request.displayedPrefix
                if (displayed == null) {
                    core.forkInclusive(scope, request.sessionId, request.checkpointId, locator, request.clientOperationId)
                } else {
                    core.forkRetainedDisplayed(
                        scope, request.sessionId, request.checkpointId, locator, request.clientOperationId, displayed,
                    )
                }
            }
        } catch (_: MobileException.Denied) {
            return ArchiveForkReply.Rejected
        } catch (_: MobileException.Revoked) {
            return ArchiveForkReply.Rejected
        } catch (_: MobileException.Session) {
            return ArchiveForkReply.Rejected
        }
        if (binding()?.attachment != at) return ArchiveForkReply.OutcomeUnknown
        if (created.sessionId.isBlank() || created.sessionId == request.sessionId) return ArchiveForkReply.OutcomeUnknown
        onCreated(created)
        return ArchiveForkReply.Created(created.sessionId)
    }
}

private fun MobileArchiveCheckpoint.checkpoint(): ArchiveCheckpoint =
    ArchiveCheckpoint(sessionId, checkpointId, ArchiveRevision(generation, durableNext))
