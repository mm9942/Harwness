package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

class ChatVersionsDedupeTest {
    private fun user(id: String, vararg refs: String) =
        TranscriptEntry(id, TranscriptKind.USER, "Same prompt", null, refs.toList(), null)

    @Test
    fun separateStableIdsKeepTheirOwnImageReferencesInOrder() {
        val first = user("first", "one", "two")
        val second = user("second", "three", "four")
        val shown = dedupeUserRuns(listOf(first, second))
        assertEquals(listOf("first", "second"), shown.map { it.id })
        assertEquals(listOf(listOf("one", "two"), listOf("three", "four")), shown.map { it.images })
        assertEquals("second", selectedForkRow(shown, 1, Int.MAX_VALUE)?.selected?.itemId)
    }

    @Test
    fun separateStableIdsStayVisibleEvenWhenContentAndReferencesMatch() {
        val first = user("first", "shared")
        val second = user("second", "shared")
        assertEquals(listOf(first, second), dedupeUserRuns(listOf(first, second)))
    }

    @Test
    fun versionNavigationKeepsPreviousSuffixSeparateFromActiveHistory() {
        val prefix = TranscriptEntry("prior", TranscriptKind.ASSISTANT, "Earlier answer", null, emptyList(), null)
        val first = user("original", "old-asset")
        val firstReply = TranscriptEntry("old-reply", TranscriptKind.ASSISTANT, "Original answer", null, emptyList(), null)
        val later = user("following", "later-asset")
        val regenerated = user("new-id", "old-asset")
        val live = listOf(prefix, regenerated)
        val saved = mapOf("prior" to listOf(ChatVersion(listOf(first, firstReply, later))))
        fun display(index: Int?) = applyViewing(live, VersionsUi(
            saved, if (index == null) emptyMap() else mapOf("prior" to index), { _, _ -> }, { _, _, _ -> },
        ))
        assertEquals(listOf(prefix, first, firstReply, later) to 1, display(0))
        assertEquals(live to Int.MAX_VALUE, display(null))
        assertEquals(listOf("prior", "new-id"), live.map { it.id })
    }

    @Test
    fun consecutiveReplayOfSameStableItemStillCollapses() {
        val event = user("same-event", "one", "two")
        assertEquals(listOf(event), dedupeUserRuns(listOf(event, event.copy())))
    }
}
