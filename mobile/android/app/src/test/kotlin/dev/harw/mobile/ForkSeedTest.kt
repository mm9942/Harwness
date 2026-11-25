package dev.harw.mobile

import org.junit.Assert.*
import org.junit.Test

class ForkSeedTest {
    private val origin = ForkSeedOrigin(ForkSeedBackend.CLOUD, "profile-a", "source", 5)
    private fun row(id: String, role: ForkVisibleRole, text: String = id) =
        ForkVisibleRow(id, role, text)

    @Test fun branchUsesOnlyVisibleStablePrefixAndLeavesSelectedUserDraftUnsent() {
        val turns = listOf(
            row("user-1", ForkVisibleRole.USER).copy(timestampMillis = 1000),
            row("tool", ForkVisibleRole.TOOL, "tool-secret"),
            row("harw-2", ForkVisibleRole.HARW, "quote:\" line:\n").copy(timestampMillis = 2000),
            row("reason", ForkVisibleRole.REASONING, "private-thought"),
            row("old-user", ForkVisibleRole.USER).copy(images = listOf(ForkSeedImage(origin,"ref-A"))),
            row("newer", ForkVisibleRole.HARW, "newer-unrelated"),
        )
        val plan = requireNotNull(deriveForkSeed(origin, origin, turns, "old-user"))
        val json = requireNotNull(plan.jsonFor(origin))
        assertEquals(listOf("user-1", "harw-2"), plan.prefix.map { it.id })
        assertEquals("old-user", plan.draft?.id)
        assertEquals(listOf("ref-A"), plan.draft?.images?.map { it.reference })
        assertTrue(json.contains("\"timestamp_ms\":1000"))
        assertTrue(json.contains("\"timestamp_ms\":null"))
        assertTrue(json.contains("\"unsent_draft\":"))
        assertTrue(json.contains("quote:\\\" line:\\n"))
        listOf("tool-secret", "private-thought", "newer-unrelated", "profile-a", "\"source\"").forEach {
            assertFalse(json.contains(it))
        }
    }

    @Test fun scopeMustMatchBothCaptureAndSerializationAndMediaCannotCrossScope() {
        val other = origin.copy(profileId = "profile-b")
        val selected = row("user", ForkVisibleRole.USER)
        assertNull(deriveForkSeed(origin, origin.copy(generation = 6), listOf(selected), "user"))
        assertNull(deriveForkSeed(origin, origin, listOf(selected.copy(images = listOf(ForkSeedImage(other, "ref")))), "user"))
        val seed = requireNotNull(deriveForkSeed(origin, origin, listOf(selected), "user"))
        assertNull(seed.jsonFor(other))
        assertNull(seed.jsonFor(origin.copy(backend = ForkSeedBackend.LOCAL)))
        assertNull(seed.jsonFor(origin.copy(sessionId = "other-room")))
    }

    @Test fun rejectsUnknownAmbiguousUnsupportedOrOversizeRatherThanCloningLatest() {
        val selected = row("user", ForkVisibleRole.USER)
        assertNull(deriveForkSeed(origin, origin, listOf(selected), "missing"))
        assertNull(deriveForkSeed(origin, origin, listOf(selected, selected), "user"))
        assertNull(deriveForkSeed(origin, origin, listOf(selected.copy(role = ForkVisibleRole.TOOL)), "user"))
        assertNull(deriveForkSeed(origin, origin, listOf(selected.copy(text = "x".repeat(130_000))), "user"))
        val unavailable = selected.copy(imagesUnavailable = true)
        assertNull(deriveForkSeed(origin, origin, listOf(unavailable), "user"))
    }

    @Test fun earlierImageRefsStayOrderedAndLaterMissingMediaDoesNotMatter() {
        val a = row("a", ForkVisibleRole.USER).copy(images = listOf(
            ForkSeedImage(origin, "ref-first"), ForkSeedImage(origin, "ref-second"),
        ))
        val b = row("b", ForkVisibleRole.USER)
        val c = row("c", ForkVisibleRole.USER).copy(imagesUnavailable = true)
        val seed = requireNotNull(deriveForkSeed(origin, origin, listOf(a, b, c), "b"))
        assertEquals(listOf("ref-first", "ref-second"), seed.prefix.single().images.map { it.reference })
    }
    @Test fun harwTargetIsIncludedAndHasNoDraft() {
        val turns = listOf(row("u", ForkVisibleRole.USER), row("h", ForkVisibleRole.HARW), row("later", ForkVisibleRole.USER))
        val plan = requireNotNull(deriveForkSeed(origin, origin, turns, "h"))
        assertEquals(listOf("u", "h"), plan.prefix.map { it.id })
        assertNull(plan.draft)
        assertTrue(requireNotNull(plan.jsonFor(origin)).contains("\"unsent_draft\":null"))
    }
}
