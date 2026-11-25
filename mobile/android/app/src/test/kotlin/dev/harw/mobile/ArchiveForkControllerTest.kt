package dev.harw.mobile

import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class ArchiveForkControllerTest {
    private val scope = ArchiveScope(ArchiveBackend.CLOUD, "profile", "server", "source")
    private val revision = ArchiveRevision(9u, 401uL)
    private val issued = ArchiveCheckpoint("source", "historical", revision)
    private fun row(sequence: Int, id: String = "item-$sequence") = ArchiveRow(
        ArchiveRowLocator(sequence.toULong(), id), ArchiveRowRole.USER,
        listOf("message $sequence"), "2026-11-23T18:00:00.123Z", 2u,
    )

    private inner class Fixture : ArchiveForkPort {
        var attached: ArchiveAttachment? = ArchiveAttachment(scope, 1, true)
        val requests = ArrayList<ArchiveForkRequest>()
        var action: () -> ArchiveForkReply = { ArchiveForkReply.Created("child") }
        var pageReply: ((ArchivePageRequest) -> ArchivePage)? = null
        var issueCalls = 0
        override suspend fun attachment() = attached
        override suspend fun issue(at: ArchiveAttachment, expected: ArchiveRevision): ArchiveCheckpoint {
            issueCalls++
            assertEquals(revision, expected)
            return issued
        }
        override suspend fun describe(at: ArchiveAttachment, checkpointId: String) = issued
        override suspend fun page(at: ArchiveAttachment, request: ArchivePageRequest): ArchivePage {
            assertEquals(64u.toUShort(), request.limit)
            assertEquals("source", request.sessionId)
            assertEquals("historical", request.checkpointId)
            pageReply?.invoke(request)?.let { return it }
            val last = request.beforeSequence?.toInt()?.minus(1) ?: 400
            val first = (last - request.limit.toInt() + 1).coerceAtLeast(1)
            return ArchivePage("source", "historical", (first..last).map(::row), first.takeIf { it > 1 }?.toULong())
        }
        override suspend fun fork(at: ArchiveAttachment, request: ArchiveForkRequest): ArchiveForkReply {
            requests += request
            return action()
        }
    }

    @Test fun noCapabilityOrLegacyRowCannotInvokeFork() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "operation" })
        port.attached = port.attached?.copy(archiveCheckpointV1 = false)
        controller.issue(revision)
        assertEquals(0, port.issueCalls)
        port.attached = port.attached?.copy(archiveCheckpointV1 = true)
        controller.fork(row(1).locator)
        assertTrue(port.requests.isEmpty())
    }

    @Test fun pagesBeyondTwoHundredRowsAndUsesExactOlderLocator() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "operation" })
        controller.issue(revision)
        assertEquals(337uL, controller.state.value.rows.first().locator.sequence)
        repeat(4) { controller.loadOlder() }
        val older = controller.state.value.rows
        assertEquals(64, older.size)
        assertEquals(81uL, older.first().locator.sequence)
        assertEquals(144uL, older.last().locator.sequence)
        assertEquals("2026-11-23T18:00:00.123Z", older.first().recordedAt)
        assertEquals(2u, older.first().imageCount)
        assertEquals("child", controller.fork(older.first().locator))
        assertEquals(ArchiveForkRequest("source", "historical", older.first().locator, "operation"), port.requests.single())
    }

    @Test fun unknownRetainsExactOperationAndRequiresExplicitSameScopeRetry() = runBlocking {
        val port = Fixture()
        var ids = 0
        val controller = ArchiveForkController(scope, port, { "operation-${++ids}" })
        controller.open(ArchiveCheckpointRef(scope, "historical"))
        val pick = controller.state.value.rows.last().locator
        port.action = { ArchiveForkReply.OutcomeUnknown }
        assertNull(controller.fork(pick))
        assertTrue(controller.state.value.outcomeUnknown)
        assertNull(controller.fork(controller.state.value.rows.first().locator))
        port.attached = ArchiveAttachment(scope.copy(serverId = "different-server"), 2, true)
        assertNull(controller.retryUnknown())
        assertEquals(1, port.requests.size)
        port.attached = ArchiveAttachment(scope, 3, true)
        port.action = { ArchiveForkReply.Created("exact-child") }
        assertEquals("exact-child", controller.retryUnknown())
        assertEquals(port.requests[0], port.requests[1])
        assertEquals(1, ids)
    }

    @Test fun sameItemIdAtDifferentSequencesIsNotCollapsed() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "once" })
        val earlier = row(1, "duplicate-id")
        val later = row(2, "duplicate-id")
        port.pageReply = { ArchivePage("source", "historical", listOf(earlier, later), null) }
        controller.issue(revision)
        assertEquals(listOf(earlier.locator, later.locator), controller.state.value.rows.map { it.locator })
        assertEquals("child", controller.fork(later.locator))
        assertEquals(later.locator, port.requests.single().row)
    }

    @Test fun wrongResponseScopeOrOrderDoesNotExposeForkableRows() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "operation" })
        port.pageReply = { ArchivePage("wrong-source", "historical", listOf(row(1)), null) }
        controller.issue(revision)
        assertTrue(controller.state.value.rows.isEmpty())
        port.pageReply = { ArchivePage("source", "historical", listOf(row(2), row(1)), null) }
        controller.issue(revision)
        assertTrue(controller.state.value.rows.isEmpty())
    }

    @Test fun disconnectOrUiSwitchAfterReplyNeverNavigatesToStaleChild() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "operation" })
        controller.issue(revision)
        val picked = controller.state.value.rows.first().locator
        port.action = {
            port.attached = ArchiveAttachment(scope.copy(profileId = "replacement"), 2, true)
            ArchiveForkReply.Created("real-child")
        }
        assertNull(controller.fork(picked))
        assertTrue(controller.state.value.outcomeUnknown)
        assertNull(controller.retryUnknown())
        assertEquals(1, port.requests.size)
    }
    @Test fun aReconnectedScopeMustRefreshThePageBeforeItCanSendANewFork() = runBlocking {
        val port = Fixture()
        val controller = ArchiveForkController(scope, port, { "new-op" })
        controller.issue(revision)
        val pick = controller.state.value.rows.first().locator
        port.attached = ArchiveAttachment(scope, 2, true)
        assertFalse(controller.state.value.rowsAreVerified(port.attached))
        assertNull(controller.fork(pick))
        assertTrue(port.requests.isEmpty())
        controller.loadNewest()
        assertTrue(controller.state.value.rowsAreVerified(port.attached))
        assertEquals("child", controller.fork(pick))
    }
}
