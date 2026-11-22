package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileHolder
import uniffi.harw_mobile_ffi.MobileHolderKind
import uniffi.harw_mobile_ffi.MobileRowOrigin
import uniffi.harw_mobile_ffi.MobileSessionRow
import uniffi.harw_mobile_ffi.MobileSessionState

class HarwConnectionProjectsTest {
    private fun row(id: String, title: String? = null, holder: MobileHolder? = null) = MobileSessionRow(
        sessionId = id, title = title, projectKey = "alpha-aaaaaaaaaaaa", updatedAt = "2026-11-22T10:00:00Z",
        turns = 1uL, state = MobileSessionState.IDLE, attached = 0u, model = null,
        origin = MobileRowOrigin.HOSTED, continuable = true, reason = null, holder = holder,
        lastOrigin = null, firstMessage = null,
    )

    @Test
    fun mergeAppendsAndDeduplicates() {
        val merged = mergeSessionRows(listOf(row("s1"), row("s2")), listOf(row("s2", "neu"), row("s3")), replace = false)
        assertEquals(listOf("s1", "s2", "s3"), merged.map { it.sessionId })
        assertEquals("neu", merged[1].title)
    }

    @Test
    fun mergeReplaceDropsOldRows() {
        assertEquals(listOf("s9"), mergeSessionRows(listOf(row("s1")), listOf(row("s9")), replace = true).map { it.sessionId })
    }

    @Test
    fun replaceAndRemoveTouchOnlyTheTarget() {
        val rows = listOf(row("s1"), row("s2"))
        assertEquals(listOf(null, "x"), replaceSessionRow(rows, row("s2", "x")).map { it.title })
        assertEquals(listOf("s1"), withoutSessionRow(rows, "s2").map { it.sessionId })
    }

    @Test
    fun applyChangeUpdatesProjectView() {
        val view = ProjectSessionsState(project = null, rows = listOf(row("s1"), row("s2")), error = "alt")
        assertSame(view, view.applyChange(ProjectChange.None))
        val removed = view.applyChange(ProjectChange.Remove("s1"))
        assertEquals(listOf("s2"), removed.rows.map { it.sessionId })
        assertNull(removed.error)
        assertEquals("t", view.applyChange(ProjectChange.Replace(row("s2", "t"))).rows[1].title)
    }

    @Test
    fun busyMessageUsesDetailOrFallback() {
        assertEquals("Läuft gerade auf dem Gateway", busyDraftMessage("Läuft gerade auf dem Gateway"))
        assertEquals("Die Session ist gerade belegt.", busyDraftMessage(" "))
        assertEquals("Die Session ist gerade belegt.", busyDraftMessage(null))
    }

    @Test
    fun windowStateKeepsHolder() {
        val holder = MobileHolder(MobileHolderKind.GATEWAY, "2026-11-22T10:00:00Z")
        assertEquals(holder, WindowState().copy(holder = holder).holder)
        assertNull(WindowState().holder)
    }
}
