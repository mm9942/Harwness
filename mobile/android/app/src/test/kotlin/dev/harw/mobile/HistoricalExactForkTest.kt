package dev.harw.mobile

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

class HistoricalExactForkTest {
    private val scope = ArchiveScope(ArchiveBackend.CLOUD, "profile", "host", "source")
    private fun row(at: ULong, id: String, role: ArchiveRowRole = ArchiveRowRole.USER) =
        ArchiveRow(ArchiveRowLocator(at, id), role, listOf("identical text"), "2026-11-23T18:00:00Z", 2u)
    private fun cached(id: String, role: TranscriptKind = TranscriptKind.USER) =
        TranscriptEntry(id, role, "irrelevant local display text", null, listOf("ignored"), null)
    private fun prior(id: String, before: ULong) =
        ArchivePredecessor(ArchiveCheckpoint("source", id, ArchiveRevision(3u, before)), before, "2026-11-23T18:00:00Z", true)

    private inner class Host : ArchiveForkPort {
        var attachment = ArchiveAttachment(scope, 7L, true, true)
        var previous = listOf(prior("second", 50uL), prior("first", 30uL))
        val pages = hashMapOf<Pair<String, ULong?>, ArchivePage>()
        val sent = mutableListOf<ArchiveForkRequest>()
        var issues = 0
        var afterPage: () -> Unit = {}
        var reply: ArchiveForkReply = ArchiveForkReply.Created("child")
        override suspend fun attachment() = attachment
        override suspend fun issue(at: ArchiveAttachment, expected: ArchiveRevision): ArchiveCheckpoint {
            issues++
            return ArchiveCheckpoint("source", "current", expected)
        }
        override suspend fun describe(at: ArchiveAttachment, checkpointId: String) = error("not used")
        override suspend fun previous(at: ArchiveAttachment, beforeSequence: ULong?): ArchivePredecessor? =
            previous.firstOrNull { beforeSequence == null || it.beforeSequence < beforeSequence }
        override suspend fun page(at: ArchiveAttachment, request: ArchivePageRequest): ArchivePage {
            afterPage()
            return pages.getValue(request.checkpointId to request.beforeSequence)
        }
        override suspend fun fork(at: ArchiveAttachment, request: ArchiveForkRequest): ArchiveForkReply {
            sent += request
            return reply
        }
        fun page(id: String, vararg rows: ArchiveRow, before: ULong? = null, next: ULong? = null) {
            pages[id to before] = ArchivePage("source", id, rows.toList(), next)
        }
    }

    @Test fun laterHistoricalRowForksItsExactInclusiveCheckpointWithoutLiveRevisionOrMedia() = runBlocking {
        val port = Host()
        port.page("second", row(33uL, "same-text-new-id"))
        port.page("first", row(22uL, "after-selection"), next = 22uL)
        port.page("first", row(1uL, "prior"), row(9uL, "historical-row"), before = 22uL)
        val selection = checkNotNull(selectedForkRow(listOf(cached("prior"), cached("historical-row")), 1, 0))
        val out = ArchiveForkController(scope, port, { "operation" })
        assertEquals("child", out.forkDisplayed(selection, null) { true })
        assertEquals(0, port.issues)
        assertEquals(ArchiveForkRequest("source", "first", ArchiveRowLocator(9uL, "historical-row"), "operation"), port.sent.single())
    }

    @Test fun sameIdWithDifferentRetainedSequenceCannotChooseBasedOnTextOrRecency() = runBlocking {
        val port = Host()
        port.page("second", row(33uL, "historical-row"))
        port.page("first", row(9uL, "historical-row"))
        val selection = checkNotNull(selectedForkRow(listOf(cached("historical-row")), 0, 0))
        val out = ArchiveForkController(scope, port)
        assertNull(out.forkDisplayed(selection, null) { true })
        assertTrue(port.sent.isEmpty())
        assertTrue(out.state.value.notice.orEmpty().contains("Mehrere"))
    }

    @Test fun missingExactIdOrWrongPredecessorLeavesOriginalAndReportsNotRetained() = runBlocking {
        val port = Host()
        port.page("second", row(2uL, "different-prefix"), row(33uL, "historical-row"))
        port.page("first", row(9uL, "same-text-not-historical"))
        val selection = checkNotNull(selectedForkRow(listOf(cached("prior"), cached("historical-row")), 1, 1))
        val out = ArchiveForkController(scope, port)
        assertNull(out.forkDisplayed(selection, null) { true })
        assertTrue(port.sent.isEmpty())
        assertTrue(out.state.value.notice.orEmpty().contains("nicht erhalten"))
    }

    @Test fun routeCloseAfterPageBeforeDispatchCannotCreateEvenOnTheSameAttachment() = runBlocking {
        val port = Host()
        val selection = checkNotNull(selectedForkRow(listOf(cached("visible")), 0, Int.MAX_VALUE))
        port.page("current", row(11uL, "visible"))
        var open = true
        port.afterPage = { open = false }
        val out = ArchiveForkController(scope, port)
        assertNull(out.forkDisplayed(selection, ArchiveRevision(3u, 15uL)) { open })
        assertEquals(1, port.issues)
        assertTrue(port.sent.isEmpty())
    }
    @Test fun displayedArrowForkCarriesEntirePrefixAndUsesSameOperationAfterLostReply() = runBlocking {
        val port = Host()
        port.page("second", row(4uL, "current-other"))
        port.page("first", row(2uL, "not-visible"), row(8uL, "question"),
            row(9uL, "answer", ArchiveRowRole.ASSISTANT))
        val entries = listOf(cached("question"), cached("answer", TranscriptKind.ASSISTANT))
        val selection = checkNotNull(retainedDisplayedSelection(entries))
        var next = 0
        val out = ArchiveForkController(scope, port, { "operation-${next++}" })
        port.reply = ArchiveForkReply.OutcomeUnknown
        assertNull(out.forkDisplayed(selection, null, entries) { true })
        assertTrue(out.state.value.outcomeUnknown)
        assertEquals(entries, port.sent.single().displayedPrefix)
        assertEquals(ArchiveRowLocator(9uL, "answer"), port.sent.single().row)
        assertNull(out.forkDisplayed(selection, null, entries.map { it.copy(text = "changed") }) { true })
        assertEquals(1, port.sent.size)
        port.reply = ArchiveForkReply.Created("child")
        assertEquals("child", out.forkDisplayed(selection, null, entries) { true })
        assertEquals(port.sent.first(), port.sent.last())
        assertEquals(1, next)
        assertEquals(0, port.issues)
    }

}
